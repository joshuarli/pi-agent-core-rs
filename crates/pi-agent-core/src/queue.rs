//! Explicit steering and follow-up queues.
//!
//! These are intentionally two named queues rather than a generic mailbox.  The run loop
//! decides when each queue may drain: steering is considered at the pinned active-turn wait
//! points, while follow-up is considered only when a run would otherwise become idle.

use std::collections::VecDeque;

/// Input waiting to be consumed by a run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueuedMessage {
    /// Monotonic sequence local to its queue.
    pub sequence: u64,
    /// User content to inject into the next eligible turn.
    pub content: String,
}

/// How a queue is drained at its eligible wait point.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum QueueMode {
    /// Drain one message, then return control to the turn loop.
    #[default]
    OneAtATime,
    /// Drain all currently available messages in insertion order.
    All,
}

#[derive(Clone, Debug, Default)]
struct MessageQueue {
    next_sequence: u64,
    entries: VecDeque<QueuedMessage>,
}

impl MessageQueue {
    fn push(&mut self, content: impl Into<String>) -> u64 {
        self.next_sequence = self.next_sequence.saturating_add(1);
        let sequence = self.next_sequence;
        self.entries.push_back(QueuedMessage {
            sequence,
            content: content.into(),
        });
        sequence
    }

    fn pop(&mut self) -> Option<QueuedMessage> {
        self.entries.pop_front()
    }

    fn drain(&mut self, mode: QueueMode) -> Vec<QueuedMessage> {
        match mode {
            QueueMode::OneAtATime => self.pop().into_iter().collect(),
            QueueMode::All => self.entries.drain(..).collect(),
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
    }

    fn snapshot(&self) -> Vec<QueuedMessage> {
        self.entries.iter().cloned().collect()
    }
}

/// Messages that should steer an active turn at the next permitted point.
#[derive(Clone, Debug, Default)]
pub struct SteeringQueue(MessageQueue);

impl SteeringQueue {
    /// Queue one steering message and return its queue-local sequence.
    pub fn push(&mut self, content: impl Into<String>) -> u64 {
        self.0.push(content)
    }

    /// Remove one message in insertion order.
    pub fn pop(&mut self) -> Option<QueuedMessage> {
        self.0.pop()
    }

    /// Drain according to the pinned queue mode.
    pub fn drain(&mut self, mode: QueueMode) -> Vec<QueuedMessage> {
        self.0.drain(mode)
    }

    /// Remove queued messages, normally as part of cancellation settlement.
    pub fn clear(&mut self) {
        self.0.clear()
    }

    /// Return an owned inspection view.
    pub fn snapshot(&self) -> Vec<QueuedMessage> {
        self.0.snapshot()
    }

    /// Whether no steering input is waiting.
    pub fn is_empty(&self) -> bool {
        self.0.entries.is_empty()
    }

    /// Number of waiting steering messages.
    pub fn len(&self) -> usize {
        self.0.entries.len()
    }
}

/// Messages that should be consumed only after a run would otherwise become idle.
#[derive(Clone, Debug, Default)]
pub struct FollowUpQueue(MessageQueue);

impl FollowUpQueue {
    /// Queue one follow-up message and return its queue-local sequence.
    pub fn push(&mut self, content: impl Into<String>) -> u64 {
        self.0.push(content)
    }

    /// Remove one message in insertion order.
    pub fn pop(&mut self) -> Option<QueuedMessage> {
        self.0.pop()
    }

    /// Drain according to the pinned queue mode.
    pub fn drain(&mut self, mode: QueueMode) -> Vec<QueuedMessage> {
        self.0.drain(mode)
    }

    /// Remove queued messages, normally as part of cancellation settlement.
    pub fn clear(&mut self) {
        self.0.clear()
    }

    /// Return an owned inspection view.
    pub fn snapshot(&self) -> Vec<QueuedMessage> {
        self.0.snapshot()
    }

    /// Whether no follow-up input is waiting.
    pub fn is_empty(&self) -> bool {
        self.0.entries.is_empty()
    }

    /// Number of waiting follow-up messages.
    pub fn len(&self) -> usize {
        self.0.entries.len()
    }
}

/// The two queues owned by an agent.
#[derive(Clone, Debug, Default)]
pub struct AgentQueues {
    /// Input considered while a run remains active.
    pub steering: SteeringQueue,
    /// Input considered at the idle boundary.
    pub follow_up: FollowUpQueue,
}

impl AgentQueues {
    /// Clear both queues as part of cancellation settlement.
    pub fn clear(&mut self) {
        self.steering.clear();
        self.follow_up.clear();
    }
}
