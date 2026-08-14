//! Versioned, host-controlled capabilities exposed to Luau extensions.
//!
//! This module describes authority; it does not acquire authority.  A host
//! parses a [`CapabilityManifest`], installs only the providers it intends to
//! expose, and uses [`CapabilityGate`] at every call boundary.  The manifest
//! is deliberately represented with the dependency-free protocol
//! [`JsonValue`] so that it can be persisted, hashed, and exchanged without
//! binding the ABI to `mlua`, an executor, or a provider SDK.
//!
//! The canonical JSON shape is:
//!
//! ```json
//! {
//!   "abi_version": 1,
//!   "agent": ["events", "tools", "stop"],
//!   "world": [
//!     "fs.read",
//!     {"mcp": {"server": "runebench", "operation": "call", "target": "execute_code"}}
//!   ],
//!   "trace": ["emit"]
//! }
//! ```
//!
//! Missing module members mean no grant.  There is no wildcard operation, and
//! an MCP grant without a server is rejected.  A missing MCP target means any
//! target on that explicitly named server and operation; hosts should prefer
//! exact targets where practical.

use pi_agent_protocol::{JsonAdapter, JsonError, JsonNumber, JsonValue};
use std::collections::{BTreeMap, BTreeSet};
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

    fn json_key(self) -> &'static str {
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

    fn parse(value: &str) -> Option<Self> {
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

    fn parse(value: &str) -> Option<Self> {
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
            && (self.target.is_none() || self.target == requested.target)
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

    fn parse(value: &str) -> Option<Self> {
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

    fn parse(value: &str) -> Option<Self> {
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

    fn parse(value: &str) -> Option<Self> {
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

    fn sort_key(&self) -> String {
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
    module: CapabilityModule,
    operations: BTreeSet<CapabilityOperation>,
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

    fn allows(&self, requested: &CapabilityOperation) -> bool {
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

/// A complete, deterministic capability grant set for one VM.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityManifest {
    abi_version: u64,
    grants: BTreeMap<CapabilityModule, CapabilityGrant>,
}

impl CapabilityManifest {
    /// Construct a manifest from one optional grant per virtual module.
    pub fn new(grants: impl IntoIterator<Item = CapabilityGrant>) -> Result<Self, CapabilityError> {
        let mut by_module = BTreeMap::new();
        for grant in grants {
            let module = grant.module;
            if by_module.insert(module, grant).is_some() {
                return Err(CapabilityError::DuplicateGrant { module });
            }
        }
        Ok(Self {
            abi_version: CAPABILITY_ABI_VERSION,
            grants: by_module,
        })
    }

    /// Construct an empty manifest with no authority.
    pub fn empty() -> Self {
        Self {
            abi_version: CAPABILITY_ABI_VERSION,
            grants: BTreeMap::new(),
        }
    }

    /// Return the ABI version encoded by this manifest.
    pub const fn abi_version(&self) -> u64 {
        self.abi_version
    }

    /// Return the grant for a module, if that module has authority.
    pub fn grant(&self, module: CapabilityModule) -> Option<&CapabilityGrant> {
        self.grants.get(&module)
    }

    /// Return all grants in module-name order.
    pub fn grants(&self) -> impl Iterator<Item = &CapabilityGrant> {
        self.grants.values()
    }

    /// Parse and validate a complete manifest from JSON text.
    pub fn parse_json(input: &str) -> Result<Self, CapabilityError> {
        let value = JsonValue::parse(input).map_err(CapabilityError::Json)?;
        Self::from_json(&value)
    }

    /// Encode this manifest as canonical, deterministic JSON text.
    pub fn to_json_string(&self) -> Result<String, CapabilityError> {
        self.to_json()
            .and_then(|value| value.to_json_string().map_err(CapabilityError::Json))
    }

    /// Check whether a request is authorized by this manifest.
    pub fn check(&self, request: &CapabilityRequest) -> Result<(), CapabilityError> {
        let module = request.operation.module();
        if self
            .grant(module)
            .is_some_and(|grant| grant.allows(&request.operation))
        {
            Ok(())
        } else {
            Err(CapabilityError::Denied {
                module,
                operation: operation_name(&request.operation),
            })
        }
    }

    fn from_json(value: &JsonValue) -> Result<Self, CapabilityError> {
        let object = expect_object(value, "manifest")?;
        let abi_version = required_u64(object, "abi_version")?;
        if abi_version != CAPABILITY_ABI_VERSION {
            return Err(CapabilityError::UnsupportedAbiVersion {
                expected: CAPABILITY_ABI_VERSION,
                actual: abi_version,
            });
        }

        let known = [
            "abi_version",
            "agent",
            "world",
            "trace",
            "task",
            "json",
            "time",
        ];
        reject_unknown_keys(object, &known, "manifest")?;

        let mut grants = Vec::new();
        for module in [
            CapabilityModule::Agent,
            CapabilityModule::World,
            CapabilityModule::Trace,
            CapabilityModule::Task,
            CapabilityModule::Json,
            CapabilityModule::Time,
        ] {
            if let Some(value) = object.get(module.json_key()) {
                grants.push(parse_grant(module, value)?);
            }
        }
        let mut manifest = Self::new(grants)?;
        manifest.abi_version = abi_version;
        Ok(manifest)
    }

    fn to_json(&self) -> Result<JsonValue, CapabilityError> {
        let mut object = BTreeMap::new();
        object.insert(
            "abi_version".to_owned(),
            JsonValue::Number(JsonNumber::Unsigned(self.abi_version)),
        );
        for grant in self.grants.values() {
            let operations = grant
                .operations
                .iter()
                .map(operation_to_json)
                .collect::<Result<Vec<_>, _>>()?;
            object.insert(
                grant.module.json_key().to_owned(),
                JsonValue::Array(operations),
            );
        }
        Ok(JsonValue::Object(object))
    }
}

impl JsonAdapter for CapabilityManifest {
    fn to_json(&self) -> Result<JsonValue, JsonError> {
        CapabilityManifest::to_json(self).map_err(|error| JsonError::Message(error.to_string()))
    }

    fn from_json(value: &JsonValue) -> Result<Self, JsonError> {
        CapabilityManifest::from_json(value).map_err(|error| JsonError::Message(error.to_string()))
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

/// The host-side authorization gate around a capability provider.
pub struct CapabilityGate<P> {
    manifest: CapabilityManifest,
    provider: P,
}

impl<P> CapabilityGate<P> {
    /// Bind one provider to one immutable manifest.
    pub fn new(manifest: CapabilityManifest, provider: P) -> Self {
        Self { manifest, provider }
    }

    /// Borrow the immutable authority manifest.
    pub fn manifest(&self) -> &CapabilityManifest {
        &self.manifest
    }
}

impl<P> CapabilityGate<P>
where
    P: CapabilityProvider,
{
    /// Check the manifest before invoking the provider.
    pub fn provide(
        &self,
        request: &CapabilityRequest,
    ) -> Result<CapabilityResponse, CapabilityError> {
        self.manifest.check(request)?;
        self.provider
            .provide(request)
            .map_err(CapabilityError::Provider)
    }
}

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

fn parse_grant(
    module: CapabilityModule,
    value: &JsonValue,
) -> Result<CapabilityGrant, CapabilityError> {
    let values = expect_array(value, module.json_key())?;
    let mut operations = Vec::new();
    for (index, value) in values.iter().enumerate() {
        operations.push(parse_operation(
            module,
            value,
            &format!("{}[{index}]", module.json_key()),
        )?);
    }
    CapabilityGrant::new(module, operations)
}

fn parse_operation(
    module: CapabilityModule,
    value: &JsonValue,
    path: &str,
) -> Result<CapabilityOperation, CapabilityError> {
    match value {
        JsonValue::String(name) => parse_simple_operation(module, name, path),
        JsonValue::Object(object) if module == CapabilityModule::World => {
            reject_unknown_keys(object, &["mcp"], path)?;
            let mcp = object
                .get("mcp")
                .ok_or_else(|| malformed(path, "missing mcp"))?;
            parse_mcp_operation(mcp, path).map(CapabilityOperation::World)
        }
        _ => Err(malformed(path, "expected a known operation string")),
    }
}

fn parse_simple_operation(
    module: CapabilityModule,
    name: &str,
    path: &str,
) -> Result<CapabilityOperation, CapabilityError> {
    let operation = match module {
        CapabilityModule::Agent => AgentOperation::parse(name).map(CapabilityOperation::Agent),
        CapabilityModule::World => match name {
            "fs.read" => Some(CapabilityOperation::World(WorldOperation::FsRead)),
            "fs.write" => Some(CapabilityOperation::World(WorldOperation::FsWrite)),
            "exec" => Some(CapabilityOperation::World(WorldOperation::Exec)),
            _ => None,
        },
        CapabilityModule::Trace => TraceOperation::Emit
            .as_str()
            .eq(name)
            .then_some(CapabilityOperation::Trace(TraceOperation::Emit)),
        CapabilityModule::Task => TaskOperation::parse(name).map(CapabilityOperation::Task),
        CapabilityModule::Json => JsonOperation::parse(name).map(CapabilityOperation::Json),
        CapabilityModule::Time => TimeOperation::parse(name).map(CapabilityOperation::Time),
    };
    operation.ok_or_else(|| malformed(path, format!("unknown operation {name:?} for {module}")))
}

fn parse_mcp_operation(value: &JsonValue, path: &str) -> Result<WorldOperation, CapabilityError> {
    let object = expect_object(value, &format!("{path}.mcp"))?;
    reject_unknown_keys(
        object,
        &["server", "operation", "target"],
        &format!("{path}.mcp"),
    )?;
    let server = required_string(object, "server", &format!("{path}.mcp"))?;
    let operation_name = required_string(object, "operation", &format!("{path}.mcp"))?;
    let operation = McpOperation::parse(&operation_name).ok_or_else(|| {
        malformed(
            format!("{path}.mcp.operation"),
            format!("unknown MCP operation {operation_name:?}"),
        )
    })?;
    let target = object
        .get("target")
        .map(|value| expect_string(value, &format!("{path}.mcp.target")))
        .transpose()?
        .map(str::to_owned);
    McpPermission::new(server, operation, target)
        .map(WorldOperation::Mcp)
        .map_err(|error| malformed(path, error.to_string()))
}

fn operation_to_json(operation: &CapabilityOperation) -> Result<JsonValue, CapabilityError> {
    let _ = operation.sort_key();
    Ok(match operation {
        CapabilityOperation::Agent(operation) => JsonValue::String(operation.as_str().to_owned()),
        CapabilityOperation::World(WorldOperation::FsRead) => {
            JsonValue::String("fs.read".to_owned())
        }
        CapabilityOperation::World(WorldOperation::FsWrite) => {
            JsonValue::String("fs.write".to_owned())
        }
        CapabilityOperation::World(WorldOperation::Exec) => JsonValue::String("exec".to_owned()),
        CapabilityOperation::World(WorldOperation::Mcp(permission)) => {
            let mut mcp = BTreeMap::new();
            mcp.insert(
                "server".to_owned(),
                JsonValue::String(permission.server.clone()),
            );
            mcp.insert(
                "operation".to_owned(),
                JsonValue::String(permission.operation.as_str().to_owned()),
            );
            if let Some(target) = permission.target.as_deref() {
                mcp.insert("target".to_owned(), JsonValue::String(target.to_owned()));
            }
            JsonValue::object([("mcp", JsonValue::Object(mcp))])
        }
        CapabilityOperation::Trace(operation) => JsonValue::String(operation.as_str().to_owned()),
        CapabilityOperation::Task(operation) => JsonValue::String(operation.as_str().to_owned()),
        CapabilityOperation::Json(operation) => JsonValue::String(operation.as_str().to_owned()),
        CapabilityOperation::Time(operation) => JsonValue::String(operation.as_str().to_owned()),
    })
}

fn operation_name(operation: &CapabilityOperation) -> String {
    operation.sort_key()
}

fn validate_non_empty(kind: &str, value: &str) -> Result<(), CapabilityError> {
    if value.trim().is_empty() {
        Err(malformed(kind, "must not be empty"))
    } else {
        Ok(())
    }
}

fn expect_object<'a>(
    value: &'a JsonValue,
    path: &str,
) -> Result<&'a BTreeMap<String, JsonValue>, CapabilityError> {
    match value {
        JsonValue::Object(object) => Ok(object),
        other => Err(malformed(
            path,
            format!("expected object, got {:?}", other.kind()),
        )),
    }
}

fn expect_array<'a>(
    value: &'a JsonValue,
    path: &str,
) -> Result<&'a Vec<JsonValue>, CapabilityError> {
    match value {
        JsonValue::Array(array) => Ok(array),
        other => Err(malformed(
            path,
            format!("expected array, got {:?}", other.kind()),
        )),
    }
}

fn expect_string<'a>(value: &'a JsonValue, path: &str) -> Result<&'a str, CapabilityError> {
    match value {
        JsonValue::String(value) if !value.trim().is_empty() => Ok(value),
        JsonValue::String(_) => Err(malformed(path, "must not be empty")),
        other => Err(malformed(
            path,
            format!("expected string, got {:?}", other.kind()),
        )),
    }
}

fn required_string(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
    path: &str,
) -> Result<String, CapabilityError> {
    let value = object
        .get(key)
        .ok_or_else(|| malformed(format!("{path}.{key}"), "missing required field"))?;
    Ok(expect_string(value, &format!("{path}.{key}"))?.to_owned())
}

fn required_u64(object: &BTreeMap<String, JsonValue>, key: &str) -> Result<u64, CapabilityError> {
    match object.get(key) {
        Some(JsonValue::Number(JsonNumber::Unsigned(value))) => Ok(*value),
        Some(JsonValue::Number(JsonNumber::Signed(value))) if *value >= 0 => Ok(*value as u64),
        Some(value) => Err(malformed(
            key,
            format!("expected non-negative integer, got {:?}", value.kind()),
        )),
        None => Err(malformed(key, "missing required field")),
    }
}

fn reject_unknown_keys(
    object: &BTreeMap<String, JsonValue>,
    known: &[&str],
    path: &str,
) -> Result<(), CapabilityError> {
    if let Some(key) = object.keys().find(|key| !known.contains(&key.as_str())) {
        return Err(malformed(path, format!("unknown field {key:?}")));
    }
    Ok(())
}

fn malformed(path: impl Into<String>, message: impl Into<String>) -> CapabilityError {
    CapabilityError::Malformed {
        path: path.into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn world_mcp(target: Option<&str>) -> CapabilityOperation {
        WorldOperation::mcp("runebench", McpOperation::Call, target)
            .expect("test MCP permission")
            .into()
    }

    #[test]
    fn manifest_round_trips_to_deterministic_json() {
        let manifest = CapabilityManifest::new([
            CapabilityGrant::new(
                CapabilityModule::World,
                [
                    CapabilityOperation::World(WorldOperation::Exec),
                    world_mcp(Some("execute_code")),
                    CapabilityOperation::World(WorldOperation::FsRead),
                ],
            )
            .expect("valid world grant"),
            CapabilityGrant::new(
                CapabilityModule::Agent,
                [CapabilityOperation::Agent(AgentOperation::Stop)],
            )
            .expect("valid agent grant"),
        ])
        .expect("valid manifest");

        let encoded = manifest.to_json_string().expect("manifest encodes");
        assert_eq!(
            encoded,
            r#"{"abi_version":1,"agent":["stop"],"world":["fs.read","exec",{"mcp":{"operation":"call","server":"runebench","target":"execute_code"}}]}"#
        );
        assert_eq!(
            CapabilityManifest::parse_json(&encoded).expect("manifest decodes"),
            manifest
        );
    }

    #[test]
    fn request_is_denied_without_an_exact_module_and_operation_grant() {
        let manifest = CapabilityManifest::new([CapabilityGrant::new(
            CapabilityModule::World,
            [CapabilityOperation::World(WorldOperation::FsRead)],
        )
        .expect("valid grant")])
        .expect("valid manifest");

        let request = CapabilityRequest::new(
            CapabilityOperation::World(WorldOperation::FsWrite),
            JsonValue::Null,
        );
        assert!(matches!(
            manifest.check(&request),
            Err(CapabilityError::Denied {
                module: CapabilityModule::World,
                ..
            })
        ));
    }

    #[test]
    fn mcp_permissions_are_server_and_target_scoped() {
        let manifest = CapabilityManifest::parse_json(
            r#"{"abi_version":1,"world":[{"mcp":{"server":"runebench","operation":"call","target":"execute_code"}}]}"#,
        )
        .expect("valid MCP manifest");

        let allowed = CapabilityRequest::new(world_mcp(Some("execute_code")), JsonValue::Null);
        let wrong_tool = CapabilityRequest::new(world_mcp(Some("read_state")), JsonValue::Null);
        let wrong_server = CapabilityRequest::new(
            WorldOperation::mcp("other", McpOperation::Call, Some("execute_code"))
                .expect("test MCP permission")
                .into(),
            JsonValue::Null,
        );
        assert!(manifest.check(&allowed).is_ok());
        assert!(manifest.check(&wrong_tool).is_err());
        assert!(manifest.check(&wrong_server).is_err());
    }

    #[test]
    fn malformed_grants_reject_unknown_modules_operations_and_fields() {
        for input in [
            r#"{"abi_version":1,"filesystem":["read"]}"#,
            r#"{"abi_version":1,"world":["network"]}"#,
            r#"{"abi_version":1,"world":[{"mcp":{"server":"runebench","operation":"call","unexpected":true}}]}"#,
            r#"{"abi_version":1,"world":[{"mcp":{"server":"","operation":"call"}}]}"#,
        ] {
            assert!(
                CapabilityManifest::parse_json(input).is_err(),
                "accepted {input}"
            );
        }
    }

    #[test]
    fn duplicate_grants_and_operations_are_rejected() {
        let duplicate_grant = CapabilityManifest::new([
            CapabilityGrant::new(
                CapabilityModule::Agent,
                [CapabilityOperation::Agent(AgentOperation::Events)],
            )
            .expect("valid grant"),
            CapabilityGrant::new(
                CapabilityModule::Agent,
                [CapabilityOperation::Agent(AgentOperation::Stop)],
            )
            .expect("valid grant"),
        ]);
        assert!(matches!(
            duplicate_grant,
            Err(CapabilityError::DuplicateGrant {
                module: CapabilityModule::Agent
            })
        ));

        let duplicate_operation = CapabilityGrant::new(
            CapabilityModule::Agent,
            [
                CapabilityOperation::Agent(AgentOperation::Events),
                CapabilityOperation::Agent(AgentOperation::Events),
            ],
        );
        assert!(matches!(
            duplicate_operation,
            Err(CapabilityError::DuplicateOperation {
                module: CapabilityModule::Agent,
                ..
            })
        ));
        assert!(
            CapabilityManifest::parse_json(r#"{"abi_version":1,"agent":["events","events"]}"#)
                .is_err()
        );
    }

    struct CountingProvider(AtomicUsize);

    impl CapabilityProvider for CountingProvider {
        fn provide(
            &self,
            _request: &CapabilityRequest,
        ) -> Result<CapabilityResponse, CapabilityProviderError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(CapabilityResponse::new(JsonValue::Bool(true)))
        }
    }

    #[test]
    fn gate_checks_before_provider_and_returns_structured_result() {
        let provider = CountingProvider(AtomicUsize::new(0));
        let gate = CapabilityGate::new(
            CapabilityManifest::new([CapabilityGrant::new(
                CapabilityModule::Agent,
                [CapabilityOperation::Agent(AgentOperation::Stop)],
            )
            .expect("valid grant")])
            .expect("valid manifest"),
            provider,
        );
        let request = CapabilityRequest::new(
            CapabilityOperation::Agent(AgentOperation::Stop),
            JsonValue::Null,
        );
        assert_eq!(
            gate.provide(&request).expect("authorized request").value,
            JsonValue::Bool(true)
        );
        assert_eq!(gate.provider.0.load(Ordering::Relaxed), 1);

        let denied = CapabilityRequest::new(
            CapabilityOperation::Agent(AgentOperation::Events),
            JsonValue::Null,
        );
        assert!(gate.provide(&denied).is_err());
        assert_eq!(gate.provider.0.load(Ordering::Relaxed), 1);
    }
}
