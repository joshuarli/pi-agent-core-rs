//! Deterministic Rust adapter for the declarative Pi parity fixtures.
//!
//! The executable is deliberately outside the library runtime boundary: it
//! uses `smol::block_on` to drive one caller-owned fixture future. It accepts
//! one fixture path, has no network/provider capability, and supports the
//! closed V0 fixture subset implemented by the Rust core.

use pi_agent_core::event::{AgentEvent, AgentEventKind, EventObserver, ObserverFuture};
use pi_agent_core::hooks::{
    AfterToolCall, BeforeToolCall, ContextEnvelope, HookFuture, HookSet, NextTurn, Replacement,
};
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
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};

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
    cancel_after_update: bool,
    enqueue_during_execution: Option<FixtureActiveQueueArrival>,
    terminate: bool,
}

/// A host fixture message injected only while the corresponding tool call is active.
/// The directive gives queue drains a deterministic source without a clock or background task.
#[derive(Clone, Debug)]
enum FixtureActiveQueueArrival {
    Steer(String),
    FollowUp(String),
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
    before_tool_policy: Option<FixtureBeforeToolPolicy>,
    after_tool_replace: Option<FixtureAfterToolReplace>,
    context_hooks: Option<FixtureContextHooks>,
    should_stop_after_turn: bool,
    hold_agent_end_observer: bool,
    tools: Vec<FixtureToolSpec>,
    streams: Vec<FixtureModelStream>,
    last_usage: FixtureUsage,
    last_stop_reason: StopReason,
}

/// One deterministic model turn, including adapter-only cancellation control.
///
/// The core provider contract intentionally receives a finite `ModelStream` in
/// this V0 harness.  A cancellation checkpoint therefore rewrites the fixture
/// stream at parse time and marks the caller-owned token before returning it;
/// both adapters still expose the same partial response and aborted terminal
/// lifecycle without relying on wall-clock scheduling.
#[derive(Clone, Debug)]
struct FixtureModelStream {
    stream: ModelStream,
    cancel_after_text_delta: bool,
}

#[derive(Clone, Debug)]
struct FixtureBeforeToolPolicy {
    tool_name: String,
    reason: String,
    terminate: bool,
    yield_once: bool,
    cancel_after_yield: bool,
}

#[derive(Clone, Debug)]
struct FixtureAfterToolReplace {
    tool_name: String,
    content: String,
    is_error: bool,
    terminate: Option<bool>,
}

#[derive(Clone, Debug)]
struct FixtureContextHooks {
    host_messages: Vec<String>,
    transform_append_host_message: String,
    convert_prefix: String,
    next_host_messages: Vec<String>,
    next_model: ModelDescriptor,
    next_thinking_level: ThinkingLevel,
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
    before_tool_policy: Option<FixtureBeforeToolPolicy>,
    after_tool_replace: Option<FixtureAfterToolReplace>,
    context_hooks: Option<FixtureContextHooks>,
    should_stop_after_turn: bool,
    /// The quality adapter enables this explicitly to retain the logical
    /// pre-conversion request envelope. Normal parity output remains byte-for-
    /// byte stable unless `PI_AGENT_QUALITY_CAPTURE=1` is set.
    request_contexts: Option<Arc<Mutex<Vec<ContextEnvelope>>>>,
}

/// A deterministic, explicitly held `agent_end` observer used to prove that terminal
/// settlement waits for listeners. It has no timers or background executor authority.
#[derive(Debug, Default)]
struct FixtureObserverGate {
    reached: AtomicBool,
    released: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl FixtureObserverGate {
    fn release(&self) {
        self.released.store(true, Ordering::Release);
        if let Some(waker) = self
            .waker
            .lock()
            .expect("fixture observer gate mutex poisoned")
            .take()
        {
            waker.wake();
        }
    }
}

impl EventObserver for FixtureObserverGate {
    fn observe<'a>(
        &'a self,
        event: &'a AgentEvent,
        _cancellation: CancellationToken,
    ) -> ObserverFuture<'a> {
        let hold_agent_end = matches!(event.kind, AgentEventKind::AgentEnd { .. });
        Box::pin(std::future::poll_fn(move |context| {
            if !hold_agent_end {
                return Poll::Ready(Ok(()));
            }
            self.reached.store(true, Ordering::Release);
            if self.released.load(Ordering::Acquire) {
                Poll::Ready(Ok(()))
            } else {
                *self
                    .waker
                    .lock()
                    .expect("fixture observer gate mutex poisoned") = Some(context.waker().clone());
                Poll::Pending
            }
        }))
    }
}

impl HookSet for FixtureHooks {
    fn before_tool_call(
        &self,
        call: &ToolCall,
    ) -> Result<BeforeToolCall, pi_agent_core::error::HookError> {
        match &self.before_tool_policy {
            Some(rule) if rule.tool_name == call.name && rule.terminate => {
                Ok(BeforeToolCall::Terminate {
                    reason: rule.reason.clone(),
                })
            }
            Some(rule) if rule.tool_name == call.name => Ok(BeforeToolCall::Block {
                reason: rule.reason.clone(),
            }),
            _ => Ok(BeforeToolCall::Allow),
        }
    }

    fn before_tool_call_async<'a>(
        &'a self,
        call: &'a ToolCall,
        _context: ContextEnvelope,
        cancellation: CancellationToken,
    ) -> HookFuture<'a, BeforeToolCall> {
        let Some(rule) = self
            .before_tool_policy
            .as_ref()
            .filter(|rule| rule.tool_name == call.name && rule.yield_once)
        else {
            return Box::pin(std::future::ready(self.before_tool_call(call)));
        };
        let cancel_after_yield = rule.cancel_after_yield;
        Box::pin(async move {
            yield_to_another_tool().await;
            if cancel_after_yield {
                cancellation.cancel();
                Ok(BeforeToolCall::Allow)
            } else {
                self.before_tool_call(call)
            }
        })
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
                terminate: rule.terminate,
                ..AfterToolCall::default()
            }),
            _ => Ok(AfterToolCall::default()),
        }
    }

    fn transform_context(
        &self,
        mut context: ContextEnvelope,
    ) -> Result<ContextEnvelope, pi_agent_core::error::HookError> {
        if let Some(policy) = &self.context_hooks {
            context.host_messages.push(SerializedJson::new(
                policy.transform_append_host_message.clone(),
            ));
        }
        Ok(context)
    }

    fn convert_to_llm(
        &self,
        context: ContextEnvelope,
    ) -> Result<String, pi_agent_core::error::HookError> {
        if let Some(request_contexts) = &self.request_contexts {
            request_contexts
                .lock()
                .expect("fixture quality request-context mutex poisoned")
                .push(context.clone());
        }
        if let Some(policy) = &self.context_hooks {
            let host_messages = context
                .host_messages
                .iter()
                .map(SerializedJson::as_str)
                .collect::<Vec<_>>()
                .join("|");
            return Ok(format!("{}{}", policy.convert_prefix, host_messages));
        }
        Ok(context
            .messages
            .into_iter()
            .map(|message| format!("{message:?}"))
            .collect::<Vec<_>>()
            .join("\n"))
    }

    fn prepare_next_turn(
        &self,
        mut context: ContextEnvelope,
    ) -> Result<NextTurn, pi_agent_core::error::HookError> {
        let Some(policy) = &self.context_hooks else {
            return Ok(NextTurn::default());
        };
        context.host_messages = policy
            .next_host_messages
            .iter()
            .cloned()
            .map(SerializedJson::new)
            .collect();
        Ok(NextTurn {
            context: Some(context),
            model: Some(policy.next_model.clone()),
            thinking_level: Some(policy.next_thinking_level),
        })
    }

    fn should_stop_after_turn(
        &self,
        _context: &ContextEnvelope,
    ) -> Result<bool, pi_agent_core::error::HookError> {
        Ok(self.should_stop_after_turn)
    }
}

#[derive(Debug)]
struct FixtureProvider {
    streams: Mutex<VecDeque<FixtureModelStream>>,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}

impl ModelProvider for FixtureProvider {
    fn stream<'a>(
        &'a self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelFuture<'a> {
        self.requests
            .lock()
            .expect("fixture model request mutex poisoned")
            .push(request);
        let cancelled_before_request = cancellation.is_cancelled();
        let scripted = self
            .streams
            .lock()
            .expect("fixture model stream mutex poisoned")
            .pop_front()
            .ok_or_else(|| pi_agent_core::error::SchedulerError::UnknownToolCall {
                tool_call_id: ToolCallId::new("fixture-exhausted-model-script")
                    .expect("fixed fixture ID is non-empty"),
            });
        Box::pin(std::future::ready(scripted.map(move |script| {
            if cancelled_before_request {
                return Box::new(ModelStream {
                    events: vec![ModelStreamEvent::Aborted {
                        message: "Operation aborted".into(),
                    }],
                }) as _;
            }
            if script.cancel_after_text_delta {
                cancellation.cancel();
            }
            Box::new(script.stream) as _
        })))
    }
}

#[derive(Debug)]
struct FixtureTool {
    name: String,
    description: String,
    execution_mode: ToolExecutionMode,
    schema: JsonValue,
    responses: Mutex<Vec<FixtureToolResponse>>,
    active_queue_target: Arc<Mutex<Option<Agent>>>,
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
        context: ToolContext,
        updates: ToolUpdateSink,
    ) -> ToolFuture<'a> {
        let call_name = call.name.clone();
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
        let enqueue_during_execution = response
            .as_ref()
            .and_then(|response| response.enqueue_during_execution.clone());
        if let Some(response) = &response {
            for content in &response.updates {
                updates.emit(pi_agent_core::tool::ToolUpdate {
                    content: content.clone(),
                    details: None,
                });
                if response.cancel_after_update {
                    context.cancellation.cancel();
                }
            }
        }
        let result = match response {
            Some(response) => Ok(ToolResult {
                tool_call_id: call.id,
                content: response.content,
                details: None,
                usage: None,
                added_tool_names: Vec::new(),
                terminate: response.terminate,
                is_error: response.is_error,
            }),
            None => Err(pi_agent_core::error::ToolError::Execution {
                tool: call.name,
                message: "fixture has no matching host tool response".into(),
            }),
        };
        if yield_once || enqueue_during_execution.is_some() {
            let active_queue_target = Arc::clone(&self.active_queue_target);
            let tool_name = call_name;
            Box::pin(async move {
                if yield_once {
                    yield_to_another_tool().await;
                }
                if let Some(arrival) = enqueue_during_execution {
                    let agent = active_queue_target
                        .lock()
                        .expect("fixture active-queue target mutex poisoned")
                        .clone()
                        .ok_or_else(|| pi_agent_core::error::ToolError::Execution {
                            tool: tool_name.clone(),
                            message: "fixture queued a message before the agent was ready".into(),
                        })?;
                    match arrival {
                        FixtureActiveQueueArrival::Steer(text) => agent.enqueue_steering(text),
                        FixtureActiveQueueArrival::FollowUp(text) => agent.enqueue_follow_up(text),
                    }
                    .map_err(|error| {
                        pi_agent_core::error::ToolError::Execution {
                            tool: tool_name,
                            message: error.to_string(),
                        }
                    })?;
                }
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
            before_tool_policy: parse_before_tool_policy(host)?,
            after_tool_replace: parse_after_tool_replace(host)?,
            context_hooks: parse_context_hooks(setup)?,
            should_stop_after_turn: match host.get("should_stop_after_turn") {
                None => false,
                Some(JsonValue::Bool(value)) => *value,
                Some(_) => return Err("host.should_stop_after_turn must be a boolean".into()),
            },
            hold_agent_end_observer: parse_hold_agent_end_observer(host)?,
            tools,
            streams,
            last_usage,
            last_stop_reason,
        })
    }
}

fn parse_hold_agent_end_observer(host: &BTreeMap<String, JsonValue>) -> Result<bool, String> {
    let Some(observer) = host.get("observer") else {
        return Ok(false);
    };
    let observer = object(observer, "host.observer")?;
    match observer.get("hold_agent_end") {
        Some(JsonValue::Bool(true)) => Ok(true),
        Some(JsonValue::Bool(false)) => {
            Err("host.observer.hold_agent_end must be true in the V0 fixture adapter".into())
        }
        Some(_) => Err("host.observer.hold_agent_end must be a boolean".into()),
        None => Err("host.observer.hold_agent_end is required".into()),
    }
}

fn parse_before_tool_policy(
    host: &BTreeMap<String, JsonValue>,
) -> Result<Option<FixtureBeforeToolPolicy>, String> {
    let Some(rule) = host.get("before_tool_call") else {
        return Ok(None);
    };
    let rule = object(rule, "host.before_tool_call")?;
    let yield_once = match rule.get("yield_once") {
        None => false,
        Some(JsonValue::Bool(value)) => *value,
        Some(_) => return Err("host.before_tool_call.yield_once must be a boolean".into()),
    };
    let cancel_after_yield = match rule.get("cancel_after_yield") {
        None => false,
        Some(JsonValue::Bool(value)) => *value,
        Some(_) => return Err("host.before_tool_call.cancel_after_yield must be a boolean".into()),
    };
    if cancel_after_yield && !yield_once {
        return Err("host.before_tool_call.cancel_after_yield requires yield_once".into());
    }
    Ok(Some(FixtureBeforeToolPolicy {
        tool_name: string_field(rule, "tool_name")?.to_owned(),
        reason: string_field(rule, "reason")?.to_owned(),
        terminate: match rule.get("terminate") {
            None => false,
            Some(JsonValue::Bool(value)) => *value,
            Some(_) => return Err("host.before_tool_call.terminate must be a boolean".into()),
        },
        yield_once,
        cancel_after_yield,
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
        terminate: match rule.get("terminate") {
            None => None,
            Some(JsonValue::Bool(value)) => Some(*value),
            Some(_) => return Err("host.after_tool_call.terminate must be a boolean".into()),
        },
    }))
}

fn parse_context_hooks(
    setup: &BTreeMap<String, JsonValue>,
) -> Result<Option<FixtureContextHooks>, String> {
    let Some(value) = setup.get("context_hooks") else {
        return Ok(None);
    };
    let value = object(value, "setup.context_hooks")?;
    let host_messages = string_array(
        field(value, "host_messages")?,
        "setup.context_hooks.host_messages",
    )?;
    let transform_append_host_message =
        string_field(value, "transform_append_host_message")?.to_owned();
    let convert_prefix = string_field(value, "convert_prefix")?.to_owned();
    let next = object(
        field(value, "prepare_next_turn")?,
        "setup.context_hooks.prepare_next_turn",
    )?;
    let next_host_messages = string_array(
        field(next, "host_messages")?,
        "setup.context_hooks.prepare_next_turn.host_messages",
    )?;
    let next_model = object(
        field(next, "model")?,
        "setup.context_hooks.prepare_next_turn.model",
    )?;
    Ok(Some(FixtureContextHooks {
        host_messages,
        transform_append_host_message,
        convert_prefix,
        next_host_messages,
        next_model: ModelDescriptor {
            provider: string_field(next_model, "provider")?.to_owned(),
            model: string_field(next_model, "id")?.to_owned(),
            revision: None,
        },
        next_thinking_level: parse_thinking_level(string_field(next, "thinking_level")?)?,
    }))
}

fn string_array(value: &JsonValue, path: &str) -> Result<Vec<String>, String> {
    array(value, path)?
        .iter()
        .enumerate()
        .map(|(index, item)| match item {
            JsonValue::String(value) => Ok(value.clone()),
            _ => Err(format!("{path}[{index}] must be a string")),
        })
        .collect()
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
    let yield_once = match value.get("yield_once") {
        None => false,
        Some(JsonValue::Bool(value)) => *value,
        Some(_) => return Err("host tool call field \"yield_once\" must be a boolean".into()),
    };
    let enqueue_during_execution = match value.get("enqueue_during_execution") {
        None => None,
        Some(value) => {
            let arrival = object(value, "host tool call enqueue_during_execution")?;
            let text = string_field(arrival, "text")?.to_owned();
            match string_field(arrival, "kind")? {
                "steer" => Some(FixtureActiveQueueArrival::Steer(text)),
                "follow_up" => Some(FixtureActiveQueueArrival::FollowUp(text)),
                kind => {
                    return Err(format!(
                        "host tool call enqueue_during_execution.kind must be steer or follow_up, got {kind:?}"
                    ))
                }
            }
        }
    };
    if enqueue_during_execution.is_some() && !yield_once {
        return Err("host tool call enqueue_during_execution requires yield_once".into());
    }
    Ok(FixtureToolResponse {
        arguments: SerializedJson::new(
            field(value, "arguments")?
                .to_json_string()
                .map_err(|error| error.to_string())?,
        ),
        content: string_field(text, "text")?.to_owned(),
        is_error: bool_field(result, "is_error")?,
        yield_once,
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
        cancel_after_update: match value.get("cancel_after_update") {
            None => false,
            Some(JsonValue::Bool(value)) => *value,
            Some(_) => {
                return Err("host tool call field \"cancel_after_update\" must be a boolean".into())
            }
        },
        enqueue_during_execution,
        terminate: match result.get("terminate") {
            None => false,
            Some(JsonValue::Bool(value)) => *value,
            Some(_) => return Err("host tool result field \"terminate\" must be a boolean".into()),
        },
    })
}

fn parse_model_script(
    value: &JsonValue,
) -> Result<(Vec<FixtureModelStream>, FixtureUsage, StopReason), String> {
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
            let cancel_after_text_delta = parse_cancel_after(turn.get("cancel_after"), turn_index)?;
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
                    "error" if chunk_index + 1 == chunks.len() => {
                        let stop_reason = parse_stop_reason(string_field(chunk, "reason")?)?;
                        if !matches!(stop_reason, StopReason::Error | StopReason::Aborted) {
                            return Err(format!(
                                "model-script error at turn {turn_index}, index {chunk_index} must use error or aborted"
                            ));
                        }
                        let usage = FixtureUsage::parse(field(chunk, "usage")?)?;
                        last_usage = Some(usage);
                        last_stop_reason = Some(stop_reason);
                        match stop_reason {
                            StopReason::Error => events.push(ModelStreamEvent::Error {
                                message: string_field(chunk, "message")?.to_owned(),
                            }),
                            StopReason::Aborted => events.push(ModelStreamEvent::Aborted {
                                message: string_field(chunk, "message")?.to_owned(),
                            }),
                            _ => {
                                return Err(
                                    "model-script error stop reason escaped validation".into(),
                                );
                            }
                        }
                    }
                    _ => {
                        return Err(format!(
                            "unsupported model-script chunk {kind:?} at turn {turn_index}, index {chunk_index}"
                        ));
                    }
                }
            }
            if cancel_after_text_delta {
                let Some(text_delta_index) = events
                    .iter()
                    .position(|event| matches!(event, ModelStreamEvent::TextDelta(_)))
                else {
                    return Err(format!(
                        "model_script[{turn_index}].cancel_after text_delta requires a text_delta chunk"
                    ));
                };
                events.truncate(text_delta_index + 1);
                events.push(ModelStreamEvent::Aborted {
                    message: "Operation aborted".into(),
                });
                last_stop_reason = Some(StopReason::Aborted);
            }
            Ok(FixtureModelStream {
                stream: ModelStream { events },
                cancel_after_text_delta,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((
        streams,
        last_usage.ok_or_else(|| "model script must end with done or error".to_owned())?,
        last_stop_reason.ok_or_else(|| "model script must end with done or error".to_owned())?,
    ))
}

fn parse_cancel_after(value: Option<&JsonValue>, turn_index: usize) -> Result<bool, String> {
    match value {
        None => Ok(false),
        Some(JsonValue::String(value)) if value == "text_delta" => Ok(true),
        Some(JsonValue::String(value)) => Err(format!(
            "model_script[{turn_index}].cancel_after does not support {value:?}; use text_delta"
        )),
        Some(_) => Err(format!(
            "model_script[{turn_index}].cancel_after must be text_delta"
        )),
    }
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
        before_tool_policy,
        after_tool_replace,
        context_hooks,
        should_stop_after_turn,
        hold_agent_end_observer,
        tools,
        streams,
        last_usage,
        last_stop_reason: _last_stop_reason,
    } = fixture;
    let tool_names = tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    let model_provider = Arc::new(FixtureProvider {
        streams: Mutex::new(streams.into()),
        requests: Arc::new(Mutex::new(Vec::new())),
    });
    let quality_capture_requests =
        env::var("PI_AGENT_QUALITY_CAPTURE").ok().as_deref() == Some("1");
    let quality_request_contexts =
        quality_capture_requests.then(|| Arc::new(Mutex::new(Vec::new())));
    let active_queue_target = Arc::new(Mutex::new(None));
    let observer_gate = hold_agent_end_observer.then(|| Arc::new(FixtureObserverGate::default()));
    if observer_gate.is_some()
        && actions
            .iter()
            .filter(|action| matches!(action, FixtureAction::Prompt(_) | FixtureAction::Continue))
            .count()
            != 1
    {
        return Err("host.observer.hold_agent_end requires exactly one run-starting action".into());
    }
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
    if before_tool_policy.is_some()
        || after_tool_replace.is_some()
        || context_hooks.is_some()
        || should_stop_after_turn
        || quality_capture_requests
    {
        builder = builder.hooks(Arc::new(FixtureHooks {
            before_tool_policy,
            after_tool_replace,
            context_hooks: context_hooks.clone(),
            should_stop_after_turn,
            request_contexts: quality_request_contexts.clone(),
        }));
    }
    if let Some(context_hooks) = &context_hooks {
        for message in &context_hooks.host_messages {
            builder = builder.host_message(SerializedJson::new(message.clone()));
        }
    }
    if let Some(observer_gate) = &observer_gate {
        builder = builder.observer(Arc::clone(observer_gate) as Arc<dyn EventObserver>);
    }
    for tool in tools {
        builder = builder.tool(Arc::new(FixtureTool {
            name: tool.name,
            description: tool.description,
            execution_mode: tool.execution_mode,
            schema: JsonValue::parse(tool.parameters.as_str())
                .map_err(|error| error.to_string())?,
            responses: Mutex::new(tool.responses),
            active_queue_target: Arc::clone(&active_queue_target),
        }));
    }
    let agent = builder.build();
    *active_queue_target
        .lock()
        .expect("fixture active-queue target mutex poisoned") = Some(agent.clone());
    let mut events = Vec::new();
    let mut event_sequence = 0;
    let mut turn_offset = 0;
    let mut outcome = "completed";
    let mut observer_active_before_release = None;
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
            // Pi represents a completed error/aborted assistant response as
            // terminal lifecycle events, not an adapter process failure. The
            // Rust library preserves its typed error for direct callers; this
            // closed parity adapter normalizes that API distinction only after
            // confirming the run settled with the equivalent terminal reason.
            let drive_result = if let Some(observer_gate) = &observer_gate {
                let mut driving = Box::pin(run.drive());
                std::future::poll_fn(|context| match driving.as_mut().poll(context) {
                    Poll::Ready(_) => Poll::Ready(Err(
                        "run settled before its held agent_end observer was released".to_owned(),
                    )),
                    Poll::Pending if observer_gate.reached.load(Ordering::Acquire) => {
                        Poll::Ready(Ok(()))
                    }
                    Poll::Pending => {
                        context.waker().wake_by_ref();
                        Poll::Pending
                    }
                })
                .await?;
                let active = !matches!(agent.snapshot().phase, AgentPhase::Idle);
                if !active {
                    return Err(
                        "agent became idle before its held agent_end observer was released".into(),
                    );
                }
                observer_active_before_release = Some(active);
                observer_gate.release();
                driving.await
            } else {
                run.drive().await
            };
            match drive_result {
                Ok(()) => outcome = "completed",
                Err(CoreError::Cancelled) => outcome = "cancelled",
                Err(CoreError::ModelError { .. } | CoreError::ModelAborted { .. }) => {
                    // Provider/model failures are terminal assistant responses
                    // in this adapter and do not mean the host cancelled the run.
                    outcome = "completed";
                }
                Err(error) => return Err(core_error(error)),
            }
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
    let actual_stop_reason = snapshot
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            Message::Assistant { stop_reason, .. } => *stop_reason,
            Message::User { .. } | Message::ToolResult { .. } => None,
        })
        .ok_or_else(|| "Rust agent did not retain a terminal assistant response".to_owned())?;
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
            JsonValue::object([("outcome", JsonValue::from(outcome))]),
        ),
    ]));

    let mut result_fields = vec![
        ("format_version", JsonValue::from(1_u64)),
        ("kind", JsonValue::from("canonical_parity_result")),
        ("fixture_id", JsonValue::from(id)),
        ("outcome", JsonValue::from(outcome)),
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
                    JsonValue::from(stop_reason_name(actual_stop_reason)),
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
    ];
    if context_hooks.is_some() {
        let requests = model_provider
            .requests
            .lock()
            .expect("fixture model request mutex poisoned");
        result_fields.push((
            "request_trace",
            JsonValue::Array(requests.iter().map(normalize_request).collect()),
        ));
    } else if quality_capture_requests {
        let requests = model_provider
            .requests
            .lock()
            .expect("fixture model request mutex poisoned");
        let contexts = quality_request_contexts
            .as_ref()
            .expect("quality request capture must allocate contexts")
            .lock()
            .expect("fixture quality request-context mutex poisoned");
        if requests.len() != contexts.len() {
            return Err("quality request capture count does not match model request count".into());
        }
        result_fields.push((
            "request_trace",
            JsonValue::Array(
                requests
                    .iter()
                    .zip(contexts.iter())
                    .map(|(request, context)| normalize_quality_request(request, context))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
        ));
    }
    if observer_gate.is_some() {
        result_fields.push((
            "observer_settlement",
            JsonValue::object([
                ("agent_end_observed", JsonValue::from(true)),
                (
                    "active_before_release",
                    JsonValue::from(observer_active_before_release == Some(true)),
                ),
                ("idle_after_release", JsonValue::from(true)),
            ]),
        ));
    }
    Ok(JsonValue::object(result_fields))
}

fn normalize_request(request: &ModelRequest) -> JsonValue {
    JsonValue::object([
        ("context", JsonValue::from(request.context.clone())),
        (
            "model",
            request
                .model
                .as_ref()
                .map(|model| {
                    JsonValue::object([
                        ("provider", JsonValue::from(model.provider.clone())),
                        ("id", JsonValue::from(model.model.clone())),
                    ])
                })
                .unwrap_or(JsonValue::Null),
        ),
        (
            "thinking_level",
            JsonValue::from(thinking_level_name(request.thinking_level)),
        ),
    ])
}

/// Normalize the request at the shared logical boundary before a provider
/// adapter serializes it. The fixture contract captures this same semantic
/// surface, so request fingerprints retain ordering and schemas without
/// conflating a transport's wire format with core parity.
fn normalize_quality_request(
    request: &ModelRequest,
    context: &ContextEnvelope,
) -> Result<JsonValue, String> {
    Ok(JsonValue::object([
        (
            "system_prompt",
            JsonValue::from(request.system_prompt.clone()),
        ),
        (
            "messages",
            JsonValue::Array(
                context
                    .messages
                    .iter()
                    .map(normalize_message)
                    .collect::<Result<Vec<_>, _>>()?,
            ),
        ),
        (
            "host_messages",
            JsonValue::Array(
                context
                    .host_messages
                    .iter()
                    .map(|message| JsonValue::from(message.as_str().to_owned()))
                    .collect(),
            ),
        ),
        (
            "tools",
            JsonValue::Array(
                request
                    .tools
                    .iter()
                    .map(|tool| {
                        JsonValue::object([
                            ("name", JsonValue::from(tool.name.clone())),
                            ("description", JsonValue::from(tool.description.clone())),
                            ("parameters", tool.schema.clone()),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "model",
            request
                .model
                .as_ref()
                .map(|model| {
                    JsonValue::object([
                        ("provider", JsonValue::from(model.provider.clone())),
                        ("id", JsonValue::from(model.model.clone())),
                    ])
                })
                .unwrap_or(JsonValue::Null),
        ),
        (
            "thinking_level",
            JsonValue::from(thinking_level_name(request.thinking_level)),
        ),
    ]))
}

fn normalize_event(
    sequence: usize,
    event: &AgentEvent,
    turn_offset: u64,
) -> Result<JsonValue, String> {
    let (kind, data) = match &event.kind {
        AgentEventKind::CompactionStart { .. } => ("compaction_start", empty_object()),
        AgentEventKind::CompactionResult { .. } => ("compaction_result", empty_object()),
        AgentEventKind::CompactionEnd { .. } => ("compaction_end", empty_object()),
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
        AgentEventKind::ModelTurnUsage { .. } => ("model_turn_usage", empty_object()),
        AgentEventKind::MessageStart { message } => (
            "message_start",
            JsonValue::object([("role", JsonValue::from(message_role_name(message)))]),
        ),
        AgentEventKind::MessageEnd { message } => (
            "message_end",
            JsonValue::object([("role", JsonValue::from(message_role_name(message)))]),
        ),
        AgentEventKind::MessageUpdate {
            message,
            text_delta,
        } => (
            "message_update",
            JsonValue::object([
                ("role", JsonValue::from(message_role_name(message))),
                (
                    "delta",
                    JsonValue::from(
                        text_delta
                            .as_deref()
                            .unwrap_or_else(|| message_text(message)),
                    ),
                ),
            ]),
        ),
        AgentEventKind::ToolExecutionStart {
            tool_call_id,
            tool_name,
            ..
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
        "minimal" => Ok(ThinkingLevel::Minimal),
        "low" => Ok(ThinkingLevel::Low),
        "medium" => Ok(ThinkingLevel::Medium),
        "high" => Ok(ThinkingLevel::High),
        "xhigh" => Ok(ThinkingLevel::XHigh),
        "max" => Ok(ThinkingLevel::Max),
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
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::XHigh => "xhigh",
        ThinkingLevel::Max => "max",
    }
}

fn parse_stop_reason(value: &str) -> Result<StopReason, String> {
    match value {
        "stop" => Ok(StopReason::EndTurn),
        "tool_call" => Ok(StopReason::ToolUse),
        "length" => Ok(StopReason::Length),
        "aborted" => Ok(StopReason::Aborted),
        "cancelled" => Ok(StopReason::Cancelled),
        "error" => Ok(StopReason::Error),
        _ => Err(format!("unsupported model stop reason {value:?}")),
    }
}

fn stop_reason_name(value: StopReason) -> &'static str {
    match value {
        StopReason::EndTurn => "stop",
        StopReason::ToolUse => "tool_call",
        StopReason::Length => "length",
        StopReason::Aborted => "aborted",
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
