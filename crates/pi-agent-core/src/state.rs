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
    /// Request the smallest explicit reasoning budget.
    Minimal,
    /// Request a low reasoning budget.
    Low,
    /// Request a medium reasoning budget.
    Medium,
    /// Request a high reasoning budget.
    High,
    /// Request an extra-high reasoning budget.
    XHigh,
    /// Request the maximum reasoning budget.
    Max,
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
        /// Terminal model stop reason, when this is the finalized assistant message.
        /// `None` is used for a partial streaming snapshot.
        stop_reason: Option<StopReason>,
        /// Provider/model diagnostic for an error or aborted response.
        error_message: Option<String>,
    },
    /// Result injected after a tool invocation.
    ToolResult {
        id: MessageId,
        tool_call_id: ToolCallId,
        tool_name: String,
        content: String,
        details: Option<SerializedJson>,
        usage: Option<Usage>,
        added_tool_names: Vec<String>,
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
    /// Input tokens served from a provider cache, when reported.
    pub cache_read_tokens: Option<u64>,
    /// Input tokens written to a provider cache, when reported.
    pub cache_write_tokens: Option<u64>,
    /// Exact provider-reported monetary value for this response, when reported.
    ///
    /// This is retained as the provider's decimal text rather than an `f64`, so a host can
    /// display or add reported prices without binary floating-point rounding. The core never
    /// derives a value from token counts or a pricing table.
    pub cost: Option<String>,
}

impl Usage {
    /// Whether this usage value contains at least one provider-reported field.
    pub fn is_reported(&self) -> bool {
        self.input_tokens.is_some()
            || self.output_tokens.is_some()
            || self.reasoning_tokens.is_some()
            || self.cache_read_tokens.is_some()
            || self.cache_write_tokens.is_some()
            || self.cost.is_some()
    }

    /// Merge a later provider update into this value without turning unknown fields into zero.
    pub fn merge(&mut self, update: Self) {
        if update.input_tokens.is_some() {
            self.input_tokens = update.input_tokens;
        }
        if update.output_tokens.is_some() {
            self.output_tokens = update.output_tokens;
        }
        if update.reasoning_tokens.is_some() {
            self.reasoning_tokens = update.reasoning_tokens;
        }
        if update.cache_read_tokens.is_some() {
            self.cache_read_tokens = update.cache_read_tokens;
        }
        if update.cache_write_tokens.is_some() {
            self.cache_write_tokens = update.cache_write_tokens;
        }
        if update.cost.is_some() {
            self.cost = update.cost;
        }
    }
}

/// Accounting attached to one settled model turn.
///
/// `run_id` and `turn_id` identify the exact response that produced the report. `model` is the
/// provider-independent request identity, which remains available even when a provider's
/// response does not echo its model name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelTurnAccounting {
    /// Run that owns this model turn.
    pub run_id: RunId,
    /// Turn within the run.
    pub turn_id: TurnId,
    /// Model requested for this turn, when configured.
    pub model: Option<ModelDescriptor>,
    /// Provider-reported token and monetary fields.
    pub usage: Usage,
}

/// Retained per-turn and aggregate model accounting for an agent.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModelAccountingSnapshot {
    /// One record for each model turn that supplied a usage update.
    pub turns: Vec<ModelTurnAccounting>,
    /// Field-wise aggregate of known provider reports. A field remains `None` until at least
    /// one turn reports it; reported zero remains `Some(0)`.
    pub aggregate: Usage,
}

impl ModelAccountingSnapshot {
    /// Record one settled model-turn report and update its aggregate view.
    pub(crate) fn record(&mut self, accounting: ModelTurnAccounting) {
        add_usage(
            &mut self.aggregate.input_tokens,
            accounting.usage.input_tokens,
        );
        add_usage(
            &mut self.aggregate.output_tokens,
            accounting.usage.output_tokens,
        );
        add_usage(
            &mut self.aggregate.reasoning_tokens,
            accounting.usage.reasoning_tokens,
        );
        add_usage(
            &mut self.aggregate.cache_read_tokens,
            accounting.usage.cache_read_tokens,
        );
        add_usage(
            &mut self.aggregate.cache_write_tokens,
            accounting.usage.cache_write_tokens,
        );
        if let Some(cost) = accounting.usage.cost.as_deref() {
            self.aggregate.cost = Some(match self.aggregate.cost.as_deref() {
                Some(previous) => decimal_add(previous, cost),
                None => cost.to_owned(),
            });
        }
        self.turns.push(accounting);
    }
}

fn add_usage(total: &mut Option<u64>, value: Option<u64>) {
    if let Some(value) = value {
        *total = Some(total.unwrap_or(0).saturating_add(value));
    }
}

/// Add two non-negative decimal strings without converting through `f64`.
fn decimal_add(lhs: &str, rhs: &str) -> String {
    let (left_digits, left_scale) = decimal_parts(lhs);
    let (right_digits, right_scale) = decimal_parts(rhs);
    let scale = left_scale.max(right_scale);
    let mut left = left_digits;
    let mut right = right_digits;
    left.extend(std::iter::repeat_n('0', scale - left_scale));
    right.extend(std::iter::repeat_n('0', scale - right_scale));
    let mut output = String::new();
    let mut carry = 0u8;
    for (left, right) in left.bytes().rev().zip(right.bytes().rev()) {
        let sum = left - b'0' + right - b'0' + carry;
        output.push(char::from(b'0' + sum % 10));
        carry = sum / 10;
    }
    if carry != 0 {
        output.push(char::from(b'0' + carry));
    }
    let mut output: String = output.chars().rev().collect();
    if scale != 0 {
        if output.len() <= scale {
            let zeros = "0".repeat(scale + 1 - output.len());
            output = format!("{zeros}{output}");
        }
        output.insert(output.len() - scale, '.');
    }
    decimal_normalize(&output)
}

fn decimal_parts(value: &str) -> (String, usize) {
    let (coefficient, exponent) = value
        .split_once(['e', 'E'])
        .map(|(coefficient, exponent)| (coefficient, exponent.parse::<i64>().unwrap_or(0)))
        .unwrap_or((value, 0));
    let (whole, fraction) = coefficient.split_once('.').unwrap_or((coefficient, ""));
    let mut digits = String::with_capacity(whole.len() + fraction.len());
    digits.push_str(whole.trim_start_matches('+'));
    digits.push_str(fraction);
    let scale = (fraction.len() as i64 - exponent).max(0) as usize;
    let mut digits = digits.trim_start_matches('0').to_owned();
    if digits.is_empty() {
        digits.push('0');
    }
    (digits, scale)
}

fn decimal_normalize(value: &str) -> String {
    let (digits, scale) = decimal_parts(value);
    if scale == 0 {
        return digits;
    }
    let mut output = if digits.len() <= scale {
        format!("0.{}{}", "0".repeat(scale - digits.len()), digits)
    } else {
        let position = digits.len() - scale;
        format!("{}.{}", &digits[..position], &digits[position..])
    };
    while output.ends_with('0') {
        output.pop();
    }
    if output.ends_with('.') {
        output.pop();
    }
    output
}

/// Why model generation stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StopReason {
    /// The provider produced a normal final response.
    EndTurn,
    /// The provider requested tool execution.
    ToolUse,
    /// The provider stopped because the output token limit was reached.
    Length,
    /// The provider aborted generation independently of host cancellation.
    Aborted,
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
    /// Explicit host-only context retained beside the transcript.
    ///
    /// The core does not invent a UI-message type or send these values to a
    /// provider by default. A context hook may filter or convert them at the
    /// model boundary.
    pub host_messages: Vec<SerializedJson>,
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
    /// Retained provider-reported model accounting.
    pub accounting: ModelAccountingSnapshot,
    next_message_id: u64,
}

impl Default for AgentState {
    fn default() -> Self {
        Self {
            system_prompt: String::new(),
            model: None,
            thinking_level: ThinkingLevel::Default,
            messages: Vec::new(),
            host_messages: Vec::new(),
            phase: AgentPhase::Idle,
            partial_response: None,
            is_streaming: false,
            pending_tool_calls: BTreeSet::new(),
            last_error: None,
            accounting: ModelAccountingSnapshot::default(),
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

    /// Replace retained history after the compaction transaction has validated it.
    ///
    /// The next generated ID advances beyond any caller-proposed replacement
    /// ID, so a later prompt cannot collide with a compactor-created summary.
    pub(crate) fn replace_messages(&mut self, messages: Vec<Message>) {
        let next_id = messages
            .iter()
            .map(message_id)
            .map(|id| id.0.saturating_add(1))
            .max()
            .unwrap_or(1);
        self.next_message_id = self.next_message_id.max(next_id);
        self.messages = messages;
    }

    /// Produce an owned inspection snapshot.
    pub(crate) fn snapshot(&self) -> AgentSnapshot {
        AgentSnapshot {
            system_prompt: self.system_prompt.clone(),
            model: self.model.clone(),
            thinking_level: self.thinking_level,
            messages: self.messages.clone(),
            host_messages: self.host_messages.clone(),
            phase: self.phase,
            partial_response: self.partial_response.clone(),
            is_streaming: self.is_streaming,
            pending_tool_calls: self.pending_tool_calls.clone(),
            last_error: self.last_error.clone(),
            accounting: self.accounting.clone(),
        }
    }
}

fn message_id(message: &Message) -> MessageId {
    match message {
        Message::User { id, .. }
        | Message::Assistant { id, .. }
        | Message::ToolResult { id, .. } => *id,
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
    /// Explicit host-only context retained beside the transcript.
    pub host_messages: Vec<SerializedJson>,
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
    /// Retained per-turn and aggregate provider-reported model accounting.
    pub accounting: ModelAccountingSnapshot,
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
