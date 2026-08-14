//! Typed failures at the core boundaries.
//!
//! Errors are values rather than control-flow shortcuts.  In particular, a tool failure is
//! not an agent-state failure: the scheduler turns it into a tool-result message, while a
//! state transition failure means the caller attempted an operation outside the run contract.

use std::fmt;

/// The top-level error returned by state-owning APIs.
#[allow(missing_docs)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CoreError {
    /// A second run was requested while another run owns the agent.
    ActiveRun { run_id: super::state::RunId },
    /// The requested operation is not valid for the current state.
    InvalidTransition(StateTransitionError),
    /// The run has already reached a terminal state.
    RunFinished { run_id: super::state::RunId },
    /// The run was cancelled before the requested operation could complete.
    Cancelled,
    /// The caller started a run without supplying a model stream capability.
    MissingModelProvider,
    /// A caller-provided model stream failed before yielding a terminal response.
    ModelProvider { message: String },
    /// A hook failed at an explicit lifecycle boundary.
    Hook(HookError),
    /// The model stream used a behavior that the active V0 slice does not yet support.
    UnsupportedModelStream { message: String },
}

/// A state-machine transition was rejected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateTransitionError {
    /// The state-machine entity, for example `agent` or `run`.
    pub entity: &'static str,
    /// A stable name for the state from which the operation was attempted.
    pub from: &'static str,
    /// A stable name for the rejected operation.
    pub operation: &'static str,
}

impl StateTransitionError {
    /// Construct a transition error with stable, searchable names.
    pub const fn new(entity: &'static str, from: &'static str, operation: &'static str) -> Self {
        Self {
            entity,
            from,
            operation,
        }
    }
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ActiveRun { run_id } => write!(f, "agent already has active run {run_id:?}"),
            Self::InvalidTransition(error) => error.fmt(f),
            Self::RunFinished { run_id } => write!(f, "run {run_id:?} is already finished"),
            Self::Cancelled => f.write_str("operation was cancelled"),
            Self::MissingModelProvider => f.write_str("agent has no model provider"),
            Self::ModelProvider { message } => write!(f, "model provider failed: {message}"),
            Self::Hook(error) => error.fmt(f),
            Self::UnsupportedModelStream { message } => {
                write!(f, "unsupported model stream behavior: {message}")
            }
        }
    }
}

impl fmt::Display for StateTransitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} cannot perform {} from {}",
            self.entity, self.operation, self.from
        )
    }
}

impl std::error::Error for CoreError {}

impl From<HookError> for CoreError {
    fn from(error: HookError) -> Self {
        Self::Hook(error)
    }
}
impl std::error::Error for StateTransitionError {}

/// A failure produced by a host hook.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HookError {
    /// Stable hook name, such as `before_tool_call`.
    pub hook: &'static str,
    /// Host-provided explanation.
    pub message: String,
}

impl HookError {
    /// Construct a hook error.
    pub fn new(hook: &'static str, message: impl Into<String>) -> Self {
        Self {
            hook,
            message: message.into(),
        }
    }
}

impl fmt::Display for HookError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} hook failed: {}", self.hook, self.message)
    }
}

impl std::error::Error for HookError {}

/// A failure while preparing or invoking a tool.
#[allow(missing_docs)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolError {
    /// The call arguments did not pass the tool's boundary validator.
    InvalidArguments { tool: String, message: String },
    /// The tool was denied by a host policy or hook.
    Blocked { tool: String, reason: String },
    /// The host implementation failed.
    Execution { tool: String, message: String },
    /// Cancellation interrupted the tool.
    Cancelled { tool: String },
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArguments { tool, message } => {
                write!(f, "invalid arguments for {tool}: {message}")
            }
            Self::Blocked { tool, reason } => write!(f, "tool {tool} blocked: {reason}"),
            Self::Execution { tool, message } => write!(f, "tool {tool} failed: {message}"),
            Self::Cancelled { tool } => write!(f, "tool {tool} cancelled"),
        }
    }
}

impl std::error::Error for ToolError {}

/// A scheduler planning or ordering failure.
#[allow(missing_docs)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SchedulerError {
    /// A completion was supplied for a call that was not in the planned batch.
    UnknownToolCall {
        tool_call_id: super::state::ToolCallId,
    },
    /// A completion was supplied more than once.
    DuplicateCompletion {
        tool_call_id: super::state::ToolCallId,
    },
}

impl fmt::Display for SchedulerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownToolCall { tool_call_id } => {
                write!(f, "unknown tool call {tool_call_id:?}")
            }
            Self::DuplicateCompletion { tool_call_id } => {
                write!(f, "duplicate completion for {tool_call_id:?}")
            }
        }
    }
}

impl std::error::Error for SchedulerError {}

/// A profile specification failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileError {
    /// Human-readable explanation.
    pub message: String,
}

impl ProfileError {
    /// Construct a profile error.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ProfileError {}
