//! Optional adapter from core lifecycle events to the compact trace contract.
//!
//! Enable the `trace` feature to use [`TraceObserver`].  The adapter consumes
//! events after the core reducer has applied them and writes one linear
//! episode to a caller-owned [`pi_agent_trace::TraceSink`].  It does not own a
//! clock, executor, task, or runtime state transition.
//!
//! Trace sinks are wrapped in [`pi_agent_trace::IsolatedSink`].  A sink failure
//! is therefore observable through [`TraceObserver::failed_events`] but cannot
//! change the agent result.  Callers that need redaction should wrap their
//! sink in [`pi_agent_trace::RedactingSink`] before passing it here.

use crate::event::{AgentEvent, AgentEventKind, EventObserver, ObserverFuture};
use crate::scheduler::CancellationToken;
use crate::state::{Message, MessageId, StopReason, ToolCallId};
use pi_agent_trace::{
    EndReason, EpisodeEnd, EpisodeHeader, IsolatedSink, Tool, TraceEvent, TraceSink, Turn,
};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// An awaited core observer that records a compact linear trace episode.
///
/// One observer may be attached to an agent and reused for multiple runs.  A
/// new [`AgentEventKind::AgentStart`] starts a new episode in the supplied
/// sink.  The episode identifier is supplied by the host because the core
/// does not own session or persistence identity.
pub struct TraceObserver<S> {
    episode_id: String,
    state: Mutex<TraceState<S>>,
}

struct TraceState<S> {
    sink: IsolatedSink<S>,
    current_turn: Option<PendingTurn>,
    pending_tools: BTreeMap<ToolCallId, Tool>,
    end_reason: EndReason,
    error: Option<String>,
}

struct PendingTurn {
    index: u32,
    input: String,
    output: Option<String>,
    last_input_message: Option<MessageId>,
}

impl<S: TraceSink> TraceObserver<S> {
    /// Creates an observer writing to `sink` under the host-assigned episode ID.
    pub fn new(episode_id: impl Into<String>, sink: S) -> Self {
        Self {
            episode_id: episode_id.into(),
            state: Mutex::new(TraceState {
                sink: IsolatedSink::new(sink),
                current_turn: None,
                pending_tools: BTreeMap::new(),
                end_reason: EndReason::Completed,
                error: None,
            }),
        }
    }

    /// Number of events rejected by the wrapped sink.
    pub fn failed_events(&self) -> u64 {
        self.state
            .lock()
            .expect("trace observer mutex poisoned")
            .sink
            .failed_events()
    }

    /// Inspect the caller-owned sink without exposing the adapter's state.
    ///
    /// The callback runs while the observer lock is held and should not call
    /// back into the agent.
    pub fn with_sink<R>(&self, inspect: impl FnOnce(&S) -> R) -> R {
        let state = self.state.lock().expect("trace observer mutex poisoned");
        inspect(state.sink.inner())
    }
}

impl<S: TraceSink + Send + 'static> EventObserver for TraceObserver<S> {
    fn observe<'a>(
        &'a self,
        event: &'a AgentEvent,
        _cancellation: CancellationToken,
    ) -> ObserverFuture<'a> {
        self.record(event);
        Box::pin(std::future::ready(Ok(())))
    }
}

impl<S: TraceSink> TraceObserver<S> {
    fn record(&self, event: &AgentEvent) {
        let mut state = self.state.lock().expect("trace observer mutex poisoned");
        match &event.kind {
            AgentEventKind::AgentStart => {
                state.current_turn = None;
                state.pending_tools.clear();
                state.end_reason = EndReason::Completed;
                state.error = None;
                state.sink.append(TraceEvent::from(EpisodeHeader::new(
                    self.episode_id.clone(),
                )));
            }
            AgentEventKind::TurnStart { turn_id } => {
                state.current_turn = Some(PendingTurn {
                    index: trace_turn_index(turn_id.0),
                    input: String::new(),
                    output: None,
                    last_input_message: None,
                });
            }
            AgentEventKind::MessageStart { message }
            | AgentEventKind::MessageUpdate { message, .. }
            | AgentEventKind::MessageEnd { message } => {
                if let Message::Assistant { error_message, .. } = message {
                    state.error = error_message.clone();
                }
                record_message(&mut state.current_turn, message);
            }
            AgentEventKind::ToolExecutionStart {
                tool_call_id,
                tool_name,
            } => {
                let turn_index = state.current_turn.as_ref().map_or(0, |turn| turn.index);
                state.pending_tools.insert(
                    tool_call_id.clone(),
                    Tool::new(turn_index, tool_call_id.to_string(), tool_name.clone(), ""),
                );
            }
            AgentEventKind::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result,
            } => {
                let turn_index = state.current_turn.as_ref().map_or(0, |turn| turn.index);
                let tool = state.pending_tools.remove(tool_call_id).unwrap_or_else(|| {
                    Tool::new(turn_index, tool_call_id.to_string(), tool_name.clone(), "")
                });
                let tool = if result.is_error {
                    tool.with_error(result.content.clone())
                } else {
                    tool.with_output(result.content.clone())
                };
                state.sink.append(TraceEvent::from(tool));
            }
            AgentEventKind::ToolExecutionUpdate { .. } => {
                // The compact V0 trace stores the settled tool record only.
                // Streaming updates remain available through core observers.
            }
            AgentEventKind::TurnEnd { turn_id, reason } => {
                let pending = state.current_turn.take().unwrap_or(PendingTurn {
                    index: trace_turn_index(turn_id.0),
                    input: String::new(),
                    output: None,
                    last_input_message: None,
                });
                let mut turn = Turn::new(pending.index, pending.input)
                    .with_stop_reason(stop_reason_name(*reason));
                if let Some(output) = pending.output {
                    turn = turn.with_output(output);
                }
                state.sink.append(TraceEvent::from(turn));
                state.end_reason = end_reason(*reason);
            }
            AgentEventKind::AgentEnd { .. } => {
                let reason = state.end_reason.clone();
                let error = state.error.clone();
                state.sink.append(TraceEvent::from(EpisodeEnd {
                    reason,
                    error,
                    finished_at_ms: None,
                }));
            }
        }
    }
}

fn record_message(turn: &mut Option<PendingTurn>, message: &Message) {
    let Some(turn) = turn.as_mut() else {
        return;
    };
    match message {
        Message::User { id, content } if turn.last_input_message != Some(*id) => {
            if !turn.input.is_empty() {
                turn.input.push('\n');
            }
            turn.input.push_str(content);
            turn.last_input_message = Some(*id);
        }
        Message::User { .. } => {}
        Message::Assistant { content, .. } => {
            turn.output = Some(content.clone());
        }
        Message::ToolResult { .. } => {}
    }
}

fn trace_turn_index(turn_id: u64) -> u32 {
    turn_id.saturating_sub(1).min(u32::MAX as u64) as u32
}

fn stop_reason_name(reason: StopReason) -> &'static str {
    match reason {
        StopReason::EndTurn => "end_turn",
        StopReason::ToolUse => "tool_use",
        StopReason::Length => "length",
        StopReason::Aborted => "aborted",
        StopReason::Cancelled => "cancelled",
        StopReason::Error => "error",
    }
}

fn end_reason(reason: StopReason) -> EndReason {
    match reason {
        StopReason::EndTurn | StopReason::ToolUse | StopReason::Length => EndReason::Completed,
        StopReason::Aborted => EndReason::Aborted,
        StopReason::Cancelled => EndReason::Cancelled,
        StopReason::Error => EndReason::Failed,
    }
}

impl<S: TraceSink> std::fmt::Debug for TraceObserver<S> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TraceObserver")
            .field("episode_id", &self.episode_id)
            .field("failed_events", &self.failed_events())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::AgentEvent;
    use crate::state::{AssistantToolCall, MessageId, RunId, TurnId};
    use crate::tool::{ToolResult, ToolUpdate};

    fn observe<S: TraceSink + Send + 'static>(observer: &TraceObserver<S>, kind: AgentEventKind) {
        let event = AgentEvent {
            run_id: RunId(1),
            sequence: crate::event::EventSequence(1),
            kind,
        };
        smol::block_on(observer.observe(&event, CancellationToken::new()))
            .expect("trace observer is best effort");
    }

    #[test]
    fn maps_lifecycle_events_to_one_linear_episode() {
        let observer = TraceObserver::new("episode-1", Vec::<TraceEvent>::new());
        let call_id = ToolCallId::new("call-1").expect("fixed ID");
        observe(&observer, AgentEventKind::AgentStart);
        observe(&observer, AgentEventKind::TurnStart { turn_id: TurnId(1) });
        observe(
            &observer,
            AgentEventKind::MessageStart {
                message: Message::User {
                    id: MessageId(1),
                    content: "hello".into(),
                },
            },
        );
        observe(
            &observer,
            AgentEventKind::MessageEnd {
                message: Message::User {
                    id: MessageId(1),
                    content: "hello".into(),
                },
            },
        );
        observe(
            &observer,
            AgentEventKind::MessageEnd {
                message: Message::Assistant {
                    id: MessageId(2),
                    content: "world".into(),
                    tool_calls: vec![AssistantToolCall {
                        id: call_id.clone(),
                        name: "echo".into(),
                        arguments: crate::state::SerializedJson::new("{}"),
                    }],
                    stop_reason: Some(StopReason::ToolUse),
                    error_message: None,
                },
            },
        );
        observe(
            &observer,
            AgentEventKind::ToolExecutionStart {
                tool_call_id: call_id.clone(),
                tool_name: "echo".into(),
            },
        );
        observe(
            &observer,
            AgentEventKind::ToolExecutionUpdate {
                tool_call_id: call_id.clone(),
                tool_name: "echo".into(),
                update: ToolUpdate {
                    content: "partial".into(),
                    details: Some(crate::state::SerializedJson::new("null")),
                },
            },
        );
        observe(
            &observer,
            AgentEventKind::ToolExecutionEnd {
                tool_call_id: call_id,
                tool_name: "echo".into(),
                result: ToolResult {
                    tool_call_id: ToolCallId::new("call-1").expect("fixed ID"),
                    content: "result".into(),
                    details: None,
                    usage: None,
                    added_tool_names: Vec::new(),
                    terminate: false,
                    is_error: false,
                },
            },
        );
        observe(
            &observer,
            AgentEventKind::TurnEnd {
                turn_id: TurnId(1),
                reason: StopReason::ToolUse,
            },
        );
        observe(&observer, AgentEventKind::AgentEnd { messages: vec![] });

        observer.with_sink(|events| {
            assert_eq!(events.len(), 4);
            assert!(matches!(events[0], TraceEvent::EpisodeHeader(_)));
            assert!(matches!(events[1], TraceEvent::Tool(_)));
            assert!(matches!(events[2], TraceEvent::Turn(_)));
            assert!(matches!(events[3], TraceEvent::EpisodeEnd(_)));
            let TraceEvent::Tool(tool) = &events[1] else {
                unreachable!()
            };
            assert_eq!(tool.output.as_deref(), Some("result"));
            let TraceEvent::Turn(turn) = &events[2] else {
                unreachable!()
            };
            assert_eq!(turn.input, "hello");
            assert_eq!(turn.output.as_deref(), Some("world"));
        });
    }

    #[test]
    fn sink_failures_are_isolated_from_event_observation() {
        struct FailingSink;
        impl TraceSink for FailingSink {
            type Error = ();

            fn append(&mut self, _event: TraceEvent) -> Result<(), Self::Error> {
                Err(())
            }
        }

        let observer = TraceObserver::new("episode-1", FailingSink);
        observe(&observer, AgentEventKind::AgentStart);
        observe(&observer, AgentEventKind::AgentEnd { messages: vec![] });
        assert_eq!(observer.failed_events(), 2);
    }
}
