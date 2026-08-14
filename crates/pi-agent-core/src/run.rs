//! Ownership handle for one active run.
//!
//! `RunHandle` is deliberately small: it owns lifecycle settlement and delegates model/tool
//! work to the caller-owned executor.  Dropping an unfinished handle requests cancellation and
//! settles the agent as cancelled, ensuring an abandoned run cannot leave the agent busy.

use crate::agent::AgentInner;
use crate::error::CoreError;
use crate::event::{AgentEvent, AgentEventKind, EventSequence};
use crate::hooks::{AfterToolCall, BeforeToolCall, NextTurn, Replacement};
use crate::scheduler::{CancellationToken, ModelEventStream, ModelRequest, ModelStreamEvent};
use crate::schema_validation::validate_tool_arguments;
use crate::state::{
    AgentPhase, AssistantToolCall, Message, ModelDescriptor, RunId, RunPhase, RunSnapshot,
    RunState, StopReason, ThinkingLevel, ToolCallId, TurnId, Usage,
};
use crate::tool::{
    AgentTool, ToolCall, ToolContext, ToolFuture, ToolResult, ToolUpdate, ToolUpdateSink,
};
use std::collections::BTreeSet;
use std::sync::mpsc::TrySendError;
use std::sync::{Arc, Mutex, Weak};
use std::task::{Poll, Waker};

enum PreparedToolCall {
    Immediate { result: ToolResult, terminate: bool },
    Execute { tool: Arc<dyn AgentTool> },
}

struct PreparedToolExecution {
    source_index: usize,
    call: ToolCall,
    preparation: PreparedToolCall,
}

/// One update captured by a tool callback and waiting for lifecycle delivery.
type PendingToolUpdate = (ToolCallId, String, ToolUpdate);

/// Tool updates captured by capability callbacks.
///
/// The callback API is synchronous, while lifecycle observers are awaited. The
/// queue bridges those boundaries without requiring a runtime: callbacks wake
/// the caller-owned run future, which drains updates as a first-class scheduler
/// step before polling another tool or settling the current tool. Call IDs are
/// closed as soon as their futures resolve so late callbacks are ignored, as in
/// Pi's `executePreparedToolCall` lifecycle.
#[derive(Clone, Default)]
struct PendingToolUpdates {
    state: Arc<Mutex<PendingToolUpdateState>>,
}

#[derive(Default)]
struct PendingToolUpdateState {
    updates: Vec<PendingToolUpdate>,
    closed_calls: BTreeSet<ToolCallId>,
    waker: Option<Waker>,
}

impl PendingToolUpdates {
    fn push(&self, update: PendingToolUpdate) {
        let waker = {
            let mut state = self.state.lock().expect("tool update mutex poisoned");
            if state.closed_calls.contains(&update.0) {
                return;
            }
            state.updates.push(update);
            state.waker.clone()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn take(&self) -> Option<Vec<PendingToolUpdate>> {
        let mut state = self.state.lock().expect("tool update mutex poisoned");
        if state.updates.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut state.updates))
        }
    }

    fn close(&self, call_id: &ToolCallId) {
        let mut state = self.state.lock().expect("tool update mutex poisoned");
        state.closed_calls.insert(call_id.clone());
    }

    fn register_waker(&self, waker: &Waker) {
        self.state.lock().expect("tool update mutex poisoned").waker = Some(waker.clone());
    }
}

struct PendingToolExecution<'a> {
    source_index: usize,
    call: ToolCall,
    future: ToolFuture<'a>,
}

struct CompletedToolExecution {
    source_index: usize,
    call: ToolCall,
    result: Result<ToolResult, crate::error::ToolError>,
}

/// A handle to the one run currently owning an agent.
pub struct RunHandle {
    pub(crate) agent: Weak<AgentInner>,
    pub(crate) state: Arc<Mutex<RunState>>,
    pub(crate) cancellation: CancellationToken,
    pub(crate) initial_messages: Vec<Message>,
    /// Index of the first message created by this invocation. `AgentEnd`
    /// reports this suffix, matching Pi continuation semantics.
    pub(crate) message_start_index: usize,
    pub(crate) skip_initial_steering: bool,
}

impl std::fmt::Debug for RunHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunHandle")
            .field("snapshot", &self.snapshot())
            .finish()
    }
}

impl RunHandle {
    /// Stable identifier for this run.
    pub fn id(&self) -> RunId {
        self.state.lock().expect("run state mutex poisoned").id
    }

    /// Return an owned snapshot that cannot mutate the run.
    pub fn snapshot(&self) -> RunSnapshot {
        self.state
            .lock()
            .expect("run state mutex poisoned")
            .snapshot()
    }

    /// Cancellation token shared with model and tool operations.
    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    /// Return events emitted by this run in their immutable source order.
    pub fn events(&self) -> Vec<AgentEvent> {
        self.state
            .lock()
            .expect("run state mutex poisoned")
            .events
            .clone()
    }

    /// Drive a complete caller-owned run.
    ///
    /// The caller polls this future on its own executor; the core neither
    /// creates an executor nor spawns detached work. A tool-use turn executes
    /// its calls, records their results, and then drives the next model turn.
    pub async fn drive(&self) -> Result<(), CoreError> {
        let result = self.drive_inner().await;
        if let Err(error) = &result {
            if !self.snapshot().phase.is_terminal() {
                if self.cancellation.is_cancelled() {
                    self.settle_cancellation().await;
                } else {
                    self.settle_failure(error).await;
                }
            }
        }
        result
    }

    async fn settle_failure(&self, error: &CoreError) {
        let Some(agent) = self.agent.upgrade() else {
            let _ = self.fail(error.to_string());
            return;
        };
        let failure = {
            let mut state = agent.state.lock().expect("agent state mutex poisoned");
            let message = Message::Assistant {
                id: state.allocate_message_id(),
                content: String::new(),
                tool_calls: Vec::new(),
                stop_reason: Some(StopReason::Error),
                error_message: Some(error.to_string()),
            };
            state.partial_response = None;
            state.pending_tool_calls.clear();
            state.messages.push(message.clone());
            message
        };
        let turn_id = self.snapshot().turn_id.unwrap_or(TurnId(1));

        let _ = self
            .emit(
                &agent,
                AgentEventKind::MessageStart {
                    message: failure.clone(),
                },
            )
            .await;
        let _ = self
            .emit(&agent, AgentEventKind::MessageEnd { message: failure })
            .await;
        let _ = self
            .emit(
                &agent,
                AgentEventKind::TurnEnd {
                    turn_id,
                    reason: StopReason::Error,
                },
            )
            .await;
        let messages = self.new_messages(&agent);
        let _ = self
            .emit(&agent, AgentEventKind::AgentEnd { messages })
            .await;
        let _ = self.fail(error.to_string());
    }

    async fn drive_inner(&self) -> Result<(), CoreError> {
        let agent = self.agent.upgrade().ok_or(CoreError::InvalidTransition(
            crate::error::StateTransitionError::new("run", "orphaned", "drive"),
        ))?;
        let run_id = self.id();
        self.start_turn(TurnId(1))?;

        {
            let state = agent.state.lock().expect("agent state mutex poisoned");
            if !matches!(state.phase, AgentPhase::Running(id) | AgentPhase::Cancelling(id) if id == run_id)
            {
                return Err(CoreError::InvalidTransition(
                    crate::error::StateTransitionError::new("agent", "not-running", "drive"),
                ));
            }
        }
        self.emit(&agent, AgentEventKind::AgentStart).await?;
        self.emit(&agent, AgentEventKind::TurnStart { turn_id: TurnId(1) })
            .await?;
        for message in &self.initial_messages {
            self.emit(
                &agent,
                AgentEventKind::MessageStart {
                    message: message.clone(),
                },
            )
            .await?;
            self.emit(
                &agent,
                AgentEventKind::MessageEnd {
                    message: message.clone(),
                },
            )
            .await?;
        }
        let mut turn_id = TurnId(1);
        let mut model_override = None::<ModelDescriptor>;
        let mut thinking_override = None::<ThinkingLevel>;
        let mut next_context = if self.skip_initial_steering {
            None
        } else {
            self.inject_queued_messages(
                &agent,
                self.current_context(&agent)?,
                self.drain_steering(&agent),
            )
            .await?
        };
        loop {
            let request = self
                .model_request(
                    &agent,
                    run_id,
                    next_context.take(),
                    model_override.as_ref(),
                    thinking_override,
                )
                .await?;
            let turn_model = request.model.clone();
            let provider = agent
                .provider
                .read()
                .expect("agent provider lock poisoned")
                .clone()
                .ok_or(CoreError::MissingModelProvider)?;
            let mut stream = provider
                .stream(request, self.cancellation.clone())
                .await
                .map_err(|error| CoreError::ModelProvider {
                    message: error.to_string(),
                })?;
            let (reason, tool_calls, error_message) = self
                .consume_assistant_stream(&agent, stream.as_mut(), turn_id, turn_model)
                .await?;

            if matches!(reason, StopReason::Error | StopReason::Aborted) {
                self.emit(&agent, AgentEventKind::TurnEnd { turn_id, reason })
                    .await?;
                let messages = self.new_messages(&agent);
                self.emit(&agent, AgentEventKind::AgentEnd { messages })
                    .await?;
                let message = error_message.unwrap_or_else(|| {
                    if reason == StopReason::Aborted {
                        "model response was aborted".into()
                    } else {
                        "model response failed".into()
                    }
                });
                let error = if reason == StopReason::Aborted {
                    CoreError::ModelAborted {
                        message: message.clone(),
                    }
                } else {
                    CoreError::ModelError {
                        message: message.clone(),
                    }
                };
                if reason == StopReason::Aborted && self.cancellation.is_cancelled() {
                    self.finish(RunPhase::Cancelled, StopReason::Aborted, None)?;
                    return Err(CoreError::Cancelled);
                }
                self.fail(message)?;
                return Err(error);
            }

            let terminate_tool_batch = if tool_calls.is_empty() {
                false
            } else if reason == StopReason::Length {
                self.fail_truncated_tool_calls(&agent, &tool_calls).await?
            } else {
                if reason != StopReason::ToolUse {
                    return Err(CoreError::UnsupportedModelStream {
                        message: format!(
                            "assistant emitted tool calls with terminal reason {reason:?}, expected ToolUse"
                        ),
                    });
                }
                self.execute_tool_calls(&agent, &tool_calls).await?
            };

            self.emit(&agent, AgentEventKind::TurnEnd { turn_id, reason })
                .await?;

            let current_context = self.current_context(&agent)?;
            let NextTurn {
                context,
                model,
                thinking_level,
            } = agent
                .hooks
                .prepare_next_turn_async(current_context.clone(), self.cancellation.clone())
                .await?;
            let prepared_context = context.unwrap_or(current_context);
            if let Some(model) = model {
                model_override = Some(model);
            }
            if let Some(thinking_level) = thinking_level {
                thinking_override = Some(thinking_level);
            }
            if agent
                .hooks
                .should_stop_after_turn_async(&prepared_context, self.cancellation.clone())
                .await?
            {
                return self.emit_agent_end_and_succeed(&agent, reason).await;
            }

            let mut queued = self.drain_steering(&agent);
            let has_more_tool_calls = !tool_calls.is_empty() && !terminate_tool_batch;
            if !has_more_tool_calls && queued.is_empty() {
                queued = self.drain_follow_up(&agent);
            }

            if has_more_tool_calls || !queued.is_empty() {
                turn_id = TurnId(turn_id.0.saturating_add(1));
                self.advance_turn(turn_id)?;
                self.emit(&agent, AgentEventKind::TurnStart { turn_id })
                    .await?;
                next_context = self
                    .inject_queued_messages(&agent, prepared_context, queued)
                    .await?;
                continue;
            }

            return self.emit_agent_end_and_succeed(&agent, reason).await;
        }
    }

    async fn model_request(
        &self,
        agent: &AgentInner,
        run_id: RunId,
        context: Option<crate::hooks::ContextEnvelope>,
        model_override: Option<&ModelDescriptor>,
        thinking_override: Option<ThinkingLevel>,
    ) -> Result<ModelRequest, CoreError> {
        let (context, system_prompt, model, thinking_level, tools) = {
            let mut state = agent.state.lock().expect("agent state mutex poisoned");
            if !matches!(state.phase, AgentPhase::Running(id) | AgentPhase::Cancelling(id) if id == run_id)
            {
                return Err(CoreError::InvalidTransition(
                    crate::error::StateTransitionError::new(
                        "agent",
                        "not-running",
                        "model_request",
                    ),
                ));
            }
            state.is_streaming = true;
            (
                context.unwrap_or_else(|| crate::hooks::ContextEnvelope {
                    version: 1,
                    messages: state.messages.clone(),
                    host_messages: state.host_messages.clone(),
                }),
                state.system_prompt.clone(),
                model_override.cloned().or_else(|| state.model.clone()),
                thinking_override.unwrap_or(state.thinking_level),
                agent.tools.definitions(),
            )
        };
        let transformed = agent
            .hooks
            .transform_context_async(context, self.cancellation.clone())
            .await?;
        let request = ModelRequest {
            system_prompt,
            context: agent
                .hooks
                .convert_to_llm_async(transformed, self.cancellation.clone())
                .await?,
            tools,
            model,
            thinking_level,
        };
        Ok(request)
    }

    fn current_context(
        &self,
        agent: &AgentInner,
    ) -> Result<crate::hooks::ContextEnvelope, CoreError> {
        let state = agent.state.lock().expect("agent state mutex poisoned");
        if !matches!(
            state.phase,
            AgentPhase::Running(_) | AgentPhase::Cancelling(_)
        ) {
            return Err(CoreError::InvalidTransition(
                crate::error::StateTransitionError::new("agent", "not-running", "current_context"),
            ));
        }
        Ok(crate::hooks::ContextEnvelope {
            version: 1,
            messages: state.messages.clone(),
            host_messages: state.host_messages.clone(),
        })
    }

    fn drain_steering(&self, agent: &AgentInner) -> Vec<crate::queue::QueuedMessage> {
        let mode = *agent
            .steering_mode
            .lock()
            .expect("agent steering mode mutex poisoned");
        agent
            .queues
            .lock()
            .expect("agent queue mutex poisoned")
            .steering
            .drain(mode)
    }

    fn drain_follow_up(&self, agent: &AgentInner) -> Vec<crate::queue::QueuedMessage> {
        let mode = *agent
            .follow_up_mode
            .lock()
            .expect("agent follow-up mode mutex poisoned");
        agent
            .queues
            .lock()
            .expect("agent queue mutex poisoned")
            .follow_up
            .drain(mode)
    }

    async fn inject_queued_messages(
        &self,
        agent: &AgentInner,
        mut context: crate::hooks::ContextEnvelope,
        queued: Vec<crate::queue::QueuedMessage>,
    ) -> Result<Option<crate::hooks::ContextEnvelope>, CoreError> {
        if queued.is_empty() {
            // Preserve the context prepared by `prepare_next_turn`, even when
            // no user message is waiting. Tool continuations still consume
            // this replacement before the next model request.
            return Ok(Some(context));
        }
        for queued_message in queued {
            let message = {
                let mut state = agent.state.lock().expect("agent state mutex poisoned");
                let message = Message::User {
                    id: state.allocate_message_id(),
                    content: queued_message.content,
                };
                state.messages.push(message.clone());
                message
            };
            context.messages.push(message.clone());
            self.emit(
                agent,
                AgentEventKind::MessageStart {
                    message: message.clone(),
                },
            )
            .await?;
            self.emit(agent, AgentEventKind::MessageEnd { message })
                .await?;
        }
        Ok(Some(context))
    }

    async fn consume_assistant_stream(
        &self,
        agent: &AgentInner,
        stream: &mut dyn ModelEventStream,
        turn_id: TurnId,
        model: Option<ModelDescriptor>,
    ) -> Result<(StopReason, Vec<AssistantToolCall>, Option<String>), CoreError> {
        let mut assistant_id = None;
        let mut assistant_text = String::new();
        let mut tool_calls = Vec::new();
        let mut reason = None;
        let mut error_message = None;
        let mut usage: Option<Usage> = None;

        loop {
            let Some(item) =
                stream
                    .next_event(self.cancellation.clone())
                    .await
                    .map_err(|error| CoreError::ModelProvider {
                        message: error.to_string(),
                    })?
            else {
                break;
            };
            if reason.is_some() {
                return Err(CoreError::UnsupportedModelStream {
                    message: "model stream contained events after its terminal event".into(),
                });
            }
            match item {
                ModelStreamEvent::TextDelta(delta) => {
                    let (message, message_id, first_delta) = {
                        let mut state = agent.state.lock().expect("agent state mutex poisoned");
                        let first_delta = assistant_id.is_none();
                        let id = *assistant_id.get_or_insert_with(|| state.allocate_message_id());
                        if first_delta {
                            state.messages.push(Message::Assistant {
                                id,
                                content: String::new(),
                                tool_calls: Vec::new(),
                                stop_reason: None,
                                error_message: None,
                            });
                        }
                        assistant_text.push_str(&delta);
                        state.partial_response = Some(assistant_text.clone());
                        let message = Message::Assistant {
                            id,
                            content: assistant_text.clone(),
                            tool_calls: Vec::new(),
                            stop_reason: None,
                            error_message: None,
                        };
                        *state
                            .messages
                            .last_mut()
                            .expect("assistant message was inserted") = message.clone();
                        (message, id, first_delta)
                    };
                    if first_delta {
                        self.emit(
                            agent,
                            AgentEventKind::MessageStart {
                                message: Message::Assistant {
                                    id: message_id,
                                    content: String::new(),
                                    tool_calls: Vec::new(),
                                    stop_reason: None,
                                    error_message: None,
                                },
                            },
                        )
                        .await?;
                    }
                    self.emit(
                        agent,
                        AgentEventKind::MessageUpdate {
                            message,
                            text_delta: Some(delta),
                        },
                    )
                    .await?;
                }
                ModelStreamEvent::ToolCall(call) => tool_calls.push(call),
                ModelStreamEvent::Usage(update) => {
                    if let Some(current) = usage.as_mut() {
                        current.merge(update);
                    } else {
                        usage = Some(update);
                    }
                }
                ModelStreamEvent::Error { message } => {
                    reason = Some(StopReason::Error);
                    error_message = Some(message);
                }
                ModelStreamEvent::Aborted { message } => {
                    reason = Some(StopReason::Aborted);
                    error_message = Some(message);
                }
                ModelStreamEvent::End(next_reason) => reason = Some(next_reason),
            }
        }

        let reason = reason.ok_or(CoreError::UnsupportedModelStream {
            message: "model stream ended without a terminal event".into(),
        })?;
        let assistant = {
            let mut state = agent.state.lock().expect("agent state mutex poisoned");
            let id = assistant_id.unwrap_or_else(|| state.allocate_message_id());
            let assistant = Message::Assistant {
                id,
                content: assistant_text,
                tool_calls: tool_calls.clone(),
                stop_reason: Some(reason),
                error_message: error_message.clone(),
            };
            state.partial_response = None;
            state.is_streaming = false;
            if assistant_id.is_some() {
                *state
                    .messages
                    .last_mut()
                    .expect("streamed assistant message was inserted") = assistant.clone();
            } else {
                state.messages.push(assistant.clone());
            }
            assistant
        };
        if assistant_id.is_none() {
            self.emit(
                agent,
                AgentEventKind::MessageStart {
                    message: assistant.clone(),
                },
            )
            .await?;
        }
        self.emit(agent, AgentEventKind::MessageEnd { message: assistant })
            .await?;
        if let Some(usage) = usage {
            let accounting = crate::state::ModelTurnAccounting {
                run_id: self.id(),
                turn_id,
                model,
                usage,
            };
            {
                let mut state = agent.state.lock().expect("agent state mutex poisoned");
                state.accounting.record(accounting.clone());
            }
            self.emit(agent, AgentEventKind::ModelTurnUsage { accounting })
                .await?;
        }
        Ok((reason, tool_calls, error_message))
    }

    /// Refuse tool calls from a length-truncated assistant response. The
    /// provider may have emitted syntactically plausible JSON after a partial
    /// argument stream, but upstream treats every such call as unsafe.
    async fn fail_truncated_tool_calls(
        &self,
        agent: &AgentInner,
        tool_calls: &[AssistantToolCall],
    ) -> Result<bool, CoreError> {
        for assistant_call in tool_calls {
            let call = ToolCall {
                id: assistant_call.id.clone(),
                name: assistant_call.name.clone(),
                arguments: assistant_call.arguments.clone(),
            };
            {
                let mut state = agent.state.lock().expect("agent state mutex poisoned");
                state.pending_tool_calls.insert(call.id.clone());
            }
            self.emit(
                agent,
                AgentEventKind::ToolExecutionStart {
                    tool_call_id: call.id.clone(),
                    tool_name: call.name.clone(),
                    arguments: call.arguments.clone(),
                },
            )
            .await?;
            let result = error_tool_result(
                &call,
                format!(
                    "Tool call \"{}\" was not executed: the response hit the output token limit, so its arguments may be truncated. Re-issue the tool call with complete arguments.",
                    call.name
                ),
            );
            self.emit_tool_execution_end(agent, &call, &result).await?;
            self.append_tool_result_message(agent, call, result).await?;
        }
        Ok(false)
    }

    async fn execute_tool_calls(
        &self,
        agent: &AgentInner,
        tool_calls: &[AssistantToolCall],
    ) -> Result<bool, CoreError> {
        let has_sequential_tool = tool_calls.iter().any(|assistant_call| {
            agent.tools.get(&assistant_call.name).is_some_and(|tool| {
                tool.execution_mode() == crate::tool::ToolExecutionMode::Sequential
            })
        });
        if has_sequential_tool {
            self.execute_tool_calls_sequential(agent, tool_calls).await
        } else {
            self.execute_tool_calls_parallel(agent, tool_calls).await
        }
    }

    async fn execute_tool_calls_sequential(
        &self,
        agent: &AgentInner,
        tool_calls: &[AssistantToolCall],
    ) -> Result<bool, CoreError> {
        let mut all_terminate = true;
        for assistant_call in tool_calls {
            let call = ToolCall {
                id: assistant_call.id.clone(),
                name: assistant_call.name.clone(),
                arguments: assistant_call.arguments.clone(),
            };
            {
                let mut state = agent.state.lock().expect("agent state mutex poisoned");
                state.pending_tool_calls.insert(call.id.clone());
            }
            self.emit(
                agent,
                AgentEventKind::ToolExecutionStart {
                    tool_call_id: call.id.clone(),
                    tool_name: call.name.clone(),
                    arguments: call.arguments.clone(),
                },
            )
            .await?;

            let (result, terminate) = self.execute_one_tool_call(agent, call.clone()).await?;
            self.emit_tool_execution_end(agent, &call, &result).await?;
            self.append_tool_result_message(agent, call, result).await?;
            all_terminate &= terminate;
        }
        Ok(all_terminate)
    }

    async fn execute_tool_calls_parallel(
        &self,
        agent: &AgentInner,
        tool_calls: &[AssistantToolCall],
    ) -> Result<bool, CoreError> {
        let mut prepared = Vec::with_capacity(tool_calls.len());
        let updates = PendingToolUpdates::default();
        let mut completions = (0..tool_calls.len())
            .map(|_| None::<(ToolResult, bool)>)
            .collect::<Vec<_>>();

        // Pi announces each call and prepares it in source order before it starts the parallel
        // batch. Immediate preparation failures therefore end before later calls are announced;
        // successful result messages remain deferred until every batch completion is known.
        for (source_index, assistant_call) in tool_calls.iter().enumerate() {
            let call = ToolCall {
                id: assistant_call.id.clone(),
                name: assistant_call.name.clone(),
                arguments: assistant_call.arguments.clone(),
            };
            {
                let mut state = agent.state.lock().expect("agent state mutex poisoned");
                state.pending_tool_calls.insert(call.id.clone());
            }
            self.emit(
                agent,
                AgentEventKind::ToolExecutionStart {
                    tool_call_id: call.id.clone(),
                    tool_name: call.name.clone(),
                    arguments: call.arguments.clone(),
                },
            )
            .await?;
            let preparation = self.prepare_tool_call(agent, &call).await?;
            if let PreparedToolCall::Immediate { result, terminate } = &preparation {
                self.emit_tool_execution_end(agent, &call, result).await?;
                completions[source_index] = Some((result.clone(), *terminate));
            }
            prepared.push(PreparedToolExecution {
                source_index,
                call,
                preparation,
            });
        }

        // `pending` borrows the tool Arcs retained in `prepared`; it is declared after the
        // prepared vector so futures are dropped before their referenced capabilities.
        let mut pending = Vec::new();
        for prepared_call in &prepared {
            if let PreparedToolCall::Execute { tool } = &prepared_call.preparation {
                let future =
                    self.start_tool_future(tool, prepared_call.call.clone(), updates.clone());
                pending.push(PendingToolExecution {
                    source_index: prepared_call.source_index,
                    call: prepared_call.call.clone(),
                    future,
                });
            }
        }
        while !pending.is_empty() {
            match next_parallel_step(&mut pending, &updates).await {
                ParallelToolStep::Updates(pending_updates) => {
                    let update_call_ids = pending_updates
                        .iter()
                        .map(|(tool_call_id, _, _)| tool_call_id.clone())
                        .collect::<std::collections::BTreeSet<_>>();
                    self.emit_tool_updates(agent, pending_updates).await?;
                    if self.cancellation.is_cancelled() {
                        // Pi's parallel scheduler gives the sibling calls a
                        // terminal result before it settles the call whose
                        // update requested cancellation. Keeping that call
                        // pending retains its normal completion semantics.
                        // A sibling is polled once only: an already-ready
                        // result is safe to preserve, while a pending future
                        // is dropped and turned into a cancellation result so
                        // a cancellation-unaware tool cannot hold the run.
                        let mut still_running = Vec::new();
                        for mut pending_call in std::mem::take(&mut pending) {
                            if update_call_ids.contains(&pending_call.call.id) {
                                still_running.push(pending_call);
                                continue;
                            }
                            let execution = std::future::poll_fn(|context| {
                                match pending_call.future.as_mut().poll(context) {
                                    Poll::Ready(result) => Poll::Ready(Some(result)),
                                    Poll::Pending => Poll::Ready(None),
                                }
                            })
                            .await;
                            updates.close(&pending_call.call.id);
                            self.flush_tool_updates(agent, &updates).await?;
                            let (result, terminate) = match execution {
                                Some(result) => {
                                    self.finalize_executed_tool(agent, &pending_call.call, result)
                                        .await?
                                }
                                None => (
                                    error_tool_result(&pending_call.call, "Operation aborted"),
                                    false,
                                ),
                            };
                            self.emit_tool_execution_end(agent, &pending_call.call, &result)
                                .await?;
                            completions[pending_call.source_index] = Some((result, terminate));
                        }
                        pending = still_running;
                    }
                }
                ParallelToolStep::Completed {
                    completed,
                    updates: pending_updates,
                } => {
                    self.emit_tool_updates(agent, pending_updates).await?;
                    let (result, terminate) = self
                        .finalize_executed_tool(agent, &completed.call, completed.result)
                        .await?;
                    // Another parallel tool may have delivered an update while
                    // the completion hook was awaited. Deliver it before this
                    // tool's terminal event.
                    self.flush_tool_updates(agent, &updates).await?;
                    self.emit_tool_execution_end(agent, &completed.call, &result)
                        .await?;
                    completions[completed.source_index] = Some((result, terminate));
                }
            }
        }
        drop(pending);

        let mut all_terminate = true;
        for prepared_call in prepared {
            let (result, terminate) = completions[prepared_call.source_index]
                .take()
                .expect("each prepared tool call must have exactly one completion");
            self.append_tool_result_message(agent, prepared_call.call, result)
                .await?;
            all_terminate &= terminate;
        }
        Ok(all_terminate)
    }

    async fn execute_one_tool_call(
        &self,
        agent: &AgentInner,
        call: ToolCall,
    ) -> Result<(ToolResult, bool), CoreError> {
        match self.prepare_tool_call(agent, &call).await? {
            PreparedToolCall::Immediate { result, terminate } => Ok((result, terminate)),
            PreparedToolCall::Execute { tool } => {
                let updates = PendingToolUpdates::default();
                let future = self.start_tool_future(&tool, call.clone(), updates.clone());
                let mut future = future;
                let execution = loop {
                    match next_tool_step(&mut future, &updates, &call.id).await {
                        ToolStep::Updates(updates) => {
                            self.emit_tool_updates(agent, updates).await?;
                        }
                        ToolStep::Completed { result, updates } => {
                            self.emit_tool_updates(agent, updates).await?;
                            break result;
                        }
                    }
                };
                self.flush_tool_updates(agent, &updates).await?;
                self.finalize_executed_tool(agent, &call, execution).await
            }
        }
    }

    async fn prepare_tool_call(
        &self,
        agent: &AgentInner,
        call: &ToolCall,
    ) -> Result<PreparedToolCall, CoreError> {
        let Some(tool) = agent.tools.get(&call.name).cloned() else {
            return Ok(PreparedToolCall::Immediate {
                result: error_tool_result(call, format!("Tool {} not found", call.name)),
                terminate: false,
            });
        };
        if let Err(error) = validate_tool_arguments(&call.name, tool.schema(), &call.arguments) {
            return Ok(PreparedToolCall::Immediate {
                result: error_tool_result(call, tool_error_message(error)),
                terminate: false,
            });
        }
        let context = self.current_context(agent)?;
        match agent
            .hooks
            .before_tool_call_async(call, context, self.cancellation.clone())
            .await
        {
            Ok(BeforeToolCall::Allow) => {}
            Ok(BeforeToolCall::Block { reason }) => {
                return Ok(PreparedToolCall::Immediate {
                    result: error_tool_result(call, reason),
                    terminate: false,
                });
            }
            Ok(BeforeToolCall::Terminate { reason }) => {
                return Ok(PreparedToolCall::Immediate {
                    result: error_tool_result(call, reason),
                    terminate: true,
                });
            }
            Err(error) => {
                return Ok(PreparedToolCall::Immediate {
                    result: error_tool_result(call, error.message),
                    terminate: false,
                });
            }
        }
        if self.cancellation.is_cancelled() {
            return Ok(PreparedToolCall::Immediate {
                result: error_tool_result(call, "Operation aborted"),
                // Pi records this tool failure and gives the provider the
                // already-aborted signal on the next turn. It is not a policy
                // termination hint.
                terminate: false,
            });
        }
        Ok(PreparedToolCall::Execute { tool })
    }

    fn start_tool_future<'a>(
        &self,
        tool: &'a Arc<dyn AgentTool>,
        call: ToolCall,
        updates: PendingToolUpdates,
    ) -> ToolFuture<'a> {
        let update_call_id = call.id.clone();
        let update_tool_name = call.name.clone();
        let update_sink = ToolUpdateSink::new({
            let updates = updates.clone();
            move |update| updates.push((update_call_id.clone(), update_tool_name.clone(), update))
        });
        tool.execute(
            call,
            ToolContext {
                cancellation: self.cancellation.clone(),
                metadata: None,
            },
            update_sink,
        )
    }

    async fn finalize_executed_tool(
        &self,
        agent: &AgentInner,
        call: &ToolCall,
        execution: Result<ToolResult, crate::error::ToolError>,
    ) -> Result<(ToolResult, bool), CoreError> {
        let mut result = match execution {
            Ok(result) if result.tool_call_id == call.id => result,
            Ok(result) => error_tool_result(
                call,
                format!(
                    "Tool {} returned mismatched tool-call ID {}",
                    call.name, result.tool_call_id
                ),
            ),
            Err(error) => error_tool_result(call, tool_error_message(error)),
        };
        let context = self.current_context(agent)?;
        let terminate = match agent
            .hooks
            .after_tool_call_async(call, &result, context, self.cancellation.clone())
            .await
        {
            Ok(after) => {
                apply_after_tool_call(&mut result, after);
                result.terminate
            }
            Err(error) => {
                result = error_tool_result(call, error.message);
                false
            }
        };
        Ok((result, terminate))
    }

    /// Flush any callbacks that raced with an awaited hook or lifecycle
    /// observer before the terminal event for the current tool is emitted.
    async fn flush_tool_updates(
        &self,
        agent: &AgentInner,
        updates: &PendingToolUpdates,
    ) -> Result<(), CoreError> {
        while let Some(updates) = updates.take() {
            self.emit_tool_updates(agent, updates).await?;
        }
        Ok(())
    }

    async fn emit_tool_updates(
        &self,
        agent: &AgentInner,
        updates: Vec<PendingToolUpdate>,
    ) -> Result<(), CoreError> {
        for (tool_call_id, tool_name, update) in updates {
            self.emit(
                agent,
                AgentEventKind::ToolExecutionUpdate {
                    tool_call_id,
                    tool_name,
                    update,
                },
            )
            .await?;
        }
        Ok(())
    }

    async fn emit_tool_execution_end(
        &self,
        agent: &AgentInner,
        call: &ToolCall,
        result: &ToolResult,
    ) -> Result<(), CoreError> {
        {
            let mut state = agent.state.lock().expect("agent state mutex poisoned");
            state.pending_tool_calls.remove(&call.id);
        }
        self.emit(
            agent,
            AgentEventKind::ToolExecutionEnd {
                tool_call_id: call.id.clone(),
                tool_name: call.name.clone(),
                result: result.clone(),
            },
        )
        .await?;
        Ok(())
    }

    async fn append_tool_result_message(
        &self,
        agent: &AgentInner,
        call: ToolCall,
        result: ToolResult,
    ) -> Result<(), CoreError> {
        let message = {
            let mut state = agent.state.lock().expect("agent state mutex poisoned");
            let message = Message::ToolResult {
                id: state.allocate_message_id(),
                tool_call_id: call.id,
                tool_name: call.name,
                content: result.content,
                details: result.details,
                usage: result.usage,
                added_tool_names: result.added_tool_names,
                is_error: result.is_error,
            };
            state.messages.push(message.clone());
            message
        };
        self.emit(
            agent,
            AgentEventKind::MessageStart {
                message: message.clone(),
            },
        )
        .await?;
        self.emit(agent, AgentEventKind::MessageEnd { message })
            .await?;
        Ok(())
    }

    async fn emit_agent_end_and_succeed(
        &self,
        agent: &AgentInner,
        reason: StopReason,
    ) -> Result<(), CoreError> {
        let messages = self.new_messages(agent);
        self.emit(agent, AgentEventKind::AgentEnd { messages })
            .await?;
        self.succeed(reason)
    }

    async fn settle_cancellation(&self) {
        let Some(agent) = self.agent.upgrade() else {
            let _ = self.finish(RunPhase::Cancelled, StopReason::Aborted, None);
            return;
        };
        let turn_id = self.snapshot().turn_id.unwrap_or(TurnId(1));
        let failure = {
            let mut state = agent.state.lock().expect("agent state mutex poisoned");
            state.partial_response = None;
            state.is_streaming = false;
            state.pending_tool_calls.clear();
            if matches!(
                state.messages.last(),
                Some(Message::Assistant {
                    stop_reason: Some(StopReason::Aborted),
                    ..
                })
            ) {
                None
            } else {
                let message = Message::Assistant {
                    id: state.allocate_message_id(),
                    content: String::new(),
                    tool_calls: Vec::new(),
                    stop_reason: Some(StopReason::Aborted),
                    error_message: Some("Operation aborted".into()),
                };
                state.messages.push(message.clone());
                Some(message)
            }
        };
        if let Some(message) = failure {
            let _ = self
                .emit(
                    &agent,
                    AgentEventKind::MessageStart {
                        message: message.clone(),
                    },
                )
                .await;
            let _ = self
                .emit(&agent, AgentEventKind::MessageEnd { message })
                .await;
        }
        let _ = self
            .emit(
                &agent,
                AgentEventKind::TurnEnd {
                    turn_id,
                    reason: StopReason::Aborted,
                },
            )
            .await;
        let messages = self.new_messages(&agent);
        let _ = self
            .emit(&agent, AgentEventKind::AgentEnd { messages })
            .await;
        let _ = self.finish(RunPhase::Cancelled, StopReason::Aborted, None);
    }

    /// Return exactly the messages created by this run invocation.
    ///
    /// Pi's low-level loop returns `newMessages`, which includes the prompt
    /// supplied to a prompt run but excludes durable context supplied to a
    /// continuation run. The durable transcript remains available through an
    /// agent snapshot; `AgentEnd` is the invocation result.
    fn new_messages(&self, agent: &AgentInner) -> Vec<Message> {
        agent
            .state
            .lock()
            .expect("agent state mutex poisoned")
            .messages
            .get(self.message_start_index..)
            .unwrap_or_default()
            .to_vec()
    }

    /// Begin the first model turn.
    pub fn start_turn(&self, turn_id: TurnId) -> Result<(), CoreError> {
        let mut state = self.state.lock().expect("run state mutex poisoned");
        if state.phase != RunPhase::Created {
            return Err(CoreError::InvalidTransition(
                crate::error::StateTransitionError::new(
                    "run",
                    phase_name(state.phase),
                    "start_turn",
                ),
            ));
        }
        state.phase = RunPhase::Running;
        state.turn_id = Some(turn_id);
        Ok(())
    }

    fn advance_turn(&self, turn_id: TurnId) -> Result<(), CoreError> {
        let mut state = self.state.lock().expect("run state mutex poisoned");
        if state.phase != RunPhase::Running {
            return Err(CoreError::InvalidTransition(
                crate::error::StateTransitionError::new(
                    "run",
                    phase_name(state.phase),
                    "advance_turn",
                ),
            ));
        }
        state.turn_id = Some(turn_id);
        Ok(())
    }

    /// Request cancellation. This operation is idempotent after settlement.
    ///
    /// A running handle is settled by [`Self::drive`] so its terminal events
    /// remain ordered and observers remain awaited. An un-driven handle has no
    /// active caller-owned future, so it settles immediately.
    pub fn abort(&self) -> Result<(), CoreError> {
        self.cancellation.cancel();
        let mut state = self.state.lock().expect("run state mutex poisoned");
        if state.phase.is_terminal() {
            return Ok(());
        }
        let settle_immediately = state.phase == RunPhase::Created;
        if settle_immediately {
            state.phase = RunPhase::Cancelled;
            state.stop_reason = Some(StopReason::Cancelled);
        }
        drop(state);
        if settle_immediately {
            self.settle_agent(AgentPhase::Idle, None);
        } else if let Some(agent) = self.agent.upgrade() {
            let mut state = agent.state.lock().expect("agent state mutex poisoned");
            state.phase = AgentPhase::Cancelling(self.id());
        }
        Ok(())
    }

    /// Enter observer settlement before selecting a terminal outcome.
    pub fn begin_settlement(&self) -> Result<(), CoreError> {
        let mut state = self.state.lock().expect("run state mutex poisoned");
        if state.phase.is_terminal() {
            return Err(CoreError::RunFinished { run_id: state.id });
        }
        state.phase = RunPhase::Settling;
        Ok(())
    }

    /// Settle a successful run and clear transient agent state first.
    pub fn succeed(&self, reason: StopReason) -> Result<(), CoreError> {
        self.finish(RunPhase::Succeeded, reason, None)
    }

    /// Settle a failed run and clear transient agent state first.
    pub fn fail(&self, message: impl Into<String>) -> Result<(), CoreError> {
        self.finish(RunPhase::Failed, StopReason::Error, Some(message.into()))
    }

    fn finish(
        &self,
        phase: RunPhase,
        reason: StopReason,
        error: Option<String>,
    ) -> Result<(), CoreError> {
        let mut state = self.state.lock().expect("run state mutex poisoned");
        if state.phase.is_terminal() {
            return Err(CoreError::RunFinished { run_id: state.id });
        }
        state.phase = phase;
        state.stop_reason = Some(reason);
        state.error = error.clone();
        let id = state.id;
        drop(state);
        self.settle_agent(AgentPhase::Idle, error);
        let _ = id;
        Ok(())
    }

    fn settle_agent(&self, phase: AgentPhase, error: Option<String>) {
        if let Some(agent) = self.agent.upgrade() {
            let mut state = agent.state.lock().expect("agent state mutex poisoned");
            // Transient state is cleared before the agent becomes idle.  Durable messages are
            // intentionally retained for the next `continue` operation.
            state.partial_response = None;
            state.is_streaming = false;
            state.pending_tool_calls.clear();
            state.last_error = error;
            state.phase = phase;
            agent
                .active_run
                .lock()
                .expect("active run mutex poisoned")
                .take();
            drop(state);
            agent.idle_notifier.notify();
        }
    }

    /// Construct an event envelope using the run's next local sequence.
    pub fn event(&self, kind: AgentEventKind) -> AgentEvent {
        self.record_event(kind)
    }

    fn record_event(&self, kind: AgentEventKind) -> AgentEvent {
        let mut state = self.state.lock().expect("run state mutex poisoned");
        state.event_count = state.event_count.saturating_add(1);
        let event = AgentEvent {
            run_id: state.id,
            sequence: EventSequence(state.event_count),
            kind,
        };
        state.events.push(event.clone());
        event
    }

    pub(crate) async fn emit(
        &self,
        agent: &AgentInner,
        kind: AgentEventKind,
    ) -> Result<AgentEvent, CoreError> {
        let event = self.record_event(kind);

        // Lossless subscriptions use an explicitly caller-owned unbounded
        // queue. Publish this copy before awaited observers so a live host is
        // not held behind an arbitrary observer future. Sending to one cannot
        // drop for capacity and does not wait for the receiver to drain; a
        // disconnected receiver is cleaned up after this event.
        let lossless_subscribers = agent
            .lossless_subscribers
            .lock()
            .expect("lossless subscriber mutex poisoned")
            .clone();
        let mut disconnected = Vec::new();
        for registration in lossless_subscribers {
            if registration.sender.send(event.clone()).is_err() {
                disconnected.push(registration.id);
            }
        }
        if !disconnected.is_empty() {
            agent
                .lossless_subscribers
                .lock()
                .expect("lossless subscriber mutex poisoned")
                .retain(|registration| !disconnected.contains(&registration.id));
        }

        // Clone a registration snapshot before awaiting callbacks. This avoids
        // retaining the registry mutex across an await and defines reentrant
        // subscribe/unsubscribe precisely: changes apply to the next event.
        let observers = agent
            .observers
            .lock()
            .expect("observer mutex poisoned")
            .clone();
        for registration in observers {
            registration
                .observer
                .observe(&event, self.cancellation.clone())
                .await?;
        }
        // These subscriptions deliberately have a distinct contract from
        // awaited observers. Snapshot their registrations so a receiver can
        // drop itself while delivery is in progress, then use `try_send` so a
        // slow receiver never holds the run open.
        let subscribers = agent
            .subscribers
            .lock()
            .expect("subscriber mutex poisoned")
            .clone();
        let mut disconnected = Vec::new();
        for registration in subscribers {
            match registration.sender.try_send(event.clone()) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    registration
                        .dropped
                        .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                }
                Err(TrySendError::Disconnected(_)) => disconnected.push(registration.id),
            }
        }
        if !disconnected.is_empty() {
            agent
                .subscribers
                .lock()
                .expect("subscriber mutex poisoned")
                .retain(|registration| !disconnected.contains(&registration.id));
        }

        Ok(event)
    }

    pub(crate) fn settle_cancelled(&self) -> Result<(), CoreError> {
        self.finish(RunPhase::Cancelled, StopReason::Cancelled, None)
    }
}

impl Drop for RunHandle {
    fn drop(&mut self) {
        let should_abort = self
            .state
            .lock()
            .map(|state| !state.phase.is_terminal())
            .unwrap_or(false);
        if should_abort {
            let _ = self.abort();
        }
    }
}

enum ToolStep {
    Updates(Vec<PendingToolUpdate>),
    Completed {
        result: Result<ToolResult, crate::error::ToolError>,
        updates: Vec<PendingToolUpdate>,
    },
}

enum ParallelToolStep {
    Updates(Vec<PendingToolUpdate>),
    Completed {
        completed: Box<CompletedToolExecution>,
        updates: Vec<PendingToolUpdate>,
    },
}

/// Poll one sequential tool until either its callback queue has work or its
/// future settles. A callback may happen from another thread while the tool is
/// pending; `PendingToolUpdates` wakes this poll so the event is not delayed
/// until settlement.
async fn next_tool_step<'a>(
    future: &mut ToolFuture<'a>,
    updates: &PendingToolUpdates,
    call_id: &ToolCallId,
) -> ToolStep {
    std::future::poll_fn(|context| {
        if let Some(updates) = updates.take() {
            return Poll::Ready(ToolStep::Updates(updates));
        }
        updates.register_waker(context.waker());
        // Close the check/register race: a callback arriving immediately
        // before registration must still be observed on this poll.
        if let Some(updates) = updates.take() {
            return Poll::Ready(ToolStep::Updates(updates));
        }
        match future.as_mut().poll(context) {
            Poll::Ready(result) => {
                updates.close(call_id);
                Poll::Ready(ToolStep::Completed {
                    result,
                    updates: updates.take().unwrap_or_default(),
                })
            }
            Poll::Pending => updates
                .take()
                .map(ToolStep::Updates)
                .map_or(Poll::Pending, Poll::Ready),
        }
    })
    .await
}

/// Poll one parallel batch until either a callback queue has work or one tool
/// settles. Updates found after each individual future poll are returned before
/// the next future is polled, which preserves callback-before-completion order.
async fn next_parallel_step<'a>(
    pending: &mut Vec<PendingToolExecution<'a>>,
    updates: &PendingToolUpdates,
) -> ParallelToolStep {
    std::future::poll_fn(|context| {
        if let Some(updates) = updates.take() {
            return Poll::Ready(ParallelToolStep::Updates(updates));
        }
        updates.register_waker(context.waker());
        if let Some(updates) = updates.take() {
            return Poll::Ready(ParallelToolStep::Updates(updates));
        }
        let mut index = 0;
        while index < pending.len() {
            if let Poll::Ready(result) = pending[index].future.as_mut().poll(context) {
                let pending = pending.swap_remove(index);
                updates.close(&pending.call.id);
                return Poll::Ready(ParallelToolStep::Completed {
                    completed: Box::new(CompletedToolExecution {
                        source_index: pending.source_index,
                        call: pending.call,
                        result,
                    }),
                    updates: updates.take().unwrap_or_default(),
                });
            }
            if let Some(updates) = updates.take() {
                return Poll::Ready(ParallelToolStep::Updates(updates));
            }
            index = index.saturating_add(1);
        }
        updates
            .take()
            .map(ParallelToolStep::Updates)
            .map_or(Poll::Pending, Poll::Ready)
    })
    .await
}

fn error_tool_result(call: &ToolCall, content: impl Into<String>) -> ToolResult {
    ToolResult {
        tool_call_id: call.id.clone(),
        content: content.into(),
        details: None,
        usage: None,
        added_tool_names: Vec::new(),
        terminate: false,
        is_error: true,
    }
}

fn tool_error_message(error: crate::error::ToolError) -> String {
    match error {
        crate::error::ToolError::InvalidArguments { message, .. }
        | crate::error::ToolError::Execution { message, .. } => message,
        crate::error::ToolError::Blocked { reason, .. } => reason,
        crate::error::ToolError::Cancelled { .. } => "Operation aborted".into(),
    }
}

fn apply_after_tool_call(result: &mut ToolResult, after: AfterToolCall) {
    if let Replacement::Replace(content) = after.content {
        result.content = content;
    }
    if let Replacement::Replace(details) = after.details {
        result.details = details;
    }
    if let Replacement::Replace(usage) = after.usage {
        result.usage = Some(usage);
    }
    if let Replacement::Replace(added_tool_names) = after.added_tool_names {
        result.added_tool_names = added_tool_names;
    }
    if let Some(terminate) = after.terminate {
        result.terminate = terminate;
    }
    if let Replacement::Replace(is_error) = after.is_error {
        result.is_error = is_error;
    }
}

fn phase_name(phase: RunPhase) -> &'static str {
    match phase {
        RunPhase::Created => "created",
        RunPhase::Running => "running",
        RunPhase::Settling => "settling",
        RunPhase::Succeeded => "succeeded",
        RunPhase::Failed => "failed",
        RunPhase::Cancelled => "cancelled",
    }
}
