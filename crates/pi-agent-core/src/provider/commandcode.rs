//! Command Code NDJSON gateway provider adapter.
//!
//! The adapter accepts all authority explicitly: its API key, the gateway model, and the host
//! context the gateway includes in each request. It never discovers a working directory, date,
//! operating system, environment variable, or local Command Code credential file. Hosts convert
//! their transcript to the standard Chat Completions message array before it reaches this adapter.

use crate::scheduler::{
    CancellationToken, ModelFuture, ModelProvider, ModelRequest, ModelStream, ModelStreamEvent,
};
use crate::state::{
    AssistantToolCall, SerializedJson, StopReason, ThinkingLevel, ToolCallId, Usage,
};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::fmt;
use std::process::{Command, Stdio};
use std::sync::Mutex;

const API_URL: &str = "https://api.commandcode.ai/alpha/generate";
// Command Code's installed 1.24.0 client sends this exact gateway version. Keep this wire
// value here, rather than inheriting a host-process version, so embeddings remain reproducible.
const CLIENT_VERSION: &str = "1.24.0";

/// Error raised when explicit Command Code configuration violates an adapter invariant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandCodeConfigError {
    /// A required caller-supplied text value was empty.
    EmptyField(&'static str),
    /// The maximum output token cap was zero.
    ZeroMaxTokens,
    /// The temperature was not finite.
    NonFiniteTemperature,
    /// Command Code only serializes canonical UUID thread identifiers.
    InvalidThreadId,
}

impl fmt::Display for CommandCodeConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyField(field) => write!(formatter, "Command Code {field} must not be empty"),
            Self::ZeroMaxTokens => {
                formatter.write_str("Command Code max tokens must be greater than zero")
            }
            Self::NonFiniteTemperature => {
                formatter.write_str("Command Code temperature must be finite")
            }
            Self::InvalidThreadId => {
                formatter.write_str("Command Code thread ID must be a canonical UUID")
            }
        }
    }
}

impl std::error::Error for CommandCodeConfigError {}

/// Caller-provided host metadata included in a Command Code gateway request.
///
/// These values are deliberately explicit rather than discovered from the local process. A
/// sandboxed host can supply the virtual workspace and platform it has actually authorized.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandCodeHostContext {
    working_directory: String,
    date: String,
    environment: String,
}

impl CommandCodeHostContext {
    /// Construct host metadata after rejecting empty values.
    pub fn new(
        working_directory: impl Into<String>,
        date: impl Into<String>,
        environment: impl Into<String>,
    ) -> Result<Self, CommandCodeConfigError> {
        Ok(Self {
            working_directory: nonempty(working_directory.into(), "working directory")?,
            date: nonempty(date.into(), "date")?,
            environment: nonempty(environment.into(), "environment")?,
        })
    }
}

/// Command Code permission mode for one request.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CommandCodePermissionMode {
    /// Let the gateway apply its standard permission behavior.
    #[default]
    Standard,
    /// Let the gateway auto-accept permitted operations.
    AutoAccept,
    /// Ask the gateway to plan instead of applying changes.
    Plan,
}

impl CommandCodePermissionMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::AutoAccept => "auto-accept",
            Self::Plan => "plan",
        }
    }
}

/// Caller-owned configuration for [`CommandCodeProvider`].
///
/// The default is the gateway's `agent` mode and a 64,000 token cap, matching the upstream Pi
/// catalog. Credentials are redacted from its [`fmt::Debug`] representation.
#[derive(Clone, PartialEq)]
pub struct CommandCodeConfig {
    api_key: String,
    model: String,
    host: CommandCodeHostContext,
    max_tokens: u64,
    permission_mode: CommandCodePermissionMode,
    thread_id: Option<String>,
    mode: String,
    temperature: Option<f64>,
    zero_data_retention: bool,
    project_slug: Option<String>,
    taste_learning_enabled: bool,
}

impl CommandCodeConfig {
    /// Configure a Command Code model with explicit credentials and host metadata.
    pub fn new(
        api_key: impl Into<String>,
        model: impl Into<String>,
        host: CommandCodeHostContext,
    ) -> Result<Self, CommandCodeConfigError> {
        Ok(Self {
            api_key: nonempty(api_key.into(), "API key")?,
            model: nonempty(model.into(), "model")?,
            host,
            max_tokens: 64_000,
            permission_mode: CommandCodePermissionMode::Standard,
            thread_id: None,
            mode: "agent".into(),
            temperature: None,
            zero_data_retention: false,
            project_slug: None,
            // Command Code CLI 1.24.0 enables this client feature by default. Embeddings can
            // turn it off explicitly; the adapter never discovers a user preference.
            taste_learning_enabled: true,
        })
    }

    /// Replace the explicit maximum output-token cap.
    pub fn with_max_tokens(mut self, max_tokens: u64) -> Result<Self, CommandCodeConfigError> {
        if max_tokens == 0 {
            return Err(CommandCodeConfigError::ZeroMaxTokens);
        }
        self.max_tokens = max_tokens;
        Ok(self)
    }

    /// Set the gateway permission mode for each request.
    pub fn with_permission_mode(mut self, permission_mode: CommandCodePermissionMode) -> Self {
        self.permission_mode = permission_mode;
        self
    }

    /// Include a caller-owned Command Code thread identifier.
    pub fn with_thread_id(
        mut self,
        thread_id: impl Into<String>,
    ) -> Result<Self, CommandCodeConfigError> {
        let thread_id = nonempty(thread_id.into(), "thread ID")?;
        if !is_canonical_uuid(&thread_id) {
            return Err(CommandCodeConfigError::InvalidThreadId);
        }
        self.thread_id = Some(thread_id);
        Ok(self)
    }

    /// Replace the gateway mode, such as `agent` or a caller-defined mode.
    pub fn with_mode(mut self, mode: impl Into<String>) -> Result<Self, CommandCodeConfigError> {
        self.mode = nonempty(mode.into(), "mode")?;
        Ok(self)
    }

    /// Include a finite temperature in each gateway request.
    pub fn with_temperature(mut self, temperature: f64) -> Result<Self, CommandCodeConfigError> {
        if !temperature.is_finite() {
            return Err(CommandCodeConfigError::NonFiniteTemperature);
        }
        self.temperature = Some(temperature);
        Ok(self)
    }

    /// Opt into the gateway's explicit zero-data-retention request header.
    pub fn with_zero_data_retention(mut self, enabled: bool) -> Self {
        self.zero_data_retention = enabled;
        self
    }

    /// Override the project slug sent to the gateway.
    ///
    /// The default is the final path component of the already-explicit working directory,
    /// matching the current Command Code client without reading the process working directory.
    pub fn with_project_slug(
        mut self,
        project_slug: impl Into<String>,
    ) -> Result<Self, CommandCodeConfigError> {
        self.project_slug = Some(nonempty(project_slug.into(), "project slug")?);
        Ok(self)
    }

    /// Enable or disable Command Code's current taste-learning client feature.
    ///
    /// It defaults to the current Command Code CLI behavior. Callers that do not want to opt
    /// into that gateway feature can set it to `false` before constructing the provider.
    pub fn with_taste_learning_enabled(mut self, enabled: bool) -> Self {
        self.taste_learning_enabled = enabled;
        self
    }
}

impl fmt::Debug for CommandCodeConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CommandCodeConfig")
            .field("api_key", &"[redacted]")
            .field("model", &self.model)
            .field("host", &self.host)
            .field("max_tokens", &self.max_tokens)
            .field("permission_mode", &self.permission_mode)
            .field("thread_id", &self.thread_id)
            .field("mode", &self.mode)
            .field("temperature", &self.temperature)
            .field("zero_data_retention", &self.zero_data_retention)
            .field("project_slug", &self.project_slug)
            .field("taste_learning_enabled", &self.taste_learning_enabled)
            .finish()
    }
}

/// Command Code implementation of the generic [`ModelProvider`] port.
///
/// This adapter deliberately returns a finite stream after collecting the gateway's NDJSON
/// response through `curl`. Its parser preserves the gateway event grammar and rejects a
/// missing terminal event; it does not make an executor, transport, or credential-discovery
/// mechanism a default core dependency.
pub struct CommandCodeProvider {
    config: CommandCodeConfig,
    usage: Mutex<Usage>,
}

impl CommandCodeProvider {
    /// Construct an adapter from explicit caller-owned configuration.
    pub fn new(config: CommandCodeConfig) -> Self {
        Self {
            config,
            usage: Mutex::new(Usage::default()),
        }
    }

    /// Return aggregate portable token usage across settled Command Code turns.
    pub fn usage_snapshot(&self) -> Usage {
        self.usage
            .lock()
            .expect("Command Code usage mutex poisoned")
            .clone()
    }

    fn response_stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelStream {
        if cancellation.is_cancelled() {
            return ModelStream {
                events: vec![ModelStreamEvent::End(StopReason::Cancelled)],
            };
        }
        match self.complete(request) {
            Ok((mut events, usage)) => {
                self.record_usage(&usage);
                if usage.input_tokens.is_some()
                    || usage.output_tokens.is_some()
                    || usage.reasoning_tokens.is_some()
                {
                    let terminal = events
                        .pop()
                        .expect("parsed Command Code response has terminal event");
                    events.push(ModelStreamEvent::Usage(usage));
                    events.push(terminal);
                }
                ModelStream { events }
            }
            Err(message) => ModelStream {
                events: vec![ModelStreamEvent::Error { message }],
            },
        }
    }

    fn record_usage(&self, usage: &Usage) {
        let mut totals = self
            .usage
            .lock()
            .expect("Command Code usage mutex poisoned");
        totals.input_tokens =
            Some(totals.input_tokens.unwrap_or(0) + usage.input_tokens.unwrap_or(0));
        totals.output_tokens =
            Some(totals.output_tokens.unwrap_or(0) + usage.output_tokens.unwrap_or(0));
        totals.reasoning_tokens =
            Some(totals.reasoning_tokens.unwrap_or(0) + usage.reasoning_tokens.unwrap_or(0));
    }

    fn complete(&self, request: ModelRequest) -> Result<(Vec<ModelStreamEvent>, Usage), String> {
        self.validate_model(&request)?;
        let payload = self.build_payload(&request)?;
        let payload = serde_json::to_vec(&payload)
            .map_err(|_| "cannot serialize Command Code request".to_owned())?;
        let response = self.run_request(&payload)?;
        parse_ndjson_response(&response)
    }

    fn validate_model(&self, request: &ModelRequest) -> Result<(), String> {
        let Some(model) = &request.model else {
            return Ok(());
        };
        if model.provider == "command-code" && model.model == self.config.model {
            return Ok(());
        }
        Err("Command Code configuration does not match the requested model".into())
    }

    fn build_payload(&self, request: &ModelRequest) -> Result<Value, String> {
        let messages = commandcode_messages(&request.context)?;
        let tools = request
            .tools
            .iter()
            .map(|tool| {
                let schema = serde_json::from_str::<Value>(
                    &tool
                        .schema
                        .to_json_string()
                        .map_err(|_| "captured tool schema cannot be serialized")?,
                )
                .map_err(|_| "captured tool schema cannot be decoded")?;
                Ok(json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": schema,
                }))
            })
            .collect::<Result<Vec<_>, &str>>()?;
        let mut params = json!({
            "model": self.config.model,
            "messages": messages,
            "tools": tools,
            "system": request.system_prompt,
            "max_tokens": self.config.max_tokens,
            "stream": true,
        });
        if let Some(temperature) = self.config.temperature {
            params["temperature"] = json!(temperature);
        }
        if let Some(reasoning) = reasoning_effort(request.thinking_level) {
            params["reasoning_effort"] = Value::String(reasoning.into());
        }
        let mut payload = json!({
            "config": {
                "workingDir": self.config.host.working_directory,
                "date": self.config.host.date,
                "environment": self.config.host.environment,
                "structure": [],
                "isGitRepo": false,
                "currentBranch": "",
                "mainBranch": "",
                // This adapter does not discover a repository. Match Command Code's current
                // non-repository shape instead of claiming an unverified clean worktree.
                "gitStatus": "",
                "recentCommits": [],
            },
            "memory": Value::Null,
            "taste": Value::Null,
            "skills": Value::Null,
            "permissionMode": self.config.permission_mode.as_str(),
            "threadId": self.config.thread_id,
            "mode": self.config.mode,
            "params": params,
        });
        // The upstream JSON serializer omits `undefined` thread IDs. Sending a JSON null is a
        // distinct gateway input and is rejected, so preserve that wire-level distinction.
        if self.config.thread_id.is_none() {
            payload
                .as_object_mut()
                .expect("Command Code payload is an object")
                .remove("threadId");
        }
        Ok(payload)
    }

    fn run_request(&self, payload: &[u8]) -> Result<Vec<u8>, String> {
        let mut command = Command::new("curl");
        command
            .arg("--silent")
            .arg("--show-error")
            .arg("--no-buffer")
            .arg("--connect-timeout")
            .arg("10")
            .arg("--max-time")
            .arg("60")
            .arg("--request")
            .arg("POST")
            .arg(API_URL);
        for header in self.request_headers() {
            command.arg("--header").arg(header);
        }
        command
            .arg("--data-binary")
            .arg("@-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        run_curl(&mut command, payload)
    }

    fn request_headers(&self) -> Vec<String> {
        let mut headers = vec![
            "Accept: application/x-ndjson".into(),
            "Content-Type: application/json".into(),
            format!("Authorization: Bearer {}", self.config.api_key),
            // Current Command Code CLI normalizes its production telemetry environment to this
            // exact wire value. It is gateway client metadata, not the host operating system.
            "X-CLI-Environment: production".into(),
            format!("X-Command-Code-Version: {CLIENT_VERSION}"),
            "User-Agent: cli".into(),
            format!("X-Project-Slug: {}", self.project_slug()),
            format!("X-Taste-Learning: {}", self.config.taste_learning_enabled),
            // The official direct-key client sends this explicit non-OAuth value.
            "X-Co-Flag: false".into(),
        ];
        // The official client uses the same generated UUID for a fresh headless thread and
        // session. Library callers own that identifier; never synthesize one from ambient state.
        if let Some(thread_id) = &self.config.thread_id {
            headers.push(format!("X-Session-Id: {thread_id}"));
        }
        if self.config.zero_data_retention {
            headers.push("X-Cmd-Zdr: 1".into());
        }
        headers
    }

    fn project_slug(&self) -> &str {
        self.config.project_slug.as_deref().unwrap_or_else(|| {
            project_slug_from_working_directory(&self.config.host.working_directory)
        })
    }
}

impl fmt::Debug for CommandCodeProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CommandCodeProvider")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl ModelProvider for CommandCodeProvider {
    fn stream<'a>(
        &'a self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelFuture<'a> {
        let stream = self.response_stream(request, cancellation);
        Box::pin(std::future::ready(Ok(Box::new(stream) as _)))
    }
}

fn nonempty(value: String, field: &'static str) -> Result<String, CommandCodeConfigError> {
    if value.trim().is_empty() {
        Err(CommandCodeConfigError::EmptyField(field))
    } else {
        Ok(value)
    }
}

/// Match the canonical UUID form accepted by Command Code's current `z.uuid()` gate without
/// taking a UUID crate solely for request validation.
fn is_canonical_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36
        || !matches!(bytes[8], b'-')
        || !matches!(bytes[13], b'-')
        || !matches!(bytes[18], b'-')
        || !matches!(bytes[23], b'-')
    {
        return false;
    }
    bytes
        .iter()
        .enumerate()
        .all(|(index, byte)| matches!(index, 8 | 13 | 18 | 23) || byte.is_ascii_hexdigit())
}

fn project_slug_from_working_directory(working_directory: &str) -> &str {
    working_directory
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|component| !component.is_empty())
        // Host context rejects an empty path, so this fallback only covers roots such as `/`.
        .unwrap_or("project")
}

fn reasoning_effort(level: ThinkingLevel) -> Option<&'static str> {
    match level {
        ThinkingLevel::Default => None,
        ThinkingLevel::Off => Some("off"),
        ThinkingLevel::Minimal => Some("minimal"),
        ThinkingLevel::Low => Some("low"),
        ThinkingLevel::Medium => Some("medium"),
        ThinkingLevel::High => Some("high"),
        ThinkingLevel::XHigh => Some("xhigh"),
        ThinkingLevel::Max => Some("max"),
    }
}

fn run_curl(command: &mut Command, payload: &[u8]) -> Result<Vec<u8>, String> {
    let mut child = command
        .spawn()
        .map_err(|_| "could not start the Command Code HTTP transport".to_owned())?;
    {
        use std::io::Write;
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| "Command Code HTTP transport has no request pipe".to_owned())?;
        stdin
            .write_all(payload)
            .map_err(|_| "could not send Command Code request".to_owned())?;
    }
    let output = child
        .wait_with_output()
        .map_err(|_| "Command Code HTTP transport did not settle".to_owned())?;
    if !output.status.success() {
        return Err("Command Code HTTP transport failed before a provider response".into());
    }
    Ok(output.stdout)
}

fn commandcode_messages(context: &str) -> Result<Vec<Value>, String> {
    let messages: Vec<Value> = serde_json::from_str(context)
        .map_err(|_| "Command Code received invalid converted context".to_owned())?;
    let mut tool_names = BTreeMap::<String, String>::new();
    messages
        .iter()
        .map(|message| commandcode_message(message, &mut tool_names))
        .collect()
}

fn commandcode_message(
    message: &Value,
    tool_names: &mut BTreeMap<String, String>,
) -> Result<Value, String> {
    let object = message
        .as_object()
        .ok_or_else(|| "Command Code context message must be an object".to_owned())?;
    let role = string_field(object, "role", "Command Code context message")?;
    match role {
        "user" => Ok(json!({
            "role": "user",
            "content": [{"type": "text", "text": string_or_null(object, "content")?}],
        })),
        "assistant" => {
            let mut content = Vec::new();
            if let Some(text) = optional_string_or_null(object, "content")? {
                if !text.is_empty() {
                    content.push(json!({"type": "text", "text": text}));
                }
            }
            if let Some(calls) = object.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    let call = call.as_object().ok_or_else(|| {
                        "Command Code assistant tool call must be an object".to_owned()
                    })?;
                    let id = string_field(call, "id", "Command Code assistant tool call")?;
                    let function =
                        call.get("function")
                            .and_then(Value::as_object)
                            .ok_or_else(|| {
                                "Command Code assistant tool call did not contain a function"
                                    .to_owned()
                            })?;
                    let name = string_field(function, "name", "Command Code tool function")?;
                    let arguments =
                        string_field(function, "arguments", "Command Code tool function")?;
                    let input: Value = serde_json::from_str(arguments).map_err(|_| {
                        "Command Code tool call arguments must be serialized JSON".to_owned()
                    })?;
                    if !input.is_object() {
                        return Err("Command Code tool call arguments must be a JSON object".into());
                    }
                    tool_names.insert(id.to_owned(), name.to_owned());
                    content.push(json!({
                        "type": "tool-call",
                        "toolCallId": id,
                        "toolName": name,
                        "input": input,
                    }));
                }
            }
            Ok(json!({"role": "assistant", "content": content}))
        }
        "tool" => {
            let id = string_field(object, "tool_call_id", "Command Code tool result")?;
            let name = tool_names.get(id).ok_or_else(|| {
                "Command Code tool result has no preceding assistant tool-call name".to_owned()
            })?;
            Ok(json!({
                "role": "tool",
                "content": [{
                    "type": "tool-result",
                    "toolCallId": id,
                    "toolName": name,
                    "output": {"type": "text", "value": string_or_null(object, "content")?},
                    "isError": object.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                }],
            }))
        }
        _ => Err("Command Code context role is unsupported".into()),
    }
}

fn string_field<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    subject: &str,
) -> Result<&'a str, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{subject} did not contain {key}"))
}

fn optional_string_or_null<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> Result<Option<&'a str>, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(format!(
            "Command Code context {key} must be a string or null"
        )),
    }
}

fn string_or_null(object: &Map<String, Value>, key: &str) -> Result<String, String> {
    Ok(optional_string_or_null(object, key)?
        .unwrap_or_default()
        .to_owned())
}

fn parse_ndjson_response(bytes: &[u8]) -> Result<(Vec<ModelStreamEvent>, Usage), String> {
    let response = std::str::from_utf8(bytes)
        .map_err(|_| "Command Code returned a non-UTF-8 NDJSON response".to_owned())?;
    let mut events = Vec::new();
    let mut usage = Usage::default();
    let mut terminal = false;
    for line in response.lines().filter(|line| !line.trim().is_empty()) {
        let event: Value = serde_json::from_str(line)
            .map_err(|_| "Command Code returned invalid NDJSON".to_owned())?;
        let event = event
            .as_object()
            .ok_or_else(|| "Command Code NDJSON event must be an object".to_owned())?;
        let event_type = string_field(event, "type", "Command Code NDJSON event")?;
        if terminal {
            // Command Code 1.24.0 emits this non-content metadata envelope after `finish`.
            // It is not a second terminal event and carries no core stream state.
            if event_type == "provider-metadata" {
                continue;
            }
            return Err("Command Code response contained events after its terminal event".into());
        }
        match event_type {
            "text-delta" => {
                let text = string_field(event, "text", "Command Code text delta")?;
                if !text.is_empty() {
                    events.push(ModelStreamEvent::TextDelta(text.to_owned()));
                }
            }
            "reasoning-start" | "reasoning-delta" => {}
            "tool-call" => {
                let id = string_field(event, "toolCallId", "Command Code tool call")?;
                let name = string_field(event, "toolName", "Command Code tool call")?;
                let input = event
                    .get("input")
                    .or_else(|| event.get("args"))
                    .filter(|value| value.is_object())
                    .ok_or_else(|| {
                        "Command Code tool call did not contain object input".to_owned()
                    })?;
                let arguments = serde_json::to_string(input)
                    .map_err(|_| "Command Code tool call input cannot be serialized".to_owned())?;
                events.push(ModelStreamEvent::ToolCall(AssistantToolCall {
                    id: ToolCallId::new(id)
                        .map_err(|_| "Command Code tool call omitted its identifier".to_owned())?,
                    name: name.to_owned(),
                    arguments: SerializedJson::new(arguments),
                }));
            }
            "finish" => {
                usage = parse_usage(event.get("totalUsage"));
                events.push(ModelStreamEvent::End(finish_reason(
                    event.get("finishReason"),
                )));
                terminal = true;
            }
            "error" => {
                events.push(ModelStreamEvent::Error {
                    message: "Command Code provider returned an error".into(),
                });
                terminal = true;
            }
            "abort" => {
                events.push(ModelStreamEvent::Aborted {
                    message: "Command Code stream aborted".into(),
                });
                terminal = true;
            }
            _ => {}
        }
    }
    if terminal {
        Ok((events, usage))
    } else {
        Err("Command Code stream ended without a terminal event".into())
    }
}

fn finish_reason(value: Option<&Value>) -> StopReason {
    match value.and_then(Value::as_str) {
        Some("tool-calls") => StopReason::ToolUse,
        Some("length") => StopReason::Length,
        _ => StopReason::EndTurn,
    }
}

fn parse_usage(value: Option<&Value>) -> Usage {
    let Some(value) = value.and_then(Value::as_object) else {
        return Usage::default();
    };
    let input_tokens = value.get("inputTokens").and_then(Value::as_u64);
    let output_tokens = value.get("outputTokens").and_then(Value::as_u64);
    let reasoning_tokens = value
        .get("reasoningTokens")
        .and_then(Value::as_u64)
        .or_else(|| value.get("reasoning_tokens").and_then(Value::as_u64));
    Usage {
        input_tokens,
        output_tokens,
        reasoning_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ModelDescriptor;
    use crate::tool::{ToolDefinition, ToolExecutionMode};
    use pi_agent_protocol::JsonValue;

    fn config() -> CommandCodeConfig {
        let host = CommandCodeHostContext::new("/sandbox/project", "2026-08-14", "darwin")
            .expect("explicit host context");
        CommandCodeConfig::new("test-key", "deepseek/deepseek-v4-flash", host)
            .expect("explicit provider config")
            .with_permission_mode(CommandCodePermissionMode::AutoAccept)
            .with_thread_id("b51a3243-2dd9-4c81-b659-a039645b7d4e")
            .expect("thread id")
            .with_temperature(0.25)
            .expect("temperature")
            .with_zero_data_retention(true)
    }

    #[test]
    fn serializes_gateway_payload_from_explicit_host_context() {
        let request = ModelRequest {
            system_prompt: "Be concise".into(),
            context: r#"[
                {"role":"user","content":"inspect the tree"},
                {"role":"assistant","content":null,"tool_calls":[{
                    "id":"call-1","type":"function",
                    "function":{"name":"read","arguments":"{\"path\":\"README.md\"}"}
                }]},
                {"role":"tool","tool_call_id":"call-1","content":"contents"}
            ]"#
            .into(),
            tools: vec![ToolDefinition {
                name: "read".into(),
                description: "Read a file".into(),
                schema: JsonValue::parse(
                    r#"{"type":"object","properties":{"path":{"type":"string"}}}"#,
                )
                .expect("tool schema"),
                execution_mode: ToolExecutionMode::Parallel,
            }],
            model: Some(ModelDescriptor {
                provider: "command-code".into(),
                model: "deepseek/deepseek-v4-flash".into(),
                revision: None,
            }),
            thinking_level: ThinkingLevel::High,
        };

        let payload = CommandCodeProvider::new(config())
            .build_payload(&request)
            .expect("payload");
        assert_eq!(payload["config"]["workingDir"], "/sandbox/project");
        assert_eq!(payload["config"]["date"], "2026-08-14");
        assert_eq!(payload["config"]["environment"], "darwin");
        assert_eq!(payload["permissionMode"], "auto-accept");
        assert_eq!(payload["threadId"], "b51a3243-2dd9-4c81-b659-a039645b7d4e");
        assert_eq!(payload["config"]["gitStatus"], "");
        assert_eq!(payload["params"]["reasoning_effort"], "high");
        assert_eq!(
            payload["params"]["tools"][0]["input_schema"]["type"],
            "object"
        );
        assert_eq!(
            payload["params"]["messages"][1]["content"][0]["type"],
            "tool-call"
        );
        assert_eq!(
            payload["params"]["messages"][2]["content"][0]["toolName"],
            "read"
        );
    }

    #[test]
    fn translates_ndjson_text_tool_usage_and_finish() {
        let (events, usage) = parse_ndjson_response(
            br#"{"type":"text-delta","text":"hi"}
{"type":"reasoning-delta","text":"thinking"}
{"type":"tool-call","toolCallId":"call-1","toolName":"read","input":{"path":"README.md"}}
{"type":"finish","finishReason":"tool-calls","totalUsage":{"inputTokens":12,"outputTokens":4,"reasoningTokens":2}}
{"type":"provider-metadata","provider":"command-code"}"#,
        )
        .expect("NDJSON response parses");
        assert_eq!(events.len(), 3);
        assert_eq!(events[0], ModelStreamEvent::TextDelta("hi".into()));
        assert_eq!(
            events[1],
            ModelStreamEvent::ToolCall(AssistantToolCall {
                id: ToolCallId::new("call-1").expect("call id"),
                name: "read".into(),
                arguments: SerializedJson::new(r#"{"path":"README.md"}"#),
            })
        );
        assert_eq!(events[2], ModelStreamEvent::End(StopReason::ToolUse));
        assert_eq!(
            usage,
            Usage {
                input_tokens: Some(12),
                output_tokens: Some(4),
                reasoning_tokens: Some(2),
            }
        );
    }

    #[test]
    fn remote_error_body_is_not_exposed_to_the_agent() {
        let (events, usage) = parse_ndjson_response(
            br#"{"type":"error","error":{"message":"key test-key leaked remotely"}}"#,
        )
        .expect("error is a terminal provider event");
        assert_eq!(usage, Usage::default());
        assert_eq!(
            events,
            vec![ModelStreamEvent::Error {
                message: "Command Code provider returned an error".into(),
            }]
        );
    }

    #[test]
    fn rejects_content_after_finish_but_accepts_only_known_metadata() {
        let error = parse_ndjson_response(
            br#"{"type":"finish","finishReason":"stop"}
{"type":"text-delta","text":"late content"}"#,
        )
        .expect_err("content after finish is not valid Command Code stream grammar");
        assert_eq!(
            error,
            "Command Code response contained events after its terminal event"
        );
    }

    #[test]
    fn configuration_rejects_ambient_placeholders_and_redacts_the_key() {
        assert_eq!(
            CommandCodeHostContext::new("", "2026-08-14", "darwin"),
            Err(CommandCodeConfigError::EmptyField("working directory"))
        );
        let debug = format!("{:?}", config());
        assert!(debug.contains("[redacted]"));
        assert!(!debug.contains("test-key"));
    }

    #[test]
    fn gateway_headers_match_the_upstream_command_code_contract() {
        let headers = CommandCodeProvider::new(config()).request_headers();
        assert!(headers.contains(&"Accept: application/x-ndjson".into()));
        assert!(headers.contains(&"Authorization: Bearer test-key".into()));
        assert!(headers.contains(&"X-CLI-Environment: production".into()));
        assert!(headers.contains(&"X-Command-Code-Version: 1.24.0".into()));
        assert!(headers.contains(&"X-Project-Slug: project".into()));
        assert!(headers.contains(&"X-Taste-Learning: true".into()));
        assert!(headers.contains(&"X-Co-Flag: false".into()));
        assert!(headers.contains(&"X-Session-Id: b51a3243-2dd9-4c81-b659-a039645b7d4e".into()));
        assert!(headers.contains(&"User-Agent: cli".into()));
        assert!(headers.contains(&"X-Cmd-Zdr: 1".into()));
    }

    #[test]
    fn rejects_a_request_for_a_different_provider_or_model() {
        let request = ModelRequest {
            model: Some(ModelDescriptor {
                provider: "openrouter".into(),
                model: "different-model".into(),
                revision: None,
            }),
            ..ModelRequest::default()
        };
        let error = CommandCodeProvider::new(config())
            .validate_model(&request)
            .expect_err("provider mismatch is explicit");
        assert_eq!(
            error,
            "Command Code configuration does not match the requested model"
        );
    }

    #[test]
    fn omits_an_unset_thread_id_instead_of_sending_json_null() {
        let host = CommandCodeHostContext::new("/sandbox/project", "2026-08-14", "darwin")
            .expect("explicit host context");
        let provider = CommandCodeProvider::new(
            CommandCodeConfig::new("test-key", "deepseek/deepseek-v4-flash", host)
                .expect("provider config"),
        );
        let payload = provider
            .build_payload(&ModelRequest {
                context: "[]".into(),
                ..ModelRequest::default()
            })
            .expect("payload");
        assert!(payload.get("threadId").is_none());
    }

    #[test]
    fn rejects_non_uuid_thread_ids_instead_of_sending_a_different_wire_shape() {
        let host = CommandCodeHostContext::new("/sandbox/project", "2026-08-14", "darwin")
            .expect("explicit host context");
        let error = CommandCodeConfig::new("test-key", "deepseek/deepseek-v4-flash", host)
            .expect("provider config")
            .with_thread_id("thread-7")
            .expect_err("the current Command Code client omits non-UUID thread IDs");
        assert_eq!(error, CommandCodeConfigError::InvalidThreadId);
    }

    #[test]
    fn caller_can_override_the_project_slug_and_disable_taste_learning() {
        let headers = CommandCodeProvider::new(
            config()
                .with_project_slug("virtual-project")
                .expect("project slug")
                .with_taste_learning_enabled(false),
        )
        .request_headers();
        assert!(headers.contains(&"X-Project-Slug: virtual-project".into()));
        assert!(headers.contains(&"X-Taste-Learning: false".into()));
    }
}
