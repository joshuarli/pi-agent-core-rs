//! JSON Lines encoding for trace events.

use super::{end_reason_name, event_type};
use crate::event::{EpisodeEnd, EpisodeHeader, Tool, TraceEvent, Turn};
use std::collections::BTreeMap;

pub(super) fn write_json_event(output: &mut String, event: &TraceEvent) {
    output.push('{');
    json_field_name(output, "schema_version");
    output.push_str(&crate::event::TRACE_SCHEMA_VERSION.to_string());
    output.push(',');
    json_field_name(output, "type");
    json_string(output, event_type(event));
    match event {
        TraceEvent::EpisodeHeader(header) => write_json_header(output, header),
        TraceEvent::Turn(turn) => write_json_turn(output, turn),
        TraceEvent::Tool(tool) => write_json_tool(output, tool),
        TraceEvent::EpisodeEnd(end) => write_json_end(output, end),
    }
    output.push('}');
}

fn write_json_header(output: &mut String, header: &EpisodeHeader) {
    output.push(',');
    json_field_string(output, "episode_id", &header.episode_id);
    output.push(',');
    json_field_name(output, "metadata");
    json_map(output, &header.metadata);
    output.push(',');
    json_field_optional_number(output, "started_at_ms", header.started_at_ms);
}

fn write_json_turn(output: &mut String, turn: &Turn) {
    output.push(',');
    json_field_name(output, "index");
    output.push_str(&turn.index.to_string());
    output.push(',');
    json_field_string(output, "input", &turn.input);
    output.push(',');
    json_field_optional_string(output, "output", turn.output.as_deref());
    output.push(',');
    json_field_optional_string(output, "stop_reason", turn.stop_reason.as_deref());
}

fn write_json_tool(output: &mut String, tool: &Tool) {
    output.push(',');
    json_field_name(output, "turn_index");
    output.push_str(&tool.turn_index.to_string());
    output.push(',');
    json_field_string(output, "call_id", &tool.call_id);
    output.push(',');
    json_field_string(output, "name", &tool.name);
    output.push(',');
    json_field_string(output, "input", &tool.input);
    output.push(',');
    json_field_optional_string(output, "output", tool.output.as_deref());
    output.push(',');
    json_field_optional_string(output, "error", tool.error.as_deref());
}

fn write_json_end(output: &mut String, end: &EpisodeEnd) {
    output.push(',');
    json_field_string(output, "reason", end_reason_name(&end.reason));
    output.push(',');
    json_field_optional_string(output, "error", end.error.as_deref());
    output.push(',');
    json_field_optional_number(output, "finished_at_ms", end.finished_at_ms);
}

fn json_field_name(output: &mut String, name: &str) {
    json_string(output, name);
    output.push(':');
}

fn json_field_string(output: &mut String, name: &str, value: &str) {
    json_field_name(output, name);
    json_string(output, value);
}

fn json_field_optional_string(output: &mut String, name: &str, value: Option<&str>) {
    json_field_name(output, name);
    match value {
        Some(value) => json_string(output, value),
        None => output.push_str("null"),
    }
}

fn json_field_optional_number(output: &mut String, name: &str, value: Option<u64>) {
    json_field_name(output, name);
    match value {
        Some(value) => output.push_str(&value.to_string()),
        None => output.push_str("null"),
    }
}

fn json_map(output: &mut String, values: &BTreeMap<String, String>) {
    output.push('{');
    for (index, (key, value)) in values.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        json_field_string(output, key, value);
    }
    output.push('}');
}

fn json_string(output: &mut String, value: &str) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{08}' => output.push_str("\\b"),
            '\u{0C}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character <= '\u{1F}' => {
                use std::fmt::Write as _;
                let _ = write!(output, "\\u{:04x}", character as u32);
            }
            character => output.push(character),
        }
    }
    output.push('"');
}
