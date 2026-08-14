//! Ownership handle for one active run.
//!
//! `RunHandle` is deliberately small: it owns lifecycle settlement and delegates model/tool
//! work to the caller-owned executor.  Dropping an unfinished handle requests cancellation and
//! settles the agent as cancelled, ensuring an abandoned run cannot leave the agent busy.

use crate::agent::AgentInner;
use crate::error::CoreError;
use crate::event::{AgentEvent, AgentEventKind, EventSequence};
use crate::hooks::{AfterToolCall, BeforeToolCall, Replacement};
use crate::scheduler::CancellationToken;
use crate::scheduler::{ModelRequest, ModelStreamEvent};
use crate::schema_validation::validate_tool_arguments;
use crate::state::{
    AgentPhase, AssistantToolCall, Message, RunId, RunPhase, RunSnapshot, RunState, StopReason,
    TurnId,
};
use crate::tool::{
    AgentTool, ToolCall, ToolContext, ToolFuture, ToolResult, ToolUpdate, ToolUpdateSink,
};
use std::sync::{Arc, Mutex, Weak};
use std::task::Poll;

enum PreparedToolCall {
    Immediate { result: ToolResult, terminate: bool },
    Execute { tool: Arc<dyn AgentTool> },
}

struct PreparedToolExecution {
    source_index: usize,
    call: ToolCall,
    preparation: PreparedToolCall,
}

struct PendingToolExecution<'a> {
    source_index: usize,
    call: ToolCall,
    future: ToolFuture<'a>,
    updates: Arc<Mutex<Vec<ToolUpdate>>>,
}

struct CompletedToolExecution {
    source_index: usize,
    call: ToolCall,
    result: Result<ToolResult, crate::error::ToolError>,
    updates: Arc<Mutex<Vec<ToolUpdate>>>,
}

/// A handle to the one run currently owning an agent.
pub struct RunHandle {
    pub(crate) agent: Weak<AgentInner>,
    pub(crate) state: Arc<Mutex<RunState>>,
    pub(crate) cancellation: CancellationToken,
    pub(crate) initial_messages: Vec<Message>,
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
        let messages = agent
            .state
            .lock()
            .expect("agent state mutex poisoned")
            .messages
            .clone();
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
            if !matches!(state.phase, AgentPhase::Running(id) if id == run_id) {
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
        if self.cancellation.is_cancelled() {
            return Err(CoreError::Cancelled);
        }

        let mut turn_id = TurnId(1);
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
            if self.cancellation.is_cancelled() {
                return Err(CoreError::Cancelled);
            }

            let request = self.model_request(&agent, run_id, next_context.take())?;
            let provider = agent
                .provider
                .as_ref()
                .ok_or(CoreError::MissingModelProvider)?;
            let stream = provider
                .stream(request, self.cancellation.clone())
                .await
                .map_err(|error| CoreError::ModelProvider {
                    message: error.to_string(),
                })?;
            let (reason, tool_calls) = self.consume_assistant_stream(&agent, stream.events).await?;

            let terminate_tool_batch = if tool_calls.is_empty() {
                false
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

            let context = self.current_context(&agent)?;
            let prepared_context = agent.hooks.prepare_next_turn(context)?;
            if agent.hooks.should_stop_after_turn(&prepared_context)? {
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

    fn model_request(
        &self,
        agent: &AgentInner,
        run_id: RunId,
        context: Option<crate::hooks::ContextEnvelope>,
    ) -> Result<ModelRequest, CoreError> {
        let (context, system_prompt, model, tools) = {
            let mut state = agent.state.lock().expect("agent state mutex poisoned");
            if !matches!(state.phase, AgentPhase::Running(id) if id == run_id) {
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
                    host_messages: Vec::new(),
                }),
                state.system_prompt.clone(),
                state.model.as_ref().map(|model| format!("{model:?}")),
                agent.tools.definitions(),
            )
        };
        let transformed = agent.hooks.transform_context(context)?;
        let request = ModelRequest {
            system_prompt,
            context: agent.hooks.convert_to_llm(transformed.clone())?,
            tools,
            model,
        };
        Ok(request)
    }

    fn current_context(
        &self,
        agent: &AgentInner,
    ) -> Result<crate::hooks::ContextEnvelope, CoreError> {
        let state = agent.state.lock().expect("agent state mutex poisoned");
        if !matches!(state.phase, AgentPhase::Running(_)) {
            return Err(CoreError::InvalidTransition(
                crate::error::StateTransitionError::new("agent", "not-running", "current_context"),
            ));
        }
        Ok(crate::hooks::ContextEnvelope {
            version: 1,
            messages: state.messages.clone(),
            host_messages: Vec::new(),
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
            return Ok(None);
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
        events: Vec<ModelStreamEvent>,
    ) -> Result<(StopReason, Vec<AssistantToolCall>), CoreError> {
        let mut assistant_id = None;
        let mut assistant_text = String::new();
        let mut tool_calls = Vec::new();
        let mut reason = None;

        for item in events {
            if self.cancellation.is_cancelled() {
                return Err(CoreError::Cancelled);
            }
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
                            });
                        }
                        assistant_text.push_str(&delta);
                        state.partial_response = Some(assistant_text.clone());
                        let message = Message::Assistant {
                            id,
                            content: assistant_text.clone(),
                            tool_calls: Vec::new(),
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
                                },
                            },
                        )
                        .await?;
                    }
                    self.emit(agent, AgentEventKind::MessageUpdate { message })
                        .await?;
                }
                ModelStreamEvent::ToolCall(call) => tool_calls.push(call),
                ModelStreamEvent::Usage(_) => {}
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
        Ok((reason, tool_calls))
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
            if self.cancellation.is_cancelled() {
                return Err(CoreError::Cancelled);
            }

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
        let mut completions = (0..tool_calls.len())
            .map(|_| None::<(ToolResult, bool)>)
            .collect::<Vec<_>>();

        // Pi announces each call and prepares it in source order before it starts the parallel
        // batch. Immediate preparation failures therefore end before later calls are announced;
        // successful result messages remain deferred until every batch completion is known.
        for (source_index, assistant_call) in tool_calls.iter().enumerate() {
            if self.cancellation.is_cancelled() {
                return Err(CoreError::Cancelled);
            }
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
                },
            )
            .await?;
            let preparation = self.prepare_tool_call(agent, &call)?;
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
                let (future, updates) = self.start_tool_future(tool, prepared_call.call.clone());
                pending.push(PendingToolExecution {
                    source_index: prepared_call.source_index,
                    call: prepared_call.call.clone(),
                    future,
                    updates,
                });
            }
        }
        while !pending.is_empty() {
            let completed = next_parallel_completion(&mut pending).await;
            let (result, terminate) = self
                .finalize_executed_tool(agent, &completed.call, completed.result, completed.updates)
                .await?;
            self.emit_tool_execution_end(agent, &completed.call, &result)
                .await?;
            completions[completed.source_index] = Some((result, terminate));
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
        match self.prepare_tool_call(agent, &call)? {
            PreparedToolCall::Immediate { result, terminate } => Ok((result, terminate)),
            PreparedToolCall::Execute { tool } => {
                let (future, updates) = self.start_tool_future(&tool, call.clone());
                self.finalize_executed_tool(agent, &call, future.await, updates)
                    .await
            }
        }
    }

    fn prepare_tool_call(
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
        match agent.hooks.before_tool_call(call) {
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
                terminate: true,
            });
        }
        Ok(PreparedToolCall::Execute { tool })
    }

    fn start_tool_future<'a>(
        &self,
        tool: &'a Arc<dyn AgentTool>,
        call: ToolCall,
    ) -> (ToolFuture<'a>, Arc<Mutex<Vec<ToolUpdate>>>) {
        let updates = Arc::new(Mutex::new(Vec::<ToolUpdate>::new()));
        let update_sink = ToolUpdateSink::new({
            let updates = Arc::clone(&updates);
            move |update| {
                updates
                    .lock()
                    .expect("tool update mutex poisoned")
                    .push(update)
            }
        });
        let future = tool.execute(
            call,
            ToolContext {
                cancellation: self.cancellation.clone(),
                metadata: None,
            },
            update_sink,
        );
        (future, updates)
    }

    async fn finalize_executed_tool(
        &self,
        agent: &AgentInner,
        call: &ToolCall,
        execution: Result<ToolResult, crate::error::ToolError>,
        updates: Arc<Mutex<Vec<ToolUpdate>>>,
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
        let updates = std::mem::take(&mut *updates.lock().expect("tool update mutex poisoned"));
        for update in updates {
            self.emit(
                agent,
                AgentEventKind::ToolExecutionUpdate {
                    tool_call_id: call.id.clone(),
                    tool_name: call.name.clone(),
                    update,
                },
            )
            .await?;
        }
        let terminate = match agent.hooks.after_tool_call(call, &result) {
            Ok(after) => {
                let terminate = after.terminate == Some(true);
                apply_after_tool_call(&mut result, after);
                terminate
            }
            Err(error) => {
                result = error_tool_result(call, error.message);
                false
            }
        };
        Ok((result, terminate))
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
        let messages = agent
            .state
            .lock()
            .expect("agent state mutex poisoned")
            .messages
            .clone();
        self.emit(agent, AgentEventKind::AgentEnd { messages })
            .await?;
        self.succeed(reason)
    }

    async fn settle_cancellation(&self) {
        let Some(agent) = self.agent.upgrade() else {
            let _ = self.finish(RunPhase::Cancelled, StopReason::Cancelled, None);
            return;
        };
        let turn_id = self.snapshot().turn_id.unwrap_or(TurnId(1));
        {
            let mut state = agent.state.lock().expect("agent state mutex poisoned");
            state.partial_response = None;
            state.is_streaming = false;
            state.pending_tool_calls.clear();
        }
        let _ = self
            .emit(
                &agent,
                AgentEventKind::TurnEnd {
                    turn_id,
                    reason: StopReason::Cancelled,
                },
            )
            .await;
        let messages = agent
            .state
            .lock()
            .expect("agent state mutex poisoned")
            .messages
            .clone();
        let _ = self
            .emit(&agent, AgentEventKind::AgentEnd { messages })
            .await;
        let _ = self.finish(RunPhase::Cancelled, StopReason::Cancelled, None);
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

    async fn emit(
        &self,
        agent: &AgentInner,
        kind: AgentEventKind,
    ) -> Result<AgentEvent, CoreError> {
        let event = self.record_event(kind);
        for observer in &agent.observers {
            observer.observe(&event, self.cancellation.clone()).await?;
        }
        Ok(event)
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

async fn next_parallel_completion<'a>(
    pending: &mut Vec<PendingToolExecution<'a>>,
) -> CompletedToolExecution {
    std::future::poll_fn(|context| {
        let mut index = 0;
        while index < pending.len() {
            if let Poll::Ready(result) = pending[index].future.as_mut().poll(context) {
                let pending = pending.swap_remove(index);
                return Poll::Ready(CompletedToolExecution {
                    source_index: pending.source_index,
                    call: pending.call,
                    result,
                    updates: pending.updates,
                });
            }
            index = index.saturating_add(1);
        }
        Poll::Pending
    })
    .await
}

fn error_tool_result(call: &ToolCall, content: impl Into<String>) -> ToolResult {
    ToolResult {
        tool_call_id: call.id.clone(),
        content: content.into(),
        details: None,
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
