//! Opt-in provider coding-evaluation adapter for the Rust default profile.
//!
//! This binary is intentionally outside the library boundary: it is invoked only by the final
//! V0 evaluation controller through a caller-owned secret-injection boundary. It supplies a
//! concrete transport to exercise the otherwise provider-free core, while retaining the core's
//! explicit workspace, profile, and Smol-owned execution boundaries.

use pi_agent_core::event::AgentEventKind;
use pi_agent_core::hooks::{AfterToolCall, BeforeToolCall, ContextEnvelope, HookSet, NextTurn};
use pi_agent_core::provider::commandcode::{
    CommandCodeConfig, CommandCodeHostContext, CommandCodeProvider,
};
use pi_agent_core::provider::openrouter::{
    OpenRouterConfig, OpenRouterCostReport, OpenRouterProvider,
};
use pi_agent_core::scheduler::ModelProvider;
use pi_agent_core::state::{Message, ModelDescriptor};
use pi_agent_core::{Agent, DefaultCodingTools};
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

const RESULT_SCHEMA: &str = "pi-coding-eval-result/v0";

/// Explicit command-line arguments supplied by `evals/controller.py`.
struct Args {
    provider: ProviderKind,
    model: String,
    task_json: PathBuf,
    workspace: PathBuf,
    capabilities_json: PathBuf,
    result_json: PathBuf,
    attempt_id: String,
    baseline_id: String,
    commandcode_date: Option<String>,
    commandcode_environment: Option<String>,
    commandcode_thread_id: Option<String>,
    commandcode_project_slug: Option<String>,
}

/// Explicit provider choice for this executable integration boundary.
#[derive(Clone, Copy)]
enum ProviderKind {
    OpenRouter,
    CommandCode,
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
        let provider = match values.get("--provider").map(String::as_str) {
            None | Some("openrouter") => ProviderKind::OpenRouter,
            Some("commandcode" | "command-code") => ProviderKind::CommandCode,
            Some(_) => return Err("--provider must be openrouter or commandcode".into()),
        };
        let commandcode_date = values.get("--commandcode-date").cloned();
        let commandcode_environment = values.get("--commandcode-environment").cloned();
        let commandcode_thread_id = values.get("--commandcode-thread-id").cloned();
        let commandcode_project_slug = values.get("--commandcode-project-slug").cloned();
        for flag in values.keys() {
            if !matches!(
                flag.as_str(),
                "--provider"
                    | "--model"
                    | "--task-json"
                    | "--workspace"
                    | "--capabilities-json"
                    | "--result-json"
                    | "--attempt-id"
                    | "--baseline-id"
                    | "--commandcode-date"
                    | "--commandcode-environment"
                    | "--commandcode-thread-id"
                    | "--commandcode-project-slug"
            ) {
                return Err(format!("unsupported evaluation adapter argument {flag}"));
            }
        }
        if matches!(provider, ProviderKind::CommandCode)
            && (commandcode_date.is_none()
                || commandcode_environment.is_none()
                || commandcode_thread_id.is_none()
                || commandcode_project_slug.is_none())
        {
            return Err(
                "Command Code requires --commandcode-date, --commandcode-environment, --commandcode-thread-id, and --commandcode-project-slug".into(),
            );
        }
        Ok(Self {
            provider,
            model: take("--model")?,
            task_json: PathBuf::from(take("--task-json")?),
            workspace: PathBuf::from(take("--workspace")?),
            capabilities_json: PathBuf::from(take("--capabilities-json")?),
            result_json: PathBuf::from(take("--result-json")?),
            attempt_id: take("--attempt-id")?,
            baseline_id: take("--baseline-id")?,
            commandcode_date,
            commandcode_environment,
            commandcode_thread_id,
            commandcode_project_slug,
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

fn openrouter_cost_json(report: &OpenRouterCostReport) -> Value {
    json!({
        "schema_version": "pi-eval-cost/v1",
        "currency": "USD",
        "pricing": "provider_reported",
        "reported_turn_count": report.reported_turn_count,
        "unavailable_turn_count": report.unavailable_turn_count,
        "complete": report.complete,
        // A partial total is useful for diagnosis, but `complete` makes it impossible to
        // mistake that value for the complete run cost.
        "reported_total_usd": report.reported_total_usd,
        "reported_upstream_inference_usd": report.reported_upstream_inference_usd,
        "turns": report.turns.iter().map(|turn| json!({
            "turn": turn.turn,
            "source": turn.source.as_str(),
            "total_usd": turn.total_usd,
            "upstream_inference_usd": turn.upstream_inference_usd,
            "model": turn.model,
            "provider": turn.provider,
            "input_tokens": turn.input_tokens,
            "output_tokens": turn.output_tokens,
            "cache_read_tokens": turn.cache_read_tokens,
            "cache_write_tokens": turn.cache_write_tokens,
            "reasoning_tokens": turn.reasoning_tokens,
        })).collect::<Vec<_>>(),
    })
}

/// A concrete opt-in provider plus only the accounting this evaluation host needs.
enum EvalProvider {
    OpenRouter(Arc<OpenRouterProvider>),
    CommandCode(Arc<CommandCodeProvider>),
}

impl EvalProvider {
    fn model_provider(&self) -> Arc<dyn ModelProvider> {
        match self {
            Self::OpenRouter(provider) => provider.clone() as Arc<dyn ModelProvider>,
            Self::CommandCode(provider) => provider.clone() as Arc<dyn ModelProvider>,
        }
    }

    fn usage_snapshot(&self) -> pi_agent_core::state::Usage {
        match self {
            Self::OpenRouter(provider) => provider.usage_snapshot(),
            Self::CommandCode(provider) => provider.usage_snapshot(),
        }
    }

    fn provider_name(&self) -> &'static str {
        match self {
            Self::OpenRouter(_) => "openrouter",
            Self::CommandCode(_) => "command-code",
        }
    }

    fn cost_json(&self) -> Option<Value> {
        match self {
            Self::OpenRouter(provider) => Some(openrouter_cost_json(&provider.cost_report())),
            // The Command Code gateway does not report price fields in its NDJSON contract.
            // Omitting this optional artifact field avoids manufacturing a local price estimate.
            Self::CommandCode(_) => None,
        }
    }

    /// Preserve actionable Command Code failure classification in the controller artifact while
    /// keeping its arbitrary remote message out of a broadly retained evaluation report.
    fn error_json(&self) -> Option<Value> {
        let Self::CommandCode(provider) = self else {
            return None;
        };
        provider.last_error_report().map(|report| {
            json!({
                "source": report.source.as_str(),
                "status_code": report.status_code,
                "error_type": report.error_type,
                "error_code": report.error_code,
                "retryable": report.retryable,
            })
        })
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
    let provider = match args.provider {
        ProviderKind::OpenRouter => {
            let api_key = env::var("OPENROUTER_API_KEY").map_err(|_| {
                "OPENROUTER_API_KEY must be supplied by the caller's secret injector".to_owned()
            })?;
            EvalProvider::OpenRouter(Arc::new(OpenRouterProvider::new(OpenRouterConfig::new(
                api_key,
                args.model.clone(),
            ))))
        }
        ProviderKind::CommandCode => {
            let api_key = env::var("COMMANDCODE_API_KEY").map_err(|_| {
                "COMMANDCODE_API_KEY must be supplied by the caller's secret injector".to_owned()
            })?;
            let host = CommandCodeHostContext::new(
                args.workspace.to_string_lossy(),
                args.commandcode_date
                    .as_deref()
                    .expect("validated Command Code date"),
                args.commandcode_environment
                    .as_deref()
                    .expect("validated Command Code environment"),
            )
            .map_err(|error| format!("invalid Command Code host context: {error}"))?;
            let config = CommandCodeConfig::new(api_key, args.model.clone(), host)
                .map_err(|error| format!("invalid Command Code configuration: {error}"))?
                .with_thread_id(
                    args.commandcode_thread_id
                        .as_deref()
                        .expect("validated Command Code thread ID"),
                )
                .and_then(|config| {
                    config.with_project_slug(
                        args.commandcode_project_slug
                            .as_deref()
                            .expect("validated Command Code project slug"),
                    )
                })
                .map_err(|error| format!("invalid Command Code configuration: {error}"))?;
            EvalProvider::CommandCode(Arc::new(CommandCodeProvider::new(config)))
        }
    };
    let agent = Agent::builder()
        .model(ModelDescriptor {
            provider: provider.provider_name().into(),
            model: args.model,
            revision: None,
        })
        .hooks(Arc::new(OpenAiContextHook))
        .model_provider(provider.model_provider())
        .pinned_default_coding_profile(default_tools)
        .map_err(|error| format!("cannot apply pinned default profile: {error}"))?
        .build();
    let run = agent
        .start_prompt(prompt)
        .map_err(|error| format!("cannot start evaluation run: {error}"))?;
    let result = smol::block_on(run.drive());
    let events = run.events();
    let totals = provider.usage_snapshot();
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
    let mut output = json!({
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
    if let Some(cost) = provider.cost_json() {
        output["cost"] = cost;
    }
    if let Some(error) = provider.error_json() {
        output["provider_error"] = error;
    }
    let encoded =
        serde_json::to_vec(&output).map_err(|_| "cannot encode evaluation result".to_owned())?;
    fs::write(&args.result_json, encoded)
        .map_err(|_| "cannot write evaluation result".to_owned())?;
    Ok(())
}
