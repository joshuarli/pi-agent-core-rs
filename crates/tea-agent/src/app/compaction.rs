//! Provider-backed compaction policy for the repository-owned terminal host.
//!
//! The core deliberately does not choose a summary prompt or provider. This
//! host supplies both explicitly by reusing the selected provider for a small,
//! tool-free summarization request and retaining the exact suffix selected by
//! the core's automatic-compaction split.

use tea_core::compaction::{
    AutomaticCompactionRequest, CompactionContext, CompactionError, CompactionFuture,
    CompactionResult, Compactor, ProviderContext,
};
use tea_core::hooks::{ContextEnvelope, HookSet};
use tea_core::provider::openai::OpenAiContextHook;
use tea_core::scheduler::{CancellationToken, ModelProvider, ModelRequest, ModelStreamEvent};
use tea_core::state::{AgentMessage, MessageId, ModelDescriptor, StopReason, ThinkingLevel};
use tea_core::Usage;
use tea_protocol::JsonValue;
use std::collections::BTreeSet;
use std::fmt;
use std::sync::{Arc, RwLock};

const SUMMARY_SYSTEM_PROMPT: &str = r#"You compact coding-agent conversation history.
Produce a concise structured summary that preserves everything needed to continue the work.
Use exactly these Markdown sections:

## Goal
[The user's active goal]

## Constraints & Preferences
- [Requirements and preferences]

## Progress
### Done
- [Completed work]

### In Progress
- [Current work]

### Blocked
- [Current blockers, or "None"]

## Key Decisions
- [Important decisions and rationale]

## Next Steps
1. [The next concrete actions]

## Critical Context
- [Exact paths, symbols, errors, and other details needed to continue]

Be concise, preserve exact file paths and identifiers, and do not omit unresolved work."#;

const SUMMARY_PREFIX: &str =
    "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";
const SUMMARY_SUFFIX: &str = "\n</summary>";
const UPDATE_SUMMARIZATION_INSTRUCTIONS: &str = r#"Update the existing compacted summary using the conversation above.
Preserve all durable facts needed to continue the work, including exact paths, symbols, errors,
constraints, unresolved work, and the next concrete actions. Return only the updated summary using
the same Markdown sections as the system instructions; do not call tools."#;
const CACHE_FRIENDLY_CONTEXT_SAFETY_MARGIN: u64 = 4_096;

/// A compactor whose provider/model pair follows the TUI's idle model selection.
pub(super) struct ProviderCompactor {
    provider: RwLock<Option<Arc<dyn ModelProvider>>>,
    model: RwLock<Option<ModelDescriptor>>,
}

impl fmt::Debug for ProviderCompactor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderCompactor")
            .field(
                "model",
                &self.model.read().ok().and_then(|model| model.clone()),
            )
            .finish_non_exhaustive()
    }
}

impl Default for ProviderCompactor {
    fn default() -> Self {
        Self {
            provider: RwLock::new(None),
            model: RwLock::new(None),
        }
    }
}

impl ProviderCompactor {
    /// Set the explicit provider/model used for future compaction requests.
    pub(super) fn configure(&self, model: ModelDescriptor, provider: Arc<dyn ModelProvider>) {
        *self
            .provider
            .write()
            .expect("TUI compactor provider lock poisoned") = Some(provider);
        *self
            .model
            .write()
            .expect("TUI compactor model lock poisoned") = Some(model);
    }

    fn configured(
        &self,
        context: &CompactionContext,
    ) -> Result<(Arc<dyn ModelProvider>, ModelDescriptor), CompactionError> {
        let provider = self
            .provider
            .read()
            .expect("TUI compactor provider lock poisoned")
            .clone()
            .ok_or_else(|| CompactionError::failed("no selected provider for compaction"))?;
        let model = context
            .model
            .clone()
            .or_else(|| {
                self.model
                    .read()
                    .expect("TUI compactor model lock poisoned")
                    .clone()
            })
            .ok_or_else(|| CompactionError::failed("no selected model for compaction"))?;
        Ok((provider, model))
    }
}

impl Compactor for ProviderCompactor {
    fn compact<'a>(
        &'a self,
        context: CompactionContext,
        cancellation: CancellationToken,
    ) -> CompactionFuture<'a> {
        let configured = self.configured(&context);
        Box::pin(async move {
            let (provider, model) = configured?;
            if context.messages.is_empty() {
                return Ok(CompactionResult::new(Vec::new()));
            }
            let (summary, usage) = summarize(
                provider,
                model,
                context.messages.clone(),
                None,
                cancellation,
            )
            .await?;
            let replacement = vec![summary_message(&context.messages, summary)];
            Ok(match usage {
                Some(usage) => CompactionResult::new(replacement).with_usage(usage),
                None => CompactionResult::new(replacement),
            })
        })
    }

    fn compact_automatic<'a>(
        &'a self,
        context: CompactionContext,
        request: AutomaticCompactionRequest,
        cancellation: CancellationToken,
    ) -> CompactionFuture<'a> {
        let configured = self.configured(&context);
        Box::pin(async move {
            let (provider, model) = configured?;
            let source_context = context
                .provider_context
                .as_ref()
                .filter(|source| source_context_fits(source, &request));
            let mut messages_to_summarize = request.prefix_messages;
            messages_to_summarize.extend(request.split_turn_prefix);
            if messages_to_summarize.is_empty() {
                return Ok(CompactionResult::new(request.retained_messages));
            }
            let (summary, usage) = summarize(
                provider,
                model,
                messages_to_summarize.clone(),
                source_context,
                cancellation,
            )
            .await?;
            let retained_messages = request.retained_messages;
            let mut all_messages = messages_to_summarize;
            all_messages.extend(retained_messages.iter().cloned());
            let mut replacement = vec![summary_message(&all_messages, summary)];
            replacement.extend(retained_messages);
            Ok(match usage {
                Some(usage) => CompactionResult::new(replacement).with_usage(usage),
                None => CompactionResult::new(replacement),
            })
        })
    }
}

fn summary_message(messages: &[AgentMessage], summary: String) -> AgentMessage {
    AgentMessage::User {
        id: next_message_id(messages),
        content: format!("{SUMMARY_PREFIX}{summary}{SUMMARY_SUFFIX}"),
    }
}

fn next_message_id(messages: &[AgentMessage]) -> MessageId {
    let used = messages
        .iter()
        .map(|message| match message {
            AgentMessage::User { id, .. }
            | AgentMessage::Assistant { id, .. }
            | AgentMessage::ToolResult { id, .. } => id.0,
        })
        .collect::<BTreeSet<_>>();
    let mut candidate = 1_u64;
    while used.contains(&candidate) {
        candidate = candidate.saturating_add(1);
    }
    MessageId(candidate)
}

async fn summarize(
    provider: Arc<dyn ModelProvider>,
    model: ModelDescriptor,
    messages: Vec<AgentMessage>,
    source_context: Option<&ProviderContext>,
    cancellation: CancellationToken,
) -> Result<(String, Option<Usage>), CompactionError> {
    let (system_prompt, context, tools) = if let Some(source) = source_context {
        (
            source.system_prompt.clone(),
            append_update_instruction(&source.context)?,
            source.tools.clone(),
        )
    } else {
        let context = OpenAiContextHook
            .convert_to_llm(ContextEnvelope {
                version: 1,
                messages,
                host_messages: Vec::new(),
            })
            .map_err(|error| CompactionError::failed(error.to_string()))?;
        (SUMMARY_SYSTEM_PROMPT.into(), context, Vec::new())
    };
    let request = ModelRequest {
        system_prompt,
        context,
        tools,
        model: Some(model),
        thinking_level: ThinkingLevel::Off,
    };
    let mut stream = provider
        .stream(request, cancellation.clone())
        .await
        .map_err(|error| CompactionError::failed(error.to_string()))?;
    let mut summary = String::new();
    let mut usage = None;
    loop {
        if cancellation.is_cancelled() {
            return Err(CompactionError::failed("compaction cancelled"));
        }
        let event = stream
            .next_event(cancellation.clone())
            .await
            .map_err(|error| CompactionError::failed(error.to_string()))?
            .ok_or_else(|| {
                CompactionError::failed("compaction provider closed without a terminal event")
            })?;
        match event {
            ModelStreamEvent::TextDelta(delta) => summary.push_str(&delta),
            ModelStreamEvent::Usage(reported) => usage = Some(reported),
            ModelStreamEvent::End(reason) => {
                if reason == StopReason::Error {
                    return Err(CompactionError::failed(
                        "compaction provider ended with an error",
                    ));
                }
                break;
            }
            ModelStreamEvent::Error { message }
            | ModelStreamEvent::ContextOverflow { message }
            | ModelStreamEvent::Aborted { message } => {
                return Err(CompactionError::failed(message));
            }
            ModelStreamEvent::ToolCall(_) => {
                return Err(CompactionError::failed(
                    "compaction provider returned a tool call instead of a summary",
                ));
            }
        }
    }
    if summary.trim().is_empty() {
        return Err(CompactionError::failed(
            "compaction provider returned an empty summary",
        ));
    }
    Ok((summary, usage))
}

fn source_context_fits(source: &ProviderContext, request: &AutomaticCompactionRequest) -> bool {
    if let Some(active_context) = &source.active_context {
        if !is_exact_message_prefix(&source.context, active_context) {
            return false;
        }
    }
    let tool_bytes = source
        .tools
        .iter()
        .map(|tool| {
            tool.schema
                .to_json_string()
                .map_or(0, |schema| schema.len())
                .saturating_add(tool.name.len())
                .saturating_add(tool.description.len())
        })
        .sum::<usize>();
    let source_bytes = source
        .system_prompt
        .len()
        .saturating_add(source.context.len())
        .saturating_add(tool_bytes);
    let source_tokens = (source_bytes as u64).saturating_add(3) / 4;
    source_tokens
        .saturating_add(request.reserved_tokens)
        .saturating_add(CACHE_FRIENDLY_CONTEXT_SAFETY_MARGIN)
        <= request.context_budget_tokens
}

fn is_exact_message_prefix(source: &str, active: &str) -> bool {
    let Ok(JsonValue::Array(source_messages)) = JsonValue::parse(source) else {
        return false;
    };
    let Ok(JsonValue::Array(active_messages)) = JsonValue::parse(active) else {
        return false;
    };
    active_messages.starts_with(&source_messages)
}

fn append_update_instruction(context: &str) -> Result<String, CompactionError> {
    let mut value = JsonValue::parse(context).map_err(|error| {
        CompactionError::failed(format!("active provider context is not JSON: {error}"))
    })?;
    let JsonValue::Array(messages) = &mut value else {
        return Err(CompactionError::failed(
            "active provider context is not a message array",
        ));
    };
    messages.push(JsonValue::object([
        ("role", JsonValue::from("user")),
        (
            "content",
            JsonValue::from(UPDATE_SUMMARIZATION_INSTRUCTIONS),
        ),
    ]));
    value
        .to_json_string()
        .map_err(|error| CompactionError::failed(error.to_string()))
}
