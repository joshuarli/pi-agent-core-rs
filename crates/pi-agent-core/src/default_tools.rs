//! Batteries-included, explicit coding tools for the pinned Pi profile.
//!
//! The standard tools in this module are deliberately capability-shaped.  A
//! [`DefaultCodingTools`] value owns an explicit workspace root and an
//! [`CodingOperations`] adapter; it never consults the process working
//! directory, home directory, Pi configuration, or a session.  Embeddings can
//! replace the complete adapter (for example with a VM or remote filesystem),
//! or replace individual tools in the returned [`ToolRegistry`].
//!
//! The local adapter uses only the standard library.  Its shell environment is
//! empty unless the caller explicitly supplies variables through
//! [`CommandEnvironment`].

use crate::error::ToolError;
use crate::scheduler::CancellationToken;
use crate::tool::{
    AgentTool, ToolCall, ToolContext, ToolFuture, ToolRegistry, ToolResult, ToolUpdate,
    ToolUpdateSink,
};
use pi_agent_protocol::{JsonNumber, JsonValue};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command;
use std::sync::Arc;

/// A future returned by a host operation adapter.
pub type OperationFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, OperationError>> + Send + 'a>>;

/// A host-side failure from a coding-tool operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationError {
    message: String,
}

impl OperationError {
    /// Construct an operation failure.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Borrow the host-provided message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for OperationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for OperationError {}

/// Metadata needed by the standard tools without exposing `std::fs::Metadata`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntryMetadata {
    /// Whether the entry is a directory.
    pub is_directory: bool,
}

/// A directory entry returned by an operation adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryEntry {
    /// Entry name, not an ambient absolute path.
    pub name: String,
    /// Whether the entry is a directory.
    pub is_directory: bool,
}

/// Output from an explicit shell operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandOutput {
    /// Process exit code, if the process exited normally.
    pub exit_code: Option<i32>,
    /// Captured standard output.
    pub stdout: Vec<u8>,
    /// Captured standard error.
    pub stderr: Vec<u8>,
}

/// Explicit environment policy for [`bash`](DefaultCodingTools::bash) calls.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CommandEnvironment {
    variables: Vec<(OsString, OsString)>,
}

impl CommandEnvironment {
    /// Create an empty environment.  This is the default and is deterministic.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Copy the current process environment explicitly.
    ///
    /// Calling this method is an intentional authority decision by the embedding;
    /// the default coding profile never calls it implicitly.
    pub fn inherited() -> Self {
        Self {
            variables: std::env::vars_os().collect(),
        }
    }

    /// Add or replace one environment variable.
    pub fn with(mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        let name = name.into();
        if let Some((_, current)) = self.variables.iter_mut().find(|(key, _)| *key == name) {
            *current = value.into();
        } else {
            self.variables.push((name, value.into()));
        }
        self
    }

    fn apply(&self, command: &mut Command) {
        command.env_clear();
        command.envs(self.variables.iter().map(|(key, value)| (key, value)));
    }
}

/// Explicit host operations used by all standard tools.
///
/// Every path has already been checked against the [`WorkspaceRoot`] before it
/// reaches this boundary.  An adapter may therefore map the path to a remote
/// namespace, while retaining the same tool schemas and result semantics.
pub trait CodingOperations: Send + Sync {
    /// Read all bytes from one file.
    fn read_file<'a>(&'a self, path: &'a Path) -> OperationFuture<'a, Vec<u8>>;
    /// Write all bytes to one file.
    fn write_file<'a>(&'a self, path: &'a Path, content: &'a [u8]) -> OperationFuture<'a, ()>;
    /// Create a directory and all missing parents.
    fn create_dir_all<'a>(&'a self, path: &'a Path) -> OperationFuture<'a, ()>;
    /// Inspect one path.
    fn metadata<'a>(&'a self, path: &'a Path) -> OperationFuture<'a, EntryMetadata>;
    /// List one directory.
    fn read_dir<'a>(&'a self, path: &'a Path) -> OperationFuture<'a, Vec<DirectoryEntry>>;
    /// Find paths below `root` using a glob pattern.
    fn find_files<'a>(
        &'a self,
        root: &'a Path,
        pattern: &'a str,
        limit: usize,
    ) -> OperationFuture<'a, Vec<String>>;
    /// Search files below `root` for a pattern.
    fn grep_files<'a>(
        &'a self,
        root: &'a Path,
        pattern: &'a str,
        options: GrepOptions,
    ) -> OperationFuture<'a, Vec<GrepMatch>>;
    /// Execute one command in the explicit workspace.
    fn execute_command<'a>(
        &'a self,
        command: &'a str,
        cwd: &'a Path,
        timeout_seconds: Option<f64>,
        environment: &'a CommandEnvironment,
        cancellation: CancellationToken,
        updates: ToolUpdateSink,
    ) -> OperationFuture<'a, CommandOutput>;
}

/// Options passed to a grep operation adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrepOptions {
    /// Search case-insensitively.
    pub ignore_case: bool,
    /// Treat `pattern` literally rather than as the supported regex subset.
    pub literal: bool,
    /// Number of context lines on each side of a match.
    pub context: usize,
    /// Maximum number of matching lines.
    pub limit: usize,
    /// Optional basename/path glob filter.
    pub glob: Option<String>,
}

/// One grep result line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrepMatch {
    /// Path relative to the search root, using `/` separators.
    pub path: String,
    /// One-indexed source line.
    pub line: usize,
    /// Rendered matching line.
    pub text: String,
}

/// An explicit, canonicalized workspace authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceRoot(PathBuf);

impl WorkspaceRoot {
    /// Canonicalize an existing directory as a workspace root.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, OperationError> {
        let root = std::fs::canonicalize(path.as_ref()).map_err(|error| {
            OperationError::new(format!("workspace root is not accessible: {error}"))
        })?;
        if !root.is_dir() {
            return Err(OperationError::new("workspace root is not a directory"));
        }
        Ok(Self(root))
    }

    /// Borrow the canonical root path.
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// Resolve an existing path, rejecting absolute paths outside the root and
    /// symlinks that resolve outside it.
    pub fn resolve_existing(&self, input: &str) -> Result<PathBuf, OperationError> {
        let candidate = self.lexically_resolve(input)?;
        let resolved = std::fs::canonicalize(&candidate)
            .map_err(|error| OperationError::new(format!("path is not accessible: {error}")))?;
        self.ensure_inside(&resolved)?;
        Ok(resolved)
    }

    /// Resolve a path that may not exist yet (for `write`). Existing parents
    /// are canonicalized so symlink escapes remain rejected.
    pub fn resolve_for_write(&self, input: &str) -> Result<PathBuf, OperationError> {
        let candidate = self.lexically_resolve(input)?;
        let mut existing = candidate.clone();
        let mut suffix = Vec::new();
        while !existing.exists() {
            let name = existing.file_name().ok_or_else(|| {
                OperationError::new("write path has no existing workspace parent")
            })?;
            suffix.push(name.to_os_string());
            existing.pop();
        }
        let canonical_existing = std::fs::canonicalize(&existing).map_err(|error| {
            OperationError::new(format!("write parent is not accessible: {error}"))
        })?;
        self.ensure_inside(&canonical_existing)?;
        let mut resolved = canonical_existing;
        for component in suffix.iter().rev() {
            resolved.push(component);
        }
        self.ensure_inside(&resolved)?;
        Ok(resolved)
    }

    fn lexically_resolve(&self, input: &str) -> Result<PathBuf, OperationError> {
        if input.is_empty() {
            return Err(OperationError::new("path cannot be empty"));
        }
        let raw = Path::new(input);
        let source = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            self.0.join(raw)
        };
        let mut result = PathBuf::new();
        for component in source.components() {
            use std::path::Component;
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    if !result.pop() {
                        return Err(OperationError::new("path escapes the workspace root"));
                    }
                }
                Component::RootDir | Component::Prefix(_) => result.push(component.as_os_str()),
                Component::Normal(value) => result.push(value),
            }
        }
        self.ensure_inside(&result)?;
        Ok(result)
    }

    fn ensure_inside(&self, path: &Path) -> Result<(), OperationError> {
        if path == self.as_path() || path.starts_with(self.as_path()) {
            Ok(())
        } else {
            Err(OperationError::new("path escapes the workspace root"))
        }
    }
}

/// Standard local filesystem/process implementation.
#[derive(Clone, Debug)]
pub struct LocalCodingOperations;

impl CodingOperations for LocalCodingOperations {
    fn read_file<'a>(&'a self, path: &'a Path) -> OperationFuture<'a, Vec<u8>> {
        Box::pin(async move {
            std::fs::read(path).map_err(|error| OperationError::new(error.to_string()))
        })
    }

    fn write_file<'a>(&'a self, path: &'a Path, content: &'a [u8]) -> OperationFuture<'a, ()> {
        Box::pin(async move {
            std::fs::write(path, content).map_err(|error| OperationError::new(error.to_string()))
        })
    }

    fn create_dir_all<'a>(&'a self, path: &'a Path) -> OperationFuture<'a, ()> {
        Box::pin(async move {
            std::fs::create_dir_all(path).map_err(|error| OperationError::new(error.to_string()))
        })
    }

    fn metadata<'a>(&'a self, path: &'a Path) -> OperationFuture<'a, EntryMetadata> {
        Box::pin(async move {
            let metadata =
                std::fs::metadata(path).map_err(|error| OperationError::new(error.to_string()))?;
            Ok(EntryMetadata {
                is_directory: metadata.is_dir(),
            })
        })
    }

    fn read_dir<'a>(&'a self, path: &'a Path) -> OperationFuture<'a, Vec<DirectoryEntry>> {
        Box::pin(async move {
            let mut entries = Vec::new();
            for entry in
                std::fs::read_dir(path).map_err(|error| OperationError::new(error.to_string()))?
            {
                let entry = entry.map_err(|error| OperationError::new(error.to_string()))?;
                let metadata = entry
                    .metadata()
                    .map_err(|error| OperationError::new(error.to_string()))?;
                entries.push(DirectoryEntry {
                    name: entry.file_name().to_string_lossy().into_owned(),
                    is_directory: metadata.is_dir(),
                });
            }
            Ok(entries)
        })
    }

    fn find_files<'a>(
        &'a self,
        root: &'a Path,
        pattern: &'a str,
        limit: usize,
    ) -> OperationFuture<'a, Vec<String>> {
        Box::pin(async move {
            let matcher = GlobMatcher::new(pattern)?;
            let mut output = Vec::new();
            walk_files(root, root, &matcher, limit, &mut output)?;
            output.sort();
            Ok(output)
        })
    }

    fn grep_files<'a>(
        &'a self,
        root: &'a Path,
        pattern: &'a str,
        options: GrepOptions,
    ) -> OperationFuture<'a, Vec<GrepMatch>> {
        Box::pin(async move { local_grep(root, pattern, options) })
    }

    fn execute_command<'a>(
        &'a self,
        command: &'a str,
        cwd: &'a Path,
        timeout_seconds: Option<f64>,
        environment: &'a CommandEnvironment,
        cancellation: CancellationToken,
        updates: ToolUpdateSink,
    ) -> OperationFuture<'a, CommandOutput> {
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(OperationError::new("cancelled"));
            }
            let mut process = Command::new("bash");
            process.arg("-c").arg(command).current_dir(cwd);
            environment.apply(&mut process);
            // The local synchronous adapter cannot interrupt a child without
            // creating a worker thread. Host adapters can provide true async
            // cancellation; local cancellation is checked at both boundaries.
            let output = process
                .output()
                .map_err(|error| OperationError::new(error.to_string()))?;
            if cancellation.is_cancelled() {
                return Err(OperationError::new("cancelled"));
            }
            if let Some(timeout) = timeout_seconds {
                // Validation happens at the tool boundary. Retaining this
                // branch documents that the local adapter does not claim to
                // enforce a timeout after a blocking child has started.
                let _ = timeout;
            }
            let mut update = Vec::new();
            update.extend_from_slice(&output.stdout);
            update.extend_from_slice(&output.stderr);
            if !update.is_empty() {
                updates.emit(ToolUpdate {
                    content: String::from_utf8_lossy(&update).into_owned(),
                    details: None,
                });
            }
            Ok(CommandOutput {
                exit_code: output.status.code(),
                stdout: output.stdout,
                stderr: output.stderr,
            })
        })
    }
}

/// The explicit batteries-included standard tool set.
#[derive(Clone)]
pub struct DefaultCodingTools {
    workspace: WorkspaceRoot,
    operations: Arc<dyn CodingOperations>,
    environment: CommandEnvironment,
}

impl std::fmt::Debug for DefaultCodingTools {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DefaultCodingTools")
            .field("workspace", &self.workspace)
            .field("environment", &self.environment)
            .finish_non_exhaustive()
    }
}

impl DefaultCodingTools {
    /// Construct the local standard tools for one existing workspace directory.
    pub fn new(workspace: impl AsRef<Path>) -> Result<Self, OperationError> {
        Self::with_operations(workspace, Arc::new(LocalCodingOperations))
    }

    /// Construct standard tools over caller-owned operations.
    pub fn with_operations(
        workspace: impl AsRef<Path>,
        operations: Arc<dyn CodingOperations>,
    ) -> Result<Self, OperationError> {
        Ok(Self {
            workspace: WorkspaceRoot::new(workspace)?,
            operations,
            environment: CommandEnvironment::empty(),
        })
    }

    /// Replace the explicit shell environment policy.
    pub fn with_environment(mut self, environment: CommandEnvironment) -> Self {
        self.environment = environment;
        self
    }

    /// Borrow the canonical workspace authority.
    pub fn workspace(&self) -> &WorkspaceRoot {
        &self.workspace
    }

    /// Return the default active coding tools in captured order.
    pub fn coding_tools(&self) -> Vec<Arc<dyn AgentTool>> {
        vec![self.read(), self.bash(), self.edit(), self.write()]
    }

    /// Return every pinned standard factory in captured order.
    pub fn all_tools(&self) -> Vec<Arc<dyn AgentTool>> {
        vec![
            self.read(),
            self.bash(),
            self.edit(),
            self.write(),
            self.grep(),
            self.find(),
            self.ls(),
        ]
    }

    /// Build a registry containing the active default coding tools.
    pub fn registry(&self) -> ToolRegistry {
        let mut registry = ToolRegistry::default();
        for tool in self.coding_tools() {
            registry.insert(tool);
        }
        registry
    }

    /// Construct the read capability.
    pub fn read(&self) -> Arc<dyn AgentTool> {
        Arc::new(ReadTool::new(
            self.workspace.clone(),
            Arc::clone(&self.operations),
        ))
    }
    /// Construct the bash capability.
    pub fn bash(&self) -> Arc<dyn AgentTool> {
        Arc::new(BashTool::new(
            self.workspace.clone(),
            Arc::clone(&self.operations),
            self.environment.clone(),
        ))
    }
    /// Construct the edit capability.
    pub fn edit(&self) -> Arc<dyn AgentTool> {
        Arc::new(EditTool::new(
            self.workspace.clone(),
            Arc::clone(&self.operations),
        ))
    }
    /// Construct the write capability.
    pub fn write(&self) -> Arc<dyn AgentTool> {
        Arc::new(WriteTool::new(
            self.workspace.clone(),
            Arc::clone(&self.operations),
        ))
    }
    /// Construct the grep capability.
    pub fn grep(&self) -> Arc<dyn AgentTool> {
        Arc::new(GrepTool::new(
            self.workspace.clone(),
            Arc::clone(&self.operations),
        ))
    }
    /// Construct the find capability.
    pub fn find(&self) -> Arc<dyn AgentTool> {
        Arc::new(FindTool::new(
            self.workspace.clone(),
            Arc::clone(&self.operations),
        ))
    }
    /// Construct the ls capability.
    pub fn ls(&self) -> Arc<dyn AgentTool> {
        Arc::new(LsTool::new(
            self.workspace.clone(),
            Arc::clone(&self.operations),
        ))
    }
}

const MAX_OUTPUT_BYTES: usize = 50 * 1024;
const MAX_OUTPUT_LINES: usize = 2_000;

fn schema_object(
    required: &[&str],
    properties: impl IntoIterator<Item = (&'static str, JsonValue)>,
) -> JsonValue {
    let mut schema = BTreeMap::from([
        ("type".to_owned(), JsonValue::String("object".to_owned())),
        ("properties".to_owned(), JsonValue::object(properties)),
    ]);
    if !required.is_empty() {
        schema.insert(
            "required".to_owned(),
            JsonValue::Array(
                required
                    .iter()
                    .map(|name| JsonValue::String((*name).to_owned()))
                    .collect(),
            ),
        );
    }
    JsonValue::Object(schema)
}

fn schema_string(description: &'static str) -> JsonValue {
    JsonValue::object([
        ("type", JsonValue::String("string".to_owned())),
        ("description", JsonValue::String(description.to_owned())),
    ])
}

fn schema_number(description: &'static str) -> JsonValue {
    JsonValue::object([
        ("type", JsonValue::String("number".to_owned())),
        ("description", JsonValue::String(description.to_owned())),
    ])
}

fn schema_boolean(description: &'static str) -> JsonValue {
    JsonValue::object([
        ("type", JsonValue::String("boolean".to_owned())),
        ("description", JsonValue::String(description.to_owned())),
    ])
}

fn read_schema() -> JsonValue {
    schema_object(
        &["path"],
        [
            (
                "path",
                schema_string("Path to the file to read (relative or absolute)"),
            ),
            (
                "offset",
                schema_number("Line number to start reading from (1-indexed)"),
            ),
            ("limit", schema_number("Maximum number of lines to read")),
        ],
    )
}

fn bash_schema() -> JsonValue {
    schema_object(
        &["command"],
        [
            ("command", schema_string("Bash command to execute")),
            (
                "timeout",
                schema_number("Timeout in seconds (optional, no default timeout)"),
            ),
        ],
    )
}

fn edit_schema() -> JsonValue {
    schema_object(
        &["path", "edits"],
        [
            ("path", schema_string("Path to the file to edit (relative or absolute)")),
            (
                "edits",
                JsonValue::object([
                    ("type", JsonValue::String("array".to_owned())),
                    (
                        "items",
                        schema_object(
                            &["oldText", "newText"],
                            [
                                ("oldText", schema_string("Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call.")),
                                ("newText", schema_string("Replacement text for this targeted edit.")),
                            ],
                        ),
                    ),
                    ("description", JsonValue::String("One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead.".to_owned())),
                ]),
            ),
        ],
    )
}

fn write_schema() -> JsonValue {
    schema_object(
        &["path", "content"],
        [
            (
                "path",
                schema_string("Path to the file to write (relative or absolute)"),
            ),
            ("content", schema_string("Content to write to the file")),
        ],
    )
}

fn grep_schema() -> JsonValue {
    schema_object(
        &["pattern"],
        [
            (
                "pattern",
                schema_string("Search pattern (regex or literal string)"),
            ),
            (
                "path",
                schema_string("Directory or file to search (default: current directory)"),
            ),
            (
                "glob",
                schema_string("Filter files by glob pattern, e.g. '*.ts' or '**/*.spec.ts'"),
            ),
            (
                "ignoreCase",
                schema_boolean("Case-insensitive search (default: false)"),
            ),
            (
                "literal",
                schema_boolean("Treat pattern as literal string instead of regex (default: false)"),
            ),
            (
                "context",
                schema_number("Number of lines to show before and after each match (default: 0)"),
            ),
            (
                "limit",
                schema_number("Maximum number of matches to return (default: 100)"),
            ),
        ],
    )
}

fn find_schema() -> JsonValue {
    schema_object(
        &["pattern"],
        [
            (
                "pattern",
                schema_string(
                    "Glob pattern to match files, e.g. '*.ts', '**/*.json', or 'src/**/*.spec.ts'",
                ),
            ),
            (
                "path",
                schema_string("Directory to search in (default: current directory)"),
            ),
            (
                "limit",
                schema_number("Maximum number of results (default: 1000)"),
            ),
        ],
    )
}

fn ls_schema() -> JsonValue {
    schema_object(
        &[],
        [
            (
                "path",
                schema_string("Directory to list (default: current directory)"),
            ),
            (
                "limit",
                schema_number("Maximum number of entries to return (default: 500)"),
            ),
        ],
    )
}

fn invalid(name: &str, message: impl Into<String>) -> ToolError {
    ToolError::InvalidArguments {
        tool: name.to_owned(),
        message: message.into(),
    }
}

fn operation_failure(name: &str, error: OperationError) -> ToolError {
    if error.message() == "cancelled" {
        ToolError::Cancelled {
            tool: name.to_owned(),
        }
    } else {
        ToolError::Execution {
            tool: name.to_owned(),
            message: error.to_string(),
        }
    }
}

fn result_ok(call: &ToolCall, content: impl Into<String>) -> ToolResult {
    ToolResult {
        tool_call_id: call.id.clone(),
        content: content.into(),
        details: None,
        usage: None,
        added_tool_names: Vec::new(),
        terminate: false,
        is_error: false,
    }
}

fn parse_object(
    name: &str,
    call: &ToolCall,
) -> Result<Arc<BTreeMap<String, JsonValue>>, ToolError> {
    match JsonValue::parse(call.arguments.as_str()) {
        Ok(JsonValue::Object(value)) => Ok(Arc::new(value)),
        Ok(_) => Err(invalid(name, "arguments must be a JSON object")),
        Err(_) => Err(invalid(name, "arguments must be valid JSON")),
    }
}

fn field<'a>(
    name: &str,
    object: &'a BTreeMap<String, JsonValue>,
    key: &str,
) -> Result<&'a JsonValue, ToolError> {
    object
        .get(key)
        .ok_or_else(|| invalid(name, format!("missing required argument {key:?}")))
}

fn string_field(
    name: &str,
    object: &BTreeMap<String, JsonValue>,
    key: &str,
) -> Result<String, ToolError> {
    match field(name, object, key)? {
        JsonValue::String(value) => Ok(value.clone()),
        _ => Err(invalid(name, format!("argument {key:?} must be a string"))),
    }
}

fn optional_string(
    name: &str,
    object: &BTreeMap<String, JsonValue>,
    key: &str,
) -> Result<Option<String>, ToolError> {
    object
        .get(key)
        .map(|value| match value {
            JsonValue::String(value) => Ok(value.clone()),
            _ => Err(invalid(name, format!("argument {key:?} must be a string"))),
        })
        .transpose()
}

fn optional_bool(
    name: &str,
    object: &BTreeMap<String, JsonValue>,
    key: &str,
) -> Result<Option<bool>, ToolError> {
    object
        .get(key)
        .map(|value| match value {
            JsonValue::Bool(value) => Ok(*value),
            _ => Err(invalid(name, format!("argument {key:?} must be a boolean"))),
        })
        .transpose()
}

fn optional_positive_usize(
    name: &str,
    object: &BTreeMap<String, JsonValue>,
    key: &str,
) -> Result<Option<usize>, ToolError> {
    object
        .get(key)
        .map(|value| number_to_usize(name, key, value))
        .transpose()
}

fn number_to_usize(name: &str, key: &str, value: &JsonValue) -> Result<usize, ToolError> {
    let number = match value {
        JsonValue::Number(number) => *number,
        _ => return Err(invalid(name, format!("argument {key:?} must be a number"))),
    };
    let integer = match number {
        JsonNumber::Unsigned(value) if value > 0 => value,
        JsonNumber::Signed(value) if value > 0 => value as u64,
        JsonNumber::Float(value) if value.is_finite() && value > 0.0 && value.fract() == 0.0 => {
            value as u64
        }
        _ => {
            return Err(invalid(
                name,
                format!("argument {key:?} must be a positive integer"),
            ))
        }
    };
    usize::try_from(integer).map_err(|_| invalid(name, format!("argument {key:?} is too large")))
}

fn optional_timeout(
    name: &str,
    object: &BTreeMap<String, JsonValue>,
) -> Result<Option<f64>, ToolError> {
    let Some(value) = object.get("timeout") else {
        return Ok(None);
    };
    let number = match value {
        JsonValue::Number(JsonNumber::Float(value)) => *value,
        JsonValue::Number(JsonNumber::Unsigned(value)) => *value as f64,
        JsonValue::Number(JsonNumber::Signed(value)) => *value as f64,
        _ => return Err(invalid(name, "argument \"timeout\" must be a number")),
    };
    if !number.is_finite() || number <= 0.0 || number > 2_147_483.647 {
        return Err(invalid(
            name,
            "timeout must be a finite positive number no greater than 2147.483647 seconds",
        ));
    }
    Ok(Some(number))
}

fn path_error(name: &str, path: Result<PathBuf, OperationError>) -> Result<PathBuf, ToolError> {
    path.map_err(|error| operation_failure(name, error))
}

fn check_cancelled(name: &str, context: &ToolContext) -> Result<(), ToolError> {
    if context.cancellation.is_cancelled() {
        Err(ToolError::Cancelled {
            tool: name.to_owned(),
        })
    } else {
        Ok(())
    }
}

fn truncate_output(bytes: &[u8]) -> (String, bool) {
    let text = String::from_utf8_lossy(bytes).into_owned();
    let mut lines = text.lines().map(str::to_owned).collect::<Vec<_>>();
    let mut truncated = false;
    if lines.len() > MAX_OUTPUT_LINES {
        truncated = true;
        lines = lines.split_off(lines.len() - MAX_OUTPUT_LINES);
    }
    let mut output = lines.join("\n");
    if output.len() > MAX_OUTPUT_BYTES {
        truncated = true;
        let mut start = output.len() - MAX_OUTPUT_BYTES;
        while start < output.len() && !output.is_char_boundary(start) {
            start += 1;
        }
        output = output[start..].to_owned();
    }
    (output, truncated)
}

/// Truncate file content from the head, preserving complete UTF-8 lines.
///
/// Bash intentionally keeps its tail because the final diagnostics are normally most useful;
/// read follows Pi's file-oriented behavior and keeps the beginning instead.  Keeping this
/// separate from [`truncate_output`] also prevents a byte limit from slicing a UTF-8 character.
fn truncate_read_output(bytes: &[u8]) -> (String, bool) {
    let text = String::from_utf8_lossy(bytes).into_owned();
    let mut lines = text.split('\n').collect::<Vec<_>>();
    if text.ends_with('\n') {
        lines.pop();
    }
    let total_bytes = text.len();
    if lines.len() <= MAX_OUTPUT_LINES && total_bytes <= MAX_OUTPUT_BYTES {
        return (text, false);
    }

    let mut output = String::new();
    for (output_lines, line) in lines.into_iter().take(MAX_OUTPUT_LINES).enumerate() {
        let separator = if output_lines == 0 { 0 } else { 1 };
        if output.len() + separator + line.len() > MAX_OUTPUT_BYTES {
            break;
        }
        if separator != 0 {
            output.push('\n');
        }
        output.push_str(line);
    }
    (output, true)
}

struct ReadTool {
    root: WorkspaceRoot,
    operations: Arc<dyn CodingOperations>,
}

impl ReadTool {
    fn new(root: WorkspaceRoot, operations: Arc<dyn CodingOperations>) -> Self {
        Self { root, operations }
    }
}

impl AgentTool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }
    fn description(&self) -> &str {
        "Read the contents of a file. Supports text files and images (jpg, png, gif, webp, bmp). Images are sent as attachments. For text files, output is truncated to 2000 lines or 50KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete."
    }
    fn schema(&self) -> &JsonValue {
        static_schema_read()
    }
    fn execute<'a>(
        &'a self,
        call: ToolCall,
        context: ToolContext,
        _updates: ToolUpdateSink,
    ) -> ToolFuture<'a> {
        let args = match parse_object(self.name(), &call).and_then(|object| {
            Ok((
                string_field(self.name(), &object, "path")?,
                optional_positive_usize(self.name(), &object, "offset")?,
                optional_positive_usize(self.name(), &object, "limit")?,
            ))
        }) {
            Ok(args) => args,
            Err(error) => return Box::pin(std::future::ready(Err(error))),
        };
        let path = match path_error(self.name(), self.root.resolve_existing(&args.0)) {
            Ok(path) => path,
            Err(error) => return Box::pin(std::future::ready(Err(error))),
        };
        if let Err(error) = check_cancelled(self.name(), &context) {
            return Box::pin(std::future::ready(Err(error)));
        }
        let operations = Arc::clone(&self.operations);
        Box::pin(async move {
            let bytes = operations
                .read_file(&path)
                .await
                .map_err(|error| operation_failure("read", error))?;
            let text = String::from_utf8_lossy(&bytes);
            let start = args.1.unwrap_or(1);
            let lines = text.split('\n').collect::<Vec<_>>();
            if start > lines.len() && !lines.is_empty() {
                return Err(ToolError::Execution {
                    tool: "read".into(),
                    message: format!("offset {start} is beyond end of file"),
                });
            }
            let begin = start.saturating_sub(1).min(lines.len());
            let selected = if let Some(limit) = args.2 {
                let end = begin.saturating_add(limit).min(lines.len());
                lines[begin..end].join("\n")
            } else {
                lines[begin..].join("\n")
            };
            let (output, truncated) = truncate_read_output(selected.as_bytes());
            let suffix = if truncated { "\n[truncated]" } else { "" };
            Ok(result_ok(&call, format!("{}{}", output, suffix)))
        })
    }
}

fn static_schema_read() -> &'static JsonValue {
    use std::sync::OnceLock;
    static VALUE: OnceLock<JsonValue> = OnceLock::new();
    VALUE.get_or_init(read_schema)
}

struct BashTool {
    root: WorkspaceRoot,
    operations: Arc<dyn CodingOperations>,
    environment: CommandEnvironment,
}

impl BashTool {
    fn new(
        root: WorkspaceRoot,
        operations: Arc<dyn CodingOperations>,
        environment: CommandEnvironment,
    ) -> Self {
        Self {
            root,
            operations,
            environment,
        }
    }
}

impl AgentTool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }
    fn description(&self) -> &str {
        "Execute a bash command in the current working directory. Returns stdout and stderr. Output is truncated to last 2000 lines or 50KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds."
    }
    fn schema(&self) -> &JsonValue {
        static_schema_bash()
    }
    fn execute<'a>(
        &'a self,
        call: ToolCall,
        context: ToolContext,
        updates: ToolUpdateSink,
    ) -> ToolFuture<'a> {
        let args = match parse_object(self.name(), &call).and_then(|object| {
            Ok((
                string_field(self.name(), &object, "command")?,
                optional_timeout(self.name(), &object)?,
            ))
        }) {
            Ok(args) => args,
            Err(error) => return Box::pin(std::future::ready(Err(error))),
        };
        let operations = Arc::clone(&self.operations);
        let root = self.root.clone();
        let environment = self.environment.clone();
        Box::pin(async move {
            check_cancelled("bash", &context)?;
            let output = operations
                .execute_command(
                    &args.0,
                    root.as_path(),
                    args.1,
                    &environment,
                    context.cancellation.clone(),
                    updates,
                )
                .await
                .map_err(|error| operation_failure("bash", error))?;
            let mut combined = output.stdout;
            if !output.stderr.is_empty() {
                if !combined.is_empty() {
                    combined.extend_from_slice(b"\n");
                }
                combined.extend_from_slice(&output.stderr);
            }
            let (text, truncated) = truncate_output(&combined);
            if output.exit_code.unwrap_or(1) != 0 {
                return Ok(ToolResult {
                    tool_call_id: call.id.clone(),
                    content: if text.is_empty() {
                        format!(
                            "command exited with status {}",
                            output.exit_code.unwrap_or(-1)
                        )
                    } else {
                        text
                    },
                    details: None,
                    usage: None,
                    added_tool_names: Vec::new(),
                    terminate: false,
                    is_error: true,
                });
            }
            let mut content = text;
            if truncated {
                content.push_str("\n[truncated]");
            }
            if content.is_empty() {
                content.push_str("(no output)");
            }
            Ok(result_ok(&call, content))
        })
    }
}

fn static_schema_bash() -> &'static JsonValue {
    use std::sync::OnceLock;
    static VALUE: OnceLock<JsonValue> = OnceLock::new();
    VALUE.get_or_init(bash_schema)
}

struct WriteTool {
    root: WorkspaceRoot,
    operations: Arc<dyn CodingOperations>,
}

impl WriteTool {
    fn new(root: WorkspaceRoot, operations: Arc<dyn CodingOperations>) -> Self {
        Self { root, operations }
    }
}

impl AgentTool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }
    fn description(&self) -> &str {
        "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories."
    }
    fn schema(&self) -> &JsonValue {
        static_schema_write()
    }
    fn execute<'a>(
        &'a self,
        call: ToolCall,
        context: ToolContext,
        _updates: ToolUpdateSink,
    ) -> ToolFuture<'a> {
        let args = match parse_object(self.name(), &call).and_then(|object| {
            Ok((
                string_field(self.name(), &object, "path")?,
                string_field(self.name(), &object, "content")?,
            ))
        }) {
            Ok(args) => args,
            Err(error) => return Box::pin(std::future::ready(Err(error))),
        };
        let path = match path_error(self.name(), self.root.resolve_for_write(&args.0)) {
            Ok(path) => path,
            Err(error) => return Box::pin(std::future::ready(Err(error))),
        };
        let parent = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.root.as_path().to_path_buf());
        let operations = Arc::clone(&self.operations);
        Box::pin(async move {
            check_cancelled("write", &context)?;
            operations
                .create_dir_all(&parent)
                .await
                .map_err(|error| operation_failure("write", error))?;
            operations
                .write_file(&path, args.1.as_bytes())
                .await
                .map_err(|error| operation_failure("write", error))?;
            Ok(result_ok(
                &call,
                format!("Successfully wrote {} bytes to {}", args.1.len(), args.0),
            ))
        })
    }
}

fn static_schema_write() -> &'static JsonValue {
    use std::sync::OnceLock;
    static VALUE: OnceLock<JsonValue> = OnceLock::new();
    VALUE.get_or_init(write_schema)
}

struct EditTool {
    root: WorkspaceRoot,
    operations: Arc<dyn CodingOperations>,
}

impl EditTool {
    fn new(root: WorkspaceRoot, operations: Arc<dyn CodingOperations>) -> Self {
        Self { root, operations }
    }
}

#[derive(Clone)]
struct EditSpec {
    old: String,
    new: String,
}

impl AgentTool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }
    fn description(&self) -> &str {
        "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes."
    }
    fn schema(&self) -> &JsonValue {
        static_schema_edit()
    }
    fn execute<'a>(
        &'a self,
        call: ToolCall,
        context: ToolContext,
        _updates: ToolUpdateSink,
    ) -> ToolFuture<'a> {
        let args = match parse_edit_args(self.name(), &call) {
            Ok(args) => args,
            Err(error) => return Box::pin(std::future::ready(Err(error))),
        };
        let path = match path_error(self.name(), self.root.resolve_existing(&args.0)) {
            Ok(path) => path,
            Err(error) => return Box::pin(std::future::ready(Err(error))),
        };
        let operations = Arc::clone(&self.operations);
        Box::pin(async move {
            check_cancelled("edit", &context)?;
            let original = operations
                .read_file(&path)
                .await
                .map_err(|error| operation_failure("edit", error))?;
            let mut text = String::from_utf8(original).map_err(|_| ToolError::Execution {
                tool: "edit".into(),
                message: "file is not valid UTF-8".into(),
            })?;
            let mut locations = Vec::new();
            for edit in &args.1 {
                if edit.old.is_empty() {
                    return Err(ToolError::InvalidArguments {
                        tool: "edit".into(),
                        message: "oldText cannot be empty".into(),
                    });
                }
                let matches = text
                    .match_indices(&edit.old)
                    .map(|(start, _)| start)
                    .collect::<Vec<_>>();
                if matches.len() != 1 {
                    return Err(ToolError::Execution {
                        tool: "edit".into(),
                        message: format!(
                            "oldText must match exactly once; found {} matches",
                            matches.len()
                        ),
                    });
                }
                let start = matches[0];
                let end = start + edit.old.len();
                locations.push((start, end, edit.clone()));
            }
            locations.sort_by_key(|(start, _, _)| *start);
            for pair in locations.windows(2) {
                if pair[0].1 > pair[1].0 {
                    return Err(ToolError::Execution {
                        tool: "edit".into(),
                        message: "edits overlap in the original file".into(),
                    });
                }
            }
            for (start, end, edit) in locations.into_iter().rev() {
                text.replace_range(start..end, &edit.new);
            }
            operations
                .write_file(&path, text.as_bytes())
                .await
                .map_err(|error| operation_failure("edit", error))?;
            Ok(result_ok(
                &call,
                format!(
                    "Successfully replaced {} block(s) in {}.",
                    args.1.len(),
                    args.0
                ),
            ))
        })
    }
}

fn parse_edit_args(name: &str, call: &ToolCall) -> Result<(String, Vec<EditSpec>), ToolError> {
    let object = parse_object(name, call)?;
    let path = string_field(name, &object, "path")?;
    let edits = match field(name, &object, "edits")? {
        JsonValue::Array(edits) => edits,
        _ => return Err(invalid(name, "argument \"edits\" must be an array")),
    };
    if edits.is_empty() {
        return Err(invalid(name, "argument \"edits\" cannot be empty"));
    }
    let mut result = Vec::with_capacity(edits.len());
    for edit in edits {
        let object = match edit {
            JsonValue::Object(object) => object,
            _ => return Err(invalid(name, "each edit must be an object")),
        };
        result.push(EditSpec {
            old: string_field(name, object, "oldText")?,
            new: string_field(name, object, "newText")?,
        });
    }
    Ok((path, result))
}

fn static_schema_edit() -> &'static JsonValue {
    use std::sync::OnceLock;
    static VALUE: OnceLock<JsonValue> = OnceLock::new();
    VALUE.get_or_init(edit_schema)
}

struct GrepTool {
    root: WorkspaceRoot,
    operations: Arc<dyn CodingOperations>,
}

impl GrepTool {
    fn new(root: WorkspaceRoot, operations: Arc<dyn CodingOperations>) -> Self {
        Self { root, operations }
    }
}

impl AgentTool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }
    fn description(&self) -> &str {
        "Search file contents for a pattern. Returns matching lines with file paths and line numbers. Respects .gitignore. Output is truncated to 100 matches or 50KB (whichever is hit first). Long lines are truncated to 500 chars."
    }
    fn schema(&self) -> &JsonValue {
        static_schema_grep()
    }
    fn execute<'a>(
        &'a self,
        call: ToolCall,
        context: ToolContext,
        _updates: ToolUpdateSink,
    ) -> ToolFuture<'a> {
        let args = match parse_object(self.name(), &call).and_then(|object| {
            Ok((
                string_field(self.name(), &object, "pattern")?,
                optional_string(self.name(), &object, "path")?,
                optional_string(self.name(), &object, "glob")?,
                optional_bool(self.name(), &object, "ignoreCase")?.unwrap_or(false),
                optional_bool(self.name(), &object, "literal")?.unwrap_or(false),
                optional_positive_usize(self.name(), &object, "context")?.unwrap_or(0),
                optional_positive_usize(self.name(), &object, "limit")?.unwrap_or(100),
            ))
        }) {
            Ok(args) => args,
            Err(error) => return Box::pin(std::future::ready(Err(error))),
        };
        if !args.4 && TinyPattern::new(&args.0, args.3).is_err() {
            return Box::pin(std::future::ready(Err(invalid(
                self.name(),
                "pattern is not a supported regular expression",
            ))));
        }
        let path = match path_error(
            self.name(),
            self.root.resolve_existing(args.1.as_deref().unwrap_or(".")),
        ) {
            Ok(path) => path,
            Err(error) => return Box::pin(std::future::ready(Err(error))),
        };
        let operations = Arc::clone(&self.operations);
        Box::pin(async move {
            check_cancelled("grep", &context)?;
            let search_root = path;
            let matches = operations
                .grep_files(
                    &search_root,
                    &args.0,
                    GrepOptions {
                        ignore_case: args.3,
                        literal: args.4,
                        context: args.5,
                        limit: args.6,
                        glob: args.2,
                    },
                )
                .await
                .map_err(|error| operation_failure("grep", error))?;
            if matches.is_empty() {
                return Ok(result_ok(&call, "No matches found"));
            }
            let mut output = String::new();
            for item in matches {
                if !output.is_empty() {
                    output.push('\n');
                }
                output.push_str(&format!("{}:{}: {}", item.path, item.line, item.text));
            }
            Ok(result_ok(&call, output))
        })
    }
}

fn static_schema_grep() -> &'static JsonValue {
    use std::sync::OnceLock;
    static VALUE: OnceLock<JsonValue> = OnceLock::new();
    VALUE.get_or_init(grep_schema)
}

struct FindTool {
    root: WorkspaceRoot,
    operations: Arc<dyn CodingOperations>,
}

impl FindTool {
    fn new(root: WorkspaceRoot, operations: Arc<dyn CodingOperations>) -> Self {
        Self { root, operations }
    }
}

impl AgentTool for FindTool {
    fn name(&self) -> &str {
        "find"
    }
    fn description(&self) -> &str {
        "Search for files by glob pattern. Returns matching file paths relative to the search directory. Respects .gitignore. Output is truncated to 1000 results or 50KB (whichever is hit first)."
    }
    fn schema(&self) -> &JsonValue {
        static_schema_find()
    }
    fn execute<'a>(
        &'a self,
        call: ToolCall,
        context: ToolContext,
        _updates: ToolUpdateSink,
    ) -> ToolFuture<'a> {
        let args = match parse_object(self.name(), &call).and_then(|object| {
            Ok((
                string_field(self.name(), &object, "pattern")?,
                optional_string(self.name(), &object, "path")?,
                optional_positive_usize(self.name(), &object, "limit")?.unwrap_or(1000),
            ))
        }) {
            Ok(args) => args,
            Err(error) => return Box::pin(std::future::ready(Err(error))),
        };
        if GlobMatcher::new(&args.0).is_err() {
            return Box::pin(std::future::ready(Err(invalid(
                self.name(),
                "pattern is not a supported glob",
            ))));
        }
        let path = match path_error(
            self.name(),
            self.root.resolve_existing(args.1.as_deref().unwrap_or(".")),
        ) {
            Ok(path) => path,
            Err(error) => return Box::pin(std::future::ready(Err(error))),
        };
        let operations = Arc::clone(&self.operations);
        Box::pin(async move {
            check_cancelled("find", &context)?;
            let metadata = operations
                .metadata(&path)
                .await
                .map_err(|error| operation_failure("find", error))?;
            if !metadata.is_directory {
                return Err(ToolError::Execution {
                    tool: "find".into(),
                    message: "path is not a directory".into(),
                });
            }
            let results = operations
                .find_files(&path, &args.0, args.2)
                .await
                .map_err(|error| operation_failure("find", error))?;
            if results.is_empty() {
                return Ok(result_ok(&call, "No files found matching pattern"));
            }
            let mut output = results.join("\n");
            if results.len() >= args.2 {
                output.push_str(&format!("\n\n[{} results limit reached]", args.2));
            }
            Ok(result_ok(&call, output))
        })
    }
}

fn static_schema_find() -> &'static JsonValue {
    use std::sync::OnceLock;
    static VALUE: OnceLock<JsonValue> = OnceLock::new();
    VALUE.get_or_init(find_schema)
}

struct LsTool {
    root: WorkspaceRoot,
    operations: Arc<dyn CodingOperations>,
}

impl LsTool {
    fn new(root: WorkspaceRoot, operations: Arc<dyn CodingOperations>) -> Self {
        Self { root, operations }
    }
}

impl AgentTool for LsTool {
    fn name(&self) -> &str {
        "ls"
    }
    fn description(&self) -> &str {
        "List directory contents. Returns entries sorted alphabetically, with '/' suffix for directories. Includes dotfiles. Output is truncated to 500 entries or 50KB (whichever is hit first)."
    }
    fn schema(&self) -> &JsonValue {
        static_schema_ls()
    }
    fn execute<'a>(
        &'a self,
        call: ToolCall,
        context: ToolContext,
        _updates: ToolUpdateSink,
    ) -> ToolFuture<'a> {
        let args = match parse_object(self.name(), &call).and_then(|object| {
            Ok((
                optional_string(self.name(), &object, "path")?,
                optional_positive_usize(self.name(), &object, "limit")?.unwrap_or(500),
            ))
        }) {
            Ok(args) => args,
            Err(error) => return Box::pin(std::future::ready(Err(error))),
        };
        let path = match path_error(
            self.name(),
            self.root.resolve_existing(args.0.as_deref().unwrap_or(".")),
        ) {
            Ok(path) => path,
            Err(error) => return Box::pin(std::future::ready(Err(error))),
        };
        let operations = Arc::clone(&self.operations);
        Box::pin(async move {
            check_cancelled("ls", &context)?;
            let metadata = operations
                .metadata(&path)
                .await
                .map_err(|error| operation_failure("ls", error))?;
            if !metadata.is_directory {
                return Err(ToolError::Execution {
                    tool: "ls".into(),
                    message: "path is not a directory".into(),
                });
            }
            let mut entries = operations
                .read_dir(&path)
                .await
                .map_err(|error| operation_failure("ls", error))?;
            entries.sort_by(|left, right| {
                left.name
                    .to_lowercase()
                    .cmp(&right.name.to_lowercase())
                    .then_with(|| left.name.cmp(&right.name))
            });
            if entries.is_empty() {
                return Ok(result_ok(&call, "(empty directory)"));
            }
            let limited = entries.len() > args.1;
            entries.truncate(args.1);
            let output = entries
                .into_iter()
                .map(|entry| {
                    if entry.is_directory {
                        format!("{}/", entry.name)
                    } else {
                        entry.name
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            let output = if limited {
                format!("{}\n\n[{} entries limit reached]", output, args.1)
            } else {
                output
            };
            Ok(result_ok(&call, output))
        })
    }
}

fn static_schema_ls() -> &'static JsonValue {
    use std::sync::OnceLock;
    static VALUE: OnceLock<JsonValue> = OnceLock::new();
    VALUE.get_or_init(ls_schema)
}

/// A deliberately small glob matcher sufficient for Pi's file-oriented
/// patterns. `*` matches within one path component, `**` crosses components,
/// and `?` matches one character. Invalid patterns are rejected at the tool
/// boundary rather than silently broadening the search.
#[derive(Clone, Debug)]
struct GlobMatcher {
    pattern: String,
}

impl GlobMatcher {
    fn new(pattern: &str) -> Result<Self, OperationError> {
        if pattern.is_empty() || pattern.contains('\0') {
            return Err(OperationError::new(
                "glob pattern cannot be empty or contain NUL",
            ));
        }
        Ok(Self {
            pattern: pattern.replace('\\', "/"),
        })
    }

    fn matches(&self, candidate: &str) -> bool {
        glob_match(
            self.pattern.as_bytes(),
            candidate.replace('\\', "/").as_bytes(),
        )
    }
}

fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    fn match_at(pattern: &[u8], text: &[u8], pi: usize, ti: usize) -> bool {
        if pi == pattern.len() {
            return ti == text.len();
        }
        if pattern[pi] == b'*' {
            let double = pi + 1 < pattern.len() && pattern[pi + 1] == b'*';
            let next = if double { pi + 2 } else { pi + 1 };
            if double
                && next < pattern.len()
                && pattern[next] == b'/'
                && match_at(pattern, text, next + 1, ti)
            {
                return true;
            }
            let mut current = ti;
            loop {
                if match_at(pattern, text, next, current) {
                    return true;
                }
                if current == text.len() || (!double && text[current] == b'/') {
                    break;
                }
                current += 1;
            }
            false
        } else if ti < text.len() && (pattern[pi] == b'?' || pattern[pi] == text[ti]) {
            match_at(pattern, text, pi + 1, ti + 1)
        } else {
            false
        }
    }
    match_at(pattern, text, 0, 0)
}

fn walk_files(
    root: &Path,
    current: &Path,
    matcher: &GlobMatcher,
    limit: usize,
    output: &mut Vec<String>,
) -> Result<(), OperationError> {
    if output.len() >= limit {
        return Ok(());
    }
    for entry in
        std::fs::read_dir(current).map_err(|error| OperationError::new(error.to_string()))?
    {
        if output.len() >= limit {
            break;
        }
        let entry = entry.map_err(|error| OperationError::new(error.to_string()))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == ".git" || name == "node_modules" {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let metadata = entry
            .metadata()
            .map_err(|error| OperationError::new(error.to_string()))?;
        if metadata.is_dir() {
            walk_files(root, &path, matcher, limit, output)?;
        } else if matcher.matches(&relative) || matcher.matches(&name) {
            output.push(relative);
        }
    }
    Ok(())
}

/// Minimal regex-like matcher used by the dependency-free local grep adapter.
/// It intentionally supports literals, `.`, `*`, `^`, and `$`; malformed
/// character classes/escapes are rejected. A host requiring full regex syntax
/// can replace [`CodingOperations::grep_files`] without changing the tool.
#[derive(Clone, Debug)]
struct TinyPattern {
    pattern: String,
    ignore_case: bool,
}

impl TinyPattern {
    fn new(pattern: &str, ignore_case: bool) -> Result<Self, OperationError> {
        if pattern.is_empty() {
            return Err(OperationError::new("pattern cannot be empty"));
        }
        let mut escaped = false;
        let mut class = false;
        for byte in pattern.bytes() {
            if escaped {
                escaped = false;
                continue;
            }
            match byte {
                b'\\' => escaped = true,
                b'[' => class = true,
                b']' if !class => return Err(OperationError::new("unmatched ] in pattern")),
                b']' => class = false,
                _ => {}
            }
        }
        if escaped || class {
            return Err(OperationError::new(
                "unterminated regex escape or character class",
            ));
        }
        Ok(Self {
            pattern: pattern.to_owned(),
            ignore_case,
        })
    }

    fn matches(&self, text: &str) -> bool {
        let pattern = if self.ignore_case {
            self.pattern.to_lowercase()
        } else {
            self.pattern.clone()
        };
        let text = if self.ignore_case {
            text.to_lowercase()
        } else {
            text.to_owned()
        };
        let anchored_start = pattern.starts_with('^');
        let anchored_end = pattern.ends_with('$') && !pattern.ends_with("\\$");
        let pattern = pattern.strip_prefix('^').unwrap_or(&pattern);
        let pattern = if anchored_end {
            &pattern[..pattern.len() - 1]
        } else {
            pattern
        };
        if anchored_start {
            tiny_match(pattern.as_bytes(), text.as_bytes(), 0, 0, true)
        } else if anchored_end {
            (0..=text.len()).any(|start| {
                tiny_match(pattern.as_bytes(), text.as_bytes(), 0, start, false)
                    && start + pattern_literal_len(pattern.as_bytes()) == text.len()
            })
        } else {
            (0..=text.len())
                .any(|start| tiny_match(pattern.as_bytes(), text.as_bytes(), 0, start, false))
        }
    }
}

fn pattern_literal_len(pattern: &[u8]) -> usize {
    pattern
        .iter()
        .filter(|byte| **byte != b'*' && **byte != b'\\')
        .count()
}

fn tiny_match(pattern: &[u8], text: &[u8], pi: usize, ti: usize, anchored: bool) -> bool {
    if pi == pattern.len() {
        return !anchored || ti <= text.len();
    }
    if pattern[pi] == b'*' {
        let mut current = ti;
        while current <= text.len() {
            if tiny_match(pattern, text, pi + 1, current, anchored) {
                return true;
            }
            if current == text.len() {
                break;
            }
            current += 1;
        }
        return false;
    }
    if ti >= text.len() {
        return false;
    }
    if pattern[pi] == b'.' || pattern[pi] == text[ti] {
        tiny_match(pattern, text, pi + 1, ti + 1, anchored)
    } else {
        false
    }
}

fn local_grep(
    root: &Path,
    pattern: &str,
    options: GrepOptions,
) -> Result<Vec<GrepMatch>, OperationError> {
    let matcher = if options.literal {
        None
    } else {
        Some(TinyPattern::new(pattern, options.ignore_case)?)
    };
    let literal = if options.ignore_case {
        pattern.to_lowercase()
    } else {
        pattern.to_owned()
    };
    let file_matcher = options.glob.as_deref().map(GlobMatcher::new).transpose()?;
    let mut files = Vec::new();
    if root.is_file() {
        files.push(root.to_path_buf());
    } else {
        collect_files(root, root, file_matcher.as_ref(), &mut files)?;
    }
    files.sort();
    let mut matches = Vec::new();
    for file in files {
        if matches.len() >= options.limit {
            break;
        }
        let bytes = match std::fs::read(&file) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let lines = text.lines().collect::<Vec<_>>();
        for (index, line) in lines.iter().enumerate() {
            if matches.len() >= options.limit {
                break;
            }
            let haystack = if options.ignore_case {
                line.to_lowercase()
            } else {
                (*line).to_owned()
            };
            let is_match = if options.literal {
                haystack.contains(&literal)
            } else {
                matcher.as_ref().is_some_and(|value| value.matches(line))
            };
            if is_match {
                let path = file
                    .strip_prefix(root)
                    .ok()
                    .filter(|relative| !relative.as_os_str().is_empty())
                    .map(|relative| relative.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_else(|| {
                        file.file_name()
                            .unwrap_or_else(|| file.as_os_str())
                            .to_string_lossy()
                            .replace('\\', "/")
                    });
                matches.push(GrepMatch {
                    path,
                    line: index + 1,
                    text: line.chars().take(500).collect(),
                });
            }
        }
    }
    Ok(matches)
}

fn collect_files(
    root: &Path,
    current: &Path,
    matcher: Option<&GlobMatcher>,
    output: &mut Vec<PathBuf>,
) -> Result<(), OperationError> {
    for entry in
        std::fs::read_dir(current).map_err(|error| OperationError::new(error.to_string()))?
    {
        let entry = entry.map_err(|error| OperationError::new(error.to_string()))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == ".git" || name == "node_modules" {
            continue;
        }
        let metadata = entry
            .metadata()
            .map_err(|error| OperationError::new(error.to_string()))?;
        if metadata.is_dir() {
            collect_files(root, &path, matcher, output)?;
        } else {
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if matcher.is_none_or(|matcher| matcher.matches(&relative) || matcher.matches(&name)) {
                output.push(path);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{SerializedJson, ToolCallId};
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_WORKSPACE: AtomicU64 = AtomicU64::new(0);

    fn workspace() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "pi-default-tools-{}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT_WORKSPACE.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn call(name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: ToolCallId::new(name).unwrap(),
            name: name.into(),
            arguments: SerializedJson::new(arguments),
        }
    }

    fn context() -> ToolContext {
        ToolContext {
            cancellation: CancellationToken::new(),
            metadata: None,
        }
    }

    #[test]
    fn workspace_rejects_escape_and_symlink_escape() {
        let root = workspace();
        let outside = workspace();
        fs::write(outside.join("secret.txt"), "secret").unwrap();
        let tools = DefaultCodingTools::new(&root).unwrap();
        assert!(tools.workspace().resolve_existing("../secret.txt").is_err());
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        #[cfg(unix)]
        assert!(tools
            .workspace()
            .resolve_existing("link/secret.txt")
            .is_err());
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn separate_default_toolsets_cannot_cross_workspace_authority() {
        let first = workspace();
        let second = workspace();
        let first_tools = DefaultCodingTools::new(&first).unwrap();
        let second_tools = DefaultCodingTools::new(&second).unwrap();
        let first_write = smol::block_on(first_tools.write().execute(
            call("write", r#"{"path":"owned.txt","content":"first"}"#),
            context(),
            ToolUpdateSink::disabled(),
        ));
        let second_write = smol::block_on(second_tools.write().execute(
            call("write", r#"{"path":"owned.txt","content":"second"}"#),
            context(),
            ToolUpdateSink::disabled(),
        ));
        assert!(first_write.is_ok());
        assert!(second_write.is_ok());

        let escape_path = second_tools.workspace().as_path().to_string_lossy();
        let escaped = smol::block_on(first_tools.write().execute(
            call(
                "write",
                &format!(r#"{{"path":"{escape_path}/escaped.txt","content":"no"}}"#),
            ),
            context(),
            ToolUpdateSink::disabled(),
        ));
        assert!(matches!(
            escaped,
            Err(ToolError::Execution { tool, .. }) if tool == "write"
        ));
        assert_eq!(
            fs::read_to_string(first.join("owned.txt")).unwrap(),
            "first"
        );
        assert_eq!(
            fs::read_to_string(second.join("owned.txt")).unwrap(),
            "second"
        );
        assert!(!second.join("escaped.txt").exists());
        fs::remove_dir_all(first).unwrap();
        fs::remove_dir_all(second).unwrap();
    }

    #[test]
    fn default_tools_are_ordered_and_writable() {
        let root = workspace();
        let tools = DefaultCodingTools::new(&root).unwrap();
        let captured = crate::profile::PiDefaultCodingProfile::pinned_default().unwrap();
        let executable_definitions = tools
            .all_tools()
            .iter()
            .map(|tool| crate::tool::ToolDefinition::from_tool(tool.as_ref()))
            .collect::<Vec<_>>();
        for (executable, pinned) in executable_definitions
            .iter()
            .zip(captured.standard_tool_definitions())
        {
            assert_eq!(executable.name, pinned.name);
            assert_eq!(executable.description, pinned.description);
            assert_eq!(
                executable.schema.to_json_string().unwrap(),
                pinned.schema.to_json_string().unwrap(),
                "schema differs for tool {}",
                executable.name
            );
        }
        assert_eq!(
            executable_definitions,
            captured.standard_tool_definitions(),
            "the executable profile must expose the capture's exact names, descriptions, schemas, and order"
        );
        assert_eq!(
            tools.registry().names().collect::<Vec<_>>(),
            vec!["read", "bash", "edit", "write"]
        );
        let write = tools.write();
        smol::block_on(write.execute(
            call("write", r#"{"path":"src/a.txt","content":"one\ntwo\n"}"#),
            context(),
            ToolUpdateSink::disabled(),
        ))
        .unwrap();
        let read = tools.read();
        let result = smol::block_on(read.execute(
            call("read", r#"{"path":"src/a.txt","offset":2,"limit":1}"#),
            context(),
            ToolUpdateSink::disabled(),
        ))
        .unwrap();
        assert_eq!(result.content, "two");
        let edit = tools.edit();
        smol::block_on(edit.execute(
            call(
                "edit",
                r#"{"path":"src/a.txt","edits":[{"oldText":"two","newText":"TWO"}]}"#,
            ),
            context(),
            ToolUpdateSink::disabled(),
        ))
        .unwrap();
        assert_eq!(
            fs::read_to_string(root.join("src/a.txt")).unwrap(),
            "one\nTWO\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pinned_builder_uses_the_explicit_workspace_in_the_captured_prompt() {
        let root = workspace();
        let tools = DefaultCodingTools::new(&root).unwrap();
        let workspace_text = tools
            .workspace()
            .as_path()
            .to_string_lossy()
            .replace('\\', "/");
        let agent = crate::Agent::builder()
            .pinned_default_coding_profile(tools)
            .expect("pinned profile accepts the complete default registry")
            .build();

        let snapshot = agent.snapshot();
        assert!(snapshot
            .system_prompt
            .contains(&format!("Current working directory: {workspace_text}")));
        assert!(!snapshot
            .system_prompt
            .contains("Current working directory: /fixture/workspace"));
        assert_eq!(
            agent
                .tool_definitions()
                .into_iter()
                .map(|definition| definition.name)
                .collect::<Vec<_>>(),
            ["read", "bash", "edit", "write"]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bash_uses_explicit_workspace_and_empty_environment() {
        let root = workspace();
        let tools = DefaultCodingTools::new(&root).unwrap();
        let result = smol::block_on(tools.bash().execute(
            call("bash", r#"{"command":"printf '%s' \"$PI_SECRET\"; pwd"}"#),
            context(),
            ToolUpdateSink::disabled(),
        ))
        .unwrap();
        assert_eq!(
            result.content,
            tools.workspace().as_path().to_string_lossy()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn grep_and_find_are_explicit_and_deterministic() {
        let root = workspace();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src/a.rs"), "TODO: one\nclean\n").unwrap();
        fs::write(root.join("src/b.txt"), "TODO: two\n").unwrap();
        let tools = DefaultCodingTools::new(&root).unwrap();
        let grep = smol::block_on(tools.grep().execute(
            call("grep", r#"{"pattern":"TODO","glob":"**/*.rs"}"#),
            context(),
            ToolUpdateSink::disabled(),
        ))
        .unwrap();
        assert_eq!(grep.content, "src/a.rs:1: TODO: one");
        let find = smol::block_on(tools.find().execute(
            call("find", r#"{"pattern":"**/*.rs"}"#),
            context(),
            ToolUpdateSink::disabled(),
        ))
        .unwrap();
        assert_eq!(find.content, "src/a.rs");
        fs::remove_dir_all(root).unwrap();
    }
}
