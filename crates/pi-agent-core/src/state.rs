//! Canonical agent and run state.
//!
//! State is split into durable conversation data and transient execution data.  A terminal
//! run must clear `partial_response` and `pending_tool_calls` before the owning agent returns
//! to [`AgentPhase::Idle`].  Snapshots are owned values, so observers cannot mutate the state
//! machine through a borrowed view.

use std::collections::BTreeSet;
use std::fmt;

/// A stable identifier for an agent conversation message.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct MessageId(pub u64);

/// A stable identifier for one agent execution.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct RunId(pub u64);

/// A stable identifier for one model turn.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct TurnId(pub u64);

/// A provider-supplied identifier for one assistant-requested tool invocation.
///
/// Unlike runtime-generated run and message counters, this remains textual so
/// Pi/provider call identifiers survive model context and result correlation
/// unchanged. An empty ID is rejected at the model boundary.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct ToolCallId(String);

impl ToolCallId {
    /// Construct a non-empty provider tool-call identifier.
    pub fn new(value: impl Into<String>) -> Result<Self, ToolCallIdError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ToolCallIdError);
        }
        Ok(Self(value))
    }

    /// Borrow the provider's exact tool-call identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ToolCallId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Error returned when a provider omits the required tool-call identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ToolCallIdError;

impl fmt::Display for ToolCallIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("tool-call ID cannot be empty")
    }
}

impl std::error::Error for ToolCallIdError {}

/// The model's reasoning budget selection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ThinkingLevel {
    /// Ask the provider for its default reasoning behavior.
    #[default]
    Default,
    /// Disable additional reasoning where the provider supports it.
    Off,
    /// Request a low reasoning budget.
    Low,
    /// Request a medium reasoning budget.
    Medium,
    /// Request a high reasoning budget.
    High,
}

/// Provider-independent model identity.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModelDescriptor {
    /// Provider name chosen by the host.
    pub provider: String,
    /// Provider model name.
    pub model: String,
    /// Optional revision or snapshot pin.
    pub revision: Option<String>,
}

/// A serialized JSON value at an integration boundary.
///
/// The core preserves this exact text in state and validates it only at the tool invocation
/// boundary. Provider and transport adapters may use the stable `pi_agent_protocol::JsonValue`
/// representation without changing the state-machine contract.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SerializedJson(pub String);

impl SerializedJson {
    /// Construct a serialized JSON boundary value without implying validation.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the serialized representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A message retained in the canonical conversation history.
#[allow(missing_docs)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Message {
    /// Host-provided user input.
    User { id: MessageId, content: String },
    /// Provider response, including any textual partial/final content.
    Assistant {
        id: MessageId,
        content: String,
        tool_calls: Vec<AssistantToolCall>,
    },
    /// Result injected after a tool invocation.
    ToolResult {
        id: MessageId,
        tool_call_id: ToolCallId,
        tool_name: String,
        content: String,
        is_error: bool,
    },
}

/// A tool call embedded in an assistant message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssistantToolCall {
    /// Stable call identifier.
    pub id: ToolCallId,
    /// Registered tool name.
    pub name: String,
    /// Serialized JSON arguments.
    pub arguments: SerializedJson,
}

/// Provider usage counters.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Usage {
    /// Input tokens, when reported by the provider.
    pub input_tokens: Option<u64>,
    /// Output tokens, when reported by the provider.
    pub output_tokens: Option<u64>,
    /// Reasoning tokens, when reported by the provider.
    pub reasoning_tokens: Option<u64>,
}

/// Why model generation stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StopReason {
    /// The provider produced a normal final response.
    EndTurn,
    /// The provider requested tool execution.
    ToolUse,
    /// The host cancelled the run.
    Cancelled,
    /// The provider or host failed.
    Error,
}

/// The externally observable agent ownership phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentPhase {
    /// No run currently owns the agent.
    Idle,
    /// A run is processing model/tool work.
    Running(RunId),
    /// Cancellation was requested and settlement is pending.
    Cancelling(RunId),
}

/// The run lifecycle.  Terminal variants are immutable outcomes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunPhase {
    /// The run handle exists but has not emitted its first turn event.
    Created,
    /// The run is processing a turn.
    Running,
    /// The run has stopped accepting work and is settling observers.
    Settling,
    /// The run completed normally.
    Succeeded,
    /// The run completed with a runtime error.
    Failed,
    /// The run completed because cancellation won settlement.
    Cancelled,
}

impl RunPhase {
    /// Whether no further run transition is legal.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

/// Mutable state owned by an [`Agent`](crate::Agent).
#[derive(Clone, Debug)]
pub struct AgentState {
    /// Static system instructions.
    pub system_prompt: String,
    /// Selected model identity.
    pub model: Option<ModelDescriptor>,
    /// Selected reasoning level.
    pub thinking_level: ThinkingLevel,
    /// Canonical conversation history.
    pub messages: Vec<Message>,
    /// Current ownership phase.
    pub phase: AgentPhase,
    /// Partial assistant content while a model stream is active.
    pub partial_response: Option<String>,
    /// Whether a provider stream is currently open.
    pub is_streaming: bool,
    /// Tool calls awaiting preparation or execution.
    pub pending_tool_calls: BTreeSet<ToolCallId>,
    /// Last runtime error, retained for state inspection.
    pub last_error: Option<String>,
    next_message_id: u64,
}

impl Default for AgentState {
    fn default() -> Self {
        Self {
            system_prompt: String::new(),
            model: None,
            thinking_level: ThinkingLevel::Default,
            messages: Vec::new(),
            phase: AgentPhase::Idle,
            partial_response: None,
            is_streaming: false,
            pending_tool_calls: BTreeSet::new(),
            last_error: None,
            next_message_id: 1,
        }
    }
}

impl AgentState {
    /// Allocate the next message identifier.
    pub(crate) fn allocate_message_id(&mut self) -> MessageId {
        let id = MessageId(self.next_message_id);
        self.next_message_id = self.next_message_id.saturating_add(1);
        id
    }

    /// Produce an owned inspection snapshot.
    pub(crate) fn snapshot(&self) -> AgentSnapshot {
        AgentSnapshot {
            system_prompt: self.system_prompt.clone(),
            model: self.model.clone(),
            thinking_level: self.thinking_level,
            messages: self.messages.clone(),
            phase: self.phase,
            partial_response: self.partial_response.clone(),
            is_streaming: self.is_streaming,
            pending_tool_calls: self.pending_tool_calls.clone(),
            last_error: self.last_error.clone(),
        }
    }
}

/// Owned, read-only view of agent state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentSnapshot {
    /// Static system instructions.
    pub system_prompt: String,
    /// Selected model identity.
    pub model: Option<ModelDescriptor>,
    /// Selected reasoning level.
    pub thinking_level: ThinkingLevel,
    /// Canonical conversation history.
    pub messages: Vec<Message>,
    /// Current ownership phase.
    pub phase: AgentPhase,
    /// Partial assistant content, if streaming.
    pub partial_response: Option<String>,
    /// Whether a provider stream is open.
    pub is_streaming: bool,
    /// Pending tool calls.
    pub pending_tool_calls: BTreeSet<ToolCallId>,
    /// Last runtime error.
    pub last_error: Option<String>,
}

/// Mutable state retained by one run handle.
#[derive(Clone, Debug)]
pub struct RunState {
    /// Stable run identifier.
    pub id: RunId,
    /// Current lifecycle phase.
    pub phase: RunPhase,
    /// Current turn, if one has started.
    pub turn_id: Option<TurnId>,
    /// Terminal reason, if known.
    pub stop_reason: Option<StopReason>,
    /// Runtime error text, if failed.
    pub error: Option<String>,
    /// Number of events emitted for this run.
    pub event_count: u64,
    /// Lifecycle events emitted in source order.
    pub events: Vec<crate::event::AgentEvent>,
}

impl RunState {
    /// Create a run before its first lifecycle event.
    pub const fn created(id: RunId) -> Self {
        Self {
            id,
            phase: RunPhase::Created,
            turn_id: None,
            stop_reason: None,
            error: None,
            event_count: 0,
            events: Vec::new(),
        }
    }

    /// Produce an owned run snapshot.
    pub fn snapshot(&self) -> RunSnapshot {
        RunSnapshot {
            id: self.id,
            phase: self.phase,
            turn_id: self.turn_id,
            stop_reason: self.stop_reason,
            error: self.error.clone(),
            event_count: self.event_count,
        }
    }
}

/// Owned, read-only view of run state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunSnapshot {
    /// Stable run identifier.
    pub id: RunId,
    /// Current lifecycle phase.
    pub phase: RunPhase,
    /// Current turn identifier.
    pub turn_id: Option<TurnId>,
    /// Terminal reason.
    pub stop_reason: Option<StopReason>,
    /// Runtime error text.
    pub error: Option<String>,
    /// Number of emitted events.
    pub event_count: u64,
}

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "run-{}", self.0)
    }
}
