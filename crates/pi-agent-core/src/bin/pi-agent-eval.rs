//! Opt-in OpenRouter coding-evaluation adapter for the Rust default profile.
//!
//! This binary is intentionally outside the library boundary: it is invoked only by the final
//! V0 evaluation controller through a caller-owned `vault OPENROUTER_API_KEY -- …` command. It
//! supplies a concrete transport to exercise the otherwise provider-free core, while retaining
//! the core's explicit workspace, profile, and Smol-owned execution boundaries.

use pi_agent_core::event::AgentEventKind;
use pi_agent_core::hooks::{AfterToolCall, BeforeToolCall, ContextEnvelope, HookSet, NextTurn};
use pi_agent_core::scheduler::{
    CancellationToken, ModelFuture, ModelProvider, ModelRequest, ModelStream, ModelStreamEvent,
};
use pi_agent_core::state::{
    AssistantToolCall, Message, ModelDescriptor, SerializedJson, StopReason, ToolCallId, Usage,
};
use pi_agent_core::{Agent, DefaultCodingTools};
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

const RESULT_SCHEMA: &str = "pi-coding-eval-result/v0";
const OPENROUTER_COMPLETIONS_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

/// Explicit command-line arguments supplied by `evals/controller.py`.
struct Args {
    model: String,
    task_json: PathBuf,
    workspace: PathBuf,
    capabilities_json: PathBuf,
    result_json: PathBuf,
    attempt_id: String,
    baseline_id: String,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut values = std::collections::BTreeMap::<String, String>::new();
        let mut arguments = env::args().skip(1);
        while let Some(flag) = arguments.next() {
            if !flag.starts_with("--") {
                return Err(format!("unexpected positional argument {flag:?}"));
            }
            let value = arguments
                .next()
                .ok_or_else(|| format!("missing value for {flag}"))?;
            if values.insert(flag, value).is_some() {
                return Err("evaluation adapter arguments must not repeat flags".into());
            }
        }
        let take = |flag: &str| {
            values
                .get(flag)
                .filter(|value| !value.is_empty())
                .cloned()
                .ok_or_else(|| format!("missing required argument {flag}"))
        };
        Ok(Self {
            model: take("--model")?,
            task_json: PathBuf::from(take("--task-json")?),
            workspace: PathBuf::from(take("--workspace")?),
            capabilities_json: PathBuf::from(take("--capabilities-json")?),
            result_json: PathBuf::from(take("--result-json")?),
            attempt_id: take("--attempt-id")?,
            baseline_id: take("--baseline-id")?,
        })
    }
}

/// The exact context-to-OpenAI-message conversion needed by this evaluation transport.
///
/// It is an adapter hook, not core behavior. The durable core transcript remains its own typed
/// data; this hook serializes a narrow, standard Chat Completions message array for the selected
/// OpenRouter transport.
#[derive(Debug, Default)]
struct OpenAiContextHook;

impl HookSet for OpenAiContextHook {
    fn before_tool_call(
        &self,
        _call: &pi_agent_core::tool::ToolCall,
    ) -> Result<BeforeToolCall, pi_agent_core::error::HookError> {
        Ok(BeforeToolCall::Allow)
    }

    fn after_tool_call(
        &self,
        _call: &pi_agent_core::tool::ToolCall,
        _result: &pi_agent_core::tool::ToolResult,
    ) -> Result<AfterToolCall, pi_agent_core::error::HookError> {
        Ok(AfterToolCall::default())
    }

    fn transform_context(
        &self,
        context: ContextEnvelope,
    ) -> Result<ContextEnvelope, pi_agent_core::error::HookError> {
        Ok(context)
    }

    fn convert_to_llm(
        &self,
        context: ContextEnvelope,
    ) -> Result<String, pi_agent_core::error::HookError> {
        let messages = context
            .messages
            .iter()
            .map(openai_message)
            .collect::<Result<Vec<_>, _>>()?;
        serde_json::to_string(&messages).map_err(|error| {
            pi_agent_core::error::HookError::new("convert_to_llm", error.to_string())
        })
    }

    fn should_stop_after_turn(
        &self,
        _context: &ContextEnvelope,
    ) -> Result<bool, pi_agent_core::error::HookError> {
        Ok(false)
    }

    fn prepare_next_turn(
        &self,
        _context: ContextEnvelope,
    ) -> Result<NextTurn, pi_agent_core::error::HookError> {
        Ok(NextTurn::default())
    }
}

fn openai_message(message: &Message) -> Result<Value, pi_agent_core::error::HookError> {
    match message {
        Message::User { content, .. } => Ok(json!({"role": "user", "content": content})),
        Message::Assistant {
            content,
            tool_calls,
            ..
        } => {
            let calls = tool_calls
                .iter()
                .map(|call| {
                    json!({
                        "id": call.id.as_str(),
                        "type": "function",
                        "function": {
                            "name": call.name,
                            "arguments": call.arguments.as_str(),
                        },
                    })
                })
                .collect::<Vec<_>>();
            Ok(json!({
                "role": "assistant",
                "content": if content.is_empty() { Value::Null } else { Value::String(content.clone()) },
                "tool_calls": calls,
            }))
        }
        Message::ToolResult {
            tool_call_id,
            content,
            ..
        } => Ok(json!({
            "role": "tool",
            "tool_call_id": tool_call_id.as_str(),
            "content": content,
        })),
    }
}

/// Aggregate, redacted token counters across each OpenRouter turn.
#[derive(Clone, Debug, Default)]
struct UsageTotals(Arc<Mutex<Usage>>);

impl UsageTotals {
    fn add(&self, usage: Usage) {
        let mut totals = self.0.lock().expect("evaluation usage mutex poisoned");
        totals.input_tokens =
            Some(totals.input_tokens.unwrap_or(0) + usage.input_tokens.unwrap_or(0));
        totals.output_tokens =
            Some(totals.output_tokens.unwrap_or(0) + usage.output_tokens.unwrap_or(0));
        totals.reasoning_tokens =
            Some(totals.reasoning_tokens.unwrap_or(0) + usage.reasoning_tokens.unwrap_or(0));
    }

    fn snapshot(&self) -> Usage {
        self.0
            .lock()
            .expect("evaluation usage mutex poisoned")
            .clone()
    }
}

/// Small blocking OpenRouter adapter used only by this executable evaluation binary.
///
/// The core has no transport dependency. This adapter returns one finite `ModelStream` after a
/// standard non-streaming Chat Completions request, which is sufficient to exercise the actual
/// agent/tool loop and preserves the V0 core's caller-owned execution model.
#[derive(Debug)]
struct OpenRouterProvider {
    api_key: String,
    model: String,
    max_tokens: u64,
    usage: UsageTotals,
}

impl OpenRouterProvider {
    fn response_stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelStream {
        if cancellation.is_cancelled() {
            return ModelStream {
                events: vec![ModelStreamEvent::End(StopReason::Cancelled)],
            };
        }
        match self.complete(request) {
            Ok((events, usage)) => {
                self.usage.add(usage.clone());
                let mut events = events;
                if usage.input_tokens.is_some() || usage.output_tokens.is_some() {
                    // V0 streams cannot deliver any event after `End`; usage is part of the
                    // provider response and must precede the terminal settlement event.
                    let terminal = events
                        .pop()
                        .expect("parsed OpenRouter response has terminal event");
                    events.push(ModelStreamEvent::Usage(usage));
                    events.push(terminal);
                }
                ModelStream { events }
            }
            Err(message) => ModelStream {
                events: vec![ModelStreamEvent::Error { message }],
            },
        }
    }

    fn complete(&self, request: ModelRequest) -> Result<(Vec<ModelStreamEvent>, Usage), String> {
        let messages: Vec<Value> = serde_json::from_str(&request.context)
            .map_err(|_| "evaluation transport received invalid converted context".to_owned())?;
        let mut chat_messages = Vec::with_capacity(messages.len() + 1);
        chat_messages.push(json!({"role": "system", "content": request.system_prompt}));
        chat_messages.extend(messages);
        let tools = request
            .tools
            .iter()
            .map(|tool| {
                let schema = serde_json::from_str::<Value>(
                    &tool
                        .schema
                        .to_json_string()
                        .map_err(|_| "captured tool schema cannot be serialized")?,
                )
                .map_err(|_| "captured tool schema cannot be decoded")?;
                Ok(json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": schema,
                    },
                }))
            })
            .collect::<Result<Vec<_>, &str>>()?;
        let mut payload = json!({
            "model": self.model,
            "messages": chat_messages,
            "temperature": 0,
            "max_tokens": self.max_tokens,
            "stream": false,
        });
        if !tools.is_empty() {
            payload["tools"] = Value::Array(tools);
        }
        let payload = serde_json::to_vec(&payload)
            .map_err(|_| "cannot serialize OpenRouter evaluation request".to_owned())?;
        let mut child = Command::new("curl")
            .arg("--silent")
            .arg("--show-error")
            .arg("--connect-timeout")
            .arg("10")
            .arg("--max-time")
            .arg("30")
            .arg("--request")
            .arg("POST")
            .arg(OPENROUTER_COMPLETIONS_URL)
            .arg("--header")
            .arg("Content-Type: application/json")
            .arg("--header")
            .arg(format!("Authorization: Bearer {}", self.api_key))
            .arg("--data-binary")
            .arg("@-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "could not start the evaluation HTTP transport".to_owned())?;
        {
            use std::io::Write;
            let stdin = child
                .stdin
                .as_mut()
                .ok_or_else(|| "evaluation HTTP transport has no request pipe".to_owned())?;
            stdin
                .write_all(&payload)
                .map_err(|_| "could not send evaluation request".to_owned())?;
        }
        let output = child
            .wait_with_output()
            .map_err(|_| "evaluation HTTP transport did not settle".to_owned())?;
        if !output.status.success() {
            return Err("evaluation HTTP transport failed before a provider response".into());
        }
        parse_openrouter_response(&output.stdout)
    }
}

impl ModelProvider for OpenRouterProvider {
    fn stream<'a>(
        &'a self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelFuture<'a> {
        let stream = self.response_stream(request, cancellation);
        Box::pin(std::future::ready(Ok(Box::new(stream) as _)))
    }
}

fn parse_openrouter_response(bytes: &[u8]) -> Result<(Vec<ModelStreamEvent>, Usage), String> {
    let response: Value = serde_json::from_slice(bytes)
        .map_err(|_| "OpenRouter returned a non-JSON evaluation response".to_owned())?;
    if response.get("error").is_some() {
        return Err("OpenRouter rejected the evaluation request".into());
    }
    let choice = response
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(|| "OpenRouter response did not contain a completion choice".to_owned())?;
    let message = choice
        .get("message")
        .and_then(Value::as_object)
        .ok_or_else(|| "OpenRouter completion choice did not contain a message".to_owned())?;
    let mut events = Vec::new();
    if let Some(content) = message.get("content").and_then(Value::as_str) {
        if !content.is_empty() {
            events.push(ModelStreamEvent::TextDelta(content.to_owned()));
        }
    }
    let mut has_tool_calls = false;
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for (index, call) in calls.iter().enumerate() {
            let id = call
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("openrouter-call-{index}"));
            let function = call
                .get("function")
                .and_then(Value::as_object)
                .ok_or_else(|| "OpenRouter tool call did not contain a function".to_owned())?;
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| "OpenRouter tool call did not contain a name".to_owned())?;
            let arguments = function
                .get("arguments")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    "OpenRouter tool call did not contain serialized arguments".to_owned()
                })?;
            events.push(ModelStreamEvent::ToolCall(AssistantToolCall {
                id: ToolCallId::new(id)
                    .map_err(|_| "OpenRouter tool call omitted its identifier".to_owned())?,
                name: name.to_owned(),
                arguments: SerializedJson::new(arguments),
            }));
            has_tool_calls = true;
        }
    }
    let stop_reason = match choice.get("finish_reason").and_then(Value::as_str) {
        Some("tool_calls" | "tool_call") if has_tool_calls => StopReason::ToolUse,
        Some("length") => StopReason::Length,
        _ if has_tool_calls => StopReason::ToolUse,
        _ => StopReason::EndTurn,
    };
    events.push(ModelStreamEvent::End(stop_reason));
    let usage = response.get("usage").cloned().unwrap_or(Value::Null);
    let token = |name: &str| usage.get(name).and_then(Value::as_u64);
    let reasoning = usage
        .get("completion_tokens_details")
        .and_then(Value::as_object)
        .and_then(|details| details.get("reasoning_tokens"))
        .and_then(Value::as_u64);
    Ok((
        events,
        Usage {
            input_tokens: token("prompt_tokens"),
            output_tokens: token("completion_tokens"),
            reasoning_tokens: reasoning,
        },
    ))
}

fn event_name(event: &AgentEventKind) -> &'static str {
    match event {
        AgentEventKind::AgentStart => "agent_start",
        AgentEventKind::TurnStart { .. } => "turn_start",
        AgentEventKind::MessageStart { .. } => "message_start",
        AgentEventKind::MessageUpdate { .. } => "message_update",
        AgentEventKind::MessageEnd { .. } => "message_end",
        AgentEventKind::ToolExecutionStart { .. } => "tool_execution_start",
        AgentEventKind::ToolExecutionUpdate { .. } => "tool_execution_update",
        AgentEventKind::ToolExecutionEnd { .. } => "tool_execution_end",
        AgentEventKind::TurnEnd { .. } => "turn_end",
        AgentEventKind::AgentEnd { .. } => "agent_end",
    }
}

fn terminal_status(result: &Result<(), pi_agent_core::CoreError>) -> &'static str {
    match result {
        Ok(()) => "completed",
        Err(pi_agent_core::CoreError::Cancelled) => "cancelled",
        Err(pi_agent_core::CoreError::ModelAborted { .. }) => "aborted",
        Err(_) => "failed",
    }
}

/// A redacted failure class for the controller artifact. It is intentionally not the provider
/// message: evaluation reports must not retain arbitrary provider payloads or credentials.
fn terminal_code(result: &Result<(), pi_agent_core::CoreError>) -> Option<&'static str> {
    match result {
        Ok(()) => None,
        Err(pi_agent_core::CoreError::Cancelled) => Some("cancelled"),
        Err(pi_agent_core::CoreError::ModelAborted { .. }) => Some("model_aborted"),
        Err(pi_agent_core::CoreError::ModelError { .. }) => Some("model_error"),
        Err(pi_agent_core::CoreError::ModelProvider { .. }) => Some("model_provider"),
        Err(pi_agent_core::CoreError::UnsupportedModelStream { .. }) => {
            Some("unsupported_model_stream")
        }
        Err(pi_agent_core::CoreError::Hook(_)) => Some("hook"),
        Err(pi_agent_core::CoreError::MissingModelProvider) => Some("missing_model_provider"),
        Err(pi_agent_core::CoreError::ActiveRun { .. }) => Some("active_run"),
        Err(pi_agent_core::CoreError::InvalidTransition(_)) => Some("invalid_transition"),
        Err(pi_agent_core::CoreError::RunFinished { .. }) => Some("run_finished"),
    }
}

fn final_text(agent: &Agent) -> String {
    agent
        .snapshot()
        .messages
        .into_iter()
        .rev()
        .find_map(|message| match message {
            Message::Assistant { content, .. } => Some(content),
            _ => None,
        })
        .unwrap_or_default()
}

fn main() -> Result<(), String> {
    let args = Args::parse()?;
    let api_key = env::var("OPENROUTER_API_KEY").map_err(|_| {
        "OPENROUTER_API_KEY must be supplied by the caller's secret injector".to_owned()
    })?;
    let task: Value = serde_json::from_slice(
        &fs::read(&args.task_json).map_err(|_| "cannot read evaluation task".to_owned())?,
    )
    .map_err(|_| "evaluation task is not JSON".to_owned())?;
    let capabilities: Value = serde_json::from_slice(
        &fs::read(&args.capabilities_json)
            .map_err(|_| "cannot read evaluation capabilities".to_owned())?,
    )
    .map_err(|_| "evaluation capabilities are not JSON".to_owned())?;
    if task.get("capabilities") != Some(&capabilities) {
        return Err("evaluation capability manifest does not match task".into());
    }
    let prompt = task
        .get("prompt")
        .and_then(Value::as_str)
        .filter(|prompt| !prompt.is_empty())
        .ok_or_else(|| "evaluation task has no prompt".to_owned())?;
    let default_tools = DefaultCodingTools::new(&args.workspace)
        .map_err(|error| format!("cannot construct explicit workspace tools: {error}"))?;
    let usage = UsageTotals::default();
    let provider = Arc::new(OpenRouterProvider {
        api_key,
        model: args.model.clone(),
        max_tokens: 1024,
        usage: usage.clone(),
    });
    let agent = Agent::builder()
        .model(ModelDescriptor {
            provider: "openrouter".into(),
            model: args.model,
            revision: None,
        })
        .hooks(Arc::new(OpenAiContextHook))
        .model_provider(provider as Arc<dyn ModelProvider>)
        .pinned_default_coding_profile(default_tools)
        .map_err(|error| format!("cannot apply pinned default profile: {error}"))?
        .build();
    let run = agent
        .start_prompt(prompt)
        .map_err(|error| format!("cannot start evaluation run: {error}"))?;
    let result = smol::block_on(run.drive());
    let events = run.events();
    let totals = usage.snapshot();
    let trace = events
        .iter()
        .map(|event| json!({"seq": event.sequence.0, "type": event_name(&event.kind)}))
        .collect::<Vec<_>>();
    let turns = events
        .iter()
        .filter(|event| matches!(event.kind, AgentEventKind::TurnStart { .. }))
        .count();
    let tool_calls = events
        .iter()
        .filter(|event| matches!(event.kind, AgentEventKind::ToolExecutionStart { .. }))
        .count();
    let output = json!({
        "schema_version": RESULT_SCHEMA,
        "attempt_id": args.attempt_id,
        "baseline_id": args.baseline_id,
        "terminal": {
            "status": terminal_status(&result),
            "code": terminal_code(&result),
        },
        "final_text": final_text(&agent),
        "turns": turns,
        "tool_calls": tool_calls,
        "usage": {
            "input": totals.input_tokens.unwrap_or(0),
            "output": totals.output_tokens.unwrap_or(0),
            "cache_read": 0,
            "cache_write": 0,
        },
        "trace": trace,
    });
    let encoded =
        serde_json::to_vec(&output).map_err(|_| "cannot encode evaluation result".to_owned())?;
    fs::write(&args.result_json, encoded)
        .map_err(|_| "cannot write evaluation result".to_owned())?;
    Ok(())
}
