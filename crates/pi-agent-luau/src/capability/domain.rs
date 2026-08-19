//! Public ABI and domain types for host-controlled Luau capabilities.

use pi_agent_protocol::{JsonError, JsonValue};
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

/// The currently supported capability ABI version.
pub const CAPABILITY_ABI_VERSION: u64 = 1;

/// A virtual module through which a host may expose a capability.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CapabilityModule {
    /// Agent lifecycle and model-facing registry observations.
    Agent,
    /// Explicit world effects such as MCP or a host-owned filesystem.
    World,
    /// Structured, non-semantic trace annotations.
    Trace,
    /// Explicit task lifecycle requests.
    Task,
    /// Pure JSON conversion helpers.
    Json,
    /// Host-provided time operations.
    Time,
}

impl CapabilityModule {
    /// Return the Luau virtual module name, including its `@` prefix.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "@agent",
            Self::World => "@world",
            Self::Trace => "@trace",
            Self::Task => "@task",
            Self::Json => "@json",
            Self::Time => "@time",
        }
    }

    pub(super) fn json_key(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::World => "world",
            Self::Trace => "trace",
            Self::Task => "task",
            Self::Json => "json",
            Self::Time => "time",
        }
    }
}

impl fmt::Display for CapabilityModule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Agent operations that can be granted to a policy.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AgentOperation {
    /// Observe the current agent event stream through the host adapter.
    Events,
    /// Observe model-facing tool registration metadata.
    Tools,
    /// Request that the current run stop at a Rust-owned boundary.
    Stop,
}

impl AgentOperation {
    /// Return the canonical manifest spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Events => "events",
            Self::Tools => "tools",
            Self::Stop => "stop",
        }
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "events" => Self::Events,
            "tools" => Self::Tools,
            "stop" => Self::Stop,
            _ => return None,
        })
    }
}

/// MCP methods that may be scoped to a named server and optional target.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum McpOperation {
    /// Invoke an MCP tool.  The optional target is the exact tool name.
    Call,
    /// List resources exposed by a server.
    ListResources,
    /// Read one resource.  The optional target is the exact resource URI.
    ReadResource,
}

impl McpOperation {
    /// Return the canonical manifest spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Call => "call",
            Self::ListResources => "list_resources",
            Self::ReadResource => "read_resource",
        }
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "call" => Self::Call,
            "list_resources" => Self::ListResources,
            "read_resource" => Self::ReadResource,
            _ => return None,
        })
    }
}

/// A server- and optionally target-scoped MCP permission.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct McpPermission {
    /// Host-local MCP server identity.  This is not a process or URL.
    pub server: String,
    /// MCP method allowed on `server`.
    pub operation: McpOperation,
    /// Exact tool name or resource URI, when the method supports a target.
    pub target: Option<String>,
}

impl McpPermission {
    /// Construct a server-scoped permission, rejecting empty names.
    pub fn new(
        server: impl Into<String>,
        operation: McpOperation,
        target: Option<impl Into<String>>,
    ) -> Result<Self, CapabilityError> {
        let server = server.into();
        let target = target.map(Into::into);
        validate_non_empty("MCP server", &server)?;
        if let Some(target) = target.as_deref() {
            validate_non_empty("MCP target", target)?;
        }
        Ok(Self {
            server,
            operation,
            target,
        })
    }

    fn allows(&self, requested: &Self) -> bool {
        self.server == requested.server
            && self.operation == requested.operation
            && self.target == requested.target
    }
}

/// World effects that can be granted to a policy.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum WorldOperation {
    /// Read through a host-owned filesystem abstraction.
    FsRead,
    /// Write through a host-owned filesystem abstraction.
    FsWrite,
    /// Execute through a host-owned command abstraction.
    Exec,
    /// Invoke one explicitly scoped MCP server method.
    Mcp(McpPermission),
}

impl WorldOperation {
    /// Construct a scoped MCP operation.
    pub fn mcp(
        server: impl Into<String>,
        operation: McpOperation,
        target: Option<impl Into<String>>,
    ) -> Result<Self, CapabilityError> {
        McpPermission::new(server, operation, target).map(Self::Mcp)
    }
}

/// Trace operations that can be granted to a policy.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TraceOperation {
    /// Add a structured annotation to the host trace.
    Emit,
}

impl TraceOperation {
    /// Return the canonical manifest spelling.
    pub const fn as_str(self) -> &'static str {
        "emit"
    }
}

/// Task operations that can be granted to a policy.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TaskOperation {
    /// Request a Rust-owned child task.
    Spawn,
    /// Await a host-owned task result.
    Join,
    /// Request cancellation of a host-owned task.
    Cancel,
    /// Observe task status.
    Status,
}

impl TaskOperation {
    /// Return the canonical manifest spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Spawn => "spawn",
            Self::Join => "join",
            Self::Cancel => "cancel",
            Self::Status => "status",
        }
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "spawn" => Self::Spawn,
            "join" => Self::Join,
            "cancel" => Self::Cancel,
            "status" => Self::Status,
            _ => return None,
        })
    }
}

/// Pure JSON operations exposed through the virtual `@json` module.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum JsonOperation {
    /// Parse a JSON string into a Luau-safe value.
    Parse,
    /// Encode a Luau-safe value as JSON.
    Stringify,
}

impl JsonOperation {
    /// Return the canonical manifest spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Parse => "parse",
            Self::Stringify => "stringify",
        }
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "parse" => Self::Parse,
            "stringify" => Self::Stringify,
            _ => return None,
        })
    }
}

/// Time operations whose source is selected by the host.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TimeOperation {
    /// Read the host's explicitly selected current time source.
    Now,
    /// Await a host-owned delay or timer.
    Sleep,
}

impl TimeOperation {
    /// Return the canonical manifest spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Now => "now",
            Self::Sleep => "sleep",
        }
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "now" => Self::Now,
            "sleep" => Self::Sleep,
            _ => return None,
        })
    }
}

/// A typed operation in a manifest or capability request.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CapabilityOperation {
    /// An `@agent` operation.
    Agent(AgentOperation),
    /// An `@world` operation.
    World(WorldOperation),
    /// An `@trace` operation.
    Trace(TraceOperation),
    /// An `@task` operation.
    Task(TaskOperation),
    /// An `@json` operation.
    Json(JsonOperation),
    /// An `@time` operation.
    Time(TimeOperation),
}

impl CapabilityOperation {
    /// Return the module to which this operation belongs.
    pub const fn module(&self) -> CapabilityModule {
        match self {
            Self::Agent(_) => CapabilityModule::Agent,
            Self::World(_) => CapabilityModule::World,
            Self::Trace(_) => CapabilityModule::Trace,
            Self::Task(_) => CapabilityModule::Task,
            Self::Json(_) => CapabilityModule::Json,
            Self::Time(_) => CapabilityModule::Time,
        }
    }

    pub(super) fn sort_key(&self) -> String {
        match self {
            Self::Agent(operation) => operation.as_str().to_owned(),
            Self::World(WorldOperation::FsRead) => "fs.read".to_owned(),
            Self::World(WorldOperation::FsWrite) => "fs.write".to_owned(),
            Self::World(WorldOperation::Exec) => "exec".to_owned(),
            Self::World(WorldOperation::Mcp(permission)) => format!(
                "mcp.{}.{}.{}",
                permission.server,
                permission.operation.as_str(),
                permission.target.as_deref().unwrap_or("")
            ),
            Self::Trace(operation) => operation.as_str().to_owned(),
            Self::Task(operation) => operation.as_str().to_owned(),
            Self::Json(operation) => operation.as_str().to_owned(),
            Self::Time(operation) => operation.as_str().to_owned(),
        }
    }
}

impl From<AgentOperation> for CapabilityOperation {
    fn from(operation: AgentOperation) -> Self {
        Self::Agent(operation)
    }
}

impl From<WorldOperation> for CapabilityOperation {
    fn from(operation: WorldOperation) -> Self {
        Self::World(operation)
    }
}

impl From<TraceOperation> for CapabilityOperation {
    fn from(operation: TraceOperation) -> Self {
        Self::Trace(operation)
    }
}

impl From<TaskOperation> for CapabilityOperation {
    fn from(operation: TaskOperation) -> Self {
        Self::Task(operation)
    }
}

impl From<JsonOperation> for CapabilityOperation {
    fn from(operation: JsonOperation) -> Self {
        Self::Json(operation)
    }
}

impl From<TimeOperation> for CapabilityOperation {
    fn from(operation: TimeOperation) -> Self {
        Self::Time(operation)
    }
}

/// A validated set of operations for exactly one virtual module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityGrant {
    pub(super) module: CapabilityModule,
    pub(super) operations: BTreeSet<CapabilityOperation>,
}

impl CapabilityGrant {
    /// Construct and validate a grant for one module.
    pub fn new(
        module: CapabilityModule,
        operations: impl IntoIterator<Item = CapabilityOperation>,
    ) -> Result<Self, CapabilityError> {
        let mut validated = BTreeSet::new();
        for operation in operations {
            if operation.module() != module {
                return Err(CapabilityError::OperationModuleMismatch {
                    module,
                    operation: operation_name(&operation),
                });
            }
            if !validated.insert(operation.clone()) {
                return Err(CapabilityError::DuplicateOperation {
                    module,
                    operation: operation_name(&operation),
                });
            }
        }
        Ok(Self {
            module,
            operations: validated,
        })
    }

    /// Return the module receiving this grant.
    pub const fn module(&self) -> CapabilityModule {
        self.module
    }

    /// Return operations in deterministic order.
    pub fn operations(&self) -> impl Iterator<Item = &CapabilityOperation> {
        self.operations.iter()
    }

    pub(super) fn allows(&self, requested: &CapabilityOperation) -> bool {
        match requested {
            CapabilityOperation::World(WorldOperation::Mcp(requested)) => {
                self.operations.iter().any(|operation| match operation {
                    CapabilityOperation::World(WorldOperation::Mcp(granted)) => {
                        granted.allows(requested)
                    }
                    _ => false,
                })
            }
            _ => self.operations.contains(requested),
        }
    }
}

/// A typed invocation submitted to a host capability provider.
#[derive(Clone, Debug, PartialEq)]
pub struct CapabilityRequest {
    /// The operation being requested, including MCP scope where applicable.
    pub operation: CapabilityOperation,
    /// Operation-specific arguments.  The provider owns the schema.
    pub arguments: JsonValue,
}

impl CapabilityRequest {
    /// Construct a request with JSON arguments.
    pub fn new(operation: CapabilityOperation, arguments: JsonValue) -> Self {
        Self {
            operation,
            arguments,
        }
    }
}

/// A provider result crossing the capability boundary.
#[derive(Clone, Debug, PartialEq)]
pub struct CapabilityResponse {
    /// Provider-owned structured result.
    pub value: JsonValue,
}

impl CapabilityResponse {
    /// Construct a response from a protocol JSON value.
    pub fn new(value: JsonValue) -> Self {
        Self { value }
    }
}

/// A host implementation of one or more capability operations.
pub trait CapabilityProvider: Send + Sync {
    /// Execute an already-authorized request.
    fn provide(
        &self,
        request: &CapabilityRequest,
    ) -> Result<CapabilityResponse, CapabilityProviderError>;
}

/// Provider failure after manifest authorization succeeded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CapabilityProviderError {
    /// The host cancelled the operation before it settled.
    Cancelled,
    /// The provider rejected the request or encountered a safe-to-report failure.
    Failed(String),
}

impl fmt::Display for CapabilityProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("capability operation cancelled"),
            Self::Failed(message) => formatter.write_str(message),
        }
    }
}

impl Error for CapabilityProviderError {}

/// Validation and encoding errors at the capability boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CapabilityError {
    /// The underlying protocol JSON could not be parsed or encoded.
    Json(JsonError),
    /// A JSON value did not have the expected shape.
    Malformed {
        /// JSON path at which validation failed.
        path: String,
        /// Stable explanation of the invalid shape.
        message: String,
    },
    /// A manifest used an ABI version this crate does not understand.
    UnsupportedAbiVersion {
        /// ABI version understood by this crate.
        expected: u64,
        /// ABI version supplied by the manifest.
        actual: u64,
    },
    /// A module was granted more than once.
    DuplicateGrant {
        /// Module that was granted twice.
        module: CapabilityModule,
    },
    /// An operation was listed more than once.
    DuplicateOperation {
        /// Module containing the duplicate operation.
        module: CapabilityModule,
        /// Canonical operation name that appeared twice.
        operation: String,
    },
    /// An operation belongs to a different module than its grant.
    OperationModuleMismatch {
        /// Module to which the operation was incorrectly assigned.
        module: CapabilityModule,
        /// Canonical operation name that belongs elsewhere.
        operation: String,
    },
    /// A request did not have a matching explicit grant.
    Denied {
        /// Module requested by the ungranted operation.
        module: CapabilityModule,
        /// Canonical operation name denied by the manifest.
        operation: String,
    },
    /// A provider failed after authorization.
    Provider(CapabilityProviderError),
}

impl fmt::Display for CapabilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(formatter, "capability JSON error: {error}"),
            Self::Malformed { path, message } => {
                write!(formatter, "malformed capability {path}: {message}")
            }
            Self::UnsupportedAbiVersion { expected, actual } => {
                write!(
                    formatter,
                    "unsupported capability ABI version {actual}; expected {expected}"
                )
            }
            Self::DuplicateGrant { module } => {
                write!(formatter, "duplicate capability grant for {module}")
            }
            Self::DuplicateOperation { module, operation } => {
                write!(formatter, "duplicate operation {operation:?} in {module}")
            }
            Self::OperationModuleMismatch { module, operation } => {
                write!(
                    formatter,
                    "operation {operation:?} does not belong to {module}"
                )
            }
            Self::Denied { module, operation } => {
                write!(
                    formatter,
                    "capability denied: {module} operation {operation:?}"
                )
            }
            Self::Provider(error) => write!(formatter, "capability provider failed: {error}"),
        }
    }
}

impl Error for CapabilityError {}

pub(super) fn operation_name(operation: &CapabilityOperation) -> String {
    operation.sort_key()
}

pub(super) fn validate_non_empty(kind: &str, value: &str) -> Result<(), CapabilityError> {
    if value.trim().is_empty() {
        Err(CapabilityError::Malformed {
            path: kind.to_owned(),
            message: "must not be empty".to_owned(),
        })
    } else {
        Ok(())
    }
}
