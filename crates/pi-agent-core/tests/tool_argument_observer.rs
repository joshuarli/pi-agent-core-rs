//! The pre-dispatch tool-start event is the auditable argument boundary.

use pi_agent_core::event::{AgentEvent, AgentEventKind, EventObserver, ObserverFuture};
use pi_agent_core::scheduler::{
    CancellationToken, ModelFuture, ModelProvider, ModelRequest, ModelStream, ModelStreamEvent,
};
use pi_agent_core::state::{AgentPhase, AssistantToolCall, SerializedJson, StopReason, ToolCallId};
use pi_agent_core::tool::{
    AgentTool, ToolCall, ToolContext, ToolFuture, ToolResult, ToolUpdateSink,
};
use pi_agent_core::Agent;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

struct ScriptedProvider {
    streams: Mutex<VecDeque<ModelStream>>,
}

impl ScriptedProvider {
    fn new(streams: impl IntoIterator<Item = ModelStream>) -> Self {
        Self {
            streams: Mutex::new(streams.into_iter().collect()),
        }
    }
}

impl ModelProvider for ScriptedProvider {
    fn stream<'a>(
        &'a self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelFuture<'a> {
        let stream = self
            .streams
            .lock()
            .expect("provider stream mutex")
            .pop_front()
            .expect("fixture supplied enough model streams");
        Box::pin(std::future::ready(Ok(Box::new(stream) as _)))
    }
}

struct RecordingObserver {
    records: Arc<Mutex<Vec<String>>>,
}

impl EventObserver for RecordingObserver {
    fn observe<'a>(
        &'a self,
        event: &'a AgentEvent,
        _cancellation: CancellationToken,
    ) -> ObserverFuture<'a> {
        let record = match &event.kind {
            AgentEventKind::ToolExecutionStart {
                tool_call_id,
                tool_name,
                arguments,
            } => Some(format!(
                "start:{tool_call_id}:{tool_name}:{}",
                arguments.as_str()
            )),
            AgentEventKind::ToolExecutionEnd { tool_call_id, .. } => {
                Some(format!("end:{tool_call_id}"))
            }
            _ => None,
        };
        if let Some(record) = record {
            self.records
                .lock()
                .expect("observer record mutex")
                .push(record);
        }
        Box::pin(std::future::ready(Ok(())))
    }
}

struct RecordingTool {
    records: Arc<Mutex<Vec<String>>>,
}

impl AgentTool for RecordingTool {
    fn name(&self) -> &str {
        "audit"
    }

    fn description(&self) -> &str {
        "Record one auditable call."
    }

    fn schema(&self) -> &pi_agent_protocol::JsonValue {
        static SCHEMA: std::sync::OnceLock<pi_agent_protocol::JsonValue> =
            std::sync::OnceLock::new();
        SCHEMA.get_or_init(|| {
            pi_agent_protocol::JsonValue::parse(r#"{"type":"object"}"#)
                .expect("fixture schema is valid JSON")
        })
    }

    fn execute<'a>(
        &'a self,
        call: ToolCall,
        _context: ToolContext,
        _updates: ToolUpdateSink,
    ) -> ToolFuture<'a> {
        self.records
            .lock()
            .expect("tool record mutex")
            .push(format!("execute:{}:{}", call.id, call.arguments.as_str()));
        Box::pin(std::future::ready(Ok(ToolResult {
            tool_call_id: call.id,
            content: "ok".into(),
            details: None,
            usage: None,
            added_tool_names: Vec::new(),
            terminate: false,
            is_error: false,
            failure: None,
        })))
    }
}

#[test]
fn tool_start_observer_receives_exact_arguments_before_dispatch_and_settlement() {
    smol::block_on(async {
        let records = Arc::new(Mutex::new(Vec::new()));
        let provider = Arc::new(ScriptedProvider::new([
            ModelStream {
                events: vec![
                    ModelStreamEvent::ToolCall(AssistantToolCall {
                        id: ToolCallId::new("call-audit").expect("non-empty call ID"),
                        name: "audit".into(),
                        arguments: SerializedJson::new(r#"{"secret":"value"}"#),
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
        let agent = Agent::builder()
            .model_provider(provider)
            .tool(Arc::new(RecordingTool {
                records: Arc::clone(&records),
            }))
            .build();
        let _subscription = agent.subscribe(Arc::new(RecordingObserver {
            records: Arc::clone(&records),
        }));

        let run = agent.start_prompt("audit this").expect("start run");
        run.drive().await.expect("scripted run succeeds");

        assert_eq!(
            records.lock().expect("record mutex").as_slice(),
            [
                "start:call-audit:audit:{\"secret\":\"value\"}",
                "execute:call-audit:{\"secret\":\"value\"}",
                "end:call-audit",
            ]
        );
        let start = run
            .events()
            .into_iter()
            .find_map(|event| match event.kind {
                AgentEventKind::ToolExecutionStart { arguments, .. } => Some(arguments),
                _ => None,
            })
            .expect("tool-start event");
        assert_eq!(start.as_str(), r#"{"secret":"value"}"#);
        assert_eq!(
            run.snapshot().phase,
            pi_agent_core::state::RunPhase::Succeeded
        );
        assert_eq!(agent.snapshot().phase, AgentPhase::Idle);
        assert!(run
            .events()
            .last()
            .is_some_and(|event| matches!(event.kind, AgentEventKind::AgentEnd { .. })));
    });
}
