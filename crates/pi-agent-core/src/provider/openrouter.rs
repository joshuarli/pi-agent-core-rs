//! OpenRouter Chat Completions provider adapter.
//!
//! This opt-in adapter invokes a caller-provided OpenRouter API key through `curl`. It is a
//! finite-response transport for the evaluation host: the core remains independent of HTTP,
//! subprocesses, credentials, and provider price formats.

use crate::json::{from_bytes, json_value, to_bytes, JsonValue};
use crate::scheduler::{
    CancellationToken, ModelFuture, ModelProvider, ModelRequest, ModelStream, ModelStreamEvent,
};
use crate::state::{AssistantToolCall, SerializedJson, StopReason, ToolCallId, Usage};
use std::fmt;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const COMPLETIONS_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const GENERATION_URL: &str = "https://openrouter.ai/api/v1/generation";

/// Caller-owned configuration for [`OpenRouterProvider`].
///
/// The API key is supplied directly by the embedding. This adapter never reads an environment
/// variable, a home-directory credential, or a provider configuration file.
#[derive(Clone, Eq, PartialEq)]
pub struct OpenRouterConfig {
    api_key: String,
    model: String,
    max_tokens: u64,
}

impl OpenRouterConfig {
    /// Configure one OpenRouter model with the evaluation default output cap of 1024 tokens.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            max_tokens: 1024,
        }
    }

    /// Replace the explicit maximum completion-token cap.
    pub fn with_max_tokens(mut self, max_tokens: u64) -> Self {
        self.max_tokens = max_tokens;
        self
    }
}

impl fmt::Debug for OpenRouterConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenRouterConfig")
            .field("api_key", &"[redacted]")
            .field("model", &self.model)
            .field("max_tokens", &self.max_tokens)
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
    /// Provider-reported upstream inference USD cost, if available.
    pub upstream_inference_usd: Option<f64>,
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
    /// Sum of reported upstream inference USD values where supplied.
    pub reported_upstream_inference_usd: f64,
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
            .filter(|turn| turn.total_usd.is_some())
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
        OpenRouterCostReport {
            reported_turn_count: reported,
            unavailable_turn_count: accounting.costs.len().saturating_sub(reported),
            complete: reported == accounting.costs.len(),
            reported_total_usd: if total == 0.0 { 0.0 } else { total },
            reported_upstream_inference_usd: if inference == 0.0 { 0.0 } else { inference },
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
        match self.complete(request) {
            Ok((mut events, usage, cost)) => {
                self.record(usage.clone(), cost);
                if usage.input_tokens.is_some() || usage.output_tokens.is_some() {
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
        accounting.usage.input_tokens =
            Some(accounting.usage.input_tokens.unwrap_or(0) + usage.input_tokens.unwrap_or(0));
        accounting.usage.output_tokens =
            Some(accounting.usage.output_tokens.unwrap_or(0) + usage.output_tokens.unwrap_or(0));
        accounting.usage.reasoning_tokens = Some(
            accounting.usage.reasoning_tokens.unwrap_or(0) + usage.reasoning_tokens.unwrap_or(0),
        );
        cost.turn = accounting.costs.len() + 1;
        accounting.costs.push(cost);
    }

    fn complete(
        &self,
        request: ModelRequest,
    ) -> Result<(Vec<ModelStreamEvent>, Usage, OpenRouterCostTurn), String> {
        let messages = JsonValue::parse(&request.context)
            .map_err(|_| "OpenRouter received invalid converted context".to_owned())?;
        let messages = messages
            .as_array()
            .ok_or_else(|| "OpenRouter converted context must be an array".to_owned())?
            .to_owned();
        let mut chat_messages = Vec::with_capacity(messages.len() + 1);
        chat_messages.push(json_value!({"role": "system", "content": request.system_prompt}));
        chat_messages.extend(messages);
        let tools = request
            .tools
            .iter()
            .map(|tool| {
                let schema = tool.schema.clone();
                Ok(json_value!({
                    "type": "function",
                    "function": json_value!({
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": schema,
                    }),
                }))
            })
            .collect::<Result<Vec<_>, &str>>()?;
        let mut payload = json_value!({
            "model": self.config.model,
            "messages": chat_messages,
            "temperature": 0,
            "max_tokens": self.config.max_tokens,
            "stream": false,
        });
        if !tools.is_empty() {
            payload
                .as_object_mut()
                .expect("OpenRouter payload is an object")
                .insert("tools".to_owned(), JsonValue::Array(tools));
        }
        let payload =
            to_bytes(&payload).map_err(|_| "cannot serialize OpenRouter request".to_owned())?;
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
                .arg("--header")
                .arg("Content-Type: application/json")
                .arg("--header")
                .arg(format!("Authorization: Bearer {}", self.config.api_key))
                .arg("--data-binary")
                .arg("@-")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
            &payload,
        )?;
        let parsed = parse_response(&output)?;
        // The completion's own `usage.cost` is the immediate accounting source. Query the
        // generation endpoint only when that provider field is absent: this avoids adding a
        // retention-sensitive metadata round trip to ordinary model turns.
        let cost = parsed
            .inline_cost
            .or_else(|| {
                parsed
                    .generation_id
                    .as_deref()
                    .and_then(|generation_id| self.generation_cost(generation_id, &parsed.usage))
            })
            .unwrap_or_else(|| unavailable_cost(&parsed.usage, &self.config.model));
        Ok((parsed.events, parsed.usage, cost))
    }

    /// Fetch redacted accounting metadata after a completion only if chat usage omitted cost.
    fn generation_cost(&self, generation_id: &str, usage: &Usage) -> Option<OpenRouterCostTurn> {
        for attempt in 0..3 {
            let output = Command::new("curl")
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
                .arg("--header")
                .arg(format!("Authorization: Bearer {}", self.config.api_key))
                .stderr(Stdio::null())
                .output();
            if let Ok(output) = output {
                if output.status.success() {
                    if let Some(cost) = parse_generation_cost(&output.stdout, usage) {
                        return Some(cost);
                    }
                }
            }
            if attempt < 2 {
                thread::sleep(Duration::from_millis(150 * (attempt + 1)));
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

fn run_curl(command: &mut Command, payload: &[u8]) -> Result<Vec<u8>, String> {
    let mut child = command
        .spawn()
        .map_err(|_| "could not start the OpenRouter HTTP transport".to_owned())?;
    {
        use std::io::Write;
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| "OpenRouter HTTP transport has no request pipe".to_owned())?;
        stdin
            .write_all(payload)
            .map_err(|_| "could not send OpenRouter request".to_owned())?;
    }
    let output = child
        .wait_with_output()
        .map_err(|_| "OpenRouter HTTP transport did not settle".to_owned())?;
    if !output.status.success() {
        return Err("OpenRouter HTTP transport failed before a provider response".into());
    }
    Ok(output.stdout)
}

fn finite_nonnegative(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite() && *value >= 0.0)
}

fn number(value: &JsonValue, key: &str) -> Option<f64> {
    finite_nonnegative(value.get(key).and_then(JsonValue::as_f64))
}

fn unavailable_cost(usage: &Usage, model: &str) -> OpenRouterCostTurn {
    OpenRouterCostTurn {
        turn: 0,
        source: OpenRouterCostSource::Unavailable,
        total_usd: None,
        upstream_inference_usd: None,
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
    };
    let inline_cost = number(&usage, "cost").map(|total_usd| OpenRouterCostTurn {
        turn: 0,
        source: OpenRouterCostSource::ChatUsage,
        total_usd: Some(total_usd),
        upstream_inference_usd: None,
        model: response
            .get("model")
            .and_then(JsonValue::as_str)
            .map(str::to_owned),
        provider: None,
        input_tokens: parsed_usage.input_tokens,
        output_tokens: parsed_usage.output_tokens,
        cache_read_tokens: usage
            .get("prompt_tokens_details")
            .and_then(|details| details.get("cached_tokens"))
            .and_then(JsonValue::as_u64),
        cache_write_tokens: usage
            .get("prompt_tokens_details")
            .and_then(|details| details.get("cache_write_tokens"))
            .and_then(JsonValue::as_u64),
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

fn parse_generation_cost(bytes: &[u8], fallback_usage: &Usage) -> Option<OpenRouterCostTurn> {
    let response = from_bytes(bytes).ok()?;
    let data = response.get("data")?;
    let total_usd = number(data, "total_cost")?;
    Some(OpenRouterCostTurn {
        turn: 0,
        source: OpenRouterCostSource::Generation,
        total_usd: Some(total_usd),
        upstream_inference_usd: number(data, "upstream_inference_cost"),
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

    #[test]
    fn parses_redacted_generation_cost_without_retaining_identifier() {
        let usage = Usage {
            input_tokens: Some(10),
            output_tokens: Some(3),
            reasoning_tokens: None,
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
        assert_eq!(cost.provider.as_deref(), Some("Poolside"));
        assert!(!format!("{cost:?}").contains("gen_must_not_be_written_to_artifacts"));
    }

    #[test]
    fn chat_usage_cost_is_preferred_without_generation_metadata() {
        let parsed = parse_response(
            br#"{
                "id": "gen_example",
                "model": "poolside/laguna-xs-2.1:free",
                "choices": [{"message": {"content": "ok"}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 2, "completion_tokens": 1, "cost": 0}
            }"#,
        )
        .expect("chat response parses");
        let cost = parsed.inline_cost.expect("inline provider cost");
        assert_eq!(cost.source, OpenRouterCostSource::ChatUsage);
        assert_eq!(cost.total_usd, Some(0.0));
        assert_eq!(parsed.generation_id.as_deref(), Some("gen_example"));
    }
}
