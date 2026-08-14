use crate::agent::Agent;
use crate::error::CoreError;
use crate::event::{AgentEvent, AgentEventKind, EventObserver, ObserverFuture};
use crate::scheduler::{
    CancellationToken, ModelFuture, ModelProvider, ModelRequest, ModelStream, ModelStreamEvent,
    Scheduler,
};
use crate::state::{AgentPhase, AssistantToolCall, SerializedJson, StopReason, ToolCallId};
use crate::tool::{
    AgentTool, ToolCall, ToolContext, ToolExecutionMode, ToolFuture, ToolResult, ToolUpdateSink,
};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

#[derive(Debug)]
struct TextOnlyProvider;

impl ModelProvider for TextOnlyProvider {
    fn stream<'a>(
        &'a self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelFuture<'a> {
        Box::pin(std::future::ready(Ok(ModelStream {
            events: vec![
                ModelStreamEvent::TextDelta("fixture capture succeeded.".into()),
                ModelStreamEvent::End(StopReason::EndTurn),
            ],
        })))
    }
}

#[derive(Debug)]
struct FailingProvider;

impl ModelProvider for FailingProvider {
    fn stream<'a>(
        &'a self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelFuture<'a> {
        Box::pin(std::future::ready(Err(
            crate::error::SchedulerError::UnknownToolCall {
                tool_call_id: ToolCallId::new("provider-call-99")
                    .expect("non-empty test tool-call ID"),
            },
        )))
    }
}

#[derive(Debug)]
struct ScriptedProvider {
    streams: Mutex<VecDeque<ModelStream>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl ScriptedProvider {
    fn new(streams: impl IntoIterator<Item = ModelStream>) -> Self {
        Self {
            streams: Mutex::new(streams.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().expect("test request mutex").clone()
    }
}

impl ModelProvider for ScriptedProvider {
    fn stream<'a>(
        &'a self,
        request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelFuture<'a> {
        self.requests
            .lock()
            .expect("test request mutex")
            .push(request);
        let stream = self
            .streams
            .lock()
            .expect("test stream mutex")
            .pop_front()
            .ok_or_else(|| crate::error::SchedulerError::UnknownToolCall {
                tool_call_id: ToolCallId::new("unexpected-model-request")
                    .expect("test tool-call ID is non-empty"),
            });
        Box::pin(std::future::ready(stream))
    }
}

#[derive(Debug)]
struct EchoTool {
    calls: Arc<Mutex<Vec<ToolCall>>>,
    schema: pi_agent_protocol::JsonValue,
}

impl AgentTool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Echoes its JSON input."
    }

    fn schema(&self) -> &pi_agent_protocol::JsonValue {
        &self.schema
    }

    fn execute<'a>(
        &'a self,
        call: ToolCall,
        _context: ToolContext,
        _updates: ToolUpdateSink,
    ) -> ToolFuture<'a> {
        self.calls
            .lock()
            .expect("test tool mutex")
            .push(call.clone());
        Box::pin(std::future::ready(Ok(ToolResult {
            tool_call_id: call.id,
            content: "echoed: hello".into(),
            details: None,
            is_error: false,
        })))
    }
}

#[derive(Debug)]
struct YieldOnceToolFuture {
    result: Option<Result<ToolResult, crate::error::ToolError>>,
    yielded: bool,
}

impl Future for YieldOnceToolFuture {
    type Output = Result<ToolResult, crate::error::ToolError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        if !self.yielded {
            self.yielded = true;
            context.waker().wake_by_ref();
            return Poll::Pending;
        }
        Poll::Ready(
            self.result
                .take()
                .expect("tool future polled after completion"),
        )
    }
}

#[derive(Debug)]
struct ParallelFixtureTool {
    name: &'static str,
    yield_once: bool,
    schema: pi_agent_protocol::JsonValue,
}

impl AgentTool for ParallelFixtureTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        self.name
    }

    fn schema(&self) -> &pi_agent_protocol::JsonValue {
        &self.schema
    }

    fn execution_mode(&self) -> ToolExecutionMode {
        ToolExecutionMode::Parallel
    }

    fn execute<'a>(
        &'a self,
        call: ToolCall,
        _context: ToolContext,
        _updates: ToolUpdateSink,
    ) -> ToolFuture<'a> {
        let result = ToolResult {
            tool_call_id: call.id,
            content: self.name.into(),
            details: None,
            is_error: false,
        };
        if self.yield_once {
            Box::pin(YieldOnceToolFuture {
                result: Some(Ok(result)),
                yielded: false,
            })
        } else {
            Box::pin(std::future::ready(Ok(result)))
        }
    }
}

#[derive(Debug)]
struct RecordingObserver {
    events: Arc<std::sync::Mutex<Vec<AgentEvent>>>,
}

impl EventObserver for RecordingObserver {
    fn observe<'a>(
        &'a self,
        event: &'a AgentEvent,
        _cancellation: CancellationToken,
    ) -> ObserverFuture<'a> {
        self.events
            .lock()
            .expect("test observer mutex")
            .push(event.clone());
        Box::pin(std::future::ready(Ok(())))
    }
}

#[derive(Debug)]
struct AbortOnAgentStartObserver {
    agent: Arc<Mutex<Option<Agent>>>,
}

impl EventObserver for AbortOnAgentStartObserver {
    fn observe<'a>(
        &'a self,
        event: &'a AgentEvent,
        _cancellation: CancellationToken,
    ) -> ObserverFuture<'a> {
        if matches!(event.kind, AgentEventKind::AgentStart) {
            if let Some(agent) = self.agent.lock().expect("test agent mutex").clone() {
                agent.abort();
            }
        }
        Box::pin(std::future::ready(Ok(())))
    }
}

#[test]
fn caller_driven_text_run_emits_lifecycle_and_settles() {
    smol::block_on(async {
        let agent = Agent::builder()
            .model_provider(Arc::new(TextOnlyProvider))
            .build();
        let run = agent.start_prompt("Return exactly: fixture capture succeeded.")?;

        run.drive().await?;

        let events = run.events();
        assert_eq!(events.len(), 9);
        assert!(matches!(events[0].kind, AgentEventKind::AgentStart));
        assert!(matches!(events[1].kind, AgentEventKind::TurnStart { .. }));
        assert!(matches!(
            events[2].kind,
            AgentEventKind::MessageStart { .. }
        ));
        assert!(matches!(events[3].kind, AgentEventKind::MessageEnd { .. }));
        assert!(matches!(
            events[4].kind,
            AgentEventKind::MessageStart { .. }
        ));
        assert!(matches!(
            events[5].kind,
            AgentEventKind::MessageUpdate { .. }
        ));
        assert!(matches!(events[6].kind, AgentEventKind::MessageEnd { .. }));
        assert!(matches!(events[7].kind, AgentEventKind::TurnEnd { .. }));
        assert!(matches!(events[8].kind, AgentEventKind::AgentEnd { .. }));

        let snapshot = agent.snapshot();
        assert_eq!(snapshot.phase, AgentPhase::Idle);
        assert!(!snapshot.is_streaming);
        assert_eq!(snapshot.messages.len(), 2);
        assert!(matches!(
            snapshot.messages[1],
            crate::state::Message::Assistant { .. }
        ));

        Ok::<(), CoreError>(())
    })
    .expect("text run should settle");
}

#[test]
fn provider_failure_settles_the_agent_before_returning_the_error() {
    smol::block_on(async {
        let agent = Agent::builder()
            .model_provider(Arc::new(FailingProvider))
            .build();
        let run = agent.start_prompt("trigger provider failure")?;

        let error = run.drive().await.expect_err("provider must fail");

        assert!(matches!(error, CoreError::ModelProvider { .. }));
        let events = run.events();
        assert_eq!(events.len(), 8);
        assert!(matches!(
            events[4].kind,
            AgentEventKind::MessageStart { .. }
        ));
        assert!(matches!(events[5].kind, AgentEventKind::MessageEnd { .. }));
        assert!(matches!(events[6].kind, AgentEventKind::TurnEnd { .. }));
        assert!(matches!(events[7].kind, AgentEventKind::AgentEnd { .. }));
        let snapshot = agent.snapshot();
        assert_eq!(snapshot.phase, AgentPhase::Idle);
        assert!(!snapshot.is_streaming);
        assert!(snapshot.pending_tool_calls.is_empty());
        assert!(snapshot.last_error.is_some());
        assert!(matches!(
            snapshot.messages.last(),
            Some(crate::state::Message::Assistant { content, .. }) if content.is_empty()
        ));

        Ok::<(), CoreError>(())
    })
    .expect("failure settlement should not leave an active agent");
}

#[test]
fn awaited_observer_receives_each_reduced_event_in_source_order() {
    smol::block_on(async {
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let agent = Agent::builder()
            .model_provider(Arc::new(TextOnlyProvider))
            .observer(Arc::new(RecordingObserver {
                events: Arc::clone(&observed),
            }))
            .build();
        let run = agent.start_prompt("observe the lifecycle")?;

        run.drive().await?;

        let emitted = run.events();
        let observed = observed.lock().expect("test observer mutex").clone();
        assert_eq!(observed, emitted);
        assert!(matches!(
            observed.last().map(|event| &event.kind),
            Some(AgentEventKind::AgentEnd { .. })
        ));

        Ok::<(), CoreError>(())
    })
    .expect("observer should settle with the run");
}

#[test]
fn cancellation_settles_terminal_events_before_wait_for_idle() {
    smol::block_on(async {
        let observer_agent = Arc::new(Mutex::new(None));
        let agent = Agent::builder()
            .model_provider(Arc::new(TextOnlyProvider))
            .observer(Arc::new(AbortOnAgentStartObserver {
                agent: Arc::clone(&observer_agent),
            }))
            .build();
        *observer_agent.lock().expect("test agent mutex") = Some(agent.clone());
        let run = agent.start_prompt("cancel through an awaited observer")?;

        let error = run
            .drive()
            .await
            .expect_err("observer requested cancellation");

        assert_eq!(error, CoreError::Cancelled);
        assert_eq!(run.snapshot().phase, crate::state::RunPhase::Cancelled);
        let events = run.events();
        assert!(matches!(events[0].kind, AgentEventKind::AgentStart));
        assert!(matches!(
            events[events.len() - 2].kind,
            AgentEventKind::TurnEnd {
                reason: StopReason::Cancelled,
                ..
            }
        ));
        assert!(matches!(
            events.last().map(|event| &event.kind),
            Some(AgentEventKind::AgentEnd { .. })
        ));
        assert_eq!(agent.snapshot().phase, AgentPhase::Idle);

        agent.wait_for_idle().await;

        Ok::<(), CoreError>(())
    })
    .expect("cancelled run must settle before wait_for_idle resolves");
}

#[test]
fn tool_turn_executes_then_continues_the_model_loop() {
    smol::block_on(async {
        let call_id = ToolCallId::new("call_openrouter_001").expect("non-empty provider ID");
        let provider = Arc::new(ScriptedProvider::new([
            ModelStream {
                events: vec![
                    ModelStreamEvent::ToolCall(AssistantToolCall {
                        id: call_id.clone(),
                        name: "echo".into(),
                        arguments: SerializedJson::new(r#"{"text":"hello"}"#),
                    }),
                    ModelStreamEvent::End(StopReason::ToolUse),
                ],
            },
            ModelStream {
                events: vec![
                    ModelStreamEvent::TextDelta("done".into()),
                    ModelStreamEvent::End(StopReason::EndTurn),
                ],
            },
        ]));
        let executed = Arc::new(Mutex::new(Vec::new()));
        let agent = Agent::builder()
            .model_provider(provider.clone())
            .tool(Arc::new(EchoTool {
                calls: Arc::clone(&executed),
                schema: pi_agent_protocol::JsonValue::parse(
                    r#"{"type":"object","required":["text"]}"#,
                )
                .expect("test schema is valid JSON"),
            }))
            .build();
        let run = agent.start_prompt("echo hello")?;

        run.drive().await?;

        assert_eq!(provider.requests().len(), 2);
        assert_eq!(
            executed.lock().expect("test tool mutex").as_slice(),
            [ToolCall {
                id: call_id.clone(),
                name: "echo".into(),
                arguments: SerializedJson::new(r#"{"text":"hello"}"#),
            }]
        );
        let events = run.events();
        assert_eq!(events.len(), 17);
        assert!(matches!(events[0].kind, AgentEventKind::AgentStart));
        assert!(matches!(events[1].kind, AgentEventKind::TurnStart { .. }));
        assert!(matches!(
            events[4].kind,
            AgentEventKind::MessageStart { .. }
        ));
        assert!(matches!(events[5].kind, AgentEventKind::MessageEnd { .. }));
        assert!(matches!(
            events[6].kind,
            AgentEventKind::ToolExecutionStart { .. }
        ));
        assert!(matches!(
            events[7].kind,
            AgentEventKind::ToolExecutionEnd { .. }
        ));
        assert!(matches!(
            events[8].kind,
            AgentEventKind::MessageStart { .. }
        ));
        assert!(matches!(events[9].kind, AgentEventKind::MessageEnd { .. }));
        assert!(matches!(
            events[10].kind,
            AgentEventKind::TurnEnd {
                reason: StopReason::ToolUse,
                ..
            }
        ));
        assert!(matches!(events[11].kind, AgentEventKind::TurnStart { .. }));
        assert!(matches!(
            events[12].kind,
            AgentEventKind::MessageStart { .. }
        ));
        assert!(matches!(
            events[13].kind,
            AgentEventKind::MessageUpdate { .. }
        ));
        assert!(matches!(events[14].kind, AgentEventKind::MessageEnd { .. }));
        assert!(matches!(
            events[15].kind,
            AgentEventKind::TurnEnd {
                reason: StopReason::EndTurn,
                ..
            }
        ));
        assert!(matches!(events[16].kind, AgentEventKind::AgentEnd { .. }));

        let snapshot = agent.snapshot();
        assert_eq!(snapshot.phase, AgentPhase::Idle);
        assert_eq!(snapshot.messages.len(), 4);
        assert!(matches!(
            snapshot.messages[1],
            crate::state::Message::Assistant { ref tool_calls, .. }
                if tool_calls == &vec![AssistantToolCall {
                    id: call_id.clone(),
                    name: "echo".into(),
                    arguments: SerializedJson::new(r#"{"text":"hello"}"#),
                }]
        ));
        assert!(matches!(
            snapshot.messages[2],
            crate::state::Message::ToolResult { ref tool_call_id, ref content, is_error: false, .. }
                if tool_call_id == &call_id && content == "echoed: hello"
        ));
        assert!(matches!(
            snapshot.messages[3],
            crate::state::Message::Assistant { ref content, ref tool_calls, .. }
                if content == "done" && tool_calls.is_empty()
        ));

        Ok::<(), CoreError>(())
    })
    .expect("tool call should continue to a final model turn");
}

#[test]
fn invalid_tool_arguments_become_an_error_result_and_the_model_can_continue() {
    smol::block_on(async {
        let provider = Arc::new(ScriptedProvider::new([
            ModelStream {
                events: vec![
                    ModelStreamEvent::ToolCall(AssistantToolCall {
                        id: ToolCallId::new("call_invalid_arguments")
                            .expect("non-empty provider ID"),
                        name: "echo".into(),
                        arguments: SerializedJson::new("{}"),
                    }),
                    ModelStreamEvent::End(StopReason::ToolUse),
                ],
            },
            ModelStream {
                events: vec![
                    ModelStreamEvent::TextDelta("recovered".into()),
                    ModelStreamEvent::End(StopReason::EndTurn),
                ],
            },
        ]));
        let executed = Arc::new(Mutex::new(Vec::new()));
        let agent = Agent::builder()
            .model_provider(provider.clone())
            .tool(Arc::new(EchoTool {
                calls: Arc::clone(&executed),
                schema: pi_agent_protocol::JsonValue::parse(
                    r#"{"type":"object","required":["text"]}"#,
                )
                .expect("test schema is valid JSON"),
            }))
            .build();
        let run = agent.start_prompt("send malformed echo arguments")?;

        run.drive().await?;

        assert_eq!(provider.requests().len(), 2);
        assert!(executed.lock().expect("test tool mutex").is_empty());
        assert!(matches!(
            agent.snapshot().messages[2],
            crate::state::Message::ToolResult { is_error: true, .. }
        ));
        assert!(matches!(
            run.events()[7].kind,
            AgentEventKind::ToolExecutionEnd {
                result: ToolResult { is_error: true, .. },
                ..
            }
        ));
        assert!(matches!(
            agent.snapshot().messages.last(),
            Some(crate::state::Message::Assistant { content, .. }) if content == "recovered"
        ));

        Ok::<(), CoreError>(())
    })
    .expect("schema failure should remain tool-scoped");
}

#[test]
fn steering_queue_drains_one_at_a_time_before_initial_and_later_turns() {
    smol::block_on(async {
        let provider = Arc::new(ScriptedProvider::new([
            ModelStream {
                events: vec![
                    ModelStreamEvent::TextDelta("first response".into()),
                    ModelStreamEvent::End(StopReason::EndTurn),
                ],
            },
            ModelStream {
                events: vec![
                    ModelStreamEvent::TextDelta("second response".into()),
                    ModelStreamEvent::End(StopReason::EndTurn),
                ],
            },
        ]));
        let agent = Agent::builder().model_provider(provider.clone()).build();
        agent.enqueue_steering("first steering")?;
        agent.enqueue_steering("second steering")?;
        let run = agent.start_prompt("initial prompt")?;

        run.drive().await?;

        assert_eq!(provider.requests().len(), 2);
        let messages = agent.snapshot().messages;
        let user_contents = messages
            .iter()
            .filter_map(|message| match message {
                crate::state::Message::User { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            user_contents,
            ["initial prompt", "first steering", "second steering"]
        );
        let turn_starts = run
            .events()
            .into_iter()
            .filter(|event| matches!(event.kind, AgentEventKind::TurnStart { .. }))
            .count();
        assert_eq!(turn_starts, 2);
        assert!(!agent.has_queued_messages());

        Ok::<(), CoreError>(())
    })
    .expect("one-at-a-time steering should produce one additional model turn");
}

#[test]
fn follow_up_queue_extends_a_run_only_at_the_idle_boundary() {
    smol::block_on(async {
        let provider = Arc::new(ScriptedProvider::new([
            ModelStream {
                events: vec![
                    ModelStreamEvent::TextDelta("first response".into()),
                    ModelStreamEvent::End(StopReason::EndTurn),
                ],
            },
            ModelStream {
                events: vec![
                    ModelStreamEvent::TextDelta("follow-up response".into()),
                    ModelStreamEvent::End(StopReason::EndTurn),
                ],
            },
        ]));
        let agent = Agent::builder().model_provider(provider.clone()).build();
        agent.enqueue_follow_up("follow-up input")?;
        let run = agent.start_prompt("initial prompt")?;

        run.drive().await?;

        assert_eq!(provider.requests().len(), 2);
        assert!(matches!(
            agent.snapshot().messages[2],
            crate::state::Message::User { ref content, .. } if content == "follow-up input"
        ));
        assert!(matches!(
            agent.snapshot().messages[3],
            crate::state::Message::Assistant { ref content, .. } if content == "follow-up response"
        ));
        assert!(!agent.has_queued_messages());

        Ok::<(), CoreError>(())
    })
    .expect("follow-up should be injected only after a normal stopping turn");
}

#[test]
fn continue_requires_a_non_assistant_tail_unless_queue_input_is_available() {
    smol::block_on(async {
        let provider = Arc::new(ScriptedProvider::new([
            ModelStream {
                events: vec![
                    ModelStreamEvent::TextDelta("first response".into()),
                    ModelStreamEvent::End(StopReason::EndTurn),
                ],
            },
            ModelStream {
                events: vec![
                    ModelStreamEvent::TextDelta("continued response".into()),
                    ModelStreamEvent::End(StopReason::EndTurn),
                ],
            },
        ]));
        let agent = Agent::builder().model_provider(provider.clone()).build();
        let first = agent.start_prompt("initial prompt")?;
        first.drive().await?;

        assert!(matches!(
            agent.start_continue(),
            Err(CoreError::InvalidTransition(error)) if error.from == "assistant-tail"
        ));
        agent.enqueue_steering("queued continuation")?;
        let continued = agent.start_continue()?;
        continued.drive().await?;

        assert_eq!(provider.requests().len(), 2);
        assert!(matches!(
            agent.snapshot().messages[2],
            crate::state::Message::User { ref content, .. } if content == "queued continuation"
        ));
        let event_kinds = continued
            .events()
            .into_iter()
            .map(|event| event.kind)
            .collect::<Vec<_>>();
        assert!(matches!(event_kinds[0], AgentEventKind::AgentStart));
        assert!(matches!(event_kinds[1], AgentEventKind::TurnStart { .. }));
        assert!(matches!(
            event_kinds[2],
            AgentEventKind::MessageStart { .. }
        ));
        assert!(matches!(event_kinds[3], AgentEventKind::MessageEnd { .. }));

        Ok::<(), CoreError>(())
    })
    .expect("queued continuation input should recover from an assistant transcript tail");
}

#[test]
fn parallel_tool_ends_follow_completion_order_while_results_keep_source_order() {
    smol::block_on(async {
        let provider = Arc::new(ScriptedProvider::new([
            ModelStream {
                events: vec![
                    ModelStreamEvent::ToolCall(AssistantToolCall {
                        id: ToolCallId::new("call_slow").expect("non-empty provider ID"),
                        name: "slow".into(),
                        arguments: SerializedJson::new("{}"),
                    }),
                    ModelStreamEvent::ToolCall(AssistantToolCall {
                        id: ToolCallId::new("call_fast").expect("non-empty provider ID"),
                        name: "fast".into(),
                        arguments: SerializedJson::new("{}"),
                    }),
                    ModelStreamEvent::End(StopReason::ToolUse),
                ],
            },
            ModelStream {
                events: vec![
                    ModelStreamEvent::TextDelta("done".into()),
                    ModelStreamEvent::End(StopReason::EndTurn),
                ],
            },
        ]));
        let schema = pi_agent_protocol::JsonValue::parse(r#"{"type":"object"}"#)
            .expect("test schema is valid JSON");
        let agent = Agent::builder()
            .model_provider(provider)
            .tool(Arc::new(ParallelFixtureTool {
                name: "slow",
                yield_once: true,
                schema: schema.clone(),
            }))
            .tool(Arc::new(ParallelFixtureTool {
                name: "fast",
                yield_once: false,
                schema,
            }))
            .build();
        let run = agent.start_prompt("run two tools")?;

        run.drive().await?;

        let ended = run
            .events()
            .into_iter()
            .filter_map(|event| match event.kind {
                AgentEventKind::ToolExecutionEnd { tool_call_id, .. } => {
                    Some(tool_call_id.as_str().to_owned())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(ended, ["call_fast", "call_slow"]);
        let result_ids = agent
            .snapshot()
            .messages
            .into_iter()
            .filter_map(|message| match message {
                crate::state::Message::ToolResult { tool_call_id, .. } => {
                    Some(tool_call_id.as_str().to_owned())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(result_ids, ["call_slow", "call_fast"]);

        Ok::<(), CoreError>(())
    })
    .expect("parallel tools should preserve Pi completion and transcript order contracts");
}

#[test]
fn agent_allows_one_run_and_drop_settles_cancellation() {
    let agent = Agent::builder().build();
    let run = agent.start_prompt("first").expect("first run should start");
    assert!(matches!(agent.snapshot().phase, AgentPhase::Running(_)));

    let second = agent.start_prompt("second");
    assert!(matches!(second, Err(CoreError::ActiveRun { .. })));

    drop(run);
    assert_eq!(agent.snapshot().phase, AgentPhase::Idle);
    assert!(!agent.snapshot().is_streaming);
    assert!(agent.snapshot().pending_tool_calls.is_empty());
}

#[test]
fn explicit_abort_settles_the_run_handle() {
    let agent = Agent::builder().build();
    let run = agent.start_prompt("cancel me").expect("run should start");
    agent.abort();
    assert_eq!(run.snapshot().phase, crate::state::RunPhase::Cancelled);
    assert_eq!(agent.snapshot().phase, AgentPhase::Idle);
}

#[test]
fn reset_is_idle_only_and_clears_retained_state_and_queues() {
    let agent = Agent::builder().build();
    agent
        .enqueue_steering("queued steering")
        .expect("idle queueing is explicit and allowed");
    agent
        .enqueue_follow_up("queued follow-up")
        .expect("idle queueing is explicit and allowed");
    let run = agent
        .start_prompt("retained until reset")
        .expect("run should start");

    assert!(matches!(agent.reset(), Err(CoreError::ActiveRun { .. })));
    agent.abort();
    assert_eq!(run.snapshot().phase, crate::state::RunPhase::Cancelled);
    agent.reset().expect("idle reset should succeed");

    let snapshot = agent.snapshot();
    assert!(snapshot.messages.is_empty());
    assert!(snapshot.last_error.is_none());
    assert!(!snapshot.is_streaming);
    assert!(snapshot.pending_tool_calls.is_empty());
    assert!(!agent.has_queued_messages());
}

#[test]
fn queue_modes_can_change_without_reconstructing_the_agent() {
    let agent = Agent::builder().build();
    assert_eq!(agent.steering_mode(), crate::queue::QueueMode::OneAtATime);
    assert_eq!(agent.follow_up_mode(), crate::queue::QueueMode::OneAtATime);

    agent.set_steering_mode(crate::queue::QueueMode::All);
    agent.set_follow_up_mode(crate::queue::QueueMode::All);

    assert_eq!(agent.steering_mode(), crate::queue::QueueMode::All);
    assert_eq!(agent.follow_up_mode(), crate::queue::QueueMode::All);
}

#[test]
fn queue_mode_preserves_insertion_order() {
    let mut queue = crate::queue::SteeringQueue::default();
    assert_eq!(queue.push("a"), 1);
    assert_eq!(queue.push("b"), 2);
    assert_eq!(queue.drain(crate::queue::QueueMode::OneAtATime).len(), 1);
    assert_eq!(
        queue
            .drain(crate::queue::QueueMode::All)
            .into_iter()
            .map(|item| item.content)
            .collect::<Vec<_>>(),
        vec!["b"]
    );
}

#[test]
fn parallel_completions_return_to_source_order_for_context() {
    let scheduler = Scheduler;
    let calls = (1..=3).map(|id| {
        (
            ToolCall {
                id: ToolCallId::new(format!("provider-call-{id}"))
                    .expect("non-empty test tool-call ID"),
                name: format!("tool-{id}"),
                arguments: SerializedJson::new("{}"),
            },
            ToolExecutionMode::Parallel,
        )
    });
    let batch = scheduler.plan_tool_batch(calls);
    let mut completions = crate::scheduler::CompletionSet::default();
    for id in [3, 1, 2] {
        batch
            .record_completion(
                &mut completions,
                ToolResult {
                    tool_call_id: ToolCallId::new(format!("provider-call-{id}"))
                        .expect("non-empty test tool-call ID"),
                    content: format!("{id}"),
                    details: None,
                    is_error: false,
                },
            )
            .expect("planned call");
    }
    assert_eq!(
        completions
            .in_source_order(&batch)
            .into_iter()
            .map(|result| result.content)
            .collect::<Vec<_>>(),
        vec!["1", "2", "3"]
    );
}

#[test]
fn tool_call_identifier_preserves_provider_text_and_rejects_empty_values() {
    let id = ToolCallId::new("provider-tool-call_42").expect("non-empty provider ID");

    assert_eq!(id.as_str(), "provider-tool-call_42");
    assert!(ToolCallId::new("").is_err());
}
