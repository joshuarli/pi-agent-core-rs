//! Public policy values and private VM state.

use mlua::{Function, Lua};
use tea_core::tool::ToolExecutionMode;
use tea_protocol::JsonValue;
use std::fmt;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};

/// Resource limits applied to one Lua policy virtual machine.
///
/// `max_interrupt_checks` bounds initial policy evaluation and each hook
/// invocation separately. Luau invokes the interrupt handler at loop and
/// function-call boundaries, so the value is a deterministic host budget
/// rather than an exact instruction count.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PolicyLimits {
    /// Largest accepted policy source in bytes.
    pub max_source_bytes: usize,
    /// Largest Luau VM allocation total in bytes.
    pub max_memory_bytes: usize,
    /// Largest number of Luau interrupt checks permitted per evaluation.
    pub max_interrupt_checks: usize,
}

impl Default for PolicyLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: 64 * 1024,
            max_memory_bytes: 1024 * 1024,
            max_interrupt_checks: 10_000,
        }
    }
}

/// A prompt-facing tool declared by a policy but not granted any authority.
#[derive(Clone, Debug, PartialEq)]
pub struct PolicyTool {
    /// Stable tool name sent to the model.
    pub name: String,
    /// Prompt-facing explanation of the tool.
    pub description: String,
    /// JSON Schema for the tool arguments.
    pub schema: JsonValue,
    /// Host-owned capability name that must be explicitly bound by an embedder.
    pub capability: String,
    /// Whether the core may overlap calls to this tool.
    pub execution_mode: ToolExecutionMode,
    /// Optional self-contained Luau source for this tool's coroutine handler.
    ///
    /// The source must evaluate to a function accepted by
    /// [`tool_handler::LuaToolHandler`]. It remains inert until an embedding
    /// deliberately adapts it into an explicit Rust capability; declaring a
    /// handler never grants a world effect by itself.
    pub handler_source: Option<String>,
}

/// A loaded, sandboxed Luau policy.
pub struct LuaPolicy {
    pub(super) runtime: Mutex<PolicyRuntime>,
    pub(super) system_prompt_append: String,
    pub(super) tools: Vec<PolicyTool>,
}

pub(super) struct PolicyRuntime {
    pub(super) lua: Lua,
    pub(super) before_tool_call: Option<Function>,
    pub(super) interrupt_budget: Arc<AtomicUsize>,
    pub(super) max_interrupt_checks: usize,
}

/// A policy loading or evaluation failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PolicyError {
    /// The source exceeds the configured boundary before entering the VM.
    SourceTooLarge {
        /// Source length in bytes.
        actual: usize,
        /// Configured maximum length in bytes.
        limit: usize,
    },
    /// A configured VM resource limit is zero.
    InvalidLimit {
        /// Stable configuration field name.
        field: &'static str,
    },
    /// The extension failed to meet the policy-table contract.
    Contract {
        /// Searchable explanation of the mismatch.
        message: String,
    },
    /// The Luau VM rejected or interrupted evaluation.
    Runtime {
        /// Host-safe diagnostic from the Luau VM.
        message: String,
    },
}

impl fmt::Display for PolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceTooLarge { actual, limit } => {
                write!(
                    formatter,
                    "policy source is {actual} bytes, exceeding {limit} bytes"
                )
            }
            Self::InvalidLimit { field } => {
                write!(formatter, "policy limit {field} must be non-zero")
            }
            Self::Contract { message } => write!(formatter, "invalid policy contract: {message}"),
            Self::Runtime { message } => write!(formatter, "Luau policy failed: {message}"),
        }
    }
}

impl std::error::Error for PolicyError {}
