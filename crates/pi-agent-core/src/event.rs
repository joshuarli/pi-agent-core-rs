//! Lifecycle events and awaited observer boundaries.
//!
//! Event construction is intentionally separate from state mutation.  The run loop must first
//! settle state, then emit the corresponding event in this order:
//! `agent_start → turn_start → message* / tool_execution* → turn_end → agent_end`.

use crate::error::CoreError;
use crate::scheduler::CancellationToken;
use crate::state::{Message, RunId, SerializedJson, StopReason, ToolCallId, TurnId};
use crate::tool::{ToolResult, ToolUpdate};
use std::future::Future;
use std::pin::Pin;

/// Monotonic sequence assigned by one run.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct EventSequence(pub u64);

/// An event envelope with stable run identity and local ordering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentEvent {
    /// Run that emitted the event.
    pub run_id: RunId,
    /// Sequence within that run.
    pub sequence: EventSequence,
    /// Event payload.
    pub kind: AgentEventKind,
}

/// Meaningful lifecycle payloads emitted by the core.
#[allow(missing_docs)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentEventKind {
    /// Run ownership began.
    AgentStart,
    /// A model turn began.
    TurnStart { turn_id: TurnId },
    /// A message became visible.
    MessageStart { message: Message },
    /// A partial message update.
    ///
    /// `text_delta` is the provider event payload, while `message` is the
    /// reduced assistant snapshot after that delta. Keeping both prevents an
    /// observer from having to diff snapshots (which is wrong for repeated
    /// text, thinking, or future interleaved content blocks).
    MessageUpdate {
        /// Reduced message snapshot after this update.
        message: Message,
        /// Exact text fragment delivered by the current V0 stream event.
        text_delta: Option<String>,
    },
    /// A message settled.
    MessageEnd { message: Message },
    /// Tool execution began.
    ToolExecutionStart {
        tool_call_id: ToolCallId,
        tool_name: String,
        /// Exact serialized JSON arguments supplied by the model.
        ///
        /// This is emitted before validation, hooks, or capability dispatch.
        /// Hosts that persist or forward events must apply their redaction
        /// policy before crossing their own trace boundary.
        arguments: SerializedJson,
    },
    /// Tool emitted a partial update.
    ToolExecutionUpdate {
        /// Call that produced the partial update.
        tool_call_id: ToolCallId,
        /// Stable tool name for the update's executable capability.
        tool_name: String,
        /// Partial result supplied by the tool.
        update: ToolUpdate,
    },
    /// Tool execution settled.
    ToolExecutionEnd {
        tool_call_id: ToolCallId,
        tool_name: String,
        result: ToolResult,
    },
    /// A model turn settled.
    TurnEnd { turn_id: TurnId, reason: StopReason },
    /// The loop emitted its final event. Awaited observers may still keep the
    /// agent active before terminal settlement makes it idle.
    AgentEnd { messages: Vec<Message> },
}

/// A boxed observer future that is settled before the run advances.
pub type ObserverFuture<'a> = Pin<Box<dyn Future<Output = Result<(), CoreError>> + Send + 'a>>;

/// An awaited lifecycle observer.
///
/// Observers see state after the event reducer has applied the event. Their
/// futures are awaited in registration order, including for `AgentEnd`, so an
/// unfinished terminal observer keeps the run active. The bounded,
/// non-blocking [`crate::EventSubscription`] returned by
/// [`crate::Agent::subscribe_nonblocking`] is a separate lossy channel
/// contract and must never be silently substituted for this one.
pub trait EventObserver: Send + Sync {
    /// Observe one reduced event using the run's cancellation scope.
    fn observe<'a>(
        &'a self,
        event: &'a AgentEvent,
        cancellation: CancellationToken,
    ) -> ObserverFuture<'a>;
}

/// A bounded, owned event collection useful for deterministic providers and tests.
#[derive(Clone, Debug, Default)]
pub struct EventLog {
    events: Vec<AgentEvent>,
}

impl EventLog {
    /// Append one event in sequence order.
    pub fn push(&mut self, event: AgentEvent) {
        self.events.push(event);
    }

    /// Borrow the ordered event view.
    pub fn as_slice(&self) -> &[AgentEvent] {
        &self.events
    }

    /// Consume the log into owned events.
    pub fn into_events(self) -> Vec<AgentEvent> {
        self.events
    }
}
