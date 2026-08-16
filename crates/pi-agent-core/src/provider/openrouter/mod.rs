//! OpenRouter Chat Completions provider adapter.
//!
//! This opt-in adapter invokes a caller-provided OpenRouter API key through `curl`. It is a
//! finite-response transport for the evaluation host: the core remains independent of HTTP,
//! subprocesses, credentials, and provider price formats.

mod accounting;
mod config;
mod payload;
mod response;
mod transport;

use super::retry::{retry_with_backoff, RetryableError};
use crate::scheduler::{
    CancellationToken, ModelFuture, ModelProvider, ModelRequest, ModelStream, ModelStreamEvent,
};
use crate::state::{StopReason, Usage};
use accounting::{add_usage, Accounting};
pub use accounting::{OpenRouterCostReport, OpenRouterCostSource, OpenRouterCostTurn};
pub use config::{OpenRouterConfig, OpenRouterConfigError};
use std::fmt;
use std::fs;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use payload::build_payload;
#[cfg(test)]
use response::exact_number_at_path;
use response::{
    decimal_add, openrouter_response_retryable, openrouter_status_retryable, parse_generation_cost,
    parse_response, response_body_prefix, unavailable_cost,
};
use transport::{run_curl, write_curl_config, COMPLETIONS_URL, GENERATION_URL};

/// The private source of the most recent OpenRouter failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenRouterErrorSource {
    /// The local curl subprocess or its capture boundary failed.
    Transport,
    /// OpenRouter returned a response that the adapter could not accept.
    Response,
    /// The adapter rejected a request before transport.
    Adapter,
}

impl OpenRouterErrorSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Transport => "transport",
            Self::Response => "response",
            Self::Adapter => "adapter",
        }
    }
}

/// Bounded diagnostic for the most recent OpenRouter failure.
///
/// The agent-facing stream error remains a stable adapter message. Hosts that own a private
/// diagnostic sink can use this report to distinguish transport, HTTP, and response-shape
/// failures without retaining the API key or an unbounded provider body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenRouterErrorReport {
    /// Failure boundary that produced the report.
    pub source: OpenRouterErrorSource,
    /// Stable local adapter message.
    pub message: String,
    /// HTTP status observed in the captured response headers.
    pub status_code: Option<u16>,
    /// Whether the adapter classified the failure as retryable.
    pub retryable: bool,
    /// One-based attempt number within the current completion request.
    pub attempt: u32,
    /// Total response body bytes captured before parsing.
    pub response_bytes: Option<usize>,
    /// Total request payload bytes sent to the provider.
    pub request_bytes: Option<usize>,
    /// Bounded, redacted response body prefix for trusted diagnostics.
    pub response_prefix: Option<String>,
}

impl fmt::Display for OpenRouterErrorReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "source={} message={:?} retryable={} attempt={}",
            self.source.as_str(),
            self.message,
            self.retryable,
            self.attempt
        )?;
        if let Some(status_code) = self.status_code {
            write!(formatter, " status_code={status_code}")?;
        }
        if let Some(response_bytes) = self.response_bytes {
            write!(formatter, " response_bytes={response_bytes}")?;
        }
        if let Some(request_bytes) = self.request_bytes {
            write!(formatter, " request_bytes={request_bytes}")?;
        }
        if let Some(response_prefix) = &self.response_prefix {
            write!(formatter, " response_prefix={response_prefix:?}")?;
        }
        Ok(())
    }
}

/// OpenRouter implementation of the generic [`ModelProvider`] port.
pub struct OpenRouterProvider {
    config: OpenRouterConfig,
    accounting: Arc<Mutex<Accounting>>,
    last_error: Arc<Mutex<Option<OpenRouterErrorReport>>>,
}

impl OpenRouterProvider {
    /// Construct an adapter from explicit caller-owned configuration.
    pub fn new(config: OpenRouterConfig) -> Self {
        Self {
            config,
            accounting: Arc::new(Mutex::new(Accounting::default())),
            last_error: Arc::new(Mutex::new(None)),
        }
    }

    /// Return the most recent adapter or provider failure observed by this provider.
    pub fn last_error_report(&self) -> Option<OpenRouterErrorReport> {
        self.last_error
            .lock()
            .expect("OpenRouter error mutex poisoned")
            .clone()
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
        self.clear_error();
        self.validate_model(&request).map_err(|message| {
            self.record_error(OpenRouterErrorReport {
                source: OpenRouterErrorSource::Adapter,
                message: message.clone(),
                status_code: None,
                retryable: false,
                attempt: 0,
                response_bytes: None,
                request_bytes: None,
                response_prefix: None,
            });
            message
        })?;
        let payload = build_payload(&self.config, &request).map_err(|message| {
            self.record_error(OpenRouterErrorReport {
                source: OpenRouterErrorSource::Adapter,
                message: message.clone(),
                status_code: None,
                retryable: false,
                attempt: 0,
                response_bytes: None,
                request_bytes: None,
                response_prefix: None,
            });
            message
        })?;
        let mut attempts: u32 = 0;
        let parsed = retry_with_backoff(self.config.retry_policy, cancellation, || {
            attempts = attempts.saturating_add(1);
            let config_path =
                write_curl_config(&self.config.api_key).map_err(RetryableError::permanent)?;
            let request_timeout = self.config.request_timeout.as_secs().max(1).to_string();
            let output = run_curl(
                Command::new("curl")
                    .arg("--silent")
                    .arg("--show-error")
                    .arg("--connect-timeout")
                    .arg("10")
                    .arg("--max-time")
                    .arg(&request_timeout)
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
                self.config.stall_timeout,
            );
            let _ = fs::remove_file(&config_path);
            let output = output.map_err(|message| {
                let retryable = !cancellation.is_cancelled();
                self.record_error(OpenRouterErrorReport {
                    source: OpenRouterErrorSource::Transport,
                    message: message.clone(),
                    status_code: None,
                    retryable,
                    attempt: attempts,
                    response_bytes: None,
                    request_bytes: Some(payload.len()),
                    response_prefix: None,
                });
                RetryableError { retryable, message }
            })?;
            let retryable = openrouter_status_retryable(output.status_code)
                || openrouter_response_retryable(&output.body);
            parse_response(&output.body).map_err(|message| {
                let message = message.replace(&self.config.api_key, "[redacted]");
                self.record_error(OpenRouterErrorReport {
                    source: OpenRouterErrorSource::Response,
                    message: message.clone(),
                    status_code: output.status_code,
                    retryable,
                    attempt: attempts,
                    response_bytes: Some(output.body.len()),
                    request_bytes: Some(payload.len()),
                    response_prefix: Some(response_body_prefix(
                        &output.body,
                        Some(&self.config.api_key),
                    )),
                });
                RetryableError { retryable, message }
            })
        })?;
        if cancellation.is_cancelled() {
            return Err("OpenRouter HTTP transport cancelled".into());
        }
        self.clear_error();
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
            let output = run_curl(
                &mut command,
                &[],
                cancellation,
                self.config.stall_timeout,
            );
            let _ = fs::remove_file(&config_path);
            if let Ok(output) = output {
                if let Some(cost) = parse_generation_cost(&output.body, usage) {
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

    fn record_error(&self, report: OpenRouterErrorReport) {
        *self
            .last_error
            .lock()
            .expect("OpenRouter error mutex poisoned") = Some(report);
    }

    fn clear_error(&self) {
        *self
            .last_error
            .lock()
            .expect("OpenRouter error mutex poisoned") = None;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::JsonValue;
    use crate::state::{ModelDescriptor, ThinkingLevel};
    use std::time::Duration;

    #[test]
    fn request_timeout_is_explicit_and_not_thirty_seconds() {
        let config = OpenRouterConfig::new("key", "model");
        assert_eq!(config.request_timeout(), Duration::from_secs(300));
        assert_eq!(config.stall_timeout(), Duration::from_secs(60));
        assert_eq!(
            config
                .with_request_timeout(Duration::from_secs(42))
                .request_timeout(),
            Duration::from_secs(42)
        );
        assert_eq!(
            OpenRouterConfig::new("key", "model")
                .with_stall_timeout(Duration::from_secs(7))
                .stall_timeout(),
            Duration::from_secs(7)
        );
        assert_eq!(
            OpenRouterConfig::new("key", "model")
                .with_stall_timeout(Duration::ZERO)
                .validate(),
            Err(OpenRouterConfigError::ZeroStallTimeout)
        );
    }

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
        assert!(openrouter_status_retryable(Some(502)));
        assert!(!openrouter_status_retryable(Some(400)));
    }

    #[test]
    fn non_json_response_keeps_stream_error_stable_and_report_body_bounded() {
        let error = match parse_response(
            br#"<html><body>upstream gateway failure with a very long diagnostic</body></html>"#,
        ) {
            Ok(_) => panic!("non-JSON response unexpectedly parsed"),
            Err(error) => error,
        };
        assert_eq!(error, "OpenRouter returned a non-JSON response");
        assert_eq!(
            response_body_prefix(
                br#"<html><body>upstream gateway failure with a very long diagnostic</body></html>"#,
                Some("failure"),
            ),
            "<html><body>upstream gateway [redacted] with a very long diagnostic</body></html>"
        );
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
        let result = run_curl(
            &mut command,
            b"",
            &cancellation,
            std::time::Duration::from_secs(1),
        );
        assert_eq!(result.unwrap_err(), "OpenRouter HTTP transport cancelled");
    }
}
