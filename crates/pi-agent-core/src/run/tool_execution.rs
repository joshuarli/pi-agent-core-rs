//! Tool preparation, execution, update delivery, and result insertion for one run.

use super::{
    apply_after_tool_call, error_tool_result, next_parallel_step, next_tool_step,
    tool_error_message, ParallelToolStep, PendingToolExecution, PendingToolUpdate,
    PendingToolUpdates, PreparedToolCall, PreparedToolExecution, RunHandle, ToolStep,
};
use crate::agent::AgentInner;
use crate::error::CoreError;
use crate::event::AgentEventKind;
use crate::hooks::BeforeToolCall;
use crate::schema_validation::validate_tool_arguments;
use crate::state::{AgentMessage, AgentToolCall};
use crate::tool::{AgentTool, AgentToolResult, ToolCall, ToolContext, ToolFuture, ToolUpdateSink};
use std::sync::Arc;
use std::task::Poll;

impl RunHandle {
    pub(super) async fn execute_tool_calls(
        &self,
        agent: &AgentInner,
        tool_calls: &[AgentToolCall],
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
        tool_calls: &[AgentToolCall],
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
        tool_calls: &[AgentToolCall],
    ) -> Result<bool, CoreError> {
        let mut prepared = Vec::with_capacity(tool_calls.len());
        let updates = PendingToolUpdates::default();
        let mut completions = (0..tool_calls.len())
            .map(|_| None::<(AgentToolResult, bool)>)
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
    ) -> Result<(AgentToolResult, bool), CoreError> {
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
        execution: Result<AgentToolResult, crate::error::ToolError>,
    ) -> Result<(AgentToolResult, bool), CoreError> {
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

    pub(super) async fn emit_tool_execution_end(
        &self,
        agent: &AgentInner,
        call: &ToolCall,
        result: &AgentToolResult,
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

    pub(super) async fn append_tool_result_message(
        &self,
        agent: &AgentInner,
        call: ToolCall,
        result: AgentToolResult,
    ) -> Result<(), CoreError> {
        let message = {
            let mut state = agent.state.lock().expect("agent state mutex poisoned");
            let message = AgentMessage::ToolResult {
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
}
