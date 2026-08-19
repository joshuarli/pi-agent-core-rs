//! TUI-owned Phi extension discovery and authoring boundaries.
//!
//! Phi is deliberately an application concern.  This module resolves the optional home
//! directory, reads the explicit ordered registry, and turns each listed extension into a
//! closed `pi-agent-luau` bundle.  It never grants a Luau capability: declarations are useful
//! for prompt composition, while declared handlers remain inert until a host supplies an
//! explicit binding.

use pi_agent_core::error::ToolError;
use pi_agent_core::tool::{
    AgentTool, AgentToolResult, ToolCall, ToolContext, ToolExecutionMode, ToolFuture,
    ToolUpdateSink,
};
use pi_agent_luau::bundle::{Bundle, BundleManifest, BUNDLE_ABI_VERSION};
use pi_agent_luau::{LuaPolicy, PolicyError, PolicyTool};
use pi_agent_protocol::JsonValue;
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

const REGISTRY_FILE: &str = "extensions.json";
const EXTENSION_MANIFEST_FILE: &str = "manifest.json";
const MAX_AUTHORED_FILE_BYTES: usize = 128 * 1024;

/// A failure while resolving or loading the TUI-owned Phi extension registry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PhiLoadError {
    /// A filesystem operation failed at an explicit path.
    Io { path: PathBuf, message: String },
    /// JSON text did not meet the registry or extension manifest contract.
    Json { path: PathBuf, message: String },
    /// An extension path or manifest field violated the host boundary.
    Contract { path: PathBuf, message: String },
    /// The closed bundle could not be constructed.
    Bundle { path: PathBuf, message: String },
    /// Luau rejected the closed policy declaration.
    Policy { path: PathBuf, message: String },
}

impl std::fmt::Display for PhiLoadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, message } => {
                write!(formatter, "Phi I/O failed at {}: {message}", path.display())
            }
            Self::Json { path, message } => write!(
                formatter,
                "invalid Phi JSON at {}: {message}",
                path.display()
            ),
            Self::Contract { path, message } => write!(
                formatter,
                "invalid Phi contract at {}: {message}",
                path.display()
            ),
            Self::Bundle { path, message } => write!(
                formatter,
                "invalid Phi bundle at {}: {message}",
                path.display()
            ),
            Self::Policy { path, message } => write!(
                formatter,
                "Phi policy failed at {}: {message}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for PhiLoadError {}

/// One extension loaded in registry order.
pub struct PhiExtension {
    name: String,
    root: PathBuf,
    bundle: Bundle,
    policy: Arc<LuaPolicy>,
}

impl std::fmt::Debug for PhiExtension {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PhiExtension")
            .field("name", &self.name)
            .field("root", &self.root)
            .field("source_hash", &self.bundle.source_hash_hex())
            .finish()
    }
}

impl PhiExtension {
    /// Return the deterministic registry name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Return the extension root selected by the explicit registry.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Return the closed source bundle.
    pub fn bundle(&self) -> &Bundle {
        &self.bundle
    }

    /// Return the loaded policy declaration.
    pub fn policy(&self) -> &Arc<LuaPolicy> {
        &self.policy
    }
}

/// Ordered Phi extensions and their prompt-facing declarations.
#[derive(Debug, Default)]
pub struct PhiExtensions {
    extensions: Vec<PhiExtension>,
}

impl PhiExtensions {
    /// Load the explicit `extensions.json` registry under `phi_home`.
    ///
    /// A missing registry is an intentional empty configuration.  Once the registry exists,
    /// every listed entry is required to load successfully; malformed entries are not skipped.
    pub fn load(phi_home: impl AsRef<Path>) -> Result<Self, PhiLoadError> {
        let phi_home = phi_home.as_ref();
        let registry_path = phi_home.join(REGISTRY_FILE);
        if let Ok(metadata) = fs::symlink_metadata(&registry_path) {
            if metadata.file_type().is_symlink() {
                return Err(contract_error(
                    &registry_path,
                    "symlinked Phi registries are not allowed",
                ));
            }
        }
        let registry = match fs::read_to_string(&registry_path) {
            Ok(source) => source,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(io_error(&registry_path, error)),
        };
        let root = parse_json(&registry_path, &registry)?;
        let entries = registry_entries(&registry_path, &root)?;
        let mut names = BTreeSet::new();
        for entry in &entries {
            if !names.insert(entry.name.clone()) {
                return Err(contract_error(
                    &registry_path,
                    format!("duplicate extension name {:?}", entry.name),
                ));
            }
        }
        let home_root = canonical_directory(phi_home, false)?;
        let mut extensions = Vec::with_capacity(entries.len());
        for entry in entries {
            let root_path = contained_path(&home_root, &entry.path).map_err(|message| {
                contract_error(
                    &registry_path,
                    format!("extension {:?}: {message}", entry.name),
                )
            })?;
            let root_metadata =
                fs::symlink_metadata(&root_path).map_err(|error| io_error(&root_path, error))?;
            if root_metadata.file_type().is_symlink() {
                return Err(contract_error(
                    &root_path,
                    "symlinked extension roots are not allowed",
                ));
            }
            let extension_root = canonical_directory(&root_path, true)
                .map_err(|error| prefix_error(error, format!("extension {:?}: ", entry.name)))?;
            ensure_contained(&home_root, &extension_root).map_err(|message| {
                contract_error(
                    &registry_path,
                    format!("extension {:?}: {message}", entry.name),
                )
            })?;
            let manifest_path = contained_path(
                &extension_root,
                entry.manifest.as_deref().unwrap_or(EXTENSION_MANIFEST_FILE),
            )
            .map_err(|message| contract_error(&registry_path, message))?;
            let loaded = load_extension(&extension_root, &manifest_path, &entry.name)?;
            extensions.push(loaded);
        }
        Ok(Self { extensions })
    }

    /// Compatibility spelling for callers that prefer an explicit loader name.
    pub fn load_from_home(phi_home: impl AsRef<Path>) -> Result<Self, PhiLoadError> {
        Self::load(phi_home)
    }

    /// Return extensions in exactly the order declared by `extensions.json`.
    pub fn extensions(&self) -> &[PhiExtension] {
        &self.extensions
    }

    /// Return all policies in registry order.
    pub fn policies(&self) -> impl Iterator<Item = &Arc<LuaPolicy>> {
        self.extensions.iter().map(PhiExtension::policy)
    }

    /// Return prompt-facing declarations in extension and declaration order.
    pub fn tools(&self) -> impl Iterator<Item = (&str, &PolicyTool)> {
        self.extensions.iter().flat_map(|extension| {
            extension
                .policy
                .tools()
                .iter()
                .map(move |tool| (extension.name.as_str(), tool))
        })
    }

    /// Return whether no registry entries were loaded.
    pub fn is_empty(&self) -> bool {
        self.extensions.is_empty()
    }
}

/// Load Phi extensions from an explicit home directory.
pub fn load_phi_extensions(phi_home: impl AsRef<Path>) -> Result<PhiExtensions, PhiLoadError> {
    PhiExtensions::load(phi_home)
}

/// Resolve the TUI's Phi home.  Core never calls this function or reads `HOME`.
pub fn resolve_phi_home(override_path: Option<&Path>) -> Result<PathBuf, PhiLoadError> {
    if let Some(path) = override_path {
        if path.as_os_str().is_empty() {
            return Err(contract_error(path, "--phi-home must not be empty"));
        }
        return Ok(path.to_path_buf());
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or_else(|| {
            contract_error(
                Path::new("~/.phi"),
                "could not resolve the user home directory",
            )
        })?;
    Ok(PathBuf::from(home).join(".phi"))
}

/// A declaration-only model tool from a Phi policy.
///
/// The tool is intentionally visible to the model but cannot perform an effect.  A policy
/// handler source is not activated here because this host supplies zero `CapabilityBindings`.
pub(crate) struct PhiDeclaredTool {
    name: String,
    description: String,
    schema: JsonValue,
    execution_mode: ToolExecutionMode,
    extension: String,
}

impl PhiDeclaredTool {
    pub(crate) fn from_policy(extension: &str, tool: &PolicyTool) -> Self {
        Self {
            name: tool.name.clone(),
            description: tool.description.clone(),
            schema: tool.schema.clone(),
            execution_mode: tool.execution_mode,
            extension: extension.to_owned(),
        }
    }
}

impl AgentTool for PhiDeclaredTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn schema(&self) -> &JsonValue {
        &self.schema
    }

    fn execution_mode(&self) -> ToolExecutionMode {
        self.execution_mode
    }

    fn execute<'a>(
        &'a self,
        call: ToolCall,
        _context: ToolContext,
        _updates: ToolUpdateSink,
    ) -> ToolFuture<'a> {
        Box::pin(std::future::ready(Err(ToolError::Execution {
            tool: call.name,
            message: format!(
                "Phi extension {:?} declared this tool without an explicit host capability binding",
                self.extension
            ),
        })))
    }
}

/// A trusted, read-only handbook describing the Phi extension boundary.
pub(crate) struct PhiExtensionHandbookTool;

impl AgentTool for PhiExtensionHandbookTool {
    fn name(&self) -> &str {
        "phi_extension_handbook"
    }

    fn description(&self) -> &str {
        "Explain the trusted Phi extension format and the host's no-activation/no-grants boundary."
    }

    fn schema(&self) -> &JsonValue {
        static SCHEMA: std::sync::OnceLock<JsonValue> = std::sync::OnceLock::new();
        SCHEMA.get_or_init(|| {
            JsonValue::object([(String::from("type"), JsonValue::String("object".into()))])
        })
    }

    fn execute<'a>(
        &'a self,
        call: ToolCall,
        _context: ToolContext,
        _updates: ToolUpdateSink,
    ) -> ToolFuture<'a> {
        Box::pin(std::future::ready(Ok(result_ok(
            &call,
            PHI_EXTENSION_HANDBOOK,
        ))))
    }
}

/// A model-visible authoring tool rooted at `<phi-home>/extensions`.
///
/// It can list and read files, write drafts, and validate a draft bundle.  The registry file is
/// never writable through this tool, so authoring cannot activate an extension or grant a
/// capability.
pub(crate) struct PhiExtensionFilesTool {
    root: PathBuf,
}

impl PhiExtensionFilesTool {
    pub(crate) fn new(phi_home: impl AsRef<Path>) -> Self {
        Self {
            root: phi_home.as_ref().join("extensions"),
        }
    }
}

impl AgentTool for PhiExtensionFilesTool {
    fn name(&self) -> &str {
        "phi_extension_files"
    }

    fn description(&self) -> &str {
        "List/read/write_draft/validate Phi extension files under the explicit Phi home; never activates or grants an extension."
    }

    fn schema(&self) -> &JsonValue {
        static SCHEMA: std::sync::OnceLock<JsonValue> = std::sync::OnceLock::new();
        SCHEMA.get_or_init(|| {
            JsonValue::object([
                ("type", JsonValue::String("object".into())),
                (
                    "required",
                    JsonValue::Array(vec![JsonValue::String("operation".into())]),
                ),
                (
                    "properties",
                    JsonValue::object([
                        (
                            "operation",
                            JsonValue::object([
                                ("type", JsonValue::String("string".into())),
                                (
                                    "enum",
                                    JsonValue::Array(
                                        ["list", "read", "write_draft", "validate"]
                                            .into_iter()
                                            .map(|value| JsonValue::String(value.into()))
                                            .collect(),
                                    ),
                                ),
                            ]),
                        ),
                        (
                            "path",
                            JsonValue::object([
                                ("type", JsonValue::String("string".into())),
                                (
                                    "description",
                                    JsonValue::String(
                                        "Path relative to the Phi extensions root.".into(),
                                    ),
                                ),
                            ]),
                        ),
                        (
                            "content",
                            JsonValue::object([
                                ("type", JsonValue::String("string".into())),
                                (
                                    "description",
                                    JsonValue::String("UTF-8 draft source for write_draft.".into()),
                                ),
                            ]),
                        ),
                    ]),
                ),
            ])
        })
    }

    fn execution_mode(&self) -> ToolExecutionMode {
        ToolExecutionMode::Sequential
    }

    fn execute<'a>(
        &'a self,
        call: ToolCall,
        context: ToolContext,
        _updates: ToolUpdateSink,
    ) -> ToolFuture<'a> {
        let result = self.execute_now(&call, &context);
        Box::pin(std::future::ready(result))
    }
}

impl PhiExtensionFilesTool {
    fn execute_now(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<AgentToolResult, ToolError> {
        if context.cancellation.is_cancelled() {
            return Err(ToolError::Cancelled {
                tool: self.name().to_owned(),
            });
        }
        let object = match JsonValue::parse(call.arguments.as_str()) {
            Ok(JsonValue::Object(object)) => object,
            Ok(_) => return Err(invalid_tool_args("arguments must be an object")),
            Err(_) => return Err(invalid_tool_args("arguments must be valid JSON")),
        };
        let operation = match object.get("operation").and_then(JsonValue::as_str) {
            Some(value) => value,
            None => return Err(invalid_tool_args("operation must be a string")),
        };
        let relative = object
            .get("path")
            .and_then(JsonValue::as_str)
            .unwrap_or(".");
        match operation {
            "list" => self.list(call, relative),
            "read" => self.read(call, relative),
            "write_draft" => {
                let content = object
                    .get("content")
                    .and_then(JsonValue::as_str)
                    .ok_or_else(|| invalid_tool_args("content must be a string for write_draft"))?;
                self.write_draft(call, relative, content)
            }
            "validate" => self.validate(call, relative),
            _ => Err(invalid_tool_args(
                "operation must be one of list, read, write_draft, validate",
            )),
        }
    }

    fn list(&self, call: &ToolCall, relative: &str) -> Result<AgentToolResult, ToolError> {
        let directory = self.resolve_existing(relative)?;
        let metadata = fs::metadata(&directory).map_err(|error| tool_io(self.name(), error))?;
        if !metadata.is_dir() {
            return Err(ToolError::Execution {
                tool: self.name().into(),
                message: "list path is not a directory".into(),
            });
        }
        let mut files = Vec::new();
        let canonical_root =
            fs::canonicalize(&self.root).map_err(|error| tool_io(self.name(), error))?;
        collect_files(&canonical_root, &directory, &mut files)?;
        files.sort();
        Ok(result_ok(
            call,
            if files.is_empty() {
                "No Phi extension files".into()
            } else {
                files.join("\n")
            },
        ))
    }

    fn read(&self, call: &ToolCall, relative: &str) -> Result<AgentToolResult, ToolError> {
        let path = self.resolve_existing(relative)?;
        let bytes = fs::read(&path).map_err(|error| tool_io(self.name(), error))?;
        if bytes.len() > MAX_AUTHORED_FILE_BYTES {
            return Err(ToolError::Execution {
                tool: self.name().into(),
                message: format!("read exceeds {MAX_AUTHORED_FILE_BYTES} bytes"),
            });
        }
        let content = String::from_utf8(bytes).map_err(|_| ToolError::Execution {
            tool: self.name().into(),
            message: "Phi extension files must be UTF-8 text".into(),
        })?;
        Ok(result_ok(call, content))
    }

    fn write_draft(
        &self,
        call: &ToolCall,
        relative: &str,
        content: &str,
    ) -> Result<AgentToolResult, ToolError> {
        if content.len() > MAX_AUTHORED_FILE_BYTES {
            return Err(ToolError::Execution {
                tool: self.name().into(),
                message: format!("draft exceeds {MAX_AUTHORED_FILE_BYTES} bytes"),
            });
        }
        let path = self.resolve_for_write(relative)?;
        if path.file_name().is_some_and(|name| name == REGISTRY_FILE) {
            return Err(ToolError::Blocked {
                tool: self.name().into(),
                reason: "the Phi registry cannot be changed by model authoring".into(),
            });
        }
        let parent = path.parent().unwrap_or(&self.root);
        create_draft_parent(&self.root, parent)?;
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            if metadata.file_type().is_symlink() {
                return Err(ToolError::Blocked {
                    tool: self.name().into(),
                    reason: "draft path cannot replace a symlink".into(),
                });
            }
        }
        fs::write(&path, content.as_bytes()).map_err(|error| tool_io(self.name(), error))?;
        Ok(result_ok(
            call,
            format!(
                "Wrote draft {} bytes at {}. A registered extension reloads after the current run settles.",
                content.len(),
                relative
            ),
        ))
    }

    fn validate(&self, call: &ToolCall, relative: &str) -> Result<AgentToolResult, ToolError> {
        let root = self.resolve_existing(relative)?;
        let metadata = fs::metadata(&root).map_err(|error| tool_io(self.name(), error))?;
        if !metadata.is_dir() {
            return Err(ToolError::Execution {
                tool: self.name().into(),
                message: "validate path is not an extension directory".into(),
            });
        }
        let root = canonical_directory(&root, true).map_err(|error| ToolError::Execution {
            tool: self.name().into(),
            message: error.to_string(),
        })?;
        let manifest = root.join(EXTENSION_MANIFEST_FILE);
        let entry_name = root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("draft")
            .to_owned();
        let extension = load_extension(&root, &manifest, &entry_name).map_err(|error| {
            ToolError::Execution {
                tool: self.name().into(),
                message: error.to_string(),
            }
        })?;
        Ok(result_ok(
            call,
            format!(
                "Validated extension {:?}; source hash {}. It remains inactive until an explicit registry edit.",
                extension.name(),
                extension.bundle().source_hash_hex()
            ),
        ))
    }

    fn resolve_existing(&self, relative: &str) -> Result<PathBuf, ToolError> {
        let lexical =
            contained_path(&self.root, relative).map_err(invalid_tool_args)?;
        let path = fs::canonicalize(&lexical).map_err(|error| tool_io(self.name(), error))?;
        ensure_contained_canonical(&self.root, &path)?;
        Ok(path)
    }

    fn resolve_for_write(&self, relative: &str) -> Result<PathBuf, ToolError> {
        let path =
            contained_path(&self.root, relative).map_err(invalid_tool_args)?;
        if path == self.root {
            return Err(invalid_tool_args("a file path is required"));
        }
        Ok(path)
    }
}

fn collect_files(root: &Path, directory: &Path, output: &mut Vec<String>) -> Result<(), ToolError> {
    let mut entries = fs::read_dir(directory)
        .map_err(|error| tool_io("phi_extension_files", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| tool_io("phi_extension_files", error))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let metadata =
            fs::symlink_metadata(&path).map_err(|error| tool_io("phi_extension_files", error))?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            collect_files(root, &path, output)?;
        } else if metadata.is_file() {
            let canonical =
                fs::canonicalize(&path).map_err(|error| tool_io("phi_extension_files", error))?;
            ensure_contained_canonical(root, &canonical)?;
            let relative = canonical
                .strip_prefix(root)
                .map_err(|_| invalid_tool_args("file escaped Phi extension root"))?;
            output.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

/// Create the parent chain for a draft without following a model-reachable symlink.
///
/// `write_draft` accepts only a lexical path below the extensions root. This second check makes
/// the filesystem traversal equally narrow: every existing component must be a real directory,
/// and the completed parent is canonicalized back under that root before the final file write.
fn create_draft_parent(root: &Path, parent: &Path) -> Result<(), ToolError> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(ToolError::Blocked {
                tool: "phi_extension_files".into(),
                reason: "Phi extensions root cannot be a symlink".into(),
            });
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(ToolError::Execution {
                tool: "phi_extension_files".into(),
                message: "Phi extensions root is not a directory".into(),
            });
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(root).map_err(|error| tool_io("phi_extension_files", error))?;
        }
        Err(error) => return Err(tool_io("phi_extension_files", error)),
    }

    let relative_parent = parent.strip_prefix(root).map_err(|_| ToolError::Blocked {
        tool: "phi_extension_files".into(),
        reason: "draft parent escapes the Phi extensions root".into(),
    })?;
    let mut current = root.to_path_buf();
    for component in relative_parent.components() {
        let Component::Normal(component) = component else {
            return Err(ToolError::Blocked {
                tool: "phi_extension_files".into(),
                reason: "draft parent contains an invalid path component".into(),
            });
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ToolError::Blocked {
                    tool: "phi_extension_files".into(),
                    reason: "draft parent cannot traverse a symlink".into(),
                });
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(ToolError::Execution {
                    tool: "phi_extension_files".into(),
                    message: "draft parent contains a non-directory path component".into(),
                });
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(|error| tool_io("phi_extension_files", error))?;
            }
            Err(error) => return Err(tool_io("phi_extension_files", error)),
        }
    }
    ensure_contained_canonical(root, parent)
}

fn load_extension(
    extension_root: &Path,
    manifest_path: &Path,
    registry_name: &str,
) -> Result<PhiExtension, PhiLoadError> {
    let source = read_contained_file(extension_root, manifest_path)?;
    let value = parse_json(manifest_path, &source)?;
    let object = expect_object(manifest_path, &value)?;
    let manifest_version = optional_u64(object, "version")
        .or_else(|| optional_u64(object, "format_version"))
        .unwrap_or(1);
    if manifest_version != 1 {
        return Err(contract_error(
            manifest_path,
            format!("unsupported extension manifest version {manifest_version}; expected 1"),
        ));
    }
    let name =
        optional_string(manifest_path, object, "name")?.unwrap_or_else(|| registry_name.to_owned());
    validate_name(manifest_path, "extension name", &name)?;
    if name != registry_name {
        return Err(contract_error(
            manifest_path,
            format!("manifest name {name:?} does not match registry name {registry_name:?}"),
        ));
    }
    let entrypoint = required_string_alias(manifest_path, object, "entrypoint", "entry")?;
    let modules = match object
        .get("modules")
        .or_else(|| object.get("files"))
        .or_else(|| object.get("sources"))
    {
        None => vec![entrypoint.clone()],
        Some(JsonValue::Array(values)) => values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                value.as_str().map(str::to_owned).ok_or_else(|| {
                    contract_error(
                        manifest_path,
                        format!("modules[{index}] must be a string path"),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => {
            return Err(contract_error(
                manifest_path,
                "modules must be an array of explicit source paths",
            ))
        }
    };
    let capabilities = match object.get("capabilities") {
        None => Vec::new(),
        Some(JsonValue::Array(values)) => values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                value.as_str().map(str::to_owned).ok_or_else(|| {
                    contract_error(
                        manifest_path,
                        format!("capabilities[{index}] must be a string"),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => {
            return Err(contract_error(
                manifest_path,
                "capabilities must be an array of strings",
            ))
        }
    };
    let mut sources: Vec<(String, String)> = Vec::with_capacity(modules.len());
    for module in modules {
        let module_path = contained_path(extension_root, &module).map_err(|message| {
            contract_error(manifest_path, format!("module {module:?}: {message}"))
        })?;
        let source = read_contained_file(extension_root, &module_path)?;
        sources.push((module, source));
    }
    let bundle_manifest = BundleManifest::new(BUNDLE_ABI_VERSION, &entrypoint, capabilities)
        .map_err(|error| bundle_error(manifest_path, error.to_string()))?;
    let bundle = Bundle::from_sources(bundle_manifest, sources)
        .map_err(|error| bundle_error(manifest_path, error.to_string()))?;
    let policy = LuaPolicy::load_bundle(bundle.clone()).map_err(|error: PolicyError| {
        PhiLoadError::Policy {
            path: manifest_path.to_path_buf(),
            message: error.to_string(),
        }
    })?;
    Ok(PhiExtension {
        name,
        root: extension_root.to_path_buf(),
        bundle,
        policy: Arc::new(policy),
    })
}

#[derive(Debug)]
struct RegistryEntry {
    name: String,
    path: String,
    manifest: Option<String>,
}

fn registry_entries(path: &Path, value: &JsonValue) -> Result<Vec<RegistryEntry>, PhiLoadError> {
    let object = expect_object(path, value)?;
    let version = optional_u64(object, "version")
        .or_else(|| optional_u64(object, "format_version"))
        .ok_or_else(|| contract_error(path, "version is required"))?;
    if version != 1 {
        return Err(contract_error(
            path,
            format!("unsupported registry version {version}; expected 1"),
        ));
    }
    let entries = match object.get("extensions") {
        Some(JsonValue::Array(entries)) => entries,
        Some(_) => return Err(contract_error(path, "extensions must be an array")),
        None => return Err(contract_error(path, "extensions is required")),
    };
    let mut result = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        match entry {
            JsonValue::String(path_value) => {
                let name = Path::new(path_value)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| {
                        contract_error(path, format!("extensions[{index}] path has no name"))
                    })?;
                validate_name(path, "extension name", name)?;
                result.push(RegistryEntry {
                    name: name.to_owned(),
                    path: path_value.clone(),
                    manifest: None,
                });
            }
            JsonValue::Object(object) => {
                let root = optional_string(path, object, "path")?
                    .or(optional_string(path, object, "root")?)
                    .ok_or_else(|| {
                        contract_error(path, format!("extensions[{index}] requires path"))
                    })?;
                let name = optional_string(path, object, "name")?
                    .or(optional_string(path, object, "id")?)
                    .or_else(|| {
                        Path::new(&root)
                            .file_name()
                            .and_then(|name| name.to_str())
                            .map(str::to_owned)
                    })
                    .ok_or_else(|| {
                        contract_error(path, format!("extensions[{index}] requires name"))
                    })?;
                validate_name(path, "extension name", &name)?;
                let manifest = optional_string(path, object, "manifest")?;
                result.push(RegistryEntry {
                    name,
                    path: root,
                    manifest,
                });
            }
            _ => {
                return Err(contract_error(
                    path,
                    format!("extensions[{index}] must be a string or object"),
                ))
            }
        }
    }
    Ok(result)
}

fn parse_json(path: &Path, source: &str) -> Result<JsonValue, PhiLoadError> {
    JsonValue::parse(source).map_err(|error| PhiLoadError::Json {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

fn expect_object<'a>(
    path: &Path,
    value: &'a JsonValue,
) -> Result<&'a std::collections::BTreeMap<String, JsonValue>, PhiLoadError> {
    value
        .as_object()
        .ok_or_else(|| contract_error(path, "root must be a JSON object"))
}

fn required_string(
    path: &Path,
    object: &std::collections::BTreeMap<String, JsonValue>,
    field: &str,
) -> Result<String, PhiLoadError> {
    let value = object
        .get(field)
        .ok_or_else(|| contract_error(path, format!("{field} is required")))?;
    let value = value
        .as_str()
        .ok_or_else(|| contract_error(path, format!("{field} must be a string")))?;
    if value.trim().is_empty() {
        return Err(contract_error(path, format!("{field} must not be empty")));
    }
    Ok(value.to_owned())
}

fn required_string_alias(
    path: &Path,
    object: &std::collections::BTreeMap<String, JsonValue>,
    primary: &str,
    alias: &str,
) -> Result<String, PhiLoadError> {
    if object.contains_key(primary) {
        required_string(path, object, primary)
    } else {
        required_string(path, object, alias)
    }
}

fn optional_string(
    path: &Path,
    object: &std::collections::BTreeMap<String, JsonValue>,
    field: &str,
) -> Result<Option<String>, PhiLoadError> {
    match object.get(field) {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| contract_error(path, format!("{field} must be a string")))
            .map(Some),
    }
}

fn optional_u64(
    object: &std::collections::BTreeMap<String, JsonValue>,
    field: &str,
) -> Option<u64> {
    object.get(field).and_then(JsonValue::as_u64)
}

fn validate_name(path: &Path, field: &str, value: &str) -> Result<(), PhiLoadError> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || value
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        return Err(contract_error(
            path,
            format!("{field} is not a safe deterministic name"),
        ));
    }
    Ok(())
}

fn contained_path(root: &Path, relative: &str) -> Result<PathBuf, String> {
    if relative.is_empty()
        || relative.contains('\\')
        || relative
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        return Err("path is empty or contains an invalid character".into());
    }
    let path = Path::new(relative);
    if path.is_absolute() {
        return Err("absolute paths are not allowed".into());
    }
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir => return Err("`.` path segments are not allowed".into()),
            Component::ParentDir => return Err("path traversal is not allowed".into()),
            Component::RootDir | Component::Prefix(_) => {
                return Err("absolute paths are not allowed".into())
            }
        }
    }
    Ok(root.join(path))
}

fn canonical_directory(path: &Path, required: bool) -> Result<PathBuf, PhiLoadError> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(error) if !required && error.kind() == io::ErrorKind::NotFound => {
            Ok(path.to_path_buf())
        }
        Err(error) => Err(io_error(path, error)),
    }
}

fn read_contained_file(root: &Path, path: &Path) -> Result<String, PhiLoadError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| io_error(path, error))?;
    if metadata.file_type().is_symlink() {
        return Err(contract_error(
            path,
            "symlinked extension files are not allowed",
        ));
    }
    if !metadata.is_file() {
        return Err(contract_error(
            path,
            "extension source must be a regular file",
        ));
    }
    let canonical = fs::canonicalize(path).map_err(|error| io_error(path, error))?;
    ensure_contained(root, &canonical).map_err(|message| contract_error(path, message))?;
    fs::read_to_string(&canonical).map_err(|error| io_error(path, error))
}

fn ensure_contained(root: &Path, path: &Path) -> Result<(), String> {
    path.strip_prefix(root)
        .map(|_| ())
        .map_err(|_| "path escapes the Phi home".to_owned())
}

fn ensure_contained_canonical(root: &Path, path: &Path) -> Result<(), ToolError> {
    let canonical_root =
        fs::canonicalize(root).map_err(|error| tool_io("phi_extension_files", error))?;
    let canonical_path =
        fs::canonicalize(path).map_err(|error| tool_io("phi_extension_files", error))?;
    canonical_path
        .strip_prefix(canonical_root)
        .map(|_| ())
        .map_err(|_| ToolError::Blocked {
            tool: "phi_extension_files".into(),
            reason: "path escapes the Phi extensions root".into(),
        })
}

fn io_error(path: &Path, error: io::Error) -> PhiLoadError {
    PhiLoadError::Io {
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

fn contract_error(path: &Path, message: impl Into<String>) -> PhiLoadError {
    PhiLoadError::Contract {
        path: path.to_path_buf(),
        message: message.into(),
    }
}

fn bundle_error(path: &Path, message: impl Into<String>) -> PhiLoadError {
    PhiLoadError::Bundle {
        path: path.to_path_buf(),
        message: message.into(),
    }
}

fn prefix_error(error: PhiLoadError, prefix: String) -> PhiLoadError {
    match error {
        PhiLoadError::Io { path, message } => PhiLoadError::Io {
            path,
            message: prefix + &message,
        },
        PhiLoadError::Json { path, message } => PhiLoadError::Json {
            path,
            message: prefix + &message,
        },
        PhiLoadError::Contract { path, message } => PhiLoadError::Contract {
            path,
            message: prefix + &message,
        },
        PhiLoadError::Bundle { path, message } => PhiLoadError::Bundle {
            path,
            message: prefix + &message,
        },
        PhiLoadError::Policy { path, message } => PhiLoadError::Policy {
            path,
            message: prefix + &message,
        },
    }
}

fn invalid_tool_args(message: impl Into<String>) -> ToolError {
    ToolError::InvalidArguments {
        tool: "phi_extension_files".into(),
        message: message.into(),
    }
}

fn tool_io(tool: &str, error: io::Error) -> ToolError {
    ToolError::Execution {
        tool: tool.into(),
        message: error.to_string(),
    }
}

fn result_ok(call: &ToolCall, content: impl Into<String>) -> AgentToolResult {
    AgentToolResult {
        tool_call_id: call.id.clone(),
        content: content.into(),
        details: None,
        usage: None,
        added_tool_names: Vec::new(),
        terminate: false,
        is_error: false,
        failure: None,
    }
}

const PHI_EXTENSION_HANDBOOK: &str = r#"Phi extensions are explicit, ordered, and inert by default.

Registry: ~/.phi/extensions.json contains {"version":1,"extensions":[{"name":"example","path":"extensions/example"}]}.
Each extension directory contains manifest.json and an explicit list of Luau module paths. The
manifest entrypoint must return { system_prompt_append = "...", tools = {...}, before_tool_call = ... }.
Only closed bundle-local relative imports are available. The TUI composes prompt text and policy
hooks in registry order. Declared tools are model-visible, but this host supplies zero
CapabilityBindings, so a declaration never grants a world effect and declared handlers are inert.

Use phi_extension_files with operation list, read, write_draft, or validate. Paths are rooted at
the Phi extensions directory. write_draft cannot edit the registry or grant capabilities. Source
for an already registered extension reloads after its current run settles; adding a new registry
entry and granting authority remain separate trusted host decisions."#;

#[cfg(test)]
mod tests {
    use super::*;
    use pi_agent_core::state::ToolCallId;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestHome(PathBuf);

    impl TestHome {
        fn new() -> Self {
            let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "pi-agent-phi-test-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("test home should be created");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestHome {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_registry_is_empty_and_registry_order_is_preserved() {
        let home = TestHome::new();
        assert!(PhiExtensions::load(home.path())
            .expect("missing registry is empty")
            .is_empty());

        let first = home.path().join("extensions/first");
        let second = home.path().join("extensions/second");
        fs::create_dir_all(&first).expect("first extension directory");
        fs::create_dir_all(&second).expect("second extension directory");
        for (root, name) in [(&first, "first"), (&second, "second")] {
            fs::write(
                root.join("manifest.json"),
                format!(
                    r#"{{"version":1,"name":"{name}","entrypoint":"main.luau","modules":["main.luau"]}}"#
                ),
            )
            .expect("manifest should be written");
            fs::write(
                root.join("main.luau"),
                format!(
                    r#"return {{system_prompt_append = "{name}", before_tool_call = function(_) return "allow" end}}"#
                ),
            )
            .expect("policy should be written");
        }
        fs::write(
            home.path().join(REGISTRY_FILE),
            r#"{"version":1,"extensions":[{"name":"second","path":"extensions/second"},{"name":"first","path":"extensions/first"}]}"#,
        )
        .expect("registry should be written");

        let loaded = PhiExtensions::load(home.path()).expect("registry should load");
        assert_eq!(
            loaded
                .extensions()
                .iter()
                .map(PhiExtension::name)
                .collect::<Vec<_>>(),
            ["second", "first"]
        );
        assert_eq!(
            loaded.extensions()[0].policy().system_prompt_append(),
            "second"
        );
    }

    #[test]
    fn loader_rejects_registry_traversal_and_duplicate_names() {
        let home = TestHome::new();
        fs::write(home.path().join(REGISTRY_FILE), r#"{"extensions":[]}"#)
            .expect("registry should be written");
        assert!(matches!(
            PhiExtensions::load(home.path()),
            Err(PhiLoadError::Contract { .. })
        ));

        fs::write(
            home.path().join(REGISTRY_FILE),
            r#"{"version":2,"extensions":[]}"#,
        )
        .expect("registry should be rewritten");
        assert!(matches!(
            PhiExtensions::load(home.path()),
            Err(PhiLoadError::Contract { .. })
        ));

        fs::write(
            home.path().join(REGISTRY_FILE),
            r#"{"version":1,"extensions":[{"name":"same","path":"extensions/../outside"}]}"#,
        )
        .expect("registry should be written");
        assert!(matches!(
            PhiExtensions::load(home.path()),
            Err(PhiLoadError::Contract { .. })
        ));

        fs::write(
            home.path().join(REGISTRY_FILE),
            r#"{"version":1,"extensions":[{"name":"same","path":"extensions/a"},{"name":"same","path":"extensions/b"}]}"#,
        )
        .expect("registry should be rewritten");
        assert!(matches!(
            PhiExtensions::load(home.path()),
            Err(PhiLoadError::Contract { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn loader_rejects_symlinked_module_and_authoring_leaf() {
        use std::os::unix::fs::symlink;
        let home = TestHome::new();
        let extension = home.path().join("extensions/demo");
        fs::create_dir_all(&extension).expect("extension directory");
        fs::write(
            extension.join("manifest.json"),
            r#"{"version":1,"name":"demo","entrypoint":"main.luau","modules":["main.luau"]}"#,
        )
        .expect("manifest");
        let outside = home.path().join("outside.luau");
        fs::write(&outside, "return { system_prompt_append = '' }").expect("outside source");
        symlink(&outside, extension.join("main.luau")).expect("module symlink");
        fs::write(
            home.path().join(REGISTRY_FILE),
            r#"{"version":1,"extensions":[{"name":"demo","path":"extensions/demo"}]}"#,
        )
        .expect("registry");
        assert!(matches!(
            PhiExtensions::load(home.path()),
            Err(PhiLoadError::Contract { .. })
        ));

        let root = home.path().join("extensions");
        let target = home.path().join("outside.txt");
        fs::write(&target, "do not replace").expect("target");
        let leaf = root.join("leaf.txt");
        symlink(&target, &leaf).expect("leaf symlink");
        let tool = PhiExtensionFilesTool::new(home.path());
        let call = ToolCall {
            id: ToolCallId::new("call-leaf").expect("call id"),
            name: "phi_extension_files".into(),
            arguments: pi_agent_core::state::SerializedJson::new(
                r#"{"operation":"write_draft","path":"leaf.txt","content":"changed"}"#,
            ),
        };
        let result = tool.execute(
            call,
            ToolContext {
                cancellation: pi_agent_core::scheduler::CancellationToken::new(),
                metadata: None,
            },
            ToolUpdateSink::disabled(),
        );
        assert!(matches!(
            smol::block_on(result),
            Err(ToolError::Blocked { .. })
        ));
        assert_eq!(
            fs::read_to_string(target).expect("target remains"),
            "do not replace"
        );

        let outside_directory = home.path().join("outside-directory");
        fs::create_dir_all(&outside_directory).expect("outside directory");
        symlink(&outside_directory, root.join("nested")).expect("nested directory symlink");
        let nested_call = ToolCall {
            id: ToolCallId::new("call-nested").expect("call id"),
            name: "phi_extension_files".into(),
            arguments: pi_agent_core::state::SerializedJson::new(
                r#"{"operation":"write_draft","path":"nested/draft.luau","content":"return nil"}"#,
            ),
        };
        let nested_result = tool.execute(
            nested_call,
            ToolContext {
                cancellation: pi_agent_core::scheduler::CancellationToken::new(),
                metadata: None,
            },
            ToolUpdateSink::disabled(),
        );
        assert!(matches!(
            smol::block_on(nested_result),
            Err(ToolError::Blocked { .. })
        ));
        assert!(!outside_directory.join("draft.luau").exists());
    }
}
