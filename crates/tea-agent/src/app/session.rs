//! Pi-compatible JSONL session persistence owned by the TUI application.
//!
//! The file shape intentionally follows Pi's coding-agent session manager: one v3 `session`
//! header followed by typed entries with `id`/`parentId` links. The repository keeps the
//! feature linear (there is no tree UI), but it does not invent a second envelope format.

use tea_core::state::{
    AgentMessage, AgentToolCall, MessageId, ModelDescriptor, SerializedJson, StopReason,
    ThinkingLevel, ToolCallId, Usage,
};
use tea_protocol::{JsonNumber, JsonValue};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Pi's current coding-agent session format version.
pub(crate) const SESSION_VERSION: u64 = 3;
const MAX_SESSION_BYTES: u64 = 16 * 1024 * 1024;
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A linear session reconstructed from a Pi-compatible JSONL file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionRecord {
    pub(crate) id: String,
    pub(crate) timestamp: String,
    pub(crate) cwd: String,
    pub(crate) model: Option<ModelDescriptor>,
    pub(crate) thinking_level: ThinkingLevel,
    pub(crate) messages: Vec<AgentMessage>,
}

impl SessionRecord {
    pub(crate) fn new(model: Option<ModelDescriptor>, thinking_level: ThinkingLevel) -> Self {
        Self {
            id: new_session_id(),
            timestamp: timestamp_iso(now_ms()),
            cwd: String::new(),
            model,
            thinking_level,
            messages: Vec::new(),
        }
    }

    pub(crate) fn with_workspace(mut self, workspace: impl Into<String>) -> Self {
        self.cwd = workspace.into();
        self
    }
}

/// Metadata shown by the minimal resume picker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionSummary {
    pub(crate) id: String,
    pub(crate) modified_at_ms: u64,
    pub(crate) cwd: String,
    pub(crate) model: Option<ModelDescriptor>,
    pub(crate) message_count: usize,
}

/// Failures at the application-owned persistence boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionError {
    Io {
        path: PathBuf,
        message: String,
    },
    Contract {
        path: PathBuf,
        message: String,
    },
    Json {
        path: PathBuf,
        line: usize,
        message: String,
    },
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, message } => {
                write!(f, "session I/O failed at {}: {message}", path.display())
            }
            Self::Contract { path, message } => {
                write!(f, "invalid session at {}: {message}", path.display())
            }
            Self::Json {
                path,
                line,
                message,
            } => write!(
                f,
                "invalid session JSON at {} line {line}: {message}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for SessionError {}

/// Store rooted below one explicit Phi home and, like Pi, partitioned by working directory.
#[derive(Clone, Debug)]
pub(crate) struct SessionStore {
    sessions_root: PathBuf,
    directory: PathBuf,
}

impl SessionStore {
    pub(crate) fn new(phi_home: impl AsRef<Path>) -> Self {
        let sessions_root = phi_home.as_ref().join("sessions");
        Self {
            directory: sessions_root.clone(),
            sessions_root,
        }
    }

    pub(crate) fn for_workspace(mut self, workspace: impl AsRef<Path>) -> Self {
        self.directory = self.sessions_root.join(encoded_cwd(workspace.as_ref()));
        self
    }

    #[cfg(test)]
    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    pub(crate) fn save(&self, record: &SessionRecord) -> Result<PathBuf, SessionError> {
        validate_session_id(&record.id).map_err(|message| contract(&self.directory, message))?;
        self.ensure_directory()?;
        let destination = self.find_path(&record.id)?.unwrap_or_else(|| {
            self.directory.join(format!(
                "{}_{}.jsonl",
                filename_timestamp(&record.timestamp),
                record.id
            ))
        });
        let source = encode_file(record)
            .into_iter()
            .map(|value| value.to_json_string().map(|line| format!("{line}\n")))
            .collect::<Result<String, _>>()
            .map_err(|error| SessionError::Json {
                path: destination.clone(),
                line: 0,
                message: error.to_string(),
            })?;
        if source.len() as u64 > MAX_SESSION_BYTES {
            return Err(contract(
                &destination,
                "session exceeds the 16 MiB safety limit",
            ));
        }
        let nonce = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let temporary = self.directory.join(format!(".session-{nonce}.tmp"));
        fs::write(&temporary, source.as_bytes()).map_err(|error| io_error(&temporary, error))?;
        if let Err(error) = set_private_file(&temporary) {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        if let Err(error) = fs::rename(&temporary, &destination) {
            let _ = fs::remove_file(&temporary);
            return Err(io_error(&destination, error));
        }
        Ok(destination)
    }

    pub(crate) fn load(&self, id: &str) -> Result<SessionRecord, SessionError> {
        validate_session_id(id).map_err(|message| contract(&self.directory, message))?;
        let path = self.find_path(id)?.ok_or_else(|| {
            io_error(
                &self.directory,
                std::io::Error::from(std::io::ErrorKind::NotFound),
            )
        })?;
        decode_file(&path)
    }

    pub(crate) fn list(&self) -> Result<Vec<SessionSummary>, SessionError> {
        let entries = match fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(io_error(&self.directory, error)),
        };
        let mut summaries = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let path = entry.path();
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_file() => metadata,
                _ => continue,
            };
            if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl")
                || metadata.len() > MAX_SESSION_BYTES
            {
                continue;
            }
            let Ok(record) = decode_file(&path) else {
                continue;
            };
            let modified_at_ms = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
                .unwrap_or(0);
            summaries.push(SessionSummary {
                id: record.id,
                modified_at_ms,
                cwd: record.cwd,
                model: record.model,
                message_count: record.messages.len(),
            });
        }
        summaries.sort_by(|left, right| {
            right
                .modified_at_ms
                .cmp(&left.modified_at_ms)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(summaries)
    }

    fn find_path(&self, id: &str) -> Result<Option<PathBuf>, SessionError> {
        let entries = match fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error(&self.directory, error)),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            if !metadata.file_type().is_file()
                || path.extension().and_then(|extension| extension.to_str()) != Some("jsonl")
            {
                continue;
            }
            if let Ok(record) = decode_file(&path) {
                if record.id == id {
                    return Ok(Some(path));
                }
            }
        }
        Ok(None)
    }

    fn ensure_directory(&self) -> Result<(), SessionError> {
        for path in [&self.sessions_root, &self.directory] {
            if let Ok(metadata) = fs::symlink_metadata(path) {
                if metadata.file_type().is_symlink() {
                    return Err(contract(path, "session directories cannot be symlinks"));
                }
            }
        }
        fs::create_dir_all(&self.directory).map_err(|error| io_error(&self.directory, error))?;
        let metadata = fs::symlink_metadata(&self.directory)
            .map_err(|error| io_error(&self.directory, error))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(contract(
                &self.directory,
                "session directory must be a real directory",
            ));
        }
        set_private_directory(&self.sessions_root)?;
        set_private_directory(&self.directory)?;
        Ok(())
    }
}

fn encode_file(record: &SessionRecord) -> Vec<JsonValue> {
    let mut values = Vec::with_capacity(record.messages.len() + 3);
    values.push(JsonValue::object([
        ("type", JsonValue::String("session".into())),
        ("version", number(SESSION_VERSION)),
        ("id", JsonValue::String(record.id.clone())),
        ("timestamp", JsonValue::String(record.timestamp.clone())),
        ("cwd", JsonValue::String(record.cwd.clone())),
    ]));
    let mut parent = None;
    if let Some(model) = &record.model {
        let id = entry_id();
        values.push(JsonValue::object([
            ("type", JsonValue::String("model_change".into())),
            ("id", JsonValue::String(id.clone())),
            ("parentId", optional_string(parent.as_deref())),
            ("timestamp", JsonValue::String(timestamp_iso(now_ms()))),
            ("provider", JsonValue::String(model.provider.clone())),
            ("modelId", JsonValue::String(model.model.clone())),
        ]));
        parent = Some(id);
    }
    let thinking_id = entry_id();
    values.push(JsonValue::object([
        ("type", JsonValue::String("thinking_level_change".into())),
        ("id", JsonValue::String(thinking_id.clone())),
        ("parentId", optional_string(parent.as_deref())),
        ("timestamp", JsonValue::String(timestamp_iso(now_ms()))),
        (
            "thinkingLevel",
            JsonValue::String(thinking_name(record.thinking_level).into()),
        ),
    ]));
    parent = Some(thinking_id);
    for message in &record.messages {
        let id = entry_id();
        values.push(JsonValue::object([
            ("type", JsonValue::String("message".into())),
            ("id", JsonValue::String(id.clone())),
            ("parentId", optional_string(parent.as_deref())),
            ("timestamp", JsonValue::String(timestamp_iso(now_ms()))),
            ("message", encode_message(message, record.model.as_ref())),
        ]));
        parent = Some(id);
    }
    values
}

fn encode_message(message: &AgentMessage, model: Option<&ModelDescriptor>) -> JsonValue {
    match message {
        AgentMessage::User { content, .. } => JsonValue::object([
            ("role", JsonValue::String("user".into())),
            ("content", JsonValue::String(content.clone())),
            ("timestamp", number(now_ms())),
        ]),
        AgentMessage::Assistant {
            content,
            tool_calls,
            stop_reason,
            error_message,
            ..
        } => {
            let mut blocks = Vec::new();
            if !content.is_empty() {
                blocks.push(JsonValue::object([
                    ("type", JsonValue::String("text".into())),
                    ("text", JsonValue::String(content.clone())),
                ]));
            }
            blocks.extend(tool_calls.iter().map(|call| {
                JsonValue::object([
                    ("type", JsonValue::String("toolCall".into())),
                    ("id", JsonValue::String(call.id.as_str().into())),
                    ("name", JsonValue::String(call.name.clone())),
                    ("arguments", parse_or_string(call.arguments.as_str())),
                ])
            }));
            let mut fields = vec![
                ("role", JsonValue::String("assistant".into())),
                ("content", JsonValue::Array(blocks)),
                (
                    "provider",
                    JsonValue::String(
                        model
                            .map(|model| model.provider.clone())
                            .unwrap_or_default(),
                    ),
                ),
                (
                    "model",
                    JsonValue::String(model.map(|model| model.model.clone()).unwrap_or_default()),
                ),
                (
                    "stopReason",
                    stop_reason
                        .map(stop_reason_name)
                        .map(String::from)
                        .map(JsonValue::String)
                        .unwrap_or(JsonValue::String("stop".into())),
                ),
                ("timestamp", number(now_ms())),
            ];
            if let Some(error) = error_message {
                fields.push(("errorMessage", JsonValue::String(error.clone())));
            }
            JsonValue::object(fields)
        }
        AgentMessage::ToolResult {
            tool_call_id,
            tool_name,
            content,
            details,
            usage,
            added_tool_names,
            is_error,
            ..
        } => {
            let mut fields = vec![
                ("role", JsonValue::String("toolResult".into())),
                (
                    "toolCallId",
                    JsonValue::String(tool_call_id.as_str().into()),
                ),
                ("toolName", JsonValue::String(tool_name.clone())),
                (
                    "content",
                    JsonValue::Array(vec![JsonValue::object([
                        ("type", JsonValue::String("text".into())),
                        ("text", JsonValue::String(content.clone())),
                    ])]),
                ),
                (
                    "details",
                    details
                        .as_ref()
                        .map(|value| parse_or_string(value.as_str()))
                        .unwrap_or(JsonValue::Null),
                ),
                (
                    "usage",
                    usage.as_ref().map(encode_usage).unwrap_or(JsonValue::Null),
                ),
                ("isError", JsonValue::Bool(*is_error)),
                ("timestamp", number(now_ms())),
            ];
            if !added_tool_names.is_empty() {
                fields.push((
                    "addedToolNames",
                    JsonValue::Array(
                        added_tool_names
                            .iter()
                            .map(|name| JsonValue::String(name.clone()))
                            .collect(),
                    ),
                ));
            }
            JsonValue::object(fields)
        }
    }
}

fn encode_usage(usage: &Usage) -> JsonValue {
    JsonValue::object([
        ("input", optional_number(usage.input_tokens)),
        ("output", optional_number(usage.output_tokens)),
        ("cacheRead", optional_number(usage.cache_read_tokens)),
        ("cacheWrite", optional_number(usage.cache_write_tokens)),
        (
            "totalTokens",
            usage
                .input_tokens
                .zip(usage.output_tokens)
                .map(|(input, output)| number(input.saturating_add(output)))
                .unwrap_or(JsonValue::Null),
        ),
        (
            "cost",
            JsonValue::object([
                ("input", JsonValue::Null),
                ("output", JsonValue::Null),
                ("cacheRead", JsonValue::Null),
                ("cacheWrite", JsonValue::Null),
                (
                    "total",
                    usage
                        .cost
                        .clone()
                        .map(JsonValue::String)
                        .unwrap_or(JsonValue::Null),
                ),
            ]),
        ),
    ])
}

fn decode_file(path: &Path) -> Result<SessionRecord, SessionError> {
    let metadata = fs::metadata(path).map_err(|error| io_error(path, error))?;
    if metadata.len() > MAX_SESSION_BYTES {
        return Err(contract(path, "session exceeds the 16 MiB safety limit"));
    }
    let source = fs::read_to_string(path).map_err(|error| io_error(path, error))?;
    let mut lines = source.lines();
    let header_line = lines
        .next()
        .ok_or_else(|| contract(path, "session has no header"))?;
    let header = JsonValue::parse(header_line).map_err(|error| SessionError::Json {
        path: path.to_path_buf(),
        line: 1,
        message: error.to_string(),
    })?;
    let header = expect_object(path, &header)?;
    if required_string(path, header, "type")? != "session" {
        return Err(contract(path, "first entry is not a session header"));
    }
    let version = header
        .get("version")
        .and_then(JsonValue::as_u64)
        .unwrap_or(1);
    if !(1..=SESSION_VERSION).contains(&version) {
        return Err(contract(
            path,
            format!("unsupported session version {version}"),
        ));
    }
    let id = required_string(path, header, "id")?;
    validate_session_id(&id).map_err(|message| contract(path, message))?;
    let timestamp = required_string(path, header, "timestamp")?;
    let cwd = header
        .get("cwd")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_owned();
    let mut model = match (
        header.get("provider").and_then(JsonValue::as_str),
        header.get("modelId").and_then(JsonValue::as_str),
    ) {
        (Some(provider), Some(model)) => Some(ModelDescriptor {
            provider: provider.into(),
            model: model.into(),
            revision: None,
        }),
        _ => None,
    };
    let mut thinking_level = header
        .get("thinkingLevel")
        .and_then(JsonValue::as_str)
        .map(|value| parse_thinking(path, value.to_owned()))
        .transpose()?
        .unwrap_or(ThinkingLevel::Off);
    let mut messages = Vec::new();
    for (line_index, line) in source.lines().enumerate().skip(1) {
        if line.trim().is_empty() {
            continue;
        }
        let value = match JsonValue::parse(line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let object = expect_object(path, &value)?;
        match required_string(path, object, "type")?.as_str() {
            "model_change" => {
                model = Some(ModelDescriptor {
                    provider: required_string(path, object, "provider")?,
                    model: required_string(path, object, "modelId")?,
                    revision: None,
                });
            }
            "thinking_level_change" => {
                thinking_level =
                    parse_thinking(path, required_string(path, object, "thinkingLevel")?)?;
            }
            "message" => {
                let message = object
                    .get("message")
                    .ok_or_else(|| contract(path, "message entry has no message"))?;
                let decoded = decode_message(path, message, MessageId(messages.len() as u64 + 1))?;
                if model.is_none() {
                    if let AgentMessage::Assistant { .. } = &decoded {
                        model = match (
                            message.get("provider").and_then(JsonValue::as_str),
                            message.get("model").and_then(JsonValue::as_str),
                        ) {
                            (Some(provider), Some(model))
                                if !provider.is_empty() && !model.is_empty() =>
                            {
                                Some(ModelDescriptor {
                                    provider: provider.into(),
                                    model: model.into(),
                                    revision: None,
                                })
                            }
                            _ => None,
                        };
                    }
                }
                messages.push(decoded);
            }
            _ => {
                let _ = line_index;
            }
        }
    }
    Ok(SessionRecord {
        id,
        timestamp,
        cwd,
        model,
        thinking_level,
        messages,
    })
}

fn decode_message(
    path: &Path,
    value: &JsonValue,
    id: MessageId,
) -> Result<AgentMessage, SessionError> {
    let object = expect_object(path, value)?;
    match required_string(path, object, "role")?.as_str() {
        "user" => Ok(AgentMessage::User {
            id,
            content: decode_content(path, object.get("content"))?,
        }),
        "assistant" => {
            let (content, tool_calls) = decode_assistant_content(path, object.get("content"))?;
            let stop_reason = object
                .get("stopReason")
                .and_then(JsonValue::as_str)
                .map(|value| parse_stop_reason(path, value))
                .transpose()?;
            Ok(AgentMessage::Assistant {
                id,
                content,
                tool_calls,
                stop_reason,
                error_message: optional_string_at(path, object, "errorMessage")?,
            })
        }
        "toolResult" => {
            let tool_call_id = ToolCallId::new(required_string(path, object, "toolCallId")?)
                .map_err(|_| contract(path, "toolCallId cannot be empty"))?;
            Ok(AgentMessage::ToolResult {
                id,
                tool_call_id,
                tool_name: required_string(path, object, "toolName")?,
                content: decode_content(path, object.get("content"))?,
                details: decode_serialized_json(object.get("details"))?,
                usage: decode_usage(path, object.get("usage"))?,
                added_tool_names: decode_string_array(object.get("addedToolNames")),
                terminate: false,
                is_error: object
                    .get("isError")
                    .and_then(JsonValue::as_bool)
                    .unwrap_or(false),
                failure: None,
            })
        }
        role => Err(contract(path, format!("unsupported message role {role:?}"))),
    }
}

fn decode_assistant_content(
    path: &Path,
    value: Option<&JsonValue>,
) -> Result<(String, Vec<AgentToolCall>), SessionError> {
    let Some(value) = value else {
        return Err(contract(path, "assistant content is missing"));
    };
    if let Some(text) = value.as_str() {
        return Ok((text.to_owned(), Vec::new()));
    }
    let values = value
        .as_array()
        .ok_or_else(|| contract(path, "assistant content must be text or an array"))?;
    let mut text = String::new();
    let mut calls = Vec::new();
    for value in values {
        let object = expect_object(path, value)?;
        match required_string(path, object, "type")?.as_str() {
            "text" => text.push_str(&required_string(path, object, "text")?),
            "toolCall" => calls.push(AgentToolCall {
                id: ToolCallId::new(required_string(path, object, "id")?)
                    .map_err(|_| contract(path, "tool call ID cannot be empty"))?,
                name: required_string(path, object, "name")?,
                arguments: SerializedJson::new(
                    object
                        .get("arguments")
                        .map(|value| value.to_json_string().unwrap_or_else(|_| "null".into()))
                        .unwrap_or_else(|| "null".into()),
                ),
            }),
            _ => {}
        }
    }
    Ok((text, calls))
}

fn decode_content(path: &Path, value: Option<&JsonValue>) -> Result<String, SessionError> {
    let Some(value) = value else {
        return Err(contract(path, "message content is missing"));
    };
    if let Some(text) = value.as_str() {
        return Ok(text.to_owned());
    }
    let values = value
        .as_array()
        .ok_or_else(|| contract(path, "message content must be text or an array"))?;
    let mut output = String::new();
    for value in values {
        if let Some(text) = value.get("text").and_then(JsonValue::as_str) {
            output.push_str(text);
        }
    }
    Ok(output)
}

fn decode_serialized_json(
    value: Option<&JsonValue>,
) -> Result<Option<SerializedJson>, SessionError> {
    match value {
        None | Some(JsonValue::Null) => Ok(None),
        Some(value) => Ok(Some(SerializedJson::new(
            value
                .to_json_string()
                .map_err(|error| error.to_string())
                .unwrap_or_else(|_| "null".into()),
        ))),
    }
}

fn decode_string_array(value: Option<&JsonValue>) -> Vec<String> {
    value
        .and_then(JsonValue::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(JsonValue::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn json_scalar_text(value: &JsonValue) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.to_json_string().ok())
}

fn decode_usage(path: &Path, value: Option<&JsonValue>) -> Result<Option<Usage>, SessionError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let object = expect_object(path, value)?;
    Ok(Some(Usage {
        input_tokens: object.get("input").and_then(JsonValue::as_u64),
        output_tokens: object.get("output").and_then(JsonValue::as_u64),
        reasoning_tokens: None,
        cache_read_tokens: object.get("cacheRead").and_then(JsonValue::as_u64),
        cache_write_tokens: object.get("cacheWrite").and_then(JsonValue::as_u64),
        cost: object
            .get("cost")
            .and_then(|value| value.get("total"))
            .and_then(json_scalar_text),
    }))
}

fn expect_object<'a>(
    path: &Path,
    value: &'a JsonValue,
) -> Result<&'a BTreeMap<String, JsonValue>, SessionError> {
    value
        .as_object()
        .ok_or_else(|| contract(path, "entry must be an object"))
}

fn required_string(
    path: &Path,
    object: &BTreeMap<String, JsonValue>,
    field: &str,
) -> Result<String, SessionError> {
    object
        .get(field)
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
        .ok_or_else(|| contract(path, format!("{field} must be a string")))
}

fn optional_string_at(
    path: &Path,
    object: &BTreeMap<String, JsonValue>,
    field: &str,
) -> Result<Option<String>, SessionError> {
    match object.get(field) {
        None | Some(JsonValue::Null) => Ok(None),
        Some(JsonValue::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(contract(path, format!("{field} must be a string or null"))),
    }
}

fn parse_or_string(value: &str) -> JsonValue {
    JsonValue::parse(value).unwrap_or_else(|_| JsonValue::String(value.into()))
}

fn parse_thinking(path: &Path, value: String) -> Result<ThinkingLevel, SessionError> {
    match value.as_str() {
        "off" => Ok(ThinkingLevel::Off),
        "minimal" => Ok(ThinkingLevel::Minimal),
        "low" => Ok(ThinkingLevel::Low),
        "medium" => Ok(ThinkingLevel::Medium),
        "high" => Ok(ThinkingLevel::High),
        "xhigh" => Ok(ThinkingLevel::XHigh),
        "max" => Ok(ThinkingLevel::Max),
        _ => Err(contract(path, format!("unknown thinking level {value:?}"))),
    }
}

fn parse_stop_reason(path: &Path, value: &str) -> Result<StopReason, SessionError> {
    match value {
        // Pi can retain deferred/pending assistant messages during provider recovery. The Rust
        // core has no corresponding non-terminal reason, so import them as settled turns.
        "pending" | "deferred" => Ok(StopReason::Stop),
        "stop" => Ok(StopReason::Stop),
        "toolUse" | "tool_use" => Ok(StopReason::ToolUse),
        "length" => Ok(StopReason::Length),
        "aborted" => Ok(StopReason::Aborted),
        "error" => Ok(StopReason::Error),
        "cancelled" => Ok(StopReason::Cancelled),
        _ => Err(contract(path, format!("unknown stop reason {value:?}"))),
    }
}

fn thinking_name(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Off => "off",
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::XHigh => "xhigh",
        ThinkingLevel::Max => "max",
    }
}

fn stop_reason_name(reason: StopReason) -> &'static str {
    match reason {
        StopReason::Stop => "stop",
        StopReason::ToolUse => "toolUse",
        StopReason::Length => "length",
        StopReason::Aborted => "aborted",
        StopReason::Cancelled => "cancelled",
        StopReason::Error => "error",
    }
}

fn optional_string(value: Option<&str>) -> JsonValue {
    value
        .map(|value| JsonValue::String(value.into()))
        .unwrap_or(JsonValue::Null)
}

fn number(value: u64) -> JsonValue {
    JsonValue::Number(JsonNumber::Unsigned(value))
}

fn optional_number(value: Option<u64>) -> JsonValue {
    value.map(number).unwrap_or(JsonValue::Null)
}

fn validate_session_id(id: &str) -> Result<(), String> {
    let bytes = id.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 128
        || !bytes[0].is_ascii_alphanumeric()
        || !bytes[bytes.len() - 1].is_ascii_alphanumeric()
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err("session ID contains forbidden characters".into());
    }
    Ok(())
}

fn encoded_cwd(cwd: &Path) -> String {
    let path = cwd.to_string_lossy().replace('\\', "/");
    format!(
        "--{}--",
        path.trim_start_matches('/')
            .replace(['/', ':'], "-")
    )
}

fn entry_id() -> String {
    format!("{:08x}", NEXT_ID.fetch_add(1, Ordering::Relaxed))
}

fn new_session_id() -> String {
    // Pi uses UUIDv7 session IDs. Keep the same sortable 48-bit millisecond prefix and UUID
    // version/variant bits without adding a UUID dependency to the host.
    let now = now_ms() & ((1_u64 << 48) - 1);
    let nonce = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let random_a = nonce & 0x0fff;
    let random_b = nonce & ((1_u64 << 62) - 1);
    let mut bytes = [0_u8; 16];
    bytes[0] = (now >> 40) as u8;
    bytes[1] = (now >> 32) as u8;
    bytes[2] = (now >> 24) as u8;
    bytes[3] = (now >> 16) as u8;
    bytes[4] = (now >> 8) as u8;
    bytes[5] = now as u8;
    bytes[6] = 0x70 | ((random_a >> 8) as u8 & 0x0f);
    bytes[7] = random_a as u8;
    bytes[8] = 0x80 | ((random_b >> 56) as u8 & 0x3f);
    for (offset, byte) in bytes[9..].iter_mut().enumerate() {
        *byte = (random_b >> (8 * (6 - offset))) as u8;
    }
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8],
        bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn filename_timestamp(timestamp: &str) -> String {
    let sanitized = timestamp.replace([':', '.'], "-");
    if !sanitized.is_empty()
        && sanitized
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        sanitized
    } else {
        timestamp_iso(now_ms()).replace([':', '.'], "-")
    }
}

fn timestamp_iso(milliseconds: u64) -> String {
    let seconds = milliseconds / 1_000;
    let millis = milliseconds % 1_000;
    let days = seconds / 86_400;
    let seconds_today = seconds % 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        seconds_today / 3_600,
        (seconds_today / 60) % 60,
        seconds_today % 60
    )
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    (year + if month <= 2 { 1 } else { 0 }, month, day)
}

fn set_private_directory(path: &Path) -> Result<(), SessionError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| io_error(path, error))?;
    }
    Ok(())
}

fn set_private_file(path: &Path) -> Result<(), SessionError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|error| io_error(path, error))?;
    }
    Ok(())
}

fn io_error(path: &Path, error: std::io::Error) -> SessionError {
    SessionError::Io {
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

fn contract(path: &Path, message: impl Into<String>) -> SessionError {
    SessionError::Contract {
        path: path.to_path_buf(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_uses_pi_v3_jsonl_header_and_linear_message_entries() {
        let root = std::env::temp_dir().join(format!(
            "tea-session-test-{}",
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let store = SessionStore::new(&root).for_workspace("/tmp/project");
        let call_id = ToolCallId::new("call-1").unwrap();
        let mut record = SessionRecord::new(
            Some(ModelDescriptor {
                provider: "local".into(),
                model: "demo".into(),
                revision: None,
            }),
            ThinkingLevel::Low,
        )
        .with_workspace("/tmp/project");
        record.messages = vec![
            AgentMessage::User {
                id: MessageId(1),
                content: "hello".into(),
            },
            AgentMessage::Assistant {
                id: MessageId(2),
                content: "hi".into(),
                tool_calls: vec![AgentToolCall {
                    id: call_id.clone(),
                    name: "shell".into(),
                    arguments: SerializedJson::new(r#"{"command":"pwd"}"#),
                }],
                stop_reason: Some(StopReason::ToolUse),
                error_message: None,
            },
            AgentMessage::ToolResult {
                id: MessageId(3),
                tool_call_id: call_id,
                tool_name: "shell".into(),
                content: "ok".into(),
                details: None,
                usage: None,
                added_tool_names: Vec::new(),
                terminate: false,
                is_error: false,
                failure: None,
            },
        ];
        let path = store.save(&record).unwrap();
        assert_eq!(
            path.extension().and_then(|value| value.to_str()),
            Some("jsonl")
        );
        let first = fs::read_to_string(path).unwrap();
        assert!(first
            .lines()
            .next()
            .unwrap()
            .contains(r#""type":"session""#));
        let loaded = store.load(&record.id).unwrap();
        assert_eq!(loaded.cwd, record.cwd);
        assert_eq!(loaded.model, record.model);
        assert_eq!(loaded.messages, record.messages);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn malformed_files_are_not_offered_by_resume_listing() {
        let root = std::env::temp_dir().join(format!(
            "tea-session-test-{}",
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let store = SessionStore::new(&root).for_workspace("/tmp/project");
        fs::create_dir_all(store.directory()).unwrap();
        fs::write(store.directory().join("broken.jsonl"), "not json\n").unwrap();
        assert!(store.list().unwrap().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn loads_pi_message_timestamps_usage_and_tool_extensions() {
        let root = std::env::temp_dir().join(format!(
            "tea-session-test-{}",
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let store = SessionStore::new(&root).for_workspace("/tmp/project");
        fs::create_dir_all(store.directory()).unwrap();
        let id = "018f0c8e-4d3b-7abc-8def-0123456789ab";
        let source = r#"{"type":"session","version":3,"id":"018f0c8e-4d3b-7abc-8def-0123456789ab","timestamp":"2025-01-01T00:00:00.000Z","cwd":"/tmp/project"}
{"type":"model_change","id":"00000001","parentId":null,"timestamp":"2025-01-01T00:00:00.001Z","provider":"local","modelId":"demo"}
{"type":"message","id":"00000002","parentId":"00000001","timestamp":"2025-01-01T00:00:00.002Z","message":{"role":"user","content":"inspect this","timestamp":1735689600002}}
{"type":"message","id":"00000003","parentId":"00000002","timestamp":"2025-01-01T00:00:00.003Z","message":{"role":"assistant","content":[{"type":"text","text":"I will inspect it"},{"type":"toolCall","id":"call-1","name":"read","arguments":{"path":"src/lib.rs"}}],"provider":"local","model":"demo","stopReason":"toolUse","timestamp":1735689600003}}
{"type":"message","id":"00000004","parentId":"00000003","timestamp":"2025-01-01T00:00:00.004Z","message":{"role":"toolResult","toolCallId":"call-1","toolName":"read","content":[{"type":"text","text":"done"}],"usage":{"input":2,"output":3,"cacheRead":0,"cacheWrite":0,"totalTokens":5,"cost":{"input":0,"output":0.2,"cacheRead":0,"cacheWrite":0,"total":0.2}},"addedToolNames":["grep"],"isError":false,"timestamp":1735689600004}}
"#;
        fs::write(
            store
                .directory()
                .join(format!("2025-01-01T00-00-00-000Z_{id}.jsonl")),
            source,
        )
        .unwrap();

        let loaded = store.load(id).unwrap();
        assert_eq!(loaded.model.unwrap().model, "demo");
        assert_eq!(loaded.messages.len(), 3);
        match &loaded.messages[2] {
            AgentMessage::ToolResult {
                added_tool_names,
                usage,
                ..
            } => {
                assert_eq!(added_tool_names, &["grep"]);
                assert_eq!(
                    usage.as_ref().and_then(|usage| usage.cost.as_deref()),
                    Some("0.2")
                );
            }
            message => panic!("expected tool result, got {message:?}"),
        }
        let _ = fs::remove_dir_all(root);
    }
}
