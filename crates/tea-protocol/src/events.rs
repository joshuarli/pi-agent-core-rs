//! Observable protocol events.
//!
//! Events are facts emitted by the Rust kernel and consumed by transports,
//! traces, UIs, or policy adapters.  They do not carry executor handles and do
//! not provide a subscription mechanism.  [`EventEnvelope`] supplies the
//! monotonic sequence needed to order events within one run; the producer owns
//! assignment and this crate intentionally does not guess how concurrent work
//! should be sequenced.

use crate::error::ProtocolError;
use crate::model_stream::ModelStreamItem;
use crate::types::{Message, ModelResponseId, RunId, TimestampMillis, TokenUsage, ToolCallId};

/// Stable category for an [`Event`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EventKind {
    /// A run entered its active lifecycle.
    RunStarted,
    /// A model response stream item was emitted.
    ModelStream,
    /// A durable message was created or updated.
    Message,
    /// A tool invocation lifecycle event was emitted.
    Tool,
    /// A run reached a terminal outcome.
    RunFinished,
    /// A protocol-safe error was emitted.
    Error,
}

/// Terminal outcome of an agent run.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RunOutcome {
    /// The run completed according to its stop policy.
    Completed,
    /// The run failed.
    Failed,
    /// The run was cancelled by its owner.
    Cancelled,
}

/// A normalized fact emitted while an agent run is active.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The run has acquired ownership of its request and may emit work.
    RunStarted,
    /// A normalized model stream item.
    ModelStream {
        /// Response stream associated with this item.
        response_id: ModelResponseId,
        /// Provider-neutral stream item.  Ordering is validated separately by
        /// [`crate::model_stream::ModelStream`].
        item: ModelStreamItem,
    },
    /// A message became durable or changed in the run history.
    Message {
        /// Message snapshot after this event.
        message: Message,
    },
    /// A tool invocation was opened.
    ToolStarted {
        /// Invocation identity.
        call_id: ToolCallId,
        /// Registered tool name.
        name: String,
    },
    /// A tool invocation emitted an incremental update.
    ToolDelta {
        /// Invocation identity.
        call_id: ToolCallId,
        /// Adapter-neutral textual update.
        delta: String,
    },
    /// A tool invocation settled.
    ToolFinished {
        /// Invocation identity.
        call_id: ToolCallId,
        /// Result message, if the tool produced one.
        result: Option<Message>,
    },
    /// The run reached a terminal outcome.
    RunFinished {
        /// Terminal outcome.
        outcome: RunOutcome,
        /// Aggregate usage, if available.
        usage: Option<TokenUsage>,
    },
    /// A protocol error was made observable.
    Error {
        /// Structured error safe to cross an adapter boundary.
        error: ProtocolError,
    },
}

impl Event {
    /// Return the stable event category.
    pub const fn kind(&self) -> EventKind {
        match self {
            Self::RunStarted => EventKind::RunStarted,
            Self::ModelStream { .. } => EventKind::ModelStream,
            Self::Message { .. } => EventKind::Message,
            Self::ToolStarted { .. } | Self::ToolDelta { .. } | Self::ToolFinished { .. } => {
                EventKind::Tool
            }
            Self::RunFinished { .. } => EventKind::RunFinished,
            Self::Error { .. } => EventKind::Error,
        }
    }

    /// Whether this event settles the run itself.
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::RunFinished { .. })
    }
}

/// An event plus the run and producer-assigned ordering metadata.
#[derive(Clone, Debug, PartialEq)]
pub struct EventEnvelope {
    /// Run that owns this event.
    pub run_id: RunId,
    /// Monotonic sequence assigned by the run producer.
    pub sequence: u64,
    /// Optional wall-clock timestamp supplied by the producer.
    pub timestamp: Option<TimestampMillis>,
    /// Event fact.
    pub event: Event,
}

impl EventEnvelope {
    /// Wrap an event without making a wall-clock claim.
    pub fn new(run_id: RunId, sequence: u64, event: Event) -> Self {
        Self {
            run_id,
            sequence,
            timestamp: None,
            event,
        }
    }

    /// Attach a producer timestamp.
    pub const fn with_timestamp(mut self, timestamp: TimestampMillis) -> Self {
        self.timestamp = Some(timestamp);
        self
    }

    /// Return this envelope's event category.
    pub const fn kind(&self) -> EventKind {
        self.event.kind()
    }

    /// Whether this envelope's event settles its run.
    pub const fn is_terminal(&self) -> bool {
        self.event.is_terminal()
    }
}

#[cfg(test)]
mod tests {
    use super::{Event, EventEnvelope, EventKind};
    use crate::types::{RunId, TokenUsage};

    #[test]
    fn envelope_delegates_kind_and_terminal_state() {
        let run_id = RunId::try_from("run").expect("test ID is non-empty");
        let envelope = EventEnvelope::new(
            run_id,
            1,
            Event::RunFinished {
                outcome: super::RunOutcome::Completed,
                usage: Some(TokenUsage::default()),
            },
        );

        assert_eq!(envelope.kind(), EventKind::RunFinished);
        assert!(envelope.is_terminal());
    }
}
