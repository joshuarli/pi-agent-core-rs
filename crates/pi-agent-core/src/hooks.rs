//! Typed policy hooks around tool execution and context preparation.
//!
//! Hooks can influence a run only through explicit return values.  They never mutate agent
//! state directly, and hook failures remain typed so the run loop can apply the pinned
//! settlement policy.

use crate::error::HookError;
use crate::state::{Message, SerializedJson, Usage};
use crate::tool::{ToolCall, ToolResult};

/// Decision made before a tool is executed.
#[allow(missing_docs)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BeforeToolCall {
    /// Proceed with execution.
    Allow,
    /// Convert the call into an error tool result.
    Block { reason: String },
    /// End the current run after recording the policy reason.
    Terminate { reason: String },
}

/// Optional replacement for one result field.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum Replacement<T> {
    /// Leave the value produced by the tool unchanged.
    #[default]
    Keep,
    /// Replace the value completely.
    Replace(T),
}

/// Changes an after-tool hook may make, with replacement rather than recursive merge semantics.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AfterToolCall {
    /// Content replacement.
    pub content: Replacement<String>,
    /// Details replacement.
    pub details: Replacement<Option<SerializedJson>>,
    /// Error-flag replacement.
    pub is_error: Replacement<bool>,
    /// Usage replacement for providers that attach usage to tool results.
    pub usage: Replacement<Usage>,
    /// Optional replacement for the batch early-termination hint.
    ///
    /// Only `Some(true)` participates in Pi's rule that every finalized call in a batch must
    /// request termination before the next model turn is suppressed.
    pub terminate: Option<bool>,
}

/// Versioned host-message envelope passed through context hooks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextEnvelope {
    /// Version of this host extension envelope.
    pub version: u16,
    /// Messages retained by the core.
    pub messages: Vec<Message>,
    /// Optional serialized host-only additions.
    pub host_messages: Vec<SerializedJson>,
}

/// Hook trait implemented by the embedding policy layer.
pub trait HookSet: Send + Sync {
    /// Decide whether one tool call may execute.
    fn before_tool_call(&self, call: &ToolCall) -> Result<BeforeToolCall, HookError>;
    /// Replace selected fields after execution.
    fn after_tool_call(
        &self,
        call: &ToolCall,
        result: &ToolResult,
    ) -> Result<AfterToolCall, HookError>;
    /// Transform retained context before conversion to provider messages.
    fn transform_context(&self, context: ContextEnvelope) -> Result<ContextEnvelope, HookError>;
    /// Convert the host envelope into the provider's serialized context format.
    fn convert_to_llm(&self, context: ContextEnvelope) -> Result<String, HookError>;
    /// Decide whether the run should stop after the turn.
    fn should_stop_after_turn(&self, _context: &ContextEnvelope) -> Result<bool, HookError> {
        Ok(false)
    }
    /// Prepare the next turn's context.
    fn prepare_next_turn(&self, context: ContextEnvelope) -> Result<ContextEnvelope, HookError> {
        Ok(context)
    }
}

/// A no-op hook implementation suitable as the default.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoHooks;

impl HookSet for NoHooks {
    fn before_tool_call(&self, _call: &ToolCall) -> Result<BeforeToolCall, HookError> {
        Ok(BeforeToolCall::Allow)
    }
    fn after_tool_call(
        &self,
        _call: &ToolCall,
        _result: &ToolResult,
    ) -> Result<AfterToolCall, HookError> {
        Ok(AfterToolCall::default())
    }
    fn transform_context(&self, context: ContextEnvelope) -> Result<ContextEnvelope, HookError> {
        Ok(context)
    }
    fn convert_to_llm(&self, context: ContextEnvelope) -> Result<String, HookError> {
        Ok(context
            .messages
            .into_iter()
            .map(|message| format!("{message:?}"))
            .collect::<Vec<_>>()
            .join("\n"))
    }
}
