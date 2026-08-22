//! HookSet adapter for the policy-owned pre-tool decision.

use super::{LuaPolicy, PolicyError};
use std::sync::Arc;
use tea_core::error::HookError;
use tea_core::hooks::{
    AfterToolCall, BeforeToolCall, ContextEnvelope, HookFuture, HookSet, NextTurn,
};
use tea_core::scheduler::CancellationToken;
use tea_core::tool::{ToolCall, ToolResult};

/// A hook adapter that gives a Lua policy the first, narrow pre-tool decision.
///
/// All other hook methods—including provider-context conversion—remain owned
/// by the embedding host. A denied call never reaches the wrapped hook set.
#[derive(Clone)]
pub struct LuaPolicyHookSet {
    policy: Arc<LuaPolicy>,
    inner: Arc<dyn HookSet>,
}

impl LuaPolicyHookSet {
    /// Compose a loaded policy with the host's provider and lifecycle hooks.
    pub fn new(policy: Arc<LuaPolicy>, inner: Arc<dyn HookSet>) -> Self {
        Self { policy, inner }
    }
}

impl HookSet for LuaPolicyHookSet {
    fn before_tool_call(&self, call: &ToolCall) -> Result<BeforeToolCall, HookError> {
        match self
            .policy
            .before_tool_call(call)
            .map_err(before_hook_error)?
        {
            BeforeToolCall::Allow => self.inner.before_tool_call(call),
            decision => Ok(decision),
        }
    }

    fn after_tool_call(
        &self,
        call: &ToolCall,
        result: &ToolResult,
    ) -> Result<AfterToolCall, HookError> {
        self.inner.after_tool_call(call, result)
    }

    fn transform_context(&self, context: ContextEnvelope) -> Result<ContextEnvelope, HookError> {
        self.inner.transform_context(context)
    }

    fn convert_to_llm(&self, context: ContextEnvelope) -> Result<String, HookError> {
        self.inner.convert_to_llm(context)
    }

    fn should_stop_after_turn(&self, context: &ContextEnvelope) -> Result<bool, HookError> {
        self.inner.should_stop_after_turn(context)
    }

    fn prepare_next_turn(&self, context: ContextEnvelope) -> Result<NextTurn, HookError> {
        self.inner.prepare_next_turn(context)
    }

    fn before_tool_call_async<'a>(
        &'a self,
        call: &'a ToolCall,
        context: ContextEnvelope,
        cancellation: CancellationToken,
    ) -> HookFuture<'a, BeforeToolCall> {
        match self
            .policy
            .before_tool_call(call)
            .map_err(before_hook_error)
        {
            Ok(BeforeToolCall::Allow) => {
                self.inner
                    .before_tool_call_async(call, context, cancellation)
            }
            Ok(decision) => Box::pin(std::future::ready(Ok(decision))),
            Err(error) => Box::pin(std::future::ready(Err(error))),
        }
    }

    fn after_tool_call_async<'a>(
        &'a self,
        call: &'a ToolCall,
        result: &'a ToolResult,
        context: ContextEnvelope,
        cancellation: CancellationToken,
    ) -> HookFuture<'a, AfterToolCall> {
        self.inner
            .after_tool_call_async(call, result, context, cancellation)
    }

    fn transform_context_async<'a>(
        &'a self,
        context: ContextEnvelope,
        cancellation: CancellationToken,
    ) -> HookFuture<'a, ContextEnvelope> {
        self.inner.transform_context_async(context, cancellation)
    }

    fn convert_to_llm_async<'a>(
        &'a self,
        context: ContextEnvelope,
        cancellation: CancellationToken,
    ) -> HookFuture<'a, String> {
        self.inner.convert_to_llm_async(context, cancellation)
    }

    fn should_stop_after_turn_async<'a>(
        &'a self,
        context: &'a ContextEnvelope,
        cancellation: CancellationToken,
    ) -> HookFuture<'a, bool> {
        self.inner
            .should_stop_after_turn_async(context, cancellation)
    }

    fn prepare_next_turn_async<'a>(
        &'a self,
        context: ContextEnvelope,
        cancellation: CancellationToken,
    ) -> HookFuture<'a, NextTurn> {
        self.inner.prepare_next_turn_async(context, cancellation)
    }
}

fn before_hook_error(error: PolicyError) -> HookError {
    HookError::new("before_tool_call", error.to_string())
}
