use crate::agent::Agent;
use crate::error::CoreError;
use crate::event::{AgentEvent, AgentEventKind, EventObserver, ObserverFuture};
use crate::hooks::{
    AfterToolCall, BeforeToolCall, ContextEnvelope, HookFuture, HookSet, NextTurn, Replacement,
};
use crate::scheduler::{
    CancellationToken, ModelFuture, ModelProvider, ModelRequest, ModelStream, ModelStreamEvent,
    Scheduler,
};
use crate::state::{
    AgentPhase, AssistantToolCall, ModelDescriptor, SerializedJson, StopReason, ThinkingLevel,
    ToolCallId, Usage,
};
use crate::tool::{
    AgentTool, ToolCall, ToolContext, ToolExecutionMode, ToolFuture, ToolResult, ToolUpdateSink,
};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

#[derive(Debug)]
struct TextOnlyProvider;

impl ModelProvider for TextOnlyProvider {
    fn stream<'a>(
        &'a self,
        _request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelFuture<'a> {
        if cancellation.is_cancelled() {
            return Box::pin(std::future::ready(Ok(Box::new(ModelStream {
                events: vec![ModelStreamEvent::End(StopReason::Aborted)],
            }) as _)));
        }
        Box::pin(std::future::ready(Ok(Box::new(ModelStream {
            events: vec![
                ModelStreamEvent::TextDelta("fixture capture succeeded.".into()),
                ModelStreamEvent::End(StopReason::EndTurn),
            ],
        }) as _)))
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
        Box::pin(std::future::ready(
            stream.map(|stream| Box::new(stream) as _),
        ))
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
            usage: None,
            added_tool_names: Vec::new(),
            terminate: false,
            is_error: false,
        })))
    }
}

#[derive(Debug)]
struct YieldOnceToolFuture {
    result: Option<Result<ToolResult, crate::error::ToolError>>,
    yielded: bool,
}

#[derive(Debug)]
struct YieldCountToolFuture {
    result: Option<Result<ToolResult, crate::error::ToolError>>,
    remaining_yields: u8,
}

impl Future for YieldCountToolFuture {
    type Output = Result<ToolResult, crate::error::ToolError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        if self.remaining_yields > 0 {
            self.remaining_yields -= 1;
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
    execution_mode: ToolExecutionMode,
    yield_once: bool,
    update: Option<&'static str>,
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
        self.execution_mode
    }

    fn execute<'a>(
        &'a self,
        call: ToolCall,
        _context: ToolContext,
        updates: ToolUpdateSink,
    ) -> ToolFuture<'a> {
        if let Some(update) = self.update {
            updates.emit(crate::tool::ToolUpdate {
                content: update.into(),
                details: None,
            });
        }
        let result = ToolResult {
            tool_call_id: call.id,
            content: self.name.into(),
            details: None,
            usage: None,
            added_tool_names: Vec::new(),
            terminate: false,
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
struct VariableDelayTool {
    name: &'static str,
    yields: u8,
    schema: pi_agent_protocol::JsonValue,
}

impl AgentTool for VariableDelayTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        self.name
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
        Box::pin(YieldCountToolFuture {
            result: Some(Ok(ToolResult {
                tool_call_id: call.id,
                content: self.name.into(),
                details: None,
                usage: None,
                added_tool_names: Vec::new(),
                terminate: false,
                is_error: false,
            })),
            remaining_yields: self.yields,
        })
    }
}

#[derive(Debug)]
struct RecordingObserver {
    events: Arc<std::sync::Mutex<Vec<AgentEvent>>>,
}

#[derive(Debug)]
struct ReplacementContextHooks;

impl HookSet for ReplacementContextHooks {
    fn before_tool_call(
        &self,
        _call: &ToolCall,
    ) -> Result<BeforeToolCall, crate::error::HookError> {
        Ok(BeforeToolCall::Allow)
    }

    fn after_tool_call(
        &self,
        _call: &ToolCall,
        _result: &ToolResult,
    ) -> Result<AfterToolCall, crate::error::HookError> {
        Ok(AfterToolCall::default())
    }

    fn transform_context(
        &self,
        context: ContextEnvelope,
    ) -> Result<ContextEnvelope, crate::error::HookError> {
        Ok(context)
    }

    fn convert_to_llm(&self, context: ContextEnvelope) -> Result<String, crate::error::HookError> {
        Ok(context
            .host_messages
            .last()
            .map(|message| message.as_str().to_owned())
            .unwrap_or_else(|| "state-context".into()))
    }

    fn prepare_next_turn(
        &self,
        mut context: ContextEnvelope,
    ) -> Result<NextTurn, crate::error::HookError> {
        context
            .host_messages
            .push(crate::state::SerializedJson::new("replacement-context"));
        Ok(NextTurn {
            context: Some(context),
            model: Some(ModelDescriptor {
                provider: "replacement-provider".into(),
                model: "replacement-model".into(),
                revision: Some("replacement-revision".into()),
            }),
            thinking_level: Some(ThinkingLevel::High),
        })
    }
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

#[derive(Debug)]
struct SubscribeOnAgentStartObserver {
    agent: Arc<Mutex<Option<Agent>>>,
    observed: Arc<Mutex<Vec<AgentEvent>>>,
    subscriptions: Arc<Mutex<Vec<crate::agent::ObserverSubscription>>>,
    subscribed: AtomicBool,
}

#[derive(Debug)]
struct FailingObserver;

impl EventObserver for FailingObserver {
    fn observe<'a>(
        &'a self,
        event: &'a AgentEvent,
        _cancellation: CancellationToken,
    ) -> ObserverFuture<'a> {
        let result = if matches!(event.kind, AgentEventKind::AgentStart) {
            Err(CoreError::Hook(crate::error::HookError::new(
                "observer",
                "fixture observer failure",
            )))
        } else {
            Ok(())
        };
        Box::pin(std::future::ready(result))
    }
}

impl EventObserver for SubscribeOnAgentStartObserver {
    fn observe<'a>(
        &'a self,
        event: &'a AgentEvent,
        _cancellation: CancellationToken,
    ) -> ObserverFuture<'a> {
        if matches!(event.kind, AgentEventKind::AgentStart)
            && !self.subscribed.swap(true, Ordering::SeqCst)
        {
            let agent = self
                .agent
                .lock()
                .expect("test agent mutex")
                .clone()
                .expect("agent is installed before the run starts");
            let subscription = agent.subscribe(Arc::new(RecordingObserver {
                events: Arc::clone(&self.observed),
            }));
            self.subscriptions
                .lock()
                .expect("test subscription mutex")
                .push(subscription);
        }
        Box::pin(std::future::ready(Ok(())))
    }
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

#[derive(Debug)]
struct AbortDuringBeforeToolHook {
    agent: Arc<Mutex<Option<Agent>>>,
}

impl HookSet for AbortDuringBeforeToolHook {
    fn before_tool_call(
        &self,
        _call: &ToolCall,
    ) -> Result<BeforeToolCall, crate::error::HookError> {
        Ok(BeforeToolCall::Allow)
    }

    fn after_tool_call(
        &self,
        _call: &ToolCall,
        _result: &ToolResult,
    ) -> Result<AfterToolCall, crate::error::HookError> {
        Ok(AfterToolCall::default())
    }

    fn transform_context(
        &self,
        context: ContextEnvelope,
    ) -> Result<ContextEnvelope, crate::error::HookError> {
        Ok(context)
    }

    fn convert_to_llm(&self, context: ContextEnvelope) -> Result<String, crate::error::HookError> {
        Ok(context
            .messages
            .into_iter()
            .map(|message| format!("{message:?}"))
            .collect::<Vec<_>>()
            .join("\n"))
    }

    fn before_tool_call_async<'a>(
        &'a self,
        _call: &'a ToolCall,
        _context: ContextEnvelope,
        cancellation: CancellationToken,
    ) -> HookFuture<'a, BeforeToolCall> {
        if let Some(agent) = self.agent.lock().expect("test agent mutex").clone() {
            agent.abort();
        }
        assert!(cancellation.is_cancelled());
        Box::pin(std::future::ready(Ok(BeforeToolCall::Allow)))
    }
}

#[derive(Debug)]
struct MetadataAfterToolHook;

impl HookSet for MetadataAfterToolHook {
    fn before_tool_call(
        &self,
        _call: &ToolCall,
    ) -> Result<BeforeToolCall, crate::error::HookError> {
        Ok(BeforeToolCall::Allow)
    }

    fn after_tool_call(
        &self,
        _call: &ToolCall,
        _result: &ToolResult,
    ) -> Result<AfterToolCall, crate::error::HookError> {
        Ok(AfterToolCall {
            details: Replacement::Replace(Some(SerializedJson::new(r#"{"source":"hook"}"#))),
            usage: Replacement::Replace(Usage {
                input_tokens: Some(3),
                output_tokens: Some(5),
                reasoning_tokens: Some(2),
                ..Usage::default()
            }),
            added_tool_names: Replacement::Replace(vec!["later-tool".into()]),
            ..AfterToolCall::default()
        })
    }

    fn transform_context(
        &self,
        context: ContextEnvelope,
    ) -> Result<ContextEnvelope, crate::error::HookError> {
        Ok(context)
    }

    fn convert_to_llm(&self, context: ContextEnvelope) -> Result<String, crate::error::HookError> {
        Ok(context
            .messages
            .into_iter()
            .map(|message| format!("{message:?}"))
            .collect::<Vec<_>>()
            .join("\n"))
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

#[cfg(feature = "trace")]
#[test]
fn trace_observer_does_not_change_observable_agent_behavior() {
    smol::block_on(async {
        let untraced = Agent::builder()
            .model_provider(Arc::new(TextOnlyProvider))
            .build();
        let untraced_run = untraced.start_prompt("trace identity")?;
        untraced_run.drive().await?;

        let json_trace = Arc::new(crate::trace::TraceObserver::new(
            "trace-jsonl",
            pi_agent_trace::JsonLinesSink::new(Vec::new()),
        ));
        let json_traced = Agent::builder()
            .model_provider(Arc::new(TextOnlyProvider))
            .observer(json_trace.clone())
            .build();
        let json_run = json_traced.start_prompt("trace identity")?;
        json_run.drive().await?;

        let cbor_trace = Arc::new(crate::trace::TraceObserver::new(
            "trace-cbor",
            pi_agent_trace::CborSink::new(Vec::new()),
        ));
        let cbor_traced = Agent::builder()
            .model_provider(Arc::new(TextOnlyProvider))
            .observer(cbor_trace.clone())
            .build();
        let cbor_run = cbor_traced.start_prompt("trace identity")?;
        cbor_run.drive().await?;

        for (events, snapshot) in [
            (json_run.events(), json_traced.snapshot()),
            (cbor_run.events(), cbor_traced.snapshot()),
        ] {
            assert_eq!(events, untraced_run.events());
            assert_eq!(snapshot, untraced.snapshot());
        }
        json_trace.with_sink(|sink| {
            let text = std::str::from_utf8(sink.inner()).expect("trace JSONL is UTF-8");
            assert!(text.contains(r#""type":"episode_header""#));
            assert!(text.contains(r#""type":"episode_end""#));
        });
        cbor_trace.with_sink(|sink| assert!(!sink.inner().is_empty()));

        Ok::<(), CoreError>(())
    })
    .expect("tracing must be observational only");
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
fn explicit_model_error_preserves_the_assistant_failure_without_synthesizing_transport_error() {
    smol::block_on(async {
        let provider = Arc::new(ScriptedProvider::new([ModelStream {
            events: vec![
                ModelStreamEvent::TextDelta("partial".into()),
                ModelStreamEvent::Error {
                    message: "model refused the request".into(),
                },
            ],
        }]));
        let agent = Agent::builder().model_provider(provider).build();
        let run = agent.start_prompt("trigger a model error")?;

        let error = run
            .drive()
            .await
            .expect_err("model error must fail the run");
        assert_eq!(
            error,
            CoreError::ModelError {
                message: "model refused the request".into()
            }
        );
        assert_eq!(run.snapshot().phase, crate::state::RunPhase::Failed);
        assert_eq!(run.snapshot().stop_reason, Some(StopReason::Error));
        assert!(matches!(
            agent.snapshot().messages.last(),
            Some(crate::state::Message::Assistant {
                content,
                stop_reason: Some(StopReason::Error),
                error_message: Some(error_message),
                ..
            }) if content == "partial" && error_message == "model refused the request"
        ));
        assert!(matches!(
            run.events().last().map(|event| &event.kind),
            Some(AgentEventKind::AgentEnd { .. })
        ));
        assert_eq!(
            agent.snapshot().last_error.as_deref(),
            Some("model refused the request")
        );

        Ok::<(), CoreError>(())
    })
    .expect("explicit model error should settle without a duplicate failure message");
}

#[test]
fn explicit_model_abort_preserves_the_provider_diagnostic_without_host_cancellation() {
    smol::block_on(async {
        let provider = Arc::new(ScriptedProvider::new([ModelStream {
            events: vec![ModelStreamEvent::Aborted {
                message: "provider stopped the response".into(),
            }],
        }]));
        let agent = Agent::builder().model_provider(provider).build();
        let run = agent.start_prompt("trigger an independent provider abort")?;

        assert_eq!(
            run.drive().await,
            Err(CoreError::ModelAborted {
                message: "provider stopped the response".into(),
            })
        );
        assert_eq!(run.snapshot().phase, crate::state::RunPhase::Failed);
        assert!(matches!(
            agent.snapshot().messages.last(),
            Some(crate::state::Message::Assistant {
                stop_reason: Some(StopReason::Aborted),
                error_message: Some(message),
                ..
            }) if message == "provider stopped the response"
        ));
        assert_eq!(agent.snapshot().phase, AgentPhase::Idle);

        Ok::<(), CoreError>(())
    })
    .expect("a provider abort must remain distinct from caller cancellation");
}

#[test]
fn length_stop_refuses_truncated_tool_calls_and_allows_a_recovery_turn() {
    smol::block_on(async {
        let call_id = ToolCallId::new("call_truncated").expect("non-empty provider ID");
        let provider = Arc::new(ScriptedProvider::new([
            ModelStream {
                events: vec![
                    ModelStreamEvent::TextDelta("partial tool request".into()),
                    ModelStreamEvent::ToolCall(AssistantToolCall {
                        id: call_id.clone(),
                        name: "echo".into(),
                        arguments: SerializedJson::new(r#"{"text":"hello"}"#),
                    }),
                    ModelStreamEvent::End(StopReason::Length),
                ],
            },
            ModelStream {
                events: vec![
                    ModelStreamEvent::TextDelta("recovered after truncation".into()),
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
        let run = agent.start_prompt("call echo, but truncate the response")?;

        run.drive().await?;

        assert_eq!(provider.requests().len(), 2);
        assert!(executed.lock().expect("test tool mutex").is_empty());
        assert!(matches!(
            agent.snapshot().messages[1],
            crate::state::Message::Assistant {
                stop_reason: Some(StopReason::Length),
                ref tool_calls,
                ..
            } if tool_calls.len() == 1 && tool_calls[0].id == call_id
        ));
        assert!(matches!(
            agent.snapshot().messages[2],
            crate::state::Message::ToolResult {
                ref tool_call_id,
                is_error: true,
                ref content,
                ..
            } if tool_call_id == &call_id && content.contains("output token limit")
        ));
        assert!(run.events().iter().any(|event| {
            matches!(
                event.kind,
                AgentEventKind::TurnEnd {
                    reason: StopReason::Length,
                    ..
                }
            )
        }));
        assert!(matches!(
            agent.snapshot().messages.last(),
            Some(crate::state::Message::Assistant { content, stop_reason: Some(StopReason::EndTurn), .. })
                if content == "recovered after truncation"
        ));

        Ok::<(), CoreError>(())
    })
    .expect("length stop should produce an error tool result and continue");
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
fn runtime_subscription_is_reentrant_and_drop_unsubscribes_for_future_events() {
    smol::block_on(async {
        let provider = Arc::new(ScriptedProvider::new([
            ModelStream {
                events: vec![
                    ModelStreamEvent::TextDelta("first run".into()),
                    ModelStreamEvent::End(StopReason::EndTurn),
                ],
            },
            ModelStream {
                events: vec![
                    ModelStreamEvent::TextDelta("second run".into()),
                    ModelStreamEvent::End(StopReason::EndTurn),
                ],
            },
        ]));
        let observed = Arc::new(Mutex::new(Vec::new()));
        let observer_agent = Arc::new(Mutex::new(None));
        let subscriptions = Arc::new(Mutex::new(Vec::new()));
        let agent = Agent::builder()
            .model_provider(provider)
            .observer(Arc::new(SubscribeOnAgentStartObserver {
                agent: Arc::clone(&observer_agent),
                observed: Arc::clone(&observed),
                subscriptions: Arc::clone(&subscriptions),
                subscribed: AtomicBool::new(false),
            }))
            .build();
        *observer_agent.lock().expect("test agent mutex") = Some(agent.clone());

        let first = agent.start_prompt("first")?;
        first.drive().await?;
        let first_events = first.events();
        let observed_after_first = observed.lock().expect("test observer mutex").clone();
        assert_eq!(observed_after_first, first_events[1..]);
        assert!(matches!(
            observed_after_first.first().map(|event| &event.kind),
            Some(AgentEventKind::TurnStart { .. })
        ));

        subscriptions
            .lock()
            .expect("test subscription mutex")
            .clear();
        let second = agent.start_prompt("second")?;
        second.drive().await?;
        assert_eq!(
            observed.lock().expect("test observer mutex").as_slice(),
            &first_events[1..]
        );

        Ok::<(), CoreError>(())
    })
    .expect("runtime subscription must be safe from observer callbacks and unsubscribe on drop");
}

#[test]
fn nonblocking_subscription_is_ordered_lossy_and_never_delays_settlement() {
    smol::block_on(async {
        let full_capacity_agent = Agent::builder()
            .model_provider(Arc::new(TextOnlyProvider))
            .build();
        let ordered = full_capacity_agent
            .subscribe_nonblocking(std::num::NonZeroUsize::new(32).expect("nonzero capacity"));
        let ordered_run = full_capacity_agent.start_prompt("ordered events")?;
        ordered_run.drive().await?;
        let mut delivered = Vec::new();
        while let Ok(event) = ordered.try_recv() {
            delivered.push(event);
        }
        assert_eq!(delivered, ordered_run.events());
        assert_eq!(ordered.dropped_events(), 0);

        let constrained_agent = Agent::builder()
            .model_provider(Arc::new(TextOnlyProvider))
            .build();
        let constrained = constrained_agent
            .subscribe_nonblocking(std::num::NonZeroUsize::new(1).expect("nonzero capacity"));
        let constrained_run = constrained_agent.start_prompt("lossy events")?;
        constrained_run.drive().await?;
        assert_eq!(constrained_agent.snapshot().phase, AgentPhase::Idle);
        assert!(matches!(
            constrained.try_recv().map(|event| event.kind),
            Ok(AgentEventKind::AgentStart)
        ));
        assert_eq!(
            constrained.dropped_events(),
            constrained_run.events().len() as u64 - 1
        );

        Ok::<(), CoreError>(())
    })
    .expect("nonblocking event delivery must not participate in run settlement");
}

#[test]
fn idle_provider_replacement_preserves_history_and_changes_the_next_request() {
    smol::block_on(async {
        let first_provider = Arc::new(ScriptedProvider::new([ModelStream {
            events: vec![
                ModelStreamEvent::TextDelta("first response".into()),
                ModelStreamEvent::End(StopReason::EndTurn),
            ],
        }]));
        let second_provider = Arc::new(ScriptedProvider::new([ModelStream {
            events: vec![
                ModelStreamEvent::TextDelta("second response".into()),
                ModelStreamEvent::End(StopReason::EndTurn),
            ],
        }]));
        let initial_model = ModelDescriptor {
            provider: "fixture".into(),
            model: "initial".into(),
            revision: None,
        };
        let replacement_model = ModelDescriptor {
            provider: "fixture".into(),
            model: "replacement".into(),
            revision: Some("pinned".into()),
        };
        let agent = Agent::builder()
            .model(initial_model)
            .model_provider(first_provider)
            .build();

        agent.start_prompt("first")?.drive().await?;
        let retained_messages = agent.snapshot().messages;

        agent.replace_model_provider(replacement_model.clone(), second_provider.clone())?;
        assert_eq!(agent.snapshot().messages, retained_messages);
        assert_eq!(agent.snapshot().model, Some(replacement_model.clone()));

        agent.start_prompt("second")?.drive().await?;
        assert_eq!(second_provider.requests().len(), 1);
        assert_eq!(second_provider.requests()[0].model, Some(replacement_model));

        Ok::<(), CoreError>(())
    })
    .expect("idle replacement must retain the linear transcript and select the new provider");
}

#[test]
fn provider_replacement_is_rejected_while_a_run_is_owned() {
    let agent = Agent::builder()
        .model_provider(Arc::new(TextOnlyProvider))
        .build();
    let active = agent.start_prompt("active").expect("run starts");
    let error = agent
        .replace_model_provider(
            ModelDescriptor {
                provider: "fixture".into(),
                model: "replacement".into(),
                revision: None,
            },
            Arc::new(TextOnlyProvider),
        )
        .expect_err("an active run owns its model/provider pair");
    assert!(matches!(error, CoreError::ActiveRun { .. }));
    active.abort().expect("created run aborts cleanly");
    assert_eq!(agent.snapshot().phase, AgentPhase::Idle);
}

#[test]
fn lossless_subscription_is_ordered_without_capacity_drops() {
    smol::block_on(async {
        let agent = Agent::builder()
            .model_provider(Arc::new(TextOnlyProvider))
            .build();
        let subscription = agent.subscribe_lossless();
        let run = agent.start_prompt("lossless ordered events")?;

        run.drive().await?;

        let mut delivered = Vec::new();
        while let Ok(event) = subscription.try_recv() {
            delivered.push(event);
        }
        assert_eq!(delivered, run.events());
        assert!(matches!(
            subscription.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));

        Ok::<(), CoreError>(())
    })
    .expect("lossless event delivery must preserve source order");
}

#[test]
fn lossless_subscription_retains_all_events_under_volume() {
    smol::block_on(async {
        let run_count = 256;
        let provider = Arc::new(ScriptedProvider::new((0..run_count).map(|index| {
            ModelStream {
                events: vec![
                    ModelStreamEvent::TextDelta(format!("lossless event volume {index}")),
                    ModelStreamEvent::End(StopReason::EndTurn),
                ],
            }
        })));
        let agent = Agent::builder().model_provider(provider).build();
        let subscription = agent.subscribe_lossless();
        let mut emitted = Vec::new();

        for index in 0..run_count {
            let run = agent
                .start_prompt(format!("lossless volume run {index}"))
                .expect("volume run starts");
            run.drive().await?;
            emitted.extend(run.events());
        }
        assert!(emitted.len() > 1_000, "volume must exceed a small queue");

        let mut delivered = Vec::new();
        while let Ok(event) = subscription.try_recv() {
            delivered.push(event);
        }
        assert_eq!(delivered, emitted);

        Ok::<(), CoreError>(())
    })
    .expect("lossless event delivery must not silently drop under volume");
}

#[test]
fn dropping_lossless_subscription_unsubscribes_cleanly() {
    smol::block_on(async {
        let provider = Arc::new(ScriptedProvider::new([
            ModelStream {
                events: vec![
                    ModelStreamEvent::TextDelta("before drop".into()),
                    ModelStreamEvent::End(StopReason::EndTurn),
                ],
            },
            ModelStream {
                events: vec![
                    ModelStreamEvent::TextDelta("after drop".into()),
                    ModelStreamEvent::End(StopReason::EndTurn),
                ],
            },
        ]));
        let agent = Agent::builder().model_provider(provider).build();
        let subscription = agent.subscribe_lossless();
        let first = agent.start_prompt("before drop")?;
        first.drive().await?;
        drop(subscription);

        let second = agent.start_prompt("after drop")?;
        second.drive().await?;
        assert_eq!(agent.snapshot().phase, AgentPhase::Idle);

        Ok::<(), CoreError>(())
    })
    .expect("dropping a lossless subscription must not poison future runs");
}

#[test]
fn observer_failure_has_one_terminal_settlement_and_leaves_the_agent_reusable() {
    smol::block_on(async {
        let agent = Agent::builder()
            .model_provider(Arc::new(TextOnlyProvider))
            .observer(Arc::new(FailingObserver))
            .build();
        let failed = agent.start_prompt("fail an observer")?;

        assert_eq!(
            failed.drive().await,
            Err(CoreError::Hook(crate::error::HookError::new(
                "observer",
                "fixture observer failure",
            )))
        );
        assert_eq!(agent.snapshot().phase, AgentPhase::Idle);
        assert_eq!(
            failed
                .events()
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::AgentEnd { .. }))
                .count(),
            1
        );

        // The same explicit observer still fails future runs, but neither the
        // active-run ownership nor terminal event grammar are poisoned.
        let reused = agent.start_prompt("reuse after observer failure")?;
        assert_eq!(
            reused.drive().await,
            Err(CoreError::Hook(crate::error::HookError::new(
                "observer",
                "fixture observer failure",
            )))
        );
        assert_eq!(agent.snapshot().phase, AgentPhase::Idle);

        Ok::<(), CoreError>(())
    })
    .expect("observer failure must settle exactly once and preserve ownership invariants");
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
                reason: StopReason::Aborted,
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
fn cancellation_during_an_async_before_hook_preserves_tool_result_then_allows_reuse() {
    smol::block_on(async {
        let call_id = ToolCallId::new("call_cancelled_before_tool").expect("non-empty ID");
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
                events: vec![ModelStreamEvent::End(StopReason::Aborted)],
            },
            ModelStream {
                events: vec![
                    ModelStreamEvent::TextDelta("reused normally".into()),
                    ModelStreamEvent::End(StopReason::EndTurn),
                ],
            },
        ]));
        let executed = Arc::new(Mutex::new(Vec::new()));
        let hook_agent = Arc::new(Mutex::new(None));
        let agent = Agent::builder()
            .model_provider(provider.clone())
            .hooks(Arc::new(AbortDuringBeforeToolHook {
                agent: Arc::clone(&hook_agent),
            }))
            .tool(Arc::new(EchoTool {
                calls: Arc::clone(&executed),
                schema: pi_agent_protocol::JsonValue::parse(
                    r#"{"type":"object","required":["text"]}"#,
                )
                .expect("test schema is valid JSON"),
            }))
            .build();
        *hook_agent.lock().expect("test agent mutex") = Some(agent.clone());

        let cancelled = agent.start_prompt("cancel at the policy boundary")?;
        assert_eq!(cancelled.drive().await, Err(CoreError::Cancelled));
        assert!(executed.lock().expect("test tool mutex").is_empty());
        assert_eq!(
            cancelled.snapshot().phase,
            crate::state::RunPhase::Cancelled
        );
        assert_eq!(agent.snapshot().phase, AgentPhase::Idle);
        assert!(agent.snapshot().pending_tool_calls.is_empty());
        let cancellation_events = cancelled.events();
        assert!(cancellation_events.iter().any(|event| {
            matches!(
                &event.kind,
                AgentEventKind::ToolExecutionEnd { result, .. }
                    if result.is_error && result.content == "Operation aborted"
            )
        }));
        assert!(matches!(
            cancellation_events.last().map(|event| &event.kind),
            Some(AgentEventKind::AgentEnd { .. })
        ));

        let reused = agent.start_prompt("reuse after cancellation")?;
        reused.drive().await?;
        assert_eq!(agent.snapshot().phase, AgentPhase::Idle);
        assert!(matches!(
            agent.snapshot().messages.last(),
            Some(crate::state::Message::Assistant { content, .. }) if content == "reused normally"
        ));
        assert_eq!(provider.requests().len(), 3);

        Ok::<(), CoreError>(())
    })
    .expect("the agent should be reusable after a cancellation-aware hook");
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
fn after_tool_metadata_is_preserved_in_the_transcript() {
    smol::block_on(async {
        let call_id = ToolCallId::new("call_metadata").expect("non-empty provider ID");
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
                events: vec![ModelStreamEvent::End(StopReason::EndTurn)],
            },
        ]));
        let agent = Agent::builder()
            .model_provider(provider)
            .hooks(Arc::new(MetadataAfterToolHook))
            .tool(Arc::new(EchoTool {
                calls: Arc::new(Mutex::new(Vec::new())),
                schema: pi_agent_protocol::JsonValue::parse(
                    r#"{"type":"object","required":["text"]}"#,
                )
                .expect("test schema is valid JSON"),
            }))
            .build();
        let run = agent.start_prompt("attach metadata to echo")?;

        run.drive().await?;

        assert!(matches!(
            &agent.snapshot().messages[2],
            crate::state::Message::ToolResult {
                tool_call_id,
                details: Some(details),
                usage: Some(Usage {
                    input_tokens: Some(3),
                    output_tokens: Some(5),
                    reasoning_tokens: Some(2),
                    ..
                }),
                added_tool_names,
                ..
            } if tool_call_id == &call_id
                && details.as_str() == r#"{"source":"hook"}"#
                && added_tool_names == &["later-tool"]
        ));

        Ok::<(), CoreError>(())
    })
    .expect("after-tool result metadata should survive transcript insertion");
}

#[test]
fn prepared_next_turn_context_survives_a_tool_continuation_without_queued_input() {
    smol::block_on(async {
        let call_id = ToolCallId::new("call_context_replacement").expect("non-empty provider ID");
        let provider = Arc::new(ScriptedProvider::new([
            ModelStream {
                events: vec![
                    ModelStreamEvent::ToolCall(AssistantToolCall {
                        id: call_id,
                        name: "echo".into(),
                        arguments: SerializedJson::new(r#"{"text":"hello"}"#),
                    }),
                    ModelStreamEvent::End(StopReason::ToolUse),
                ],
            },
            ModelStream {
                events: vec![
                    ModelStreamEvent::TextDelta("continued with replacement context".into()),
                    ModelStreamEvent::End(StopReason::EndTurn),
                ],
            },
        ]));
        let agent = Agent::builder()
            .model(ModelDescriptor {
                provider: "initial-provider".into(),
                model: "initial-model".into(),
                revision: None,
            })
            .thinking_level(ThinkingLevel::Low)
            .model_provider(provider.clone())
            .hooks(Arc::new(ReplacementContextHooks))
            .tool(Arc::new(EchoTool {
                calls: Arc::new(Mutex::new(Vec::new())),
                schema: pi_agent_protocol::JsonValue::parse(
                    r#"{"type":"object","required":["text"]}"#,
                )
                .expect("test schema is valid JSON"),
            }))
            .build();
        let run = agent.start_prompt("echo with replaced continuation context")?;

        run.drive().await?;

        let requests = provider.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0].model.as_ref().map(|model| model.model.as_str()),
            Some("initial-model")
        );
        assert_eq!(requests[0].thinking_level, ThinkingLevel::Low);
        assert_eq!(requests[1].context, "replacement-context");
        assert_eq!(
            requests[1].model.as_ref().map(|model| model.model.as_str()),
            Some("replacement-model")
        );
        assert_eq!(requests[1].thinking_level, ThinkingLevel::High);

        Ok::<(), CoreError>(())
    })
    .expect("prepared context should reach the next model request");
}

#[test]
fn host_only_context_is_explicit_and_reaches_only_the_converter_hook() {
    smol::block_on(async {
        let provider = Arc::new(ScriptedProvider::new([ModelStream {
            events: vec![
                ModelStreamEvent::TextDelta("converted host context".into()),
                ModelStreamEvent::End(StopReason::EndTurn),
            ],
        }]));
        let agent = Agent::builder()
            .host_message(SerializedJson::new("host-only-value"))
            .hooks(Arc::new(ReplacementContextHooks))
            .model_provider(provider.clone())
            .build();
        let run = agent.start_prompt("use host context")?;

        run.drive().await?;

        assert_eq!(provider.requests()[0].context, "host-only-value");
        assert_eq!(
            agent.snapshot().host_messages,
            [SerializedJson::new("host-only-value")]
        );

        Ok::<(), CoreError>(())
    })
    .expect("host-only context should remain an explicit hook capability");
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
        assert!(matches!(
            event_kinds.last(),
            Some(AgentEventKind::AgentEnd { messages })
                if messages.len() == 2
                    && matches!(messages[0], crate::state::Message::User { ref content, .. } if content == "queued continuation")
                    && matches!(messages[1], crate::state::Message::Assistant { ref content, .. } if content == "continued response")
        ));

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
                execution_mode: ToolExecutionMode::Parallel,
                yield_once: true,
                update: Some("slow update"),
                schema: schema.clone(),
            }))
            .tool(Arc::new(ParallelFixtureTool {
                name: "fast",
                execution_mode: ToolExecutionMode::Parallel,
                yield_once: false,
                update: None,
                schema,
            }))
            .build();
        let run = agent.start_prompt("run two tools")?;

        run.drive().await?;

        let events = run.events();
        let ended = events
            .iter()
            .filter_map(|event| match &event.kind {
                AgentEventKind::ToolExecutionEnd { tool_call_id, .. } => {
                    Some(tool_call_id.as_str().to_owned())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(ended, ["call_fast", "call_slow"]);
        let update_index = events
            .iter()
            .position(|event| {
                matches!(
                    &event.kind,
                    AgentEventKind::ToolExecutionUpdate {
                        tool_call_id,
                        update,
                        ..
                    } if tool_call_id.as_str() == "call_slow" && update.content == "slow update"
                )
            })
            .expect("slow tool update should be emitted");
        let fast_end_index = events
            .iter()
            .position(|event| {
                matches!(
                    &event.kind,
                    AgentEventKind::ToolExecutionEnd { tool_call_id, .. }
                        if tool_call_id.as_str() == "call_fast"
                )
            })
            .expect("fast tool end should be emitted");
        assert!(
            update_index < fast_end_index,
            "an update emitted before another completion must not wait for its own future"
        );
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
fn parallel_result_context_is_source_ordered_across_deterministic_completion_permutations() {
    smol::block_on(async {
        for yields in [[2, 0, 1], [1, 2, 0], [0, 1, 2]] {
            let provider = Arc::new(ScriptedProvider::new([
                ModelStream {
                    events: vec![
                        ModelStreamEvent::ToolCall(AssistantToolCall {
                            id: ToolCallId::new("call_a").expect("non-empty provider ID"),
                            name: "a".into(),
                            arguments: SerializedJson::new("{}"),
                        }),
                        ModelStreamEvent::ToolCall(AssistantToolCall {
                            id: ToolCallId::new("call_b").expect("non-empty provider ID"),
                            name: "b".into(),
                            arguments: SerializedJson::new("{}"),
                        }),
                        ModelStreamEvent::ToolCall(AssistantToolCall {
                            id: ToolCallId::new("call_c").expect("non-empty provider ID"),
                            name: "c".into(),
                            arguments: SerializedJson::new("{}"),
                        }),
                        ModelStreamEvent::End(StopReason::ToolUse),
                    ],
                },
                ModelStream {
                    events: vec![ModelStreamEvent::End(StopReason::EndTurn)],
                },
            ]));
            let schema = pi_agent_protocol::JsonValue::parse(r#"{"type":"object"}"#)
                .expect("test schema is valid JSON");
            let agent = Agent::builder()
                .model_provider(provider)
                .tool(Arc::new(VariableDelayTool {
                    name: "a",
                    yields: yields[0],
                    schema: schema.clone(),
                }))
                .tool(Arc::new(VariableDelayTool {
                    name: "b",
                    yields: yields[1],
                    schema: schema.clone(),
                }))
                .tool(Arc::new(VariableDelayTool {
                    name: "c",
                    yields: yields[2],
                    schema,
                }))
                .build();
            let run = agent.start_prompt("exercise completion permutation")?;
            run.drive().await?;

            let source_result_ids = agent
                .snapshot()
                .messages
                .into_iter()
                .filter_map(|message| match message {
                    crate::state::Message::ToolResult { tool_call_id, .. } => {
                        Some(tool_call_id.to_string())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(source_result_ids, ["call_a", "call_b", "call_c"]);

            let completed_ids = run
                .events()
                .into_iter()
                .filter_map(|event| match event.kind {
                    AgentEventKind::ToolExecutionEnd { tool_call_id, .. } => {
                        Some(tool_call_id.to_string())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(completed_ids.len(), 3);
            let mut sorted_completed = completed_ids;
            sorted_completed.sort();
            assert_eq!(sorted_completed, ["call_a", "call_b", "call_c"]);
        }

        Ok::<(), CoreError>(())
    })
    .expect("completion order may vary while model context remains source ordered");
}

#[test]
fn any_sequential_tool_serializes_a_mixed_tool_batch() {
    smol::block_on(async {
        let provider = Arc::new(ScriptedProvider::new([
            ModelStream {
                events: vec![
                    ModelStreamEvent::ToolCall(AssistantToolCall {
                        id: ToolCallId::new("call_serial").expect("non-empty provider ID"),
                        name: "serial".into(),
                        arguments: SerializedJson::new("{}"),
                    }),
                    ModelStreamEvent::ToolCall(AssistantToolCall {
                        id: ToolCallId::new("call_parallel").expect("non-empty provider ID"),
                        name: "parallel".into(),
                        arguments: SerializedJson::new("{}"),
                    }),
                    ModelStreamEvent::End(StopReason::ToolUse),
                ],
            },
            ModelStream {
                events: vec![ModelStreamEvent::End(StopReason::EndTurn)],
            },
        ]));
        let schema = pi_agent_protocol::JsonValue::parse(r#"{"type":"object"}"#)
            .expect("test schema is valid JSON");
        let agent = Agent::builder()
            .model_provider(provider)
            .tool(Arc::new(ParallelFixtureTool {
                name: "serial",
                execution_mode: ToolExecutionMode::Sequential,
                yield_once: true,
                update: None,
                schema: schema.clone(),
            }))
            .tool(Arc::new(ParallelFixtureTool {
                name: "parallel",
                execution_mode: ToolExecutionMode::Parallel,
                yield_once: false,
                update: None,
                schema,
            }))
            .build();
        let run = agent.start_prompt("run the mixed batch")?;

        run.drive().await?;

        let lifecycle = run
            .events()
            .into_iter()
            .filter_map(|event| match event.kind {
                AgentEventKind::ToolExecutionStart { tool_call_id, .. } => {
                    Some(("start", tool_call_id.to_string()))
                }
                AgentEventKind::ToolExecutionEnd { tool_call_id, .. } => {
                    Some(("end", tool_call_id.to_string()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            lifecycle,
            vec![
                ("start", "call_serial".into()),
                ("end", "call_serial".into()),
                ("start", "call_parallel".into()),
                ("end", "call_parallel".into()),
            ]
        );

        Ok::<(), CoreError>(())
    })
    .expect("a sequential tool must serialize the whole Pi tool batch");
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
fn generated_run_message_and_event_ids_are_monotonic_after_cancellation() {
    smol::block_on(async {
        let agent = Agent::builder()
            .model_provider(Arc::new(TextOnlyProvider))
            .build();
        let cancelled = agent.start_prompt("cancel before driving")?;
        assert_eq!(cancelled.id(), crate::state::RunId(1));
        agent.abort();
        assert_eq!(
            cancelled.snapshot().phase,
            crate::state::RunPhase::Cancelled
        );

        let completed = agent.start_prompt("assign stable identifiers")?;
        assert_eq!(completed.id(), crate::state::RunId(2));
        completed.drive().await?;

        let events = completed.events();
        assert!(events.iter().all(|event| event.run_id == completed.id()));
        assert_eq!(
            events
                .iter()
                .map(|event| event.sequence.0)
                .collect::<Vec<_>>(),
            (1..=events.len() as u64).collect::<Vec<_>>()
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::AgentEnd { .. }))
                .count(),
            1
        );
        let snapshot = agent.snapshot();
        let ids = snapshot
            .messages
            .iter()
            .map(|message| match message {
                crate::state::Message::User { id, .. }
                | crate::state::Message::Assistant { id, .. }
                | crate::state::Message::ToolResult { id, .. } => id.0,
            })
            .collect::<Vec<_>>();
        // The cancelled prompt is retained as message 1; the next prompt and
        // its response receive fresh IDs rather than reusing that durable
        // transcript entry.
        assert_eq!(ids, vec![1, 2, 3]);

        Ok::<(), CoreError>(())
    })
    .expect("V0-generated correlation identifiers must not be reused after cancellation");
}

#[test]
fn deterministic_scale_creates_and_settles_one_thousand_isolated_agents() {
    smol::block_on(async {
        for index in 0..1_000 {
            let agent = Agent::builder()
                .model_provider(Arc::new(TextOnlyProvider))
                .build();
            let run = agent.start_prompt(format!("scale fixture {index}"))?;
            run.drive().await?;

            let snapshot = agent.snapshot();
            assert_eq!(snapshot.phase, AgentPhase::Idle);
            assert!(!snapshot.is_streaming);
            assert!(snapshot.pending_tool_calls.is_empty());
            assert_eq!(snapshot.messages.len(), 2);
        }
        Ok::<(), CoreError>(())
    })
    .expect("independent agents must settle without hidden shared runtime state");
}

#[test]
fn concurrent_run_claims_admit_exactly_one_owner_and_leave_no_stale_active_run() {
    for _ in 0..100 {
        let agent = Agent::builder().build();
        let start_barrier = Arc::new(std::sync::Barrier::new(3));
        let claim_barrier = Arc::new(std::sync::Barrier::new(3));
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        std::thread::scope(|scope| {
            for prompt in ["left", "right"] {
                let agent = agent.clone();
                let start_barrier = Arc::clone(&start_barrier);
                let claim_barrier = Arc::clone(&claim_barrier);
                let outcomes = Arc::clone(&outcomes);
                scope.spawn(move || {
                    start_barrier.wait();
                    let run = agent.start_prompt(prompt);
                    let outcome = match &run {
                        Ok(_) => "owner",
                        Err(CoreError::ActiveRun { .. }) => "busy",
                        Err(error) => panic!("unexpected concurrent start error: {error}"),
                    };
                    outcomes.lock().expect("test outcomes mutex").push(outcome);
                    // Keep the winning handle alive until both competing
                    // claims have completed, rather than letting a fast drop
                    // create a new legal idle boundary for the other call.
                    claim_barrier.wait();
                    drop(run);
                });
            }
            start_barrier.wait();
            claim_barrier.wait();
        });
        let outcomes = outcomes.lock().expect("test outcomes mutex");
        assert_eq!(
            outcomes
                .iter()
                .filter(|&&outcome| outcome == "owner")
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|&&outcome| outcome == "busy")
                .count(),
            1
        );
        assert_eq!(agent.snapshot().phase, AgentPhase::Idle);
    }
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
    let agent = Agent::builder()
        .host_message(SerializedJson::new("retained host value"))
        .build();
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
    assert!(snapshot.host_messages.is_empty());
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
                    usage: None,
                    added_tool_names: Vec::new(),
                    terminate: false,
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
