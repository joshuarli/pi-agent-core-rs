//! Local OpenAI-compatible response parsing.

use crate::json::{from_bytes, JsonValue};
use crate::scheduler::ModelStreamEvent;
use crate::state::{AgentToolCall, SerializedJson, StopReason, ToolCallId, Usage};
use std::collections::BTreeMap;
pub(super) fn parse_local_response(
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
            events.push(ModelStreamEvent::ToolCall(AgentToolCall {
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
        _ => StopReason::Stop,
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
