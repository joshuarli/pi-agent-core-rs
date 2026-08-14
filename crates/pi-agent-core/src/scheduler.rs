//! Caller-owned scheduling seams and deterministic tool ordering.
//!
//! The scheduler plans work but does not create threads, an executor, or a background task.
//! For parallel batches, calls are prepared in assistant/source order, completions are emitted
//! in actual completion order, and context results are recovered in source order.

use crate::error::SchedulerError;
use crate::state::{AssistantToolCall, ToolCallId};
use crate::tool::{ToolCall, ToolDefinition, ToolExecutionMode, ToolResult};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A boxed provider stream operation, driven by the embedding executor.
pub type ModelFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ModelStream, SchedulerError>> + Send + 'a>>;

/// A model response stream abstraction.  The provider owns transport and retry policy.
pub trait ModelProvider: Send + Sync {
    /// Start one inference request.
    fn stream<'a>(
        &'a self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelFuture<'a>;
}

/// Provider request assembled by the core.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelRequest {
    /// System instructions that remain separate from conversation messages.
    pub system_prompt: String,
    /// Serialized conversation/context envelope.
    pub context: String,
    /// Prompt-facing executable capabilities in registry/source order.
    pub tools: Vec<ToolDefinition>,
    /// Serialized model descriptor or provider options.
    pub model: Option<String>,
}

/// Provider events consumed by the run loop.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelStreamEvent {
    /// Incremental assistant text.
    TextDelta(String),
    /// A complete assistant tool call.
    ToolCall(AssistantToolCall),
    /// Provider usage update.
    Usage(crate::state::Usage),
    /// Normal stream settlement.
    End(crate::state::StopReason),
}

/// A finite provider event stream.  Polling/async adaptation is owned by the provider boundary.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModelStream {
    /// Events in provider order for a recorded or deterministic provider.
    pub events: Vec<ModelStreamEvent>,
}

/// Shared cancellation state with idempotent cancellation.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    /// Create a fresh uncancelled token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation.  Repeated calls are harmless.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Check whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// One source-ordered tool call and its scheduler policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlannedToolCall {
    /// Zero-based source index in the assistant message.
    pub source_index: usize,
    /// Call payload.
    pub call: ToolCall,
    /// Registered execution policy.
    pub execution_mode: ToolExecutionMode,
}

/// A planned assistant tool batch.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ToolBatch {
    /// Calls in assistant/source order.
    pub calls: Vec<PlannedToolCall>,
}

impl ToolBatch {
    /// Prepare calls in source order.  Actual execution is deliberately separate.
    pub fn prepare(calls: impl IntoIterator<Item = (ToolCall, ToolExecutionMode)>) -> Self {
        Self {
            calls: calls
                .into_iter()
                .enumerate()
                .map(|(source_index, (call, execution_mode))| PlannedToolCall {
                    source_index,
                    call,
                    execution_mode,
                })
                .collect(),
        }
    }

    /// Record a completion into a source-order result set.
    pub fn record_completion(
        &self,
        results: &mut CompletionSet,
        result: ToolResult,
    ) -> Result<(), SchedulerError> {
        if !self
            .calls
            .iter()
            .any(|call| call.call.id == result.tool_call_id)
        {
            return Err(SchedulerError::UnknownToolCall {
                tool_call_id: result.tool_call_id,
            });
        }
        results.insert(result)
    }
}

/// Tool completions keyed by call ID and emitted in source order when settled.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CompletionSet {
    results: BTreeMap<ToolCallId, ToolResult>,
}

impl CompletionSet {
    fn insert(&mut self, result: ToolResult) -> Result<(), SchedulerError> {
        let tool_call_id = result.tool_call_id.clone();
        if self.results.insert(tool_call_id.clone(), result).is_some() {
            return Err(SchedulerError::DuplicateCompletion { tool_call_id });
        }
        Ok(())
    }

    /// Return settled results in assistant/source order, excluding incomplete calls.
    pub fn in_source_order(&self, batch: &ToolBatch) -> Vec<ToolResult> {
        batch
            .calls
            .iter()
            .filter_map(|call| self.results.get(&call.call.id).cloned())
            .collect()
    }

    /// Number of completed calls.
    pub fn len(&self) -> usize {
        self.results.len()
    }

    /// Whether no calls have completed.
    pub fn is_empty(&self) -> bool {
        self.results.is_empty()
    }
}

/// Stateless planning facade kept separate from agent ownership.
#[derive(Clone, Copy, Debug, Default)]
pub struct Scheduler;

impl Scheduler {
    /// Build a source-ordered batch.  The caller decides which allowed calls to execute
    /// concurrently and reports their actual completion order through [`ToolBatch::record_completion`].
    pub fn plan_tool_batch(
        &self,
        calls: impl IntoIterator<Item = (ToolCall, ToolExecutionMode)>,
    ) -> ToolBatch {
        ToolBatch::prepare(calls)
    }
}
