//! Local OpenAI-compatible Chat Completions provider.
//!
//! This adapter is intentionally transport-specific but server-agnostic: the caller supplies a
//! base URL and model, and the adapter sends one finite `chat/completions` request through the
//! local `curl` binary. It does not discover a server, read credentials, inspect the home
//! directory, or select a model from the environment. oMLX is the first supported local server;
//! its Laguna XS 2.1 endpoint is represented by [`LocalConfig::laguna_xs_2_1`].

use crate::json::{from_bytes, json_value, JsonValue};
use crate::scheduler::{
    CancellationToken, ModelFuture, ModelProvider, ModelRequest, ModelStream, ModelStreamEvent,
};
use crate::state::{
    AssistantToolCall, SerializedJson, StopReason, ThinkingLevel, ToolCallId, Usage,
};
use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

/// The model ID exposed by the documented 5-bit Laguna checkpoint.
pub const LAGUNA_XS_2_1_MODEL: &str = "Laguna-XS-2.1-5bit";

/// Default local OpenAI-compatible API root used by oMLX.
pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8000/v1";

/// Configuration failure at the local provider boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalConfigError {
    /// A required caller-supplied text value was empty.
    EmptyField(&'static str),
    /// The base URL must use an HTTP URL because it is passed directly to curl.
    InvalidBaseUrl,
    /// The maximum output-token cap was zero.
    ZeroMaxTokens,
    /// A sampling value was not finite or was outside the server's accepted range.
    InvalidSampling(&'static str),
    /// The request timeout was zero.
    ZeroRequestTimeout,
}

impl fmt::Display for LocalConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyField(field) => write!(formatter, "local {field} must not be empty"),
            Self::InvalidBaseUrl => {
                formatter.write_str("local base URL must start with http:// or https://")
            }
            Self::ZeroMaxTokens => {
                formatter.write_str("local max tokens must be greater than zero")
            }
            Self::InvalidSampling(field) => {
                write!(formatter, "local {field} must be finite and non-negative")
            }
            Self::ZeroRequestTimeout => {
                formatter.write_str("local request timeout must be greater than zero")
            }
        }
    }
}

impl std::error::Error for LocalConfigError {}

/// Caller-owned configuration for [`LocalProvider`].
#[derive(Clone, PartialEq)]
pub struct LocalConfig {
    base_url: String,
    model: String,
    max_tokens: u64,
    temperature: f64,
    top_p: f64,
    min_p: f64,
    enable_thinking: bool,
    request_timeout: Duration,
}

impl fmt::Debug for LocalConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalConfig")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("max_tokens", &self.max_tokens)
            .field("temperature", &self.temperature)
            .field("top_p", &self.top_p)
            .field("min_p", &self.min_p)
            .field("enable_thinking", &self.enable_thinking)
            .field("request_timeout", &self.request_timeout)
            .finish()
    }
}

impl LocalConfig {
    /// Configure a local OpenAI-compatible model with Laguna-compatible defaults.
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            max_tokens: 4_096,
            temperature: 1.0,
            top_p: 1.0,
            min_p: 0.0,
            enable_thinking: true,
            request_timeout: Duration::from_secs(300),
        }
    }

    /// Configure the oMLX Laguna XS 2.1 5-bit endpoint with its known request defaults.
    pub fn laguna_xs_2_1(base_url: impl Into<String>) -> Self {
        Self::new(base_url, LAGUNA_XS_2_1_MODEL)
    }

    /// Borrow the configured local API root.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Borrow the configured model identifier.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Replace the maximum number of generated tokens.
    pub fn with_max_tokens(mut self, max_tokens: u64) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Replace the sampling temperature.
    pub fn with_temperature(mut self, temperature: f64) -> Self {
        self.temperature = temperature;
        self
    }

    /// Replace nucleus sampling probability.
    pub fn with_top_p(mut self, top_p: f64) -> Self {
        self.top_p = top_p;
        self
    }

    /// Replace minimum probability sampling threshold.
    pub fn with_min_p(mut self, min_p: f64) -> Self {
        self.min_p = min_p;
        self
    }

    /// Enable or disable the model's chat-template reasoning mode.
    pub fn with_thinking(mut self, enabled: bool) -> Self {
        self.enable_thinking = enabled;
        self
    }

    /// Replace the complete request timeout used by the local transport.
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Validate the explicit configuration before constructing a provider.
    pub fn validate(&self) -> Result<(), LocalConfigError> {
        if self.base_url.trim().is_empty() {
            return Err(LocalConfigError::EmptyField("base URL"));
        }
        if !(self.base_url.starts_with("http://") || self.base_url.starts_with("https://")) {
            return Err(LocalConfigError::InvalidBaseUrl);
        }
        if self.model.trim().is_empty() {
            return Err(LocalConfigError::EmptyField("model"));
        }
        if self.max_tokens == 0 {
            return Err(LocalConfigError::ZeroMaxTokens);
        }
        for (field, value) in [
            ("temperature", self.temperature),
            ("top_p", self.top_p),
            ("min_p", self.min_p),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(LocalConfigError::InvalidSampling(field));
            }
        }
        if self.request_timeout.is_zero() {
            return Err(LocalConfigError::ZeroRequestTimeout);
        }
        Ok(())
    }

    /// Construct and validate one explicit local configuration.
    pub fn try_new(
        base_url: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<Self, LocalConfigError> {
        let config = Self::new(base_url, model);
        config.validate()?;
        Ok(config)
    }
}

/// A finite-response local OpenAI-compatible provider.
pub struct LocalProvider {
    config: LocalConfig,
}

impl fmt::Debug for LocalProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalProvider")
            .field("config", &self.config)
            .finish()
    }
}

impl LocalProvider {
    /// Construct a provider from already validated explicit configuration.
    pub fn new(config: LocalConfig) -> Self {
        Self { config }
    }

    fn response_stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelStream {
        if cancellation.is_cancelled() {
            return cancelled_stream();
        }
        match self.complete(request, &cancellation) {
            Ok((mut events, usage)) => {
                if cancellation.is_cancelled() {
                    return cancelled_stream();
                }
                let terminal = events
                    .pop()
                    .expect("local response parser always returns a terminal event");
                events.push(ModelStreamEvent::Usage(usage));
                events.push(terminal);
                ModelStream { events }
            }
            Err(_message) if cancellation.is_cancelled() => cancelled_stream(),
            Err(message) => ModelStream {
                events: vec![ModelStreamEvent::Error { message }],
            },
        }
    }

    fn complete(
        &self,
        request: ModelRequest,
        cancellation: &CancellationToken,
    ) -> Result<(Vec<ModelStreamEvent>, Usage), String> {
        let model = request
            .model
            .as_ref()
            .ok_or_else(|| "local request omitted its model descriptor".to_owned())?;
        if model.provider != "local" || model.model != self.config.model {
            return Err(format!(
                "local provider received model {}/{} but serves local/{}",
                model.provider, model.model, self.config.model
            ));
        }
        let payload = local_payload(&self.config, request)?;
        let (stdout_path, stdout) = process_capture_file("stdout")?;
        let (stderr_path, stderr) = match process_capture_file("stderr") {
            Ok(capture) => capture,
            Err(error) => {
                let _ = fs::remove_file(&stdout_path);
                return Err(error);
            }
        };
        let endpoint = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let output: Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>), String> = (|| {
            let timeout = self.config.request_timeout.as_secs().max(1).to_string();
            let mut child = Command::new("/usr/bin/curl")
                .args([
                    "--silent",
                    "--show-error",
                    "--connect-timeout",
                    "10",
                    "--max-time",
                    timeout.as_str(),
                    "--header",
                    "Content-Type: application/json",
                    "--request",
                    "POST",
                    "--data-binary",
                    "@-",
                    "--write-out",
                    "\n%{http_code}",
                    endpoint.as_str(),
                ])
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .stdin(Stdio::piped())
                .stdout(Stdio::from(stdout))
                .stderr(Stdio::from(stderr))
                .spawn()
                .map_err(|error| format!("could not start local transport: {error}"))?;
            child
                .stdin
                .as_mut()
                .ok_or_else(|| "local transport did not expose request stdin".to_owned())?
                .write_all(payload.as_bytes())
                .map_err(|error| format!("could not write local request: {error}"))?;
            drop(child.stdin.take());
            let status = wait_for_child_or_cancellation(&mut child, cancellation)?;
            let stdout = fs::read(&stdout_path)
                .map_err(|error| format!("cannot read local response capture: {error}"))?;
            let stderr = fs::read(&stderr_path)
                .map_err(|error| format!("cannot read local error capture: {error}"))?;
            Ok((status, stdout, stderr))
        })();
        let _ = fs::remove_file(&stdout_path);
        let _ = fs::remove_file(&stderr_path);
        let (status, stdout, stderr) = output?;
        if cancellation.is_cancelled() {
            return Err("local transport cancelled".to_owned());
        }
        if !status.success() {
            return Err(format!(
                "local transport failed before a provider response: {}",
                String::from_utf8_lossy(&stderr).trim()
            ));
        }
        let (body, status) = split_curl_status(&stdout)?;
        parse_local_response(body, status)
    }
}

impl ModelProvider for LocalProvider {
    fn stream<'a>(
        &'a self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelFuture<'a> {
        let stream = self.response_stream(request, cancellation);
        Box::pin(std::future::ready(Ok(Box::new(stream) as _)))
    }
}

fn cancelled_stream() -> ModelStream {
    ModelStream {
        events: vec![ModelStreamEvent::End(StopReason::Cancelled)],
    }
}

fn local_payload(config: &LocalConfig, request: ModelRequest) -> Result<String, String> {
    let context = JsonValue::parse(request.context.trim())
        .map_err(|_| "local context was not valid JSON".to_owned())?;
    let JsonValue::Array(mut messages) = context else {
        return Err("local context was not a JSON message array".to_owned());
    };
    messages.insert(
        0,
        JsonValue::object([
            ("role", JsonValue::from("system")),
            ("content", JsonValue::from(request.system_prompt)),
        ]),
    );
    let tools = request
        .tools
        .iter()
        .map(|tool| {
            JsonValue::object([
                ("type", JsonValue::from("function")),
                (
                    "function",
                    JsonValue::object([
                        ("name", JsonValue::from(tool.name.clone())),
                        ("description", JsonValue::from(tool.description.clone())),
                        ("parameters", tool.schema.clone()),
                    ]),
                ),
            ])
        })
        .collect::<Vec<_>>();
    let body = json_value!({
        "model": config.model.clone(),
        "messages": JsonValue::Array(messages),
        "temperature": config.temperature,
        "top_p": config.top_p,
        "min_p": config.min_p,
        "max_tokens": config.max_tokens,
        "stream": false,
        "chat_template_kwargs": JsonValue::object([(
            "enable_thinking",
            JsonValue::from(config.enable_thinking && request.thinking_level != ThinkingLevel::Off),
        )]),
        "tools": JsonValue::Array(tools)
    });
    body.to_json_string()
        .map_err(|error| format!("could not serialize local request: {error}"))
}

fn parse_local_response(
    bytes: &[u8],
    http_status: u16,
) -> Result<(Vec<ModelStreamEvent>, Usage), String> {
    let response = from_bytes(bytes)?;
    if let Some(error) = response.get("error") {
        return Err(format!(
            "local server rejected the request with HTTP {http_status}: {}",
            error_message(error)
        ));
    }
    if !(200..300).contains(&http_status) {
        return Err(format!(
            "local server returned HTTP {http_status} without a completion"
        ));
    }
    let choice = array_field(&response, "choices")?
        .first()
        .ok_or_else(|| "local response did not contain a completion choice".to_owned())?;
    let message = object_field(choice, "message")?;
    let mut events = Vec::new();
    if let Some(content) = optional_string(message.get("content"))? {
        if !content.is_empty() {
            events.push(ModelStreamEvent::TextDelta(content.to_owned()));
        }
    }
    let mut has_tool_calls = false;
    if let Some(calls) = optional_array(message.get("tool_calls"))? {
        for (index, call) in calls.iter().enumerate() {
            let call_object = as_object(call, "local tool call")?;
            let id = optional_string(call_object.get("id"))?
                .filter(|id| !id.is_empty())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| format!("local-call-{index}"));
            let function = object_field(call, "function")?;
            let name = required_string(function.get("name"), "local tool call name")?;
            let arguments =
                required_string(function.get("arguments"), "local serialized tool arguments")?;
            events.push(ModelStreamEvent::ToolCall(AssistantToolCall {
                id: ToolCallId::new(id).map_err(|error| error.to_string())?,
                name: name.to_owned(),
                arguments: SerializedJson::new(arguments),
            }));
            has_tool_calls = true;
        }
    }
    let finish_reason = optional_string(as_object(choice, "local choice")?.get("finish_reason"))?;
    let stop_reason = match finish_reason {
        Some("tool_calls" | "tool_call") if has_tool_calls => StopReason::ToolUse,
        Some("length") => StopReason::Length,
        _ if has_tool_calls => StopReason::ToolUse,
        _ => StopReason::EndTurn,
    };
    events.push(ModelStreamEvent::End(stop_reason));
    Ok((events, parse_usage(response.get("usage"))?))
}

fn parse_usage(value: Option<&JsonValue>) -> Result<Usage, String> {
    let Some(value) = value else {
        return Ok(Usage::default());
    };
    let cache_read_tokens = match value.get("prompt_tokens_details") {
        None | Some(JsonValue::Null) => None,
        Some(details) => number_field(details, "cached_tokens")?,
    };
    Ok(Usage {
        input_tokens: number_field(value, "prompt_tokens")?,
        output_tokens: number_field(value, "completion_tokens")?,
        cache_read_tokens,
        ..Usage::default()
    })
}

fn split_curl_status(bytes: &[u8]) -> Result<(&[u8], u16), String> {
    let output = std::str::from_utf8(bytes)
        .map_err(|_| "local transport returned non-UTF-8 output".to_owned())?;
    let (body, status) = output
        .rsplit_once('\n')
        .ok_or_else(|| "local transport did not report an HTTP status".to_owned())?;
    let status = status
        .trim()
        .parse::<u16>()
        .map_err(|_| "local transport reported an invalid HTTP status".to_owned())?;
    Ok((body.as_bytes(), status))
}

fn process_capture_file(stream: &str) -> Result<(PathBuf, File), String> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    for _ in 0..16 {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "pi-agent-local-{}-{sequence}-{stream}",
            std::process::id()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("cannot create local transport capture: {error}")),
        }
    }
    Err("cannot allocate a unique local transport capture".to_owned())
}

fn wait_for_child_or_cancellation(
    child: &mut std::process::Child,
    cancellation: &CancellationToken,
) -> Result<std::process::ExitStatus, String> {
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("local transport status could not be read: {error}"))?
        {
            return Ok(status);
        }
        if cancellation.is_cancelled() {
            let _ = child.kill();
            return child.wait().map_err(|error| {
                format!("cancelled local transport could not be reaped: {error}")
            });
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn as_object<'a>(
    value: &'a JsonValue,
    description: &str,
) -> Result<&'a BTreeMap<String, JsonValue>, String> {
    match value {
        JsonValue::Object(value) => Ok(value),
        _ => Err(format!("{description} was not a JSON object")),
    }
}

fn object_field<'a>(
    value: &'a JsonValue,
    name: &str,
) -> Result<&'a BTreeMap<String, JsonValue>, String> {
    as_object(value, "local JSON value")?
        .get(name)
        .ok_or_else(|| format!("local response omitted {name:?}"))
        .and_then(|value| as_object(value, name))
}

fn array_field<'a>(value: &'a JsonValue, name: &str) -> Result<&'a [JsonValue], String> {
    match as_object(value, "local JSON value")?.get(name) {
        Some(JsonValue::Array(value)) => Ok(value),
        Some(_) => Err(format!("local response field {name:?} was not an array")),
        None => Err(format!("local response omitted {name:?}")),
    }
}

fn optional_array(value: Option<&JsonValue>) -> Result<Option<&[JsonValue]>, String> {
    match value {
        None | Some(JsonValue::Null) => Ok(None),
        Some(JsonValue::Array(value)) => Ok(Some(value)),
        Some(_) => Err("local tool_calls was not an array".to_owned()),
    }
}

fn optional_string(value: Option<&JsonValue>) -> Result<Option<&str>, String> {
    match value {
        None | Some(JsonValue::Null) => Ok(None),
        Some(JsonValue::String(value)) => Ok(Some(value)),
        Some(_) => Err("local response field was not a string".to_owned()),
    }
}

fn required_string<'a>(value: Option<&'a JsonValue>, description: &str) -> Result<&'a str, String> {
    optional_string(value)?
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{description} was missing or empty"))
}

fn number_field(value: &JsonValue, name: &str) -> Result<Option<u64>, String> {
    let object = as_object(value, "local usage")?;
    match object.get(name) {
        None | Some(JsonValue::Null) => Ok(None),
        Some(JsonValue::Number(pi_agent_protocol::JsonNumber::Unsigned(value))) => Ok(Some(*value)),
        Some(JsonValue::Number(pi_agent_protocol::JsonNumber::Signed(value))) if *value >= 0 => {
            Ok(Some(*value as u64))
        }
        Some(_) => Err(format!(
            "local usage field {name:?} was not a non-negative integer"
        )),
    }
}

fn error_message(error: &JsonValue) -> String {
    error
        .get("message")
        .and_then(|value| match value {
            JsonValue::String(value) => Some(value.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "local server rejected the request".to_owned())
}

#[cfg(test)]
mod tests {
    use super::{
        local_payload, parse_local_response, LocalConfig, LocalProvider, LAGUNA_XS_2_1_MODEL,
    };
    use crate::scheduler::{CancellationToken, ModelRequest, ModelStreamEvent};
    use crate::state::{ModelDescriptor, ThinkingLevel, Usage};
    use crate::tool::ToolDefinition;
    use pi_agent_protocol::JsonValue;
    #[cfg(unix)]
    use std::io::{Read, Write};
    #[cfg(unix)]
    use std::net::TcpListener;

    #[test]
    fn laguna_defaults_target_o_mlx_without_ambient_configuration() {
        let config = LocalConfig::laguna_xs_2_1("http://127.0.0.1:8000/v1");
        assert_eq!(config.model(), LAGUNA_XS_2_1_MODEL);
        assert!(config.validate().is_ok());
        assert!(format!("{config:?}").contains("enable_thinking: true"));
    }

    #[test]
    fn payload_uses_o_mlx_thinking_and_openai_tool_shapes() {
        let config = LocalConfig::laguna_xs_2_1("http://127.0.0.1:8000/v1");
        let payload = local_payload(
            &config,
            crate::scheduler::ModelRequest {
                system_prompt: "system".into(),
                context: "[{\"role\":\"user\",\"content\":\"hello\"}]".into(),
                tools: vec![ToolDefinition {
                    name: "write".into(),
                    description: "write a file".into(),
                    schema: JsonValue::object([("type", JsonValue::from("object"))]),
                    execution_mode: crate::tool::ToolExecutionMode::Parallel,
                }],
                model: Some(ModelDescriptor {
                    provider: "local".into(),
                    model: LAGUNA_XS_2_1_MODEL.into(),
                    revision: None,
                }),
                thinking_level: ThinkingLevel::High,
            },
        )
        .expect("payload should serialize");
        assert!(payload.contains("chat_template_kwargs"));
        assert!(payload.contains("enable_thinking"));
        assert!(payload.contains("\"tools\""));
        assert!(payload.contains("\"write\""));
    }

    #[cfg(unix)]
    #[test]
    fn transport_posts_the_serialized_body_to_the_local_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock server should bind");
        let address = listener.local_addr().expect("mock server address");
        let response = br#"{"choices":[{"finish_reason":"stop","message":{"content":"READY","tool_calls":[]}}],"usage":{"prompt_tokens":4,"completion_tokens":1}}"#;
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("mock server should accept");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            let body_start = loop {
                let read = stream.read(&mut buffer).expect("mock request should read");
                assert!(read > 0, "mock client closed before headers");
                request.extend_from_slice(&buffer[..read]);
                if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    break index + 4;
                }
            };
            let headers = String::from_utf8_lossy(&request[..body_start]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.strip_prefix("Content-Length:")?
                        .trim()
                        .parse::<usize>()
                        .ok()
                })
                .expect("curl should send a content length");
            while request.len() < body_start + content_length {
                let read = stream.read(&mut buffer).expect("mock body should read");
                assert!(read > 0, "mock client closed before body");
                request.extend_from_slice(&buffer[..read]);
            }
            let body = String::from_utf8_lossy(&request[body_start..body_start + content_length]);
            assert!(body.contains("\"model\":\"Laguna-XS-2.1-5bit\""));
            assert!(body.contains("\"enable_thinking\":true"));
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.len()
            );
            stream
                .write_all(header.as_bytes())
                .expect("mock headers should write");
            stream.write_all(response).expect("mock body should write");
        });
        let config = LocalConfig::laguna_xs_2_1(format!("http://{address}/v1"))
            .with_request_timeout(std::time::Duration::from_secs(5));
        let provider = LocalProvider::new(config);
        let request = ModelRequest {
            system_prompt: "system".into(),
            context: "[{\"role\":\"user\",\"content\":\"hello\"}]".into(),
            tools: Vec::new(),
            model: Some(ModelDescriptor {
                provider: "local".into(),
                model: LAGUNA_XS_2_1_MODEL.into(),
                revision: None,
            }),
            thinking_level: ThinkingLevel::High,
        };
        let (events, usage) = provider
            .complete(request, &CancellationToken::new())
            .expect("mock local response should parse");
        server.join().expect("mock server should finish");
        assert!(
            matches!(events.first(), Some(ModelStreamEvent::TextDelta(text)) if text == "READY")
        );
        assert_eq!(usage.input_tokens, Some(4));
        assert_eq!(usage.output_tokens, Some(1));
    }

    #[test]
    fn parses_o_mlx_tool_calls_and_usage() {
        let response = br#"{
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "content": null,
                    "reasoning_content": "think",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "write", "arguments": "{\"path\":\"a.py\"}"}
                    }]
                }
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 20,
                "prompt_tokens_details": {"cached_tokens": 3}
            }
        }"#;
        let (events, usage) = parse_local_response(response, 200).expect("response should parse");
        assert!(matches!(events[0], ModelStreamEvent::ToolCall(_)));
        assert!(matches!(events.last(), Some(ModelStreamEvent::End(_))));
        assert_eq!(
            usage,
            Usage {
                input_tokens: Some(10),
                output_tokens: Some(20),
                cache_read_tokens: Some(3),
                ..Usage::default()
            }
        );
    }
}
