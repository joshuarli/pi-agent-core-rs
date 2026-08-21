//! Caller-supplied, transactional conversation compaction.
//!
//! The core owns the retained conversation and therefore owns the compaction
//! transaction. A [`Compactor`] receives an owned snapshot, but cannot mutate
//! the agent directly. Its proposed replacement is validated and committed
//! only when the owning [`CompactionHandle`] is still active and uncancelled.

use crate::agent::{ActiveRun, Agent, AgentInner};
use crate::error::CoreError;
use crate::event::{AgentEvent, AgentEventKind, CompactionOutcome};
use crate::run::RunHandle;
use crate::scheduler::CancellationToken;
use crate::state::{
    AgentMessage, AgentPhase, MessageId, ModelDescriptor, RunPhase, RunState, StopReason,
};
use crate::tool::ToolDefinition;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::future::Future;
use std::num::NonZeroU64;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

/// Version of the context shape supplied to [`Compactor`].
pub const COMPACTION_CONTEXT_VERSION: u32 = 1;

/// Why an automatic compaction transaction was requested.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutomaticCompactionReason {
    /// The estimated next-request context crossed the configured threshold.
    Threshold,
    /// A provider explicitly reported that its context capacity was exceeded.
    Overflow,
}

/// The explicit source of a context capacity used by automatic compaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextBudgetSource {
    /// A model/provider context window supplied by the embedding.
    ContextWindow(NonZeroU64),
    /// A host-selected request budget that may be smaller than a context window.
    ContextBudget(NonZeroU64),
}

impl ContextBudgetSource {
    /// Return the usable input capacity before the compaction reserve is deducted.
    pub const fn tokens(self) -> u64 {
        match self {
            Self::ContextWindow(tokens) | Self::ContextBudget(tokens) => tokens.get(),
        }
    }
}

/// What to do after an explicit provider context-overflow signal.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OverflowRecovery {
    /// Preserve the provider error; do not compact or retry automatically.
    #[default]
    Disabled,
    /// Compact the prior transcript then retry the incomplete continuation once.
    CompactAndRetry,
}

/// Opt-in automatic compaction configuration.
///
/// The configuration deliberately has no provider or summary-prompt fields:
/// a caller must still configure a [`Compactor`] explicitly. `recent_tokens`
/// is supplied to that compactor along with an exact safe retained suffix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutomaticCompactionPolicy {
    /// Whether automatic compaction participates in normal run progression.
    pub enabled: bool,
    /// Explicit context capacity source; the core never guesses from a model name.
    pub context_budget: ContextBudgetSource,
    /// Tokens reserved for the compactor request/output and not available to the next request.
    pub reserved_tokens: u64,
    /// Approximate number of recent transcript tokens selected as an intact retained suffix.
    pub recent_tokens: u64,
    /// Typed overflow recovery policy.
    pub overflow_recovery: OverflowRecovery,
    /// Maximum successful or attempted automatic compaction transactions in one run.
    pub max_compactions_per_run: u32,
    /// Maximum overflow-recovery retries across distinct continuations in one run.
    ///
    /// Each incomplete continuation can still be retried at most once.
    pub max_overflow_retries_per_run: u32,
}

impl AutomaticCompactionPolicy {
    /// Construct a disabled policy with an explicit inert capacity placeholder.
    ///
    /// Use [`Self::enabled`] to opt in after selecting a real capacity.
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            // One is inert while disabled and keeps this type free of an
            // invalid zero-capacity state.
            context_budget: ContextBudgetSource::ContextBudget(NonZeroU64::MIN),
            reserved_tokens: 0,
            recent_tokens: 0,
            overflow_recovery: OverflowRecovery::Disabled,
            max_compactions_per_run: 0,
            max_overflow_retries_per_run: 0,
        }
    }

    /// Validate cross-field policy invariants before an agent is built.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !self.enabled {
            return Ok(());
        }
        if self.reserved_tokens >= self.context_budget.tokens() {
            return Err("automatic compaction reserve must be smaller than the context budget");
        }
        if self.max_compactions_per_run == 0 {
            return Err(
                "enabled automatic compaction requires a non-zero per-run compaction limit",
            );
        }
        if self.overflow_recovery == OverflowRecovery::CompactAndRetry
            && self.max_overflow_retries_per_run == 0
        {
            return Err("overflow retry recovery requires a non-zero retry limit");
        }
        Ok(())
    }

    /// Return the threshold for a normal next model request.
    pub const fn threshold_tokens(&self) -> u64 {
        self.context_budget
            .tokens()
            .saturating_sub(self.reserved_tokens)
    }
}

impl Default for AutomaticCompactionPolicy {
    fn default() -> Self {
        Self::disabled()
    }
}

/// The exact automatic split supplied to a compactor.
///
/// `retained_messages` is an intact suffix that keeps assistant tool calls
/// paired with results. `prefix_messages` may end during a tool turn only when
/// `split_turn_prefix` names the assistant content the compactor should retain
/// in its summary. The core never fabricates summary text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutomaticCompactionRequest {
    /// Trigger that selected this transaction.
    pub reason: AutomaticCompactionReason,
    /// Estimated context tokens before compaction, if known.
    pub estimated_tokens_before: Option<u64>,
    /// Configured capacity before the reserve is deducted.
    pub context_budget_tokens: u64,
    /// Tokens reserved for the compactor operation.
    pub reserved_tokens: u64,
    /// Requested approximate tail size.
    pub recent_tokens: u64,
    /// Prefix selected for summary/reduction.
    pub prefix_messages: Vec<AgentMessage>,
    /// Intact suffix that the replacement must preserve exactly.
    pub retained_messages: Vec<AgentMessage>,
    /// A partial user/assistant/tool turn in the summarized prefix, when the
    /// intact retained suffix begins at an assistant message.
    pub split_turn_prefix: Vec<AgentMessage>,
    /// Whether a successful compaction will retry the same provider continuation.
    pub retry_provider_request: bool,
}

/// An owned, versioned view of the conversation a compactor may replace.
///
/// It contains only data retained by the core. The selected model remains
/// informational: choosing or replacing a provider is a separate idle-only
/// operation on [`Agent`].
#[derive(Clone, Debug, PartialEq)]
pub struct CompactionContext {
    /// Version of this request shape.
    pub version: u32,
    /// Static instructions associated with this conversation.
    pub system_prompt: String,
    /// Selected model identity, if the host configured one.
    pub model: Option<ModelDescriptor>,
    /// Canonical retained conversation.
    pub messages: Vec<AgentMessage>,
    /// Explicit host-only context retained beside the conversation.
    pub host_messages: Vec<crate::state::SerializedJson>,
    /// Provider-visible context built through the active projection and hook pipeline when the
    /// compaction was requested from a running model turn. `None` means the compactor must use
    /// the standalone summary path; idle manual compaction intentionally has no request snapshot.
    pub provider_context: Option<ProviderContext>,
}

/// The provider-visible prompt snapshot available to an automatic compactor.
///
/// `context` is intentionally opaque: the core does not impose a provider message schema. A
/// host that understands its own conversion (for example the TUI's OpenAI-compatible adapter)
/// may append a single summary instruction while preserving the exact preceding context bytes.
#[derive(Clone, Debug, PartialEq)]
pub struct ProviderContext {
    /// System instructions used for the active request.
    pub system_prompt: String,
    /// Converted provider conversation/context.
    pub context: String,
    /// Complete active provider context used to verify that `context` is an exact message-prefix.
    /// Hosts that understand the conversion can reject cache-friendly summarization when a
    /// transform reordered or injected content into the candidate source.
    pub active_context: Option<String>,
    /// Ordered prompt-facing tool definitions used for the active request.
    pub tools: Vec<ToolDefinition>,
}

/// A validated-on-return proposal from a [`Compactor`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompactionResult {
    /// Replacement canonical conversation.
    pub messages: Vec<AgentMessage>,
    /// Optional accounting reported by the compactor's own provider call.
    ///
    /// This stays attached to the compaction event because compaction is not a
    /// normal model turn. The core does not estimate, aggregate, or price it.
    pub usage: Option<crate::state::Usage>,
}

impl CompactionResult {
    /// Construct a result with no provider-reported compaction accounting.
    pub fn new(messages: Vec<AgentMessage>) -> Self {
        Self {
            messages,
            usage: None,
        }
    }

    /// Attach provider-reported compaction accounting without deriving a price.
    pub fn with_usage(mut self, usage: crate::state::Usage) -> Self {
        self.usage = Some(usage);
        self
    }
}

/// A typed compactor failure returned to the core transaction boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompactionError {
    /// The caller-supplied compactor could not produce a replacement.
    Failed {
        /// Redacted, host-supplied diagnostic.
        message: String,
    },
    /// The compactor returned a replacement that violates conversation invariants.
    InvalidReplacement {
        /// Stable explanation of the rejected relationship or identifier.
        message: String,
    },
}

impl CompactionError {
    /// Construct a caller-supplied failure without exposing a provider type.
    pub fn failed(message: impl Into<String>) -> Self {
        Self::Failed {
            message: message.into(),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidReplacement {
            message: message.into(),
        }
    }
}

impl fmt::Display for CompactionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Failed { message } => write!(formatter, "compactor failed: {message}"),
            Self::InvalidReplacement { message } => {
                write!(formatter, "invalid compacted conversation: {message}")
            }
        }
    }
}

impl std::error::Error for CompactionError {}

/// A caller-polled compactor operation.
pub type CompactionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<CompactionResult, CompactionError>> + Send + 'a>>;

/// A caller-supplied policy and execution boundary for manual compaction.
///
/// Implementations may call a model, use a local algorithm, or reject the
/// request. They receive cancellation and must not assume an executor owned
/// by the core. There is no implicit summary prompt or provider fallback.
pub trait Compactor: Send + Sync {
    /// Produce a replacement for this owned context.
    fn compact<'a>(
        &'a self,
        context: CompactionContext,
        cancellation: CancellationToken,
    ) -> CompactionFuture<'a>;

    /// Produce an automatic replacement using the core-selected safe split.
    ///
    /// Existing manual compactors remain valid: by default they receive the
    /// same complete snapshot through [`Self::compact`]. Compactors that can
    /// preserve a prior summary and exact recent tail should override this
    /// method and use `request` rather than inferring boundaries themselves.
    fn compact_automatic<'a>(
        &'a self,
        context: CompactionContext,
        _request: AutomaticCompactionRequest,
        cancellation: CancellationToken,
    ) -> CompactionFuture<'a> {
        self.compact(context, cancellation)
    }
}

/// A reserved, caller-driven manual compaction operation.
///
/// The handle has the same ownership and cancellation rules as a normal run:
/// construct it while idle, then drive it on the embedding's executor. Its
/// events are intentionally a separate `Compaction*` grammar rather than a
/// synthetic assistant response.
pub struct CompactionHandle {
    run: RunHandle,
    compactor: Arc<dyn Compactor>,
}

impl fmt::Debug for CompactionHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompactionHandle")
            .field("snapshot", &self.run.snapshot())
            .finish()
    }
}

impl CompactionHandle {
    /// Stable operation ID, allocated from the agent's run-ID sequence.
    pub fn id(&self) -> crate::state::RunId {
        self.run.id()
    }

    /// Return the ordered lifecycle events emitted by this compaction.
    pub fn events(&self) -> Vec<AgentEvent> {
        self.run.events()
    }

    /// Request cancellation. This is idempotent after terminal settlement.
    pub fn abort(&self) -> Result<(), CoreError> {
        self.run.abort()
    }

    /// Drive the transaction to a terminal outcome on the caller's executor.
    pub async fn drive(&self) -> Result<(), CoreError> {
        let agent = self
            .run
            .agent
            .upgrade()
            .ok_or(CoreError::InvalidTransition(
                crate::error::StateTransitionError::new("compaction", "orphaned", "drive"),
            ))?;
        let source_message_count = {
            let state = agent.state.lock().expect("agent state mutex poisoned");
            state.messages.len()
        };

        if let Err(error) = self
            .run
            .emit(
                &agent,
                AgentEventKind::CompactionStart {
                    source_message_count,
                },
            )
            .await
        {
            return self.settle_emit_failure(error);
        }
        if self.run.cancellation.is_cancelled() {
            return self.settle_cancelled(&agent).await;
        }

        let context = snapshot_context(&agent);
        let replacement = match self
            .compactor
            .compact(context, self.run.cancellation.clone())
            .await
        {
            Ok(replacement) => replacement,
            Err(error) => return self.settle_failure(&agent, error).await,
        };
        if self.run.cancellation.is_cancelled() {
            return self.settle_cancelled(&agent).await;
        }
        if let Err(error) = validate_messages(&replacement.messages) {
            return self.settle_failure(&agent, error).await;
        }

        let retained_message_count = replacement.messages.len();
        if let Err(error) = commit_replacement(
            &agent,
            self.id(),
            &self.run.cancellation,
            replacement.messages,
        ) {
            return match error {
                CoreError::Cancelled => self.settle_cancelled(&agent).await,
                error => self.settle_emit_failure(error),
            };
        }
        if let Err(error) = self
            .run
            .emit(
                &agent,
                AgentEventKind::CompactionResult {
                    retained_message_count,
                    usage: replacement.usage,
                },
            )
            .await
        {
            return self.settle_emit_failure(error);
        }
        if let Err(error) = self
            .run
            .emit(
                &agent,
                AgentEventKind::CompactionEnd {
                    outcome: CompactionOutcome::Succeeded {
                        retained_message_count,
                    },
                },
            )
            .await
        {
            return self.settle_emit_failure(error);
        }
        self.run.succeed(StopReason::Stop)
    }

    async fn settle_failure(
        &self,
        agent: &AgentInner,
        error: CompactionError,
    ) -> Result<(), CoreError> {
        let message = error.to_string();
        if let Err(observer_error) = self
            .run
            .emit(
                agent,
                AgentEventKind::CompactionEnd {
                    outcome: CompactionOutcome::Failed {
                        message: message.clone(),
                    },
                },
            )
            .await
        {
            return self.settle_emit_failure(observer_error);
        }
        let _ = self.run.fail(message);
        Err(CoreError::Compaction(error))
    }

    async fn settle_cancelled(&self, agent: &AgentInner) -> Result<(), CoreError> {
        if let Err(error) = self
            .run
            .emit(
                agent,
                AgentEventKind::CompactionEnd {
                    outcome: CompactionOutcome::Cancelled,
                },
            )
            .await
        {
            return self.settle_emit_failure(error);
        }
        self.run.settle_cancelled()?;
        Err(CoreError::Cancelled)
    }

    fn settle_emit_failure(&self, error: CoreError) -> Result<(), CoreError> {
        let _ = self.run.fail(error.to_string());
        Err(error)
    }
}

impl Agent {
    /// Reserve an idle agent for a caller-driven manual compaction operation.
    ///
    /// This rejects active and cancelling agents without changing their state.
    /// An agent without an explicit [`Compactor`] also rejects the request;
    /// hosts must not invent a summary policy at this boundary.
    pub fn start_compaction(&self) -> Result<CompactionHandle, CoreError> {
        let run_number = self
            .inner
            .next_run_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .saturating_add(1);
        let run_id = crate::state::RunId(run_number);
        {
            let state = self.inner.state.lock().expect("agent state mutex poisoned");
            if let AgentPhase::Running(active) | AgentPhase::Cancelling(active) = state.phase {
                return Err(CoreError::ActiveRun { run_id: active });
            }
        }
        let compactor = self
            .inner
            .compactor
            .read()
            .expect("agent compactor lock poisoned")
            .clone()
            .ok_or(CoreError::MissingCompactor)?;
        let mut state = self.inner.state.lock().expect("agent state mutex poisoned");
        if let AgentPhase::Running(active) | AgentPhase::Cancelling(active) = state.phase {
            return Err(CoreError::ActiveRun { run_id: active });
        }
        let configuration = Arc::clone(
            &*self
                .inner
                .configuration
                .read()
                .expect("agent configuration lock poisoned"),
        );
        state.phase = AgentPhase::Running(run_id);
        state.last_error = None;
        state.partial_response = None;
        state.is_streaming = false;
        state.pending_tool_calls.clear();
        drop(state);

        let run_state = Arc::new(Mutex::new(RunState::created(run_id)));
        run_state
            .lock()
            .expect("compaction run mutex poisoned")
            .phase = RunPhase::Running;
        let cancellation = CancellationToken::new();
        *self
            .inner
            .active_run
            .lock()
            .expect("active run mutex poisoned") = Some(ActiveRun {
            id: run_id,
            state: Arc::clone(&run_state),
            cancellation: cancellation.clone(),
        });

        Ok(CompactionHandle {
            run: RunHandle {
                agent: Arc::downgrade(&self.inner),
                state: run_state,
                cancellation,
                initial_messages: Vec::new(),
                message_start_index: 0,
                skip_initial_steering: true,
                configuration,
                policy: Mutex::new(crate::run::RunPolicyState::default()),
            },
            compactor,
        })
    }
}

pub(crate) fn snapshot_context(agent: &AgentInner) -> CompactionContext {
    let state = agent.state.lock().expect("agent state mutex poisoned");
    CompactionContext {
        version: COMPACTION_CONTEXT_VERSION,
        system_prompt: state.system_prompt.clone(),
        model: state.model.clone(),
        messages: state.messages.clone(),
        host_messages: state.host_messages.clone(),
        provider_context: None,
    }
}

pub(crate) fn commit_replacement(
    agent: &AgentInner,
    run_id: crate::state::RunId,
    cancellation: &CancellationToken,
    replacement: Vec<AgentMessage>,
) -> Result<(), CoreError> {
    let mut state = agent.state.lock().expect("agent state mutex poisoned");
    if cancellation.is_cancelled() {
        return Err(CoreError::Cancelled);
    }
    if !matches!(state.phase, AgentPhase::Running(id) if id == run_id) {
        return Err(CoreError::Cancelled);
    }
    state.replace_messages(replacement);
    Ok(())
}

pub(crate) fn validate_messages(messages: &[AgentMessage]) -> Result<(), CompactionError> {
    let mut message_ids = BTreeSet::new();
    let mut tool_calls = BTreeMap::new();
    let mut tool_results = BTreeSet::new();
    for message in messages {
        let id = message_id(message);
        if id.0 == 0 || id.0 == u64::MAX {
            return Err(CompactionError::invalid(
                "message IDs zero and u64::MAX are reserved",
            ));
        }
        if !message_ids.insert(id) {
            return Err(CompactionError::invalid(format!(
                "message ID {} occurs more than once",
                id.0
            )));
        }
        match message {
            AgentMessage::Assistant {
                tool_calls: calls, ..
            } => {
                for call in calls {
                    if let Some(previous_name) =
                        tool_calls.insert(call.id.clone(), call.name.as_str())
                    {
                        return Err(CompactionError::invalid(format!(
                            "tool call ID {} is reused by {previous_name:?} and {:?}",
                            call.id, call.name
                        )));
                    }
                }
            }
            AgentMessage::ToolResult {
                tool_call_id,
                tool_name,
                ..
            } => match tool_calls.get(tool_call_id) {
                Some(call_name) if *call_name == tool_name => {
                    if !tool_results.insert(tool_call_id.clone()) {
                        return Err(CompactionError::invalid(format!(
                            "tool call {} has more than one retained result",
                            tool_call_id
                        )));
                    }
                }
                Some(call_name) => {
                    return Err(CompactionError::invalid(format!(
                        "tool result {} names {tool_name:?}, but its call names {call_name:?}",
                        tool_call_id
                    )));
                }
                None => {
                    return Err(CompactionError::invalid(format!(
                        "tool result {} has no preceding assistant call",
                        tool_call_id
                    )));
                }
            },
            AgentMessage::User { .. } => {}
        }
    }
    let missing_results = tool_calls
        .keys()
        .filter(|tool_call_id| !tool_results.contains(*tool_call_id))
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if !missing_results.is_empty() {
        return Err(CompactionError::invalid(format!(
            "retained assistant tool calls have no result: {}",
            missing_results.join(", ")
        )));
    }
    Ok(())
}

fn message_id(message: &AgentMessage) -> MessageId {
    match message {
        AgentMessage::User { id, .. }
        | AgentMessage::Assistant { id, .. }
        | AgentMessage::ToolResult { id, .. } => *id,
    }
}
