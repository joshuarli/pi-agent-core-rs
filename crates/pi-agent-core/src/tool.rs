//! Tool capability boundaries.
//!
//! A tool is an explicit host capability.  The core owns names, schemas, ordering, and result
//! placement; the host owns authority and the actual side effect. Schemas use
//! the stable protocol JSON value, while call arguments retain their exact
//! serialized form for provider correlation. The core validates arguments
//! through a private, replaceable JSON Schema adapter before invoking a tool.

use crate::error::ToolError;
use crate::scheduler::CancellationToken;
use crate::state::{SerializedJson, ToolCallId, Usage};
use pi_agent_protocol::JsonValue;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// A boxed future used so callers may drive tools on their own executor.
pub type ToolFuture<'a> = Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send + 'a>>;

/// Whether calls to a tool may overlap within one assistant message.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ToolExecutionMode {
    /// The scheduler must await this call before starting another call in the batch.
    Sequential,
    /// The scheduler may execute this call concurrently with other parallel calls.
    #[default]
    Parallel,
}

/// An assistant-requested tool invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolCall {
    /// Stable call identifier.
    pub id: ToolCallId,
    /// Registered capability name.
    pub name: String,
    /// Serialized JSON arguments.
    pub arguments: SerializedJson,
}

/// A tool result to be inserted into model context.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolResult {
    /// Call to which the result belongs.
    pub tool_call_id: ToolCallId,
    /// Serialized result content.
    pub content: String,
    /// Optional serialized host details.
    pub details: Option<SerializedJson>,
    /// Optional provider/accounting usage attached by the capability.
    pub usage: Option<Usage>,
    /// Names of capabilities added for a later model request, when an explicit
    /// host policy supports dynamic tool exposure.
    pub added_tool_names: Vec<String>,
    /// Whether this finalized result asks to stop after the current batch.
    ///
    /// The scheduler stops before another model request only when every
    /// finalized call in the batch has this flag set. An after-tool hook may
    /// replace the flag explicitly.
    pub terminate: bool,
    /// Whether the result represents a tool failure.
    pub is_error: bool,
}

/// A partial update emitted during tool execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolUpdate {
    /// Human/model-visible update content.
    pub content: String,
    /// Optional serialized host details.
    pub details: Option<SerializedJson>,
}

/// A cancellation handle shared by model and tool operations.
#[derive(Clone, Default)]
pub struct ToolUpdateSink {
    callback: Option<Arc<dyn Fn(ToolUpdate) + Send + Sync>>,
}

impl std::fmt::Debug for ToolUpdateSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolUpdateSink")
            .field("enabled", &self.callback.is_some())
            .finish()
    }
}

impl ToolUpdateSink {
    /// Create a sink that discards updates.
    pub const fn disabled() -> Self {
        Self { callback: None }
    }

    /// Create a sink backed by a host callback.
    pub fn new(callback: impl Fn(ToolUpdate) + Send + Sync + 'static) -> Self {
        Self {
            callback: Some(Arc::new(callback)),
        }
    }

    /// Deliver one update to the host, if configured.
    pub fn emit(&self, update: ToolUpdate) {
        if let Some(callback) = &self.callback {
            callback(update);
        }
    }
}

/// Context supplied to an explicit tool capability.
#[derive(Clone, Debug)]
pub struct ToolContext {
    /// Cancellation state owned by the run.
    pub cancellation: CancellationToken,
    /// Arbitrary serialized host metadata for this execution.
    pub metadata: Option<SerializedJson>,
}

/// A registered executable capability.
pub trait AgentTool: Send + Sync {
    /// Stable tool name used by assistant calls.
    fn name(&self) -> &str;
    /// Prompt-facing description.
    fn description(&self) -> &str;
    /// Raw JSON Schema-compatible value for arguments.
    ///
    /// This intentionally uses the protocol JSON representation rather than a
    /// Rust schema DSL or a Serde value. The validator adapter remains private
    /// to the core and must not leak its dependency types here.
    fn schema(&self) -> &JsonValue;
    /// Execution ordering policy.
    fn execution_mode(&self) -> ToolExecutionMode {
        ToolExecutionMode::Parallel
    }
    /// Execute the call on the caller-owned executor.
    fn execute<'a>(
        &'a self,
        call: ToolCall,
        context: ToolContext,
        updates: ToolUpdateSink,
    ) -> ToolFuture<'a>;
}

/// Prompt-facing, non-executable description of a tool.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolDefinition {
    /// Stable tool name.
    pub name: String,
    /// Description supplied to the model.
    pub description: String,
    /// Raw JSON Schema-compatible value.
    pub schema: JsonValue,
    /// Scheduling mode.
    pub execution_mode: ToolExecutionMode,
}

impl ToolDefinition {
    /// Build a definition from a capability.
    pub fn from_tool(tool: &dyn AgentTool) -> Self {
        Self {
            name: tool.name().to_owned(),
            description: tool.description().to_owned(),
            schema: tool.schema().clone(),
            execution_mode: tool.execution_mode(),
        }
    }
}

/// Ordered registry of explicit tools.
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn AgentTool>>,
    order: Vec<String>,
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRegistry")
            .field("names", &self.order)
            .finish()
    }
}

impl ToolRegistry {
    /// Add a tool, replacing an existing tool with the same name without changing order.
    pub fn insert(&mut self, tool: Arc<dyn AgentTool>) {
        let name = tool.name().to_owned();
        if !self.tools.contains_key(&name) {
            self.order.push(name.clone());
        }
        self.tools.insert(name, tool);
    }

    /// Remove a named tool and return it to the caller.
    pub fn remove(&mut self, name: &str) -> Option<Arc<dyn AgentTool>> {
        let removed = self.tools.remove(name);
        if removed.is_some() {
            self.order.retain(|entry| entry != name);
        }
        removed
    }

    /// Find an executable tool by name.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn AgentTool>> {
        self.tools.get(name)
    }

    /// Return registered names in prompt/source order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.order.iter().map(String::as_str)
    }

    /// Return prompt definitions in registry order.
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.order
            .iter()
            .filter_map(|name| self.tools.get(name))
            .map(|tool| ToolDefinition::from_tool(tool.as_ref()))
            .collect()
    }

    /// Whether no capabilities are registered.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}
