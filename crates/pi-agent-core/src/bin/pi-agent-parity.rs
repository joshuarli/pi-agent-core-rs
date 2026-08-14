//! Deterministic Rust adapter for the declarative Pi parity fixtures.
//!
//! The executable is deliberately outside the library runtime boundary: it
//! uses `smol::block_on` to drive one caller-owned fixture future. It accepts
//! one fixture path, has no network/provider capability, and supports the
//! closed V0 fixture subset implemented by the Rust core.

use pi_agent_core::event::{AgentEvent, AgentEventKind};
use pi_agent_core::hooks::{AfterToolCall, BeforeToolCall, ContextEnvelope, HookSet, Replacement};
use pi_agent_core::queue::QueueMode;
use pi_agent_core::scheduler::{
    CancellationToken, ModelFuture, ModelProvider, ModelRequest, ModelStream, ModelStreamEvent,
};
use pi_agent_core::state::{
    AgentPhase, AssistantToolCall, Message, ModelDescriptor, SerializedJson, StopReason,
    ThinkingLevel, ToolCallId,
};
use pi_agent_core::tool::{
    AgentTool, ToolCall, ToolContext, ToolExecutionMode, ToolFuture, ToolResult, ToolUpdateSink,
};
use pi_agent_core::{Agent, CoreError};
use pi_agent_protocol::{JsonNumber, JsonValue};
use std::collections::{BTreeMap, VecDeque};
use std::env;
use std::fs;
use std::sync::{Arc, Mutex};
use std::task::Poll;

#[derive(Clone, Debug)]
struct FixtureUsage {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    total_tokens: u64,
}

#[derive(Clone, Debug)]
struct FixtureToolResponse {
    arguments: SerializedJson,
    content: String,
    is_error: bool,
    yield_once: bool,
    updates: Vec<String>,
}

#[derive(Clone, Debug)]
struct FixtureToolSpec {
    name: String,
    description: String,
    execution_mode: ToolExecutionMode,
    parameters: SerializedJson,
    responses: Vec<FixtureToolResponse>,
}

#[derive(Clone, Debug)]
struct Fixture {
    id: String,
    system_prompt: String,
    provider: String,
    model: String,
    thinking_level: ThinkingLevel,
    steering_mode: QueueMode,
    follow_up_mode: QueueMode,
    actions: Vec<FixtureAction>,
    before_tool_block: Option<FixtureBeforeToolBlock>,
    after_tool_replace: Option<FixtureAfterToolReplace>,
    tools: Vec<FixtureToolSpec>,
    streams: Vec<ModelStream>,
    last_usage: FixtureUsage,
    last_stop_reason: StopReason,
}

#[derive(Clone, Debug)]
struct FixtureBeforeToolBlock {
    tool_name: String,
    reason: String,
}

#[derive(Clone, Debug)]
struct FixtureAfterToolReplace {
    tool_name: String,
    content: String,
    is_error: bool,
}

#[derive(Clone, Debug)]
enum FixtureAction {
    Steer(String),
    FollowUp(String),
    Prompt(String),
    Continue,
}

#[derive(Debug)]
struct FixtureHooks {
    before_tool_block: Option<FixtureBeforeToolBlock>,
    after_tool_replace: Option<FixtureAfterToolReplace>,
}

impl HookSet for FixtureHooks {
    fn before_tool_call(
        &self,
        call: &ToolCall,
    ) -> Result<BeforeToolCall, pi_agent_core::error::HookError> {
        match &self.before_tool_block {
            Some(rule) if rule.tool_name == call.name => Ok(BeforeToolCall::Block {
                reason: rule.reason.clone(),
            }),
            _ => Ok(BeforeToolCall::Allow),
        }
    }

    fn after_tool_call(
        &self,
        call: &ToolCall,
        _result: &ToolResult,
    ) -> Result<AfterToolCall, pi_agent_core::error::HookError> {
        match &self.after_tool_replace {
            Some(rule) if rule.tool_name == call.name => Ok(AfterToolCall {
                content: Replacement::Replace(rule.content.clone()),
                is_error: Replacement::Replace(rule.is_error),
                ..AfterToolCall::default()
            }),
            _ => Ok(AfterToolCall::default()),
        }
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
        Ok(context
            .messages
            .into_iter()
            .map(|message| format!("{message:?}"))
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

#[derive(Debug)]
struct FixtureProvider {
    streams: Mutex<VecDeque<ModelStream>>,
}

impl ModelProvider for FixtureProvider {
    fn stream<'a>(
        &'a self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelFuture<'a> {
        let stream = self
            .streams
            .lock()
            .expect("fixture model stream mutex poisoned")
            .pop_front()
            .ok_or_else(|| pi_agent_core::error::SchedulerError::UnknownToolCall {
                tool_call_id: ToolCallId::new("fixture-exhausted-model-script")
                    .expect("fixed fixture ID is non-empty"),
            });
        Box::pin(std::future::ready(stream))
    }
}

#[derive(Debug)]
struct FixtureTool {
    name: String,
    description: String,
    execution_mode: ToolExecutionMode,
    schema: JsonValue,
    responses: Mutex<Vec<FixtureToolResponse>>,
}

impl AgentTool for FixtureTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn schema(&self) -> &JsonValue {
        &self.schema
    }

    fn execution_mode(&self) -> ToolExecutionMode {
        self.execution_mode
    }

    fn execute<'a>(
        &'a self,
        call: ToolCall,
        _context: ToolContext,
        updates: ToolUpdateSink,
    ) -> ToolFuture<'a> {
        let response = {
            let mut responses = self
                .responses
                .lock()
                .expect("fixture tool response mutex poisoned");
            responses
                .iter()
                .position(|response| response.arguments == call.arguments)
                .map(|index| responses.remove(index))
        };
        let yield_once = response
            .as_ref()
            .is_some_and(|response| response.yield_once);
        if let Some(response) = &response {
            for content in &response.updates {
                updates.emit(pi_agent_core::tool::ToolUpdate {
                    content: content.clone(),
                    details: None,
                });
            }
        }
        let result = match response {
            Some(response) => Ok(ToolResult {
                tool_call_id: call.id,
                content: response.content,
                details: None,
                is_error: response.is_error,
            }),
            None => Err(pi_agent_core::error::ToolError::Execution {
                tool: call.name,
                message: "fixture has no matching host tool response".into(),
            }),
        };
        if yield_once {
            Box::pin(async move {
                yield_to_another_tool().await;
                result
            })
        } else {
            Box::pin(std::future::ready(result))
        }
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let fixture_path = single_fixture_path()?;
    let fixture =
        Fixture::parse(&fs::read_to_string(fixture_path).map_err(|error| error.to_string())?)?;
    let result = smol::block_on(run_fixture(fixture))?;
    print!(
        "{}",
        result.to_json_string().map_err(|error| error.to_string())?
    );
    Ok(())
}

fn single_fixture_path() -> Result<String, String> {
    let mut arguments = env::args();
    let _program = arguments.next();
    let path = arguments
        .next()
        .ok_or_else(|| "expected exactly one declarative fixture path".to_owned())?;
    if arguments.next().is_some() {
        return Err("expected exactly one declarative fixture path".into());
    }
    Ok(path)
}

impl Fixture {
    fn parse(input: &str) -> Result<Self, String> {
        let fixture = JsonValue::parse(input).map_err(|error| error.to_string())?;
        let root = object(&fixture, "fixture")?;
        if number_field(root, "format_version")? != 1
            || string_field(root, "kind")? != "declarative_parity_fixture"
        {
            return Err("expected format_version 1 declarative_parity_fixture".into());
        }
        let setup = object(field(root, "setup")?, "setup")?;
        let model = object(field(setup, "model")?, "setup.model")?;
        let host = object(field(root, "host")?, "host")?;
        let tools = parse_tools(setup, host)?;

        let actions = parse_actions(field(root, "actions")?)?;

        let (streams, last_usage, last_stop_reason) =
            parse_model_script(field(root, "model_script")?)?;
        Ok(Self {
            id: string_field(root, "id")?.to_owned(),
            system_prompt: string_field(setup, "system_prompt")?.to_owned(),
            provider: string_field(model, "provider")?.to_owned(),
            model: string_field(model, "id")?.to_owned(),
            thinking_level: parse_thinking_level(string_field(setup, "thinking_level")?)?,
            steering_mode: parse_queue_mode(setup.get("steering_mode"))?,
            follow_up_mode: parse_queue_mode(setup.get("follow_up_mode"))?,
            actions,
            before_tool_block: parse_before_tool_block(host)?,
            after_tool_replace: parse_after_tool_replace(host)?,
            tools,
            streams,
            last_usage,
            last_stop_reason,
        })
    }
}

fn parse_before_tool_block(
    host: &BTreeMap<String, JsonValue>,
) -> Result<Option<FixtureBeforeToolBlock>, String> {
    let Some(rule) = host.get("before_tool_call") else {
        return Ok(None);
    };
    let rule = object(rule, "host.before_tool_call")?;
    Ok(Some(FixtureBeforeToolBlock {
        tool_name: string_field(rule, "tool_name")?.to_owned(),
        reason: string_field(rule, "reason")?.to_owned(),
    }))
}

fn parse_after_tool_replace(
    host: &BTreeMap<String, JsonValue>,
) -> Result<Option<FixtureAfterToolReplace>, String> {
    let Some(rule) = host.get("after_tool_call") else {
        return Ok(None);
    };
    let rule = object(rule, "host.after_tool_call")?;
    Ok(Some(FixtureAfterToolReplace {
        tool_name: string_field(rule, "tool_name")?.to_owned(),
        content: string_field(rule, "content")?.to_owned(),
        is_error: bool_field(rule, "is_error")?,
    }))
}

fn parse_actions(value: &JsonValue) -> Result<Vec<FixtureAction>, String> {
    let actions = array(value, "actions")?;
    if actions.is_empty() {
        return Err("the V0 runner requires at least one action".into());
    }
    let parsed = actions
        .iter()
        .map(|action| {
            let action = object(action, "fixture action")?;
            match string_field(action, "kind")? {
                "steer" => Ok(FixtureAction::Steer(
                    string_field(action, "text")?.to_owned(),
                )),
                "follow_up" => Ok(FixtureAction::FollowUp(
                    string_field(action, "text")?.to_owned(),
                )),
                "prompt" => Ok(FixtureAction::Prompt(
                    string_field(action, "text")?.to_owned(),
                )),
                "continue" => Ok(FixtureAction::Continue),
                kind => Err(format!("the V0 runner does not support action {kind:?}")),
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    if !parsed
        .iter()
        .any(|action| matches!(action, FixtureAction::Prompt(_) | FixtureAction::Continue))
    {
        return Err("the V0 runner requires an action that starts a run".into());
    }
    Ok(parsed)
}

fn parse_queue_mode(value: Option<&JsonValue>) -> Result<QueueMode, String> {
    match value {
        None => Ok(QueueMode::OneAtATime),
        Some(JsonValue::String(value)) if value == "all" => Ok(QueueMode::All),
        Some(JsonValue::String(value)) if value == "one-at-a-time" => Ok(QueueMode::OneAtATime),
        Some(_) => Err("setup queue mode must be all or one-at-a-time".into()),
    }
}

fn parse_tools(
    setup: &BTreeMap<String, JsonValue>,
    host: &BTreeMap<String, JsonValue>,
) -> Result<Vec<FixtureToolSpec>, String> {
    let mut host_responses = BTreeMap::<String, Vec<FixtureToolResponse>>::new();
    for (index, entry) in array(field(host, "tools")?, "host.tools")?
        .iter()
        .enumerate()
    {
        let entry = object(entry, "host.tools entry")?;
        let name = string_field(entry, "name")?.to_owned();
        if host_responses.contains_key(&name) {
            return Err(format!("host.tools[{index}] repeats {name:?}"));
        }
        let responses = array(field(entry, "calls")?, "host.tools calls")?
            .iter()
            .map(parse_tool_response)
            .collect::<Result<Vec<_>, _>>()?;
        host_responses.insert(name, responses);
    }

    let mut names = std::collections::BTreeSet::new();
    array(field(setup, "tools")?, "setup.tools")?
        .iter()
        .map(|entry| {
            let entry = object(entry, "setup.tools entry")?;
            let name = string_field(entry, "name")?.to_owned();
            if !names.insert(name.clone()) {
                return Err(format!("setup.tools repeats {name:?}"));
            }
            Ok(FixtureToolSpec {
                description: string_field(entry, "description")?.to_owned(),
                execution_mode: parse_tool_execution_mode(entry.get("execution_mode"))?,
                parameters: SerializedJson::new(
                    field(entry, "parameters")?
                        .to_json_string()
                        .map_err(|error| error.to_string())?,
                ),
                responses: host_responses.remove(&name).unwrap_or_default(),
                name,
            })
        })
        .collect()
}

fn parse_tool_response(value: &JsonValue) -> Result<FixtureToolResponse, String> {
    let value = object(value, "host tool call")?;
    let result = object(field(value, "result")?, "host tool result")?;
    let content = array(field(result, "content")?, "host tool result.content")?;
    if content.len() != 1 {
        return Err(
            "the V0 fixture adapter supports exactly one text tool-result content part".into(),
        );
    }
    let text = object(&content[0], "host tool result.content[0]")?;
    if string_field(text, "type")? != "text" {
        return Err("the V0 fixture adapter supports text tool-result content only".into());
    }
    Ok(FixtureToolResponse {
        arguments: SerializedJson::new(
            field(value, "arguments")?
                .to_json_string()
                .map_err(|error| error.to_string())?,
        ),
        content: string_field(text, "text")?.to_owned(),
        is_error: bool_field(result, "is_error")?,
        yield_once: match value.get("yield_once") {
            None => false,
            Some(JsonValue::Bool(value)) => *value,
            Some(_) => return Err("host tool call field \"yield_once\" must be a boolean".into()),
        },
        updates: match value.get("updates") {
            None => Vec::new(),
            Some(JsonValue::Array(updates)) => updates
                .iter()
                .enumerate()
                .map(|(index, update)| match update {
                    JsonValue::String(update) => Ok(update.clone()),
                    _ => Err(format!("host tool call updates[{index}] must be a string")),
                })
                .collect::<Result<Vec<_>, _>>()?,
            Some(_) => return Err("host tool call field \"updates\" must be an array".into()),
        },
    })
}

fn parse_model_script(
    value: &JsonValue,
) -> Result<(Vec<ModelStream>, FixtureUsage, StopReason), String> {
    let turns = array(value, "model_script")?;
    if turns.is_empty() {
        return Err("model_script must contain at least one turn".into());
    }
    let mut last_usage = None;
    let mut last_stop_reason = None;
    let streams = turns
        .iter()
        .enumerate()
        .map(|(turn_index, turn)| {
            let turn = object(turn, "model_script turn")?;
            let chunks = array(field(turn, "chunks")?, "model_script chunks")?;
            if chunks.is_empty() {
                return Err(format!("model_script[{turn_index}] has no chunks"));
            }
            let mut events = Vec::new();
            for (chunk_index, chunk) in chunks.iter().enumerate() {
                let chunk = object(chunk, "model_script chunk")?;
                let kind = string_field(chunk, "kind")?;
                match kind {
                    "text_delta" if chunk_index + 1 < chunks.len() => events.push(
                        ModelStreamEvent::TextDelta(string_field(chunk, "text")?.to_owned()),
                    ),
                    "tool_call" if chunk_index + 1 < chunks.len() => {
                        let id = ToolCallId::new(string_field(chunk, "id")?)
                            .map_err(|error| error.to_string())?;
                        events.push(ModelStreamEvent::ToolCall(AssistantToolCall {
                            id,
                            name: string_field(chunk, "name")?.to_owned(),
                            arguments: SerializedJson::new(
                                field(chunk, "arguments")?
                                    .to_json_string()
                                    .map_err(|error| error.to_string())?,
                            ),
                        }));
                    }
                    "done" if chunk_index + 1 == chunks.len() => {
                        let stop_reason = parse_stop_reason(string_field(chunk, "stop_reason")?)?;
                        let usage = FixtureUsage::parse(field(chunk, "usage")?)?;
                        last_usage = Some(usage);
                        last_stop_reason = Some(stop_reason);
                        events.push(ModelStreamEvent::End(stop_reason));
                    }
                    _ => {
                        return Err(format!(
                            "unsupported model-script chunk {kind:?} at turn {turn_index}, index {chunk_index}"
                        ));
                    }
                }
            }
            Ok(ModelStream { events })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((
        streams,
        last_usage.ok_or_else(|| "model script must end with done".to_owned())?,
        last_stop_reason.ok_or_else(|| "model script must end with done".to_owned())?,
    ))
}

impl FixtureUsage {
    fn parse(value: &JsonValue) -> Result<Self, String> {
        let usage = object(value, "usage")?;
        Ok(Self {
            input: number_field(usage, "input")?,
            output: number_field(usage, "output")?,
            cache_read: number_field(usage, "cache_read")?,
            cache_write: number_field(usage, "cache_write")?,
            total_tokens: number_field(usage, "total_tokens")?,
        })
    }
}

async fn run_fixture(fixture: Fixture) -> Result<JsonValue, String> {
    let Fixture {
        id,
        system_prompt,
        provider,
        model,
        thinking_level,
        steering_mode,
        follow_up_mode,
        actions,
        before_tool_block,
        after_tool_replace,
        tools,
        streams,
        last_usage,
        last_stop_reason,
    } = fixture;
    let tool_names = tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    let model_provider = Arc::new(FixtureProvider {
        streams: Mutex::new(streams.into()),
    });
    let mut builder = Agent::builder()
        .system_prompt(system_prompt.clone())
        .model(ModelDescriptor {
            provider: provider.clone(),
            model: model.clone(),
            revision: None,
        })
        .thinking_level(thinking_level)
        .steering_mode(steering_mode)
        .follow_up_mode(follow_up_mode)
        .model_provider(Arc::clone(&model_provider) as Arc<dyn ModelProvider>);
    if before_tool_block.is_some() || after_tool_replace.is_some() {
        builder = builder.hooks(Arc::new(FixtureHooks {
            before_tool_block,
            after_tool_replace,
        }));
    }
    for tool in tools {
        builder = builder.tool(Arc::new(FixtureTool {
            name: tool.name,
            description: tool.description,
            execution_mode: tool.execution_mode,
            schema: JsonValue::parse(tool.parameters.as_str())
                .map_err(|error| error.to_string())?,
            responses: Mutex::new(tool.responses),
        }));
    }
    let agent = builder.build();
    let mut events = Vec::new();
    let mut event_sequence = 0;
    let mut turn_offset = 0;
    for action in actions {
        let run = match action {
            FixtureAction::Steer(input) => {
                agent.enqueue_steering(input).map_err(core_error)?;
                None
            }
            FixtureAction::FollowUp(input) => {
                agent.enqueue_follow_up(input).map_err(core_error)?;
                None
            }
            FixtureAction::Prompt(input) => Some(agent.start_prompt(input).map_err(core_error)?),
            FixtureAction::Continue => Some(agent.start_continue().map_err(core_error)?),
        };
        if let Some(run) = run {
            run.drive().await.map_err(core_error)?;
            let run_events = run.events();
            let turns_in_run = run_events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::TurnStart { .. }))
                .count() as u64;
            for event in &run_events {
                events.push(normalize_event(event_sequence, event, turn_offset)?);
                event_sequence = event_sequence.saturating_add(1);
            }
            turn_offset = turn_offset.saturating_add(turns_in_run);
        }
    }
    let snapshot = agent.snapshot();
    if snapshot.phase != AgentPhase::Idle
        || snapshot.is_streaming
        || !snapshot.pending_tool_calls.is_empty()
    {
        return Err("Rust agent did not settle the fixture run".into());
    }
    if !model_provider
        .streams
        .lock()
        .expect("fixture model stream mutex poisoned")
        .is_empty()
    {
        return Err("model_script contains unused turns".into());
    }

    events.push(JsonValue::object([
        ("seq", JsonValue::from(events.len() as u64)),
        ("type", JsonValue::from("agent_settled")),
        (
            "data",
            JsonValue::object([("outcome", JsonValue::from("completed"))]),
        ),
    ]));

    Ok(JsonValue::object([
        ("format_version", JsonValue::from(1_u64)),
        ("kind", JsonValue::from("canonical_parity_result")),
        ("fixture_id", JsonValue::from(id)),
        ("outcome", JsonValue::from("completed")),
        ("settled", JsonValue::from(true)),
        (
            "state",
            JsonValue::object([
                ("system_prompt", JsonValue::from(snapshot.system_prompt)),
                (
                    "model",
                    JsonValue::object([
                        ("provider", JsonValue::from(provider)),
                        ("id", JsonValue::from(model)),
                    ]),
                ),
                (
                    "thinking_level",
                    JsonValue::from(thinking_level_name(snapshot.thinking_level)),
                ),
                (
                    "tool_names",
                    JsonValue::Array(tool_names.into_iter().map(JsonValue::from).collect()),
                ),
                ("pending_tool_calls", JsonValue::Array(Vec::new())),
            ]),
        ),
        ("events", JsonValue::Array(events)),
        (
            "messages",
            JsonValue::Array(
                snapshot
                    .messages
                    .iter()
                    .map(normalize_message)
                    .collect::<Result<Vec<_>, _>>()?,
            ),
        ),
        (
            "last_response",
            JsonValue::object([
                ("api", JsonValue::from("fixture")),
                (
                    "stop_reason",
                    JsonValue::from(stop_reason_name(last_stop_reason)),
                ),
            ]),
        ),
        (
            "usage",
            JsonValue::object([
                ("input", JsonValue::from(last_usage.input)),
                ("output", JsonValue::from(last_usage.output)),
                ("cache_read", JsonValue::from(last_usage.cache_read)),
                ("cache_write", JsonValue::from(last_usage.cache_write)),
                ("total_tokens", JsonValue::from(last_usage.total_tokens)),
            ]),
        ),
        ("error", JsonValue::Null),
    ]))
}

fn normalize_event(
    sequence: usize,
    event: &AgentEvent,
    turn_offset: u64,
) -> Result<JsonValue, String> {
    let (kind, data) = match &event.kind {
        AgentEventKind::AgentStart => ("agent_start", empty_object()),
        AgentEventKind::AgentEnd { .. } => ("agent_end", empty_object()),
        AgentEventKind::TurnStart { turn_id } => (
            "turn_start",
            JsonValue::object([(
                "turn",
                JsonValue::from(turn_offset.saturating_add(turn_id.0.saturating_sub(1))),
            )]),
        ),
        AgentEventKind::TurnEnd { reason, .. } => (
            "turn_end",
            JsonValue::object([("stop_reason", JsonValue::from(stop_reason_name(*reason)))]),
        ),
        AgentEventKind::MessageStart { message } => (
            "message_start",
            JsonValue::object([("role", JsonValue::from(message_role_name(message)))]),
        ),
        AgentEventKind::MessageEnd { message } => (
            "message_end",
            JsonValue::object([("role", JsonValue::from(message_role_name(message)))]),
        ),
        AgentEventKind::MessageUpdate { message } => (
            "message_update",
            JsonValue::object([
                ("role", JsonValue::from(message_role_name(message))),
                ("delta", JsonValue::from(message_text(message))),
            ]),
        ),
        AgentEventKind::ToolExecutionStart {
            tool_call_id,
            tool_name,
        } => (
            "tool_execution_start",
            JsonValue::object([
                ("tool_call_id", JsonValue::from(tool_call_id.as_str())),
                ("tool_name", JsonValue::from(tool_name.clone())),
            ]),
        ),
        AgentEventKind::ToolExecutionEnd {
            tool_call_id,
            tool_name,
            result,
        } => (
            "tool_execution_end",
            JsonValue::object([
                ("tool_call_id", JsonValue::from(tool_call_id.as_str())),
                ("tool_name", JsonValue::from(tool_name.clone())),
                ("is_error", JsonValue::from(result.is_error)),
            ]),
        ),
        AgentEventKind::ToolExecutionUpdate {
            tool_call_id,
            tool_name,
            update,
        } => (
            "tool_execution_update",
            JsonValue::object([
                ("tool_call_id", JsonValue::from(tool_call_id.as_str())),
                ("tool_name", JsonValue::from(tool_name.clone())),
                ("content", JsonValue::from(update.content.clone())),
            ]),
        ),
    };
    Ok(JsonValue::object([
        ("seq", JsonValue::from(sequence as u64)),
        ("type", JsonValue::from(kind)),
        ("data", data),
    ]))
}

fn normalize_message(message: &Message) -> Result<JsonValue, String> {
    let content = match message {
        Message::User { content, .. } | Message::ToolResult { content, .. } => {
            vec![text_content(content)]
        }
        Message::Assistant {
            content,
            tool_calls,
            ..
        } => {
            let mut parts = Vec::new();
            if !content.is_empty() {
                parts.push(text_content(content));
            }
            for tool_call in tool_calls {
                parts.push(JsonValue::object([
                    ("type", JsonValue::from("tool_call")),
                    ("id", JsonValue::from(tool_call.id.as_str())),
                    ("name", JsonValue::from(tool_call.name.clone())),
                    (
                        "arguments",
                        JsonValue::parse(tool_call.arguments.as_str())
                            .map_err(|error| error.to_string())?,
                    ),
                ]));
            }
            parts
        }
    };
    Ok(JsonValue::object([
        ("role", JsonValue::from(message_role_name(message))),
        ("content", JsonValue::Array(content)),
    ]))
}

fn text_content(text: &str) -> JsonValue {
    JsonValue::object([
        ("type", JsonValue::from("text")),
        ("text", JsonValue::from(text)),
    ])
}

fn message_role_name(message: &Message) -> &'static str {
    match message {
        Message::User { .. } => "user",
        Message::Assistant { .. } => "assistant",
        Message::ToolResult { .. } => "tool_result",
    }
}

fn message_text(message: &Message) -> &str {
    match message {
        Message::User { content, .. }
        | Message::Assistant { content, .. }
        | Message::ToolResult { content, .. } => content,
    }
}

fn parse_thinking_level(value: &str) -> Result<ThinkingLevel, String> {
    match value {
        "off" => Ok(ThinkingLevel::Off),
        "low" => Ok(ThinkingLevel::Low),
        "medium" => Ok(ThinkingLevel::Medium),
        "high" => Ok(ThinkingLevel::High),
        _ => Err(format!("unsupported thinking level {value:?}")),
    }
}

fn parse_tool_execution_mode(value: Option<&JsonValue>) -> Result<ToolExecutionMode, String> {
    match value {
        None => Ok(ToolExecutionMode::Parallel),
        Some(JsonValue::String(value)) if value == "parallel" => Ok(ToolExecutionMode::Parallel),
        Some(JsonValue::String(value)) if value == "sequential" => {
            Ok(ToolExecutionMode::Sequential)
        }
        Some(_) => Err("setup tool field \"execution_mode\" must be parallel or sequential".into()),
    }
}

async fn yield_to_another_tool() {
    let mut yielded = false;
    std::future::poll_fn(|context| {
        if yielded {
            Poll::Ready(())
        } else {
            yielded = true;
            context.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await;
}

fn thinking_level_name(value: ThinkingLevel) -> &'static str {
    match value {
        ThinkingLevel::Default => "default",
        ThinkingLevel::Off => "off",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
    }
}

fn parse_stop_reason(value: &str) -> Result<StopReason, String> {
    match value {
        "stop" => Ok(StopReason::EndTurn),
        "tool_call" => Ok(StopReason::ToolUse),
        _ => Err(format!("unsupported model stop reason {value:?}")),
    }
}

fn stop_reason_name(value: StopReason) -> &'static str {
    match value {
        StopReason::EndTurn => "stop",
        StopReason::ToolUse => "tool_call",
        StopReason::Cancelled => "cancelled",
        StopReason::Error => "error",
    }
}

fn core_error(error: CoreError) -> String {
    error.to_string()
}

fn empty_object() -> JsonValue {
    JsonValue::Object(BTreeMap::new())
}

fn field<'a>(object: &'a BTreeMap<String, JsonValue>, name: &str) -> Result<&'a JsonValue, String> {
    object
        .get(name)
        .ok_or_else(|| format!("fixture is missing required field {name:?}"))
}

fn object<'a>(value: &'a JsonValue, path: &str) -> Result<&'a BTreeMap<String, JsonValue>, String> {
    match value {
        JsonValue::Object(value) => Ok(value),
        _ => Err(format!("{path} must be an object")),
    }
}

fn array<'a>(value: &'a JsonValue, path: &str) -> Result<&'a [JsonValue], String> {
    match value {
        JsonValue::Array(value) => Ok(value),
        _ => Err(format!("{path} must be an array")),
    }
}

fn string_field<'a>(
    object: &'a BTreeMap<String, JsonValue>,
    name: &str,
) -> Result<&'a str, String> {
    match field(object, name)? {
        JsonValue::String(value) => Ok(value),
        _ => Err(format!("fixture field {name:?} must be a string")),
    }
}

fn bool_field(object: &BTreeMap<String, JsonValue>, name: &str) -> Result<bool, String> {
    match field(object, name)? {
        JsonValue::Bool(value) => Ok(*value),
        _ => Err(format!("fixture field {name:?} must be a boolean")),
    }
}

fn number_field(object: &BTreeMap<String, JsonValue>, name: &str) -> Result<u64, String> {
    match field(object, name)? {
        JsonValue::Number(JsonNumber::Unsigned(value)) => Ok(*value),
        JsonValue::Number(JsonNumber::Signed(value)) if *value >= 0 => Ok(*value as u64),
        _ => Err(format!(
            "fixture field {name:?} must be a non-negative integer"
        )),
    }
}
