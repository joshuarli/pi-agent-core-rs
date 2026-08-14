//! OpenRouter Chat Completions provider adapter.
//!
//! This opt-in adapter invokes a caller-provided OpenRouter API key through `curl`. It is a
//! finite-response transport for the evaluation host: the core remains independent of HTTP,
//! subprocesses, credentials, and provider price formats.

use super::retry::{retry_with_backoff, RetryableError};
use super::RetryPolicy;
use crate::json::{from_bytes, json_value, to_bytes, JsonValue};
use crate::scheduler::{
    CancellationToken, ModelFuture, ModelProvider, ModelRequest, ModelStream, ModelStreamEvent,
};
use crate::state::{AssistantToolCall, SerializedJson, StopReason, ToolCallId, Usage};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const COMPLETIONS_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const GENERATION_URL: &str = "https://openrouter.ai/api/v1/generation";

/// Error raised when explicit OpenRouter configuration violates an adapter invariant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpenRouterConfigError {
    /// A required caller-supplied text value was empty.
    EmptyField(&'static str),
    /// The maximum output token cap was zero.
    ZeroMaxTokens,
    /// The API key contains a line break and cannot be represented safely in a curl config.
    ApiKeyContainsLineBreak,
}

impl fmt::Display for OpenRouterConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyField(field) => write!(formatter, "OpenRouter {field} must not be empty"),
            Self::ZeroMaxTokens => {
                formatter.write_str("OpenRouter max tokens must be greater than zero")
            }
            Self::ApiKeyContainsLineBreak => {
                formatter.write_str("OpenRouter API key must not contain line breaks")
            }
        }
    }
}

impl std::error::Error for OpenRouterConfigError {}

/// Caller-owned configuration for [`OpenRouterProvider`].
///
/// The API key is supplied directly by the embedding. This adapter never reads an environment
/// variable, a home-directory credential, or a provider configuration file.
#[derive(Clone, Eq, PartialEq)]
pub struct OpenRouterConfig {
    api_key: String,
    model: String,
    max_tokens: u64,
    retry_policy: RetryPolicy,
}

impl OpenRouterConfig {
    /// Configure one OpenRouter model with the evaluation default output cap of 1024 tokens.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            max_tokens: 1024,
            retry_policy: RetryPolicy::standard(),
        }
    }

    /// Borrow the explicitly configured OpenRouter model identifier.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Replace the explicit maximum completion-token cap.
    pub fn with_max_tokens(mut self, max_tokens: u64) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Replace the bounded backoff policy used for replay-safe transport attempts.
    pub fn with_retry_policy(mut self, retry_policy: RetryPolicy) -> Self {
        self.retry_policy = retry_policy;
        self
    }

    /// Validate configuration before a host admits it for provider use.
    pub fn validate(&self) -> Result<(), OpenRouterConfigError> {
        if self.api_key.trim().is_empty() {
            return Err(OpenRouterConfigError::EmptyField("API key"));
        }
        if self.model.trim().is_empty() {
            return Err(OpenRouterConfigError::EmptyField("model"));
        }
        if self.max_tokens == 0 {
            return Err(OpenRouterConfigError::ZeroMaxTokens);
        }
        if self.api_key.contains(['\n', '\r']) {
            return Err(OpenRouterConfigError::ApiKeyContainsLineBreak);
        }
        Ok(())
    }

    /// Construct and validate explicit configuration in one operation.
    pub fn try_new(
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<Self, OpenRouterConfigError> {
        let config = Self::new(api_key, model);
        config.validate()?;
        Ok(config)
    }
}

impl fmt::Debug for OpenRouterConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenRouterConfig")
            .field("api_key", &"[redacted]")
            .field("model", &self.model)
            .field("max_tokens", &self.max_tokens)
            .field("retry_policy", &self.retry_policy)
            .finish()
    }
}

/// Origin of one OpenRouter cost record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenRouterCostSource {
    /// The Chat Completions response supplied `usage.cost` directly.
    ChatUsage,
    /// A follow-up OpenRouter generation lookup supplied richer accounting metadata.
    Generation,
    /// OpenRouter reported neither usable chat nor generation accounting.
    Unavailable,
}

impl OpenRouterCostSource {
    /// Stable JSON/report spelling for this source.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ChatUsage => "openrouter_chat_usage",
            Self::Generation => "openrouter_generation",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Redacted, provider-reported cost for one model turn.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenRouterCostTurn {
    /// One-based model-turn sequence in the provider instance.
    pub turn: usize,
    /// Accounting response that supplied this record.
    pub source: OpenRouterCostSource,
    /// Provider-reported total USD cost, if available.
    pub total_usd: Option<f64>,
    /// Exact non-negative decimal token from the provider response.
    ///
    /// This is the authoritative representation for accounting. `total_usd` is retained as a
    /// convenience for legacy callers but is inherently lossy for decimal provider prices.
    pub total_usd_exact: Option<String>,
    /// Provider-reported upstream inference USD cost, if available.
    pub upstream_inference_usd: Option<f64>,
    /// Exact provider decimal for upstream inference cost, when supplied.
    pub upstream_inference_usd_exact: Option<String>,
    /// Concrete provider model, when OpenRouter supplied it.
    pub model: Option<String>,
    /// OpenRouter-selected provider name, when generation metadata supplied it.
    pub provider: Option<String>,
    /// Provider-reported input token count, if available.
    pub input_tokens: Option<u64>,
    /// Provider-reported output token count, if available.
    pub output_tokens: Option<u64>,
    /// Provider-reported cache-read token count, if available.
    pub cache_read_tokens: Option<u64>,
    /// Provider-reported cache-write token count, if available.
    pub cache_write_tokens: Option<u64>,
    /// Provider-reported reasoning token count, if available.
    pub reasoning_tokens: Option<u64>,
}

/// Snapshot of redacted provider accounting for all completed turns.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenRouterCostReport {
    /// Number of turns for which OpenRouter reported a total price.
    pub reported_turn_count: usize,
    /// Number of turns without provider accounting.
    pub unavailable_turn_count: usize,
    /// Whether every completed turn has provider-reported cost.
    pub complete: bool,
    /// Sum of reported total USD values. See [`Self::complete`] before treating it as a run total.
    pub reported_total_usd: f64,
    /// Exact decimal sum of all reported total prices, without floating-point rounding.
    pub reported_total_usd_exact: Option<String>,
    /// Sum of reported upstream inference USD values where supplied.
    pub reported_upstream_inference_usd: f64,
    /// Exact decimal sum of all reported upstream inference prices.
    pub reported_upstream_inference_usd_exact: Option<String>,
    /// Per-turn accounting records without request text, response IDs, raw payloads, or secrets.
    pub turns: Vec<OpenRouterCostTurn>,
}

#[derive(Clone, Debug, Default)]
struct Accounting {
    usage: Usage,
    costs: Vec<OpenRouterCostTurn>,
}

/// OpenRouter implementation of the generic [`ModelProvider`] port.
pub struct OpenRouterProvider {
    config: OpenRouterConfig,
    accounting: Arc<Mutex<Accounting>>,
}

impl OpenRouterProvider {
    /// Construct an adapter from explicit caller-owned configuration.
    pub fn new(config: OpenRouterConfig) -> Self {
        Self {
            config,
            accounting: Arc::new(Mutex::new(Accounting::default())),
        }
    }

    /// Return aggregate portable token usage across settled OpenRouter turns.
    pub fn usage_snapshot(&self) -> Usage {
        self.accounting
            .lock()
            .expect("OpenRouter accounting mutex poisoned")
            .usage
            .clone()
    }

    /// Return a redacted snapshot of provider-reported cost accounting.
    pub fn cost_report(&self) -> OpenRouterCostReport {
        let accounting = self
            .accounting
            .lock()
            .expect("OpenRouter accounting mutex poisoned");
        let reported = accounting
            .costs
            .iter()
            .filter(|turn| turn.total_usd_exact.is_some())
            .count();
        let total = accounting
            .costs
            .iter()
            .filter_map(|turn| turn.total_usd)
            .sum::<f64>();
        let inference = accounting
            .costs
            .iter()
            .filter_map(|turn| turn.upstream_inference_usd)
            .sum::<f64>();
        let exact_total = accounting
            .costs
            .iter()
            .filter_map(|turn| turn.total_usd_exact.as_deref())
            .fold(None, |sum, value| Some(decimal_add(sum.as_deref(), value)));
        let exact_inference = accounting
            .costs
            .iter()
            .filter_map(|turn| turn.upstream_inference_usd_exact.as_deref())
            .fold(None, |sum, value| Some(decimal_add(sum.as_deref(), value)));
        OpenRouterCostReport {
            reported_turn_count: reported,
            unavailable_turn_count: accounting.costs.len().saturating_sub(reported),
            complete: reported == accounting.costs.len(),
            reported_total_usd: if total == 0.0 { 0.0 } else { total },
            reported_total_usd_exact: exact_total,
            reported_upstream_inference_usd: if inference == 0.0 { 0.0 } else { inference },
            reported_upstream_inference_usd_exact: exact_inference,
            turns: accounting.costs.clone(),
        }
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
        match self.complete(request, &cancellation) {
            Ok((mut events, mut usage, cost)) => {
                usage.cache_read_tokens = usage.cache_read_tokens.or(cost.cache_read_tokens);
                usage.cache_write_tokens = usage.cache_write_tokens.or(cost.cache_write_tokens);
                usage.cost = cost.total_usd_exact.clone();
                self.record(usage.clone(), cost);
                if usage.is_reported() {
                    // V0 streams cannot deliver any event after `End`; usage is part of the
                    // provider response and must precede the terminal settlement event.
                    let terminal = events
                        .pop()
                        .expect("parsed OpenRouter response has terminal event");
                    events.push(ModelStreamEvent::Usage(usage));
                    events.push(terminal);
                }
                ModelStream { events }
            }
            Err(_message) if cancellation.is_cancelled() => ModelStream {
                events: vec![ModelStreamEvent::End(StopReason::Cancelled)],
            },
            Err(message) => ModelStream {
                events: vec![ModelStreamEvent::Error { message }],
            },
        }
    }

    fn record(&self, usage: Usage, mut cost: OpenRouterCostTurn) {
        let mut accounting = self
            .accounting
            .lock()
            .expect("OpenRouter accounting mutex poisoned");
        add_usage(&mut accounting.usage.input_tokens, usage.input_tokens);
        add_usage(&mut accounting.usage.output_tokens, usage.output_tokens);
        add_usage(
            &mut accounting.usage.reasoning_tokens,
            usage.reasoning_tokens,
        );
        add_usage(
            &mut accounting.usage.cache_read_tokens,
            usage.cache_read_tokens,
        );
        add_usage(
            &mut accounting.usage.cache_write_tokens,
            usage.cache_write_tokens,
        );
        if let Some(cost) = usage.cost.as_deref() {
            accounting.usage.cost = Some(match accounting.usage.cost.as_deref() {
                Some(previous) => decimal_add(Some(previous), cost),
                None => cost.to_owned(),
            });
        }
        cost.turn = accounting.costs.len() + 1;
        accounting.costs.push(cost);
    }

    fn validate_model(&self, request: &ModelRequest) -> Result<(), String> {
        self.config.validate().map_err(|error| error.to_string())?;
        let model = request
            .model
            .as_ref()
            .ok_or_else(|| "OpenRouter request omitted its exact model descriptor".to_owned())?;
        if model.provider != "openrouter" || model.model != self.config.model {
            return Err(format!(
                "OpenRouter configuration does not match requested model: expected openrouter/{}, got {}/{}",
                self.config.model, model.provider, model.model
            ));
        }
        Ok(())
    }

    fn complete(
        &self,
        request: ModelRequest,
        cancellation: &CancellationToken,
    ) -> Result<(Vec<ModelStreamEvent>, Usage, OpenRouterCostTurn), String> {
        self.validate_model(&request)?;
        let payload = build_payload(&self.config, &request)?;
        let parsed = retry_with_backoff(self.config.retry_policy, cancellation, || {
            let config_path =
                write_curl_config(&self.config.api_key).map_err(RetryableError::permanent)?;
            let output = run_curl(
                Command::new("curl")
                    .arg("--silent")
                    .arg("--show-error")
                    .arg("--connect-timeout")
                    .arg("10")
                    .arg("--max-time")
                    .arg("30")
                    .arg("--request")
                    .arg("POST")
                    .arg(COMPLETIONS_URL)
                    .arg("--config")
                    .arg(&config_path)
                    .arg("--data-binary")
                    .arg("@-")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .env_clear()
                    .env("PATH", "/usr/bin:/bin"),
                &payload,
                cancellation,
            );
            let _ = fs::remove_file(&config_path);
            let output = output.map_err(|message| RetryableError {
                retryable: !cancellation.is_cancelled(),
                message,
            })?;
            parse_response(&output).map_err(|message| RetryableError {
                retryable: openrouter_response_retryable(&output),
                message,
            })
        })?;
        if cancellation.is_cancelled() {
            return Err("OpenRouter HTTP transport cancelled".into());
        }
        // The completion's own `usage.cost` is the immediate accounting source. Query the
        // generation endpoint only when that provider field is absent: this avoids adding a
        // retention-sensitive metadata round trip to ordinary model turns.
        let cost = parsed
            .inline_cost
            .or_else(|| {
                parsed.generation_id.as_deref().and_then(|generation_id| {
                    self.generation_cost(generation_id, &parsed.usage, cancellation)
                })
            })
            .unwrap_or_else(|| unavailable_cost(&parsed.usage, &self.config.model));
        Ok((parsed.events, parsed.usage, cost))
    }

    /// Fetch redacted accounting metadata after a completion only if chat usage omitted cost.
    fn generation_cost(
        &self,
        generation_id: &str,
        usage: &Usage,
        cancellation: &CancellationToken,
    ) -> Option<OpenRouterCostTurn> {
        for attempt in 0..=self.config.retry_policy.max_retries() {
            if cancellation.is_cancelled() {
                return None;
            }
            let config_path = write_curl_config(&self.config.api_key).ok()?;
            let mut command = Command::new("curl");
            command
                .arg("--silent")
                .arg("--show-error")
                .arg("--connect-timeout")
                .arg("10")
                .arg("--max-time")
                .arg("15")
                .arg("--get")
                .arg(GENERATION_URL)
                .arg("--data-urlencode")
                .arg(format!("id={generation_id}"))
                .arg("--config")
                .arg(&config_path)
                .env_clear()
                .env("PATH", "/usr/bin:/bin");
            let output = run_curl(&mut command, &[], cancellation);
            let _ = fs::remove_file(&config_path);
            if let Ok(output) = output {
                if let Some(cost) = parse_generation_cost(&output, usage) {
                    return Some(cost);
                }
            }
            if attempt < self.config.retry_policy.max_retries()
                && !super::retry::wait_with_cancellation(
                    self.config.retry_policy.delay_before_retry(attempt),
                    cancellation,
                )
            {
                return None;
            }
        }
        None
    }
}

impl fmt::Debug for OpenRouterProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenRouterProvider")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl ModelProvider for OpenRouterProvider {
    fn stream<'a>(
        &'a self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelFuture<'a> {
        let stream = self.response_stream(request, cancellation);
        Box::pin(std::future::ready(Ok(Box::new(stream) as _)))
    }
}

struct ParsedResponse {
    events: Vec<ModelStreamEvent>,
    usage: Usage,
    generation_id: Option<String>,
    inline_cost: Option<OpenRouterCostTurn>,
}

static TRANSPORT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn run_curl(
    command: &mut Command,
    payload: &[u8],
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, String> {
    let (stdout_path, stdout) = capture_file("stdout")?;
    let (stderr_path, stderr) = match capture_file("stderr") {
        Ok(capture) => capture,
        Err(error) => {
            let _ = fs::remove_file(&stdout_path);
            return Err(error);
        }
    };
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    let mut child = command.spawn().map_err(|error| {
        let _ = fs::remove_file(&stdout_path);
        let _ = fs::remove_file(&stderr_path);
        format!("could not start the OpenRouter HTTP transport: {error}")
    })?;
    let write_result = child
        .stdin
        .take()
        .ok_or_else(|| "OpenRouter HTTP transport has no request pipe".to_owned())
        .and_then(|mut stdin| {
            stdin
                .write_all(payload)
                .map_err(|_| "could not send OpenRouter request".to_owned())
        });
    if let Err(error) = write_result {
        let _ = child.kill();
        let _ = child.wait();
        let _ = fs::remove_file(&stdout_path);
        let _ = fs::remove_file(&stderr_path);
        return Err(error);
    }
    let (status, cancelled) = wait_for_child_or_cancellation(&mut child, cancellation)?;
    let output = fs::read(&stdout_path)
        .map_err(|error| format!("could not read OpenRouter response capture: {error}"));
    let error_output = fs::read(&stderr_path).unwrap_or_default();
    let _ = fs::remove_file(&stdout_path);
    let _ = fs::remove_file(&stderr_path);
    let output = output?;
    if cancelled {
        return Err("OpenRouter HTTP transport cancelled".into());
    }
    if !status.success() {
        let detail = String::from_utf8_lossy(&error_output).trim().to_owned();
        if detail.is_empty() {
            return Err("OpenRouter HTTP transport failed before a provider response".into());
        }
        return Err(format!(
            "OpenRouter HTTP transport failed before a provider response: {detail}"
        ));
    }
    Ok(output)
}

fn capture_file(stream: &str) -> Result<(PathBuf, File), String> {
    for _ in 0..16 {
        let sequence = TRANSPORT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "pi-agent-core-openrouter-{}-{sequence}-{stream}",
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
            Err(error) => {
                return Err(format!(
                    "could not create private transport capture: {error}"
                ));
            }
        }
    }
    Err("could not allocate a private OpenRouter transport capture".into())
}

fn write_curl_config(api_key: &str) -> Result<PathBuf, String> {
    if api_key.contains(['\n', '\r']) {
        return Err(OpenRouterConfigError::ApiKeyContainsLineBreak.to_string());
    }
    let escaped = api_key.replace('\\', "\\\\").replace('"', "\\\"");
    let mut path = std::env::temp_dir();
    path.push(format!(
        "pi-agent-core-openrouter-{}-{}.curl",
        std::process::id(),
        TRANSPORT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(|error| {
        format!("could not create private OpenRouter transport config: {error}")
    })?;
    writeln!(file, "header = \"Authorization: Bearer {escaped}\"")
        .and_then(|_| writeln!(file, "header = \"Content-Type: application/json\""))
        .map_err(|error| format!("could not write private OpenRouter transport config: {error}"))?;
    Ok(path)
}

fn wait_for_child_or_cancellation(
    child: &mut Child,
    cancellation: &CancellationToken,
) -> Result<(ExitStatus, bool), String> {
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("OpenRouter transport status could not be read: {error}"))?
        {
            return Ok((status, false));
        }
        if cancellation.is_cancelled() {
            match child.kill() {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {}
                Err(error) => {
                    return Err(format!(
                        "cancelled OpenRouter transport could not be killed: {error}"
                    ));
                }
            }
            let status = child.wait().map_err(|error| {
                format!("cancelled OpenRouter transport could not be reaped: {error}")
            })?;
            return Ok((status, true));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn add_usage(total: &mut Option<u64>, value: Option<u64>) {
    if let Some(value) = value {
        *total = Some(total.unwrap_or(0).saturating_add(value));
    }
}

fn reasoning_effort(level: crate::state::ThinkingLevel) -> Option<&'static str> {
    match level {
        crate::state::ThinkingLevel::Default => None,
        crate::state::ThinkingLevel::Off => Some("none"),
        crate::state::ThinkingLevel::Minimal => Some("minimal"),
        crate::state::ThinkingLevel::Low => Some("low"),
        crate::state::ThinkingLevel::Medium => Some("medium"),
        crate::state::ThinkingLevel::High => Some("high"),
        crate::state::ThinkingLevel::XHigh => Some("xhigh"),
        crate::state::ThinkingLevel::Max => Some("max"),
    }
}

fn build_payload(config: &OpenRouterConfig, request: &ModelRequest) -> Result<Vec<u8>, String> {
    let messages = JsonValue::parse(&request.context)
        .map_err(|_| "OpenRouter received invalid converted context".to_owned())?;
    let messages = messages
        .as_array()
        .ok_or_else(|| "OpenRouter converted context must be an array".to_owned())?
        .to_owned();
    let mut chat_messages = Vec::with_capacity(messages.len() + 1);
    chat_messages.push(json_value!({
        "role": "system",
        "content": request.system_prompt.clone()
    }));
    chat_messages.extend(messages);
    let tools = request
        .tools
        .iter()
        .map(|tool| {
            let schema = tool.schema.clone();
            Ok(json_value!({
                "type": "function",
                "function": json_value!({
                    "name": tool.name.clone(),
                    "description": tool.description.clone(),
                    "parameters": schema,
                }),
            }))
        })
        .collect::<Result<Vec<_>, &str>>()?;
    let mut payload = json_value!({
        "model": config.model.clone(),
        "messages": chat_messages,
        "temperature": 0,
        "max_tokens": config.max_tokens,
        "stream": false,
    });
    if let Some(effort) = reasoning_effort(request.thinking_level) {
        payload
            .as_object_mut()
            .expect("OpenRouter payload is an object")
            .insert("reasoning".to_owned(), json_value!({"effort": effort}));
    }
    if !tools.is_empty() {
        payload
            .as_object_mut()
            .expect("OpenRouter payload is an object")
            .insert("tools".to_owned(), JsonValue::Array(tools));
    }
    to_bytes(&payload).map_err(|_| "cannot serialize OpenRouter request".to_owned())
}

fn finite_nonnegative(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite() && *value >= 0.0)
}

fn number(value: &JsonValue, key: &str) -> Option<f64> {
    finite_nonnegative(value.get(key).and_then(JsonValue::as_f64))
}

/// Extract one provider number without routing it through `f64`.
///
/// `JsonValue` intentionally models JSON floating-point numbers as `f64`, which is a useful
/// generic protocol boundary but cannot preserve a billing decimal's source spelling. This
/// narrow path scanner runs only after the response has passed normal JSON parsing and is used
/// solely for the redacted cost fields.
fn exact_number_at_path(input: &[u8], path: &[&str]) -> Option<String> {
    let mut cursor = RawJsonCursor { input, position: 0 };
    let value = cursor.value_at_path(path)?;
    if !valid_nonnegative_json_number(&value) {
        return None;
    }
    Some(value)
}

fn valid_nonnegative_json_number(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes[0] == b'-' {
        return false;
    }
    let mut index = 0;
    if bytes[index] == b'0' {
        index += 1;
    } else if bytes[index].is_ascii_digit() {
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
    } else {
        return false;
    }
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        let fraction_start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == fraction_start {
            return false;
        }
    }
    if bytes
        .get(index)
        .is_some_and(|byte| matches!(byte, b'e' | b'E'))
    {
        index += 1;
        if bytes
            .get(index)
            .is_some_and(|byte| matches!(byte, b'+' | b'-'))
        {
            index += 1;
        }
        let exponent_start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == exponent_start {
            return false;
        }
    }
    index == bytes.len()
}

struct RawJsonCursor<'a> {
    input: &'a [u8],
    position: usize,
}

impl<'a> RawJsonCursor<'a> {
    fn value_at_path(&mut self, path: &[&str]) -> Option<String> {
        self.skip_space();
        if path.is_empty() {
            return self.number();
        }
        if self.input.get(self.position) != Some(&b'{') {
            return None;
        }
        self.position += 1;
        self.skip_space();
        if self.input.get(self.position) == Some(&b'}') {
            return None;
        }
        loop {
            self.skip_space();
            let key = self.string()?;
            self.skip_space();
            if self.input.get(self.position) != Some(&b':') {
                return None;
            }
            self.position += 1;
            self.skip_space();
            if key == path[0] {
                return self.value_at_path(&path[1..]);
            }
            self.skip_value()?;
            self.skip_space();
            match self.input.get(self.position) {
                Some(b',') => self.position += 1,
                Some(b'}') | None => return None,
                _ => return None,
            }
        }
    }

    fn skip_value(&mut self) -> Option<()> {
        self.skip_space();
        match self.input.get(self.position).copied()? {
            b'{' => {
                self.position += 1;
                self.skip_space();
                if self.input.get(self.position) == Some(&b'}') {
                    self.position += 1;
                    return Some(());
                }
                loop {
                    self.skip_space();
                    self.string()?;
                    self.skip_space();
                    if self.input.get(self.position) != Some(&b':') {
                        return None;
                    }
                    self.position += 1;
                    self.skip_value()?;
                    self.skip_space();
                    match self.input.get(self.position) {
                        Some(b',') => self.position += 1,
                        Some(b'}') => {
                            self.position += 1;
                            return Some(());
                        }
                        _ => return None,
                    }
                }
            }
            b'[' => {
                self.position += 1;
                self.skip_space();
                if self.input.get(self.position) == Some(&b']') {
                    self.position += 1;
                    return Some(());
                }
                loop {
                    self.skip_value()?;
                    self.skip_space();
                    match self.input.get(self.position) {
                        Some(b',') => self.position += 1,
                        Some(b']') => {
                            self.position += 1;
                            return Some(());
                        }
                        _ => return None,
                    }
                }
            }
            b'"' => {
                self.string()?;
                Some(())
            }
            b'-' | b'0'..=b'9' => {
                self.number()?;
                Some(())
            }
            _ if self.input[self.position..].starts_with(b"true") => {
                self.position += 4;
                Some(())
            }
            _ if self.input[self.position..].starts_with(b"false") => {
                self.position += 5;
                Some(())
            }
            _ if self.input[self.position..].starts_with(b"null") => {
                self.position += 4;
                Some(())
            }
            _ => None,
        }
    }

    fn string(&mut self) -> Option<String> {
        if self.input.get(self.position) != Some(&b'"') {
            return None;
        }
        self.position += 1;
        let start = self.position;
        let mut escaped = false;
        while let Some(byte) = self.input.get(self.position).copied() {
            self.position += 1;
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                return std::str::from_utf8(&self.input[start..self.position - 1])
                    .ok()
                    .map(ToOwned::to_owned);
            }
        }
        None
    }

    fn number(&mut self) -> Option<String> {
        let start = self.position;
        while let Some(byte) = self.input.get(self.position).copied() {
            if byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E') {
                self.position += 1;
            } else {
                break;
            }
        }
        if start == self.position {
            return None;
        }
        std::str::from_utf8(&self.input[start..self.position])
            .ok()
            .map(ToOwned::to_owned)
    }

    fn skip_space(&mut self) {
        while self
            .input
            .get(self.position)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.position += 1;
        }
    }
}

fn decimal_add(lhs: Option<&str>, rhs: &str) -> String {
    let Some(lhs) = lhs else {
        return decimal_normalize(rhs);
    };
    let (left_digits, left_scale) = decimal_parts(lhs);
    let (right_digits, right_scale) = decimal_parts(rhs);
    let scale = left_scale.max(right_scale);
    let mut left = left_digits;
    let mut right = right_digits;
    left.extend(std::iter::repeat_n('0', scale - left_scale));
    right.extend(std::iter::repeat_n('0', scale - right_scale));
    let mut output = String::new();
    let mut carry = 0u8;
    for (left, right) in left.bytes().rev().zip(right.bytes().rev()) {
        let sum = left - b'0' + right - b'0' + carry;
        output.push(char::from(b'0' + sum % 10));
        carry = sum / 10;
    }
    if carry != 0 {
        output.push(char::from(b'0' + carry));
    }
    let mut output: String = output.chars().rev().collect();
    if scale != 0 {
        if output.len() <= scale {
            let zeros = "0".repeat(scale + 1 - output.len());
            output = format!("{zeros}{output}");
        }
        let position = output.len() - scale;
        output.insert(position, '.');
    }
    decimal_normalize(&output)
}

fn decimal_parts(value: &str) -> (String, usize) {
    let (coefficient, exponent) = value
        .split_once(['e', 'E'])
        .map(|(coefficient, exponent)| (coefficient, exponent.parse::<i64>().unwrap_or(0)))
        .unwrap_or((value, 0));
    let (whole, fraction) = coefficient.split_once('.').unwrap_or((coefficient, ""));
    let mut digits = String::with_capacity(whole.len() + fraction.len());
    digits.push_str(whole.trim_start_matches('+'));
    digits.push_str(fraction);
    let scale = (fraction.len() as i64 - exponent).max(0) as usize;
    let mut digits = digits.trim_start_matches('0').to_owned();
    if digits.is_empty() {
        digits.push('0');
    }
    (digits, scale)
}

fn decimal_normalize(value: &str) -> String {
    let (digits, scale) = decimal_parts(value);
    if scale == 0 {
        return digits;
    }
    let mut output = if digits.len() <= scale {
        format!("0.{}{}", "0".repeat(scale - digits.len()), digits)
    } else {
        let position = digits.len() - scale;
        format!("{}.{}", &digits[..position], &digits[position..])
    };
    while output.ends_with('0') {
        output.pop();
    }
    if output.ends_with('.') {
        output.pop();
    }
    output
}

fn unavailable_cost(usage: &Usage, model: &str) -> OpenRouterCostTurn {
    OpenRouterCostTurn {
        turn: 0,
        source: OpenRouterCostSource::Unavailable,
        total_usd: None,
        total_usd_exact: None,
        upstream_inference_usd: None,
        upstream_inference_usd_exact: None,
        model: Some(model.to_owned()),
        provider: None,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: usage.reasoning_tokens,
    }
}

fn parse_response(bytes: &[u8]) -> Result<ParsedResponse, String> {
    let response =
        from_bytes(bytes).map_err(|_| "OpenRouter returned a non-JSON response".to_owned())?;
    if response.get("error").is_some() {
        return Err("OpenRouter rejected the request".into());
    }
    let choice = response
        .get("choices")
        .and_then(JsonValue::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(|| "OpenRouter response did not contain a completion choice".to_owned())?;
    let message = choice
        .get("message")
        .and_then(JsonValue::as_object)
        .ok_or_else(|| "OpenRouter completion choice did not contain a message".to_owned())?;
    let mut events = Vec::new();
    if let Some(content) = message.get("content").and_then(JsonValue::as_str) {
        if !content.is_empty() {
            events.push(ModelStreamEvent::TextDelta(content.to_owned()));
        }
    }
    let mut has_tool_calls = false;
    if let Some(calls) = message.get("tool_calls").and_then(JsonValue::as_array) {
        for (index, call) in calls.iter().enumerate() {
            let id = call
                .get("id")
                .and_then(JsonValue::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("openrouter-call-{index}"));
            let function = call
                .get("function")
                .and_then(JsonValue::as_object)
                .ok_or_else(|| "OpenRouter tool call did not contain a function".to_owned())?;
            let name = function
                .get("name")
                .and_then(JsonValue::as_str)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| "OpenRouter tool call did not contain a name".to_owned())?;
            let arguments = function
                .get("arguments")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| {
                    "OpenRouter tool call did not contain serialized arguments".to_owned()
                })?;
            events.push(ModelStreamEvent::ToolCall(AssistantToolCall {
                id: ToolCallId::new(id)
                    .map_err(|_| "OpenRouter tool call omitted its identifier".to_owned())?,
                name: name.to_owned(),
                arguments: SerializedJson::new(arguments),
            }));
            has_tool_calls = true;
        }
    }
    let stop_reason = match choice.get("finish_reason").and_then(JsonValue::as_str) {
        Some("tool_calls" | "tool_call") if has_tool_calls => StopReason::ToolUse,
        Some("length") => StopReason::Length,
        _ if has_tool_calls => StopReason::ToolUse,
        _ => StopReason::EndTurn,
    };
    events.push(ModelStreamEvent::End(stop_reason));
    let usage = response.get("usage").cloned().unwrap_or(JsonValue::Null);
    let token = |name: &str| usage.get(name).and_then(JsonValue::as_u64);
    let reasoning = usage
        .get("completion_tokens_details")
        .and_then(JsonValue::as_object)
        .and_then(|details| details.get("reasoning_tokens"))
        .and_then(JsonValue::as_u64);
    let parsed_usage = Usage {
        input_tokens: token("prompt_tokens"),
        output_tokens: token("completion_tokens"),
        reasoning_tokens: reasoning,
        cache_read_tokens: usage
            .get("prompt_tokens_details")
            .and_then(|details| details.get("cached_tokens"))
            .and_then(JsonValue::as_u64),
        cache_write_tokens: usage
            .get("prompt_tokens_details")
            .and_then(|details| details.get("cache_write_tokens"))
            .and_then(JsonValue::as_u64),
        cost: None,
    };
    let total_usd_exact = exact_number_at_path(bytes, &["usage", "cost"]);
    let inline_cost = total_usd_exact.map(|total_usd_exact| OpenRouterCostTurn {
        turn: 0,
        source: OpenRouterCostSource::ChatUsage,
        total_usd: number(&usage, "cost"),
        total_usd_exact: Some(total_usd_exact),
        upstream_inference_usd: None,
        upstream_inference_usd_exact: None,
        model: response
            .get("model")
            .and_then(JsonValue::as_str)
            .map(str::to_owned),
        provider: None,
        input_tokens: parsed_usage.input_tokens,
        output_tokens: parsed_usage.output_tokens,
        cache_read_tokens: parsed_usage.cache_read_tokens,
        cache_write_tokens: parsed_usage.cache_write_tokens,
        reasoning_tokens: parsed_usage.reasoning_tokens,
    });
    Ok(ParsedResponse {
        events,
        usage: parsed_usage,
        generation_id: response
            .get("id")
            .and_then(JsonValue::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_owned),
        inline_cost,
    })
}

/// Classify an OpenRouter JSON error without exposing its remote diagnostic to the agent.
/// OpenRouter places the HTTP status in the error object's numeric `code` field for the common
/// rate-limit and transient-service failures.
fn openrouter_response_retryable(bytes: &[u8]) -> bool {
    let Some(error) = from_bytes(bytes)
        .ok()
        .and_then(|response| response.get("error").cloned())
        .and_then(|error| error.as_object().cloned())
    else {
        return false;
    };
    let status = error
        .get("code")
        .and_then(JsonValue::as_u64)
        .or_else(|| error.get("status").and_then(JsonValue::as_u64));
    matches!(status, Some(429) | Some(500..=599))
}

fn parse_generation_cost(bytes: &[u8], fallback_usage: &Usage) -> Option<OpenRouterCostTurn> {
    let response = from_bytes(bytes).ok()?;
    let data = response.get("data")?;
    let total_usd_exact = exact_number_at_path(bytes, &["data", "total_cost"])?;
    Some(OpenRouterCostTurn {
        turn: 0,
        source: OpenRouterCostSource::Generation,
        total_usd: number(data, "total_cost"),
        total_usd_exact: Some(total_usd_exact),
        upstream_inference_usd: number(data, "upstream_inference_cost"),
        upstream_inference_usd_exact: exact_number_at_path(
            bytes,
            &["data", "upstream_inference_cost"],
        ),
        model: data
            .get("model")
            .and_then(JsonValue::as_str)
            .map(str::to_owned),
        provider: data
            .get("provider_name")
            .and_then(JsonValue::as_str)
            .map(str::to_owned),
        input_tokens: data
            .get("tokens_prompt")
            .and_then(JsonValue::as_u64)
            .or(fallback_usage.input_tokens),
        output_tokens: data
            .get("tokens_completion")
            .and_then(JsonValue::as_u64)
            .or(fallback_usage.output_tokens),
        cache_read_tokens: data.get("tokens_cached").and_then(JsonValue::as_u64),
        cache_write_tokens: data.get("tokens_cache_write").and_then(JsonValue::as_u64),
        reasoning_tokens: data
            .get("tokens_reasoning")
            .and_then(JsonValue::as_u64)
            .or(fallback_usage.reasoning_tokens),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ModelDescriptor, ThinkingLevel};

    #[test]
    fn parses_redacted_generation_cost_without_retaining_identifier() {
        let usage = Usage {
            input_tokens: Some(10),
            output_tokens: Some(3),
            reasoning_tokens: None,
            ..Usage::default()
        };
        let cost = parse_generation_cost(
            br#"{
                "data": {
                    "id": "gen_must_not_be_written_to_artifacts",
                    "total_cost": 0.0000123,
                    "upstream_inference_cost": 0.00001,
                    "model": "poolside/laguna-xs-2.1:free",
                    "provider_name": "Poolside",
                    "tokens_prompt": 12,
                    "tokens_completion": 4,
                    "tokens_cached": 2,
                    "tokens_reasoning": 1
                }
            }"#,
            &usage,
        )
        .expect("provider cost is parsed");
        assert_eq!(cost.source, OpenRouterCostSource::Generation);
        assert_eq!(cost.total_usd, Some(0.0000123));
        assert_eq!(cost.total_usd_exact.as_deref(), Some("0.0000123"));
        assert_eq!(
            cost.upstream_inference_usd_exact.as_deref(),
            Some("0.00001")
        );
        assert_eq!(cost.provider.as_deref(), Some("Poolside"));
        assert!(!format!("{cost:?}").contains("gen_must_not_be_written_to_artifacts"));
    }

    #[test]
    fn chat_usage_cost_is_preferred_without_generation_metadata() {
        let bytes = br#"{
                "id": "gen_example",
                "model": "poolside/laguna-xs-2.1:free",
                "choices": [{"message": {"content": "ok"}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 2, "completion_tokens": 1, "cost": 0}
            }"#;
        assert_eq!(
            exact_number_at_path(bytes, &["usage", "cost"]).as_deref(),
            Some("0")
        );
        let parsed = parse_response(bytes).expect("chat response parses");
        let cost = parsed.inline_cost.expect("inline provider cost");
        assert_eq!(cost.source, OpenRouterCostSource::ChatUsage);
        assert_eq!(cost.total_usd, Some(0.0));
        assert_eq!(cost.total_usd_exact.as_deref(), Some("0"));
        assert_eq!(parsed.generation_id.as_deref(), Some("gen_example"));
    }

    #[test]
    fn builds_explicit_output_cap_and_openrouter_reasoning_wire() {
        let config = OpenRouterConfig::try_new("key", "openai/gpt-5.6-luna").unwrap();
        let payload = build_payload(
            &config.with_max_tokens(128_000),
            &ModelRequest {
                system_prompt: "system".into(),
                context: "[]".into(),
                model: Some(ModelDescriptor {
                    provider: "openrouter".into(),
                    model: "openai/gpt-5.6-luna".into(),
                    revision: None,
                }),
                thinking_level: ThinkingLevel::XHigh,
                ..ModelRequest::default()
            },
        )
        .unwrap();
        let payload = JsonValue::parse(std::str::from_utf8(&payload).unwrap()).unwrap();
        assert_eq!(
            payload.get("max_tokens").and_then(JsonValue::as_u64),
            Some(128_000)
        );
        assert_eq!(
            payload
                .get("reasoning")
                .and_then(|value| value.get("effort"))
                .and_then(JsonValue::as_str),
            Some("xhigh")
        );
    }

    #[test]
    fn rejects_missing_or_mismatched_model_before_transport() {
        let provider = OpenRouterProvider::new(OpenRouterConfig::try_new("key", "model").unwrap());
        let cancellation = CancellationToken::new();
        let request = ModelRequest {
            context: "[]".into(),
            ..ModelRequest::default()
        };
        let stream = provider.response_stream(request, cancellation);
        assert!(
            matches!(stream.events.first(), Some(ModelStreamEvent::Error { message }) if message.contains("omitted its exact model"))
        );

        let stream = provider.response_stream(
            ModelRequest {
                context: "[]".into(),
                model: Some(ModelDescriptor {
                    provider: "openrouter".into(),
                    model: "other-model".into(),
                    revision: None,
                }),
                ..ModelRequest::default()
            },
            CancellationToken::new(),
        );
        assert!(
            matches!(stream.events.first(), Some(ModelStreamEvent::Error { message }) if message.contains("does not match requested model"))
        );
    }

    #[test]
    fn retries_only_transient_openrouter_response_statuses() {
        assert!(openrouter_response_retryable(
            br#"{"error":{"code":429,"message":"slow down"}}"#
        ));
        assert!(openrouter_response_retryable(
            br#"{"error":{"code":503,"message":"temporarily unavailable"}}"#
        ));
        assert!(!openrouter_response_retryable(
            br#"{"error":{"code":400,"message":"invalid request"}}"#
        ));
        assert!(!openrouter_response_retryable(
            br#"{"error":{"message":"invalid request"}}"#
        ));
    }

    #[test]
    fn preserves_decimal_costs_and_usage_without_float_aggregation() {
        let provider = OpenRouterProvider::new(OpenRouterConfig::new("key", "model"));
        provider.record(
            Usage {
                input_tokens: Some(2),
                output_tokens: Some(3),
                reasoning_tokens: None,
                ..Usage::default()
            },
            OpenRouterCostTurn {
                turn: 0,
                source: OpenRouterCostSource::ChatUsage,
                total_usd: Some(0.1),
                total_usd_exact: Some("0.100000000000000001".into()),
                upstream_inference_usd: None,
                upstream_inference_usd_exact: None,
                model: Some("model".into()),
                provider: None,
                input_tokens: Some(2),
                output_tokens: Some(3),
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
            },
        );
        provider.record(
            Usage::default(),
            OpenRouterCostTurn {
                turn: 0,
                source: OpenRouterCostSource::ChatUsage,
                total_usd: Some(0.2),
                total_usd_exact: Some("0.2".into()),
                upstream_inference_usd: None,
                upstream_inference_usd_exact: None,
                model: Some("model".into()),
                provider: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
            },
        );
        let report = provider.cost_report();
        assert_eq!(
            report.reported_total_usd_exact.as_deref(),
            Some("0.300000000000000001")
        );
        assert_eq!(provider.usage_snapshot().input_tokens, Some(2));
        assert_eq!(provider.usage_snapshot().output_tokens, Some(3));
        assert_eq!(provider.usage_snapshot().reasoning_tokens, None);
    }

    #[test]
    fn cancellation_kills_and_reaps_direct_transport_child() {
        let cancellation = CancellationToken::new();
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30"]);
        cancellation.cancel();
        let result = run_curl(&mut command, b"", &cancellation);
        assert_eq!(result.unwrap_err(), "OpenRouter HTTP transport cancelled");
    }
}
