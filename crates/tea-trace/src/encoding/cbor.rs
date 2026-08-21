//! Definite-length CBOR sequence encoding for trace events.

use super::{end_reason_name, event_type};
use crate::event::{EpisodeEnd, EpisodeHeader, Tool, TraceEvent, Turn};

pub(super) fn write_cbor_event(output: &mut Vec<u8>, event: &TraceEvent) {
    let field_count = match event {
        TraceEvent::EpisodeHeader(_) => 5,
        TraceEvent::Turn(_) => 6,
        TraceEvent::EpisodeEnd(_) => 5,
        TraceEvent::Tool(_) => 8,
    };
    cbor_map(output, field_count);
    cbor_text(output, "schema_version");
    cbor_unsigned(output, crate::event::TRACE_SCHEMA_VERSION.into());
    cbor_text(output, "type");
    cbor_text(output, event_type(event));
    match event {
        TraceEvent::EpisodeHeader(header) => write_cbor_header(output, header),
        TraceEvent::Turn(turn) => write_cbor_turn(output, turn),
        TraceEvent::Tool(tool) => write_cbor_tool(output, tool),
        TraceEvent::EpisodeEnd(end) => write_cbor_end(output, end),
    }
}

fn write_cbor_header(output: &mut Vec<u8>, header: &EpisodeHeader) {
    cbor_text(output, "episode_id");
    cbor_text(output, &header.episode_id);
    cbor_text(output, "metadata");
    cbor_map(output, header.metadata.len());
    for (key, value) in &header.metadata {
        cbor_text(output, key);
        cbor_text(output, value);
    }
    cbor_text(output, "started_at_ms");
    cbor_optional_unsigned(output, header.started_at_ms);
}

fn write_cbor_turn(output: &mut Vec<u8>, turn: &Turn) {
    cbor_text(output, "index");
    cbor_unsigned(output, turn.index.into());
    cbor_text(output, "input");
    cbor_text(output, &turn.input);
    cbor_text(output, "output");
    cbor_optional_text(output, turn.output.as_deref());
    cbor_text(output, "stop_reason");
    cbor_optional_text(output, turn.stop_reason.as_deref());
}

fn write_cbor_tool(output: &mut Vec<u8>, tool: &Tool) {
    cbor_text(output, "turn_index");
    cbor_unsigned(output, tool.turn_index.into());
    cbor_text(output, "call_id");
    cbor_text(output, &tool.call_id);
    cbor_text(output, "name");
    cbor_text(output, &tool.name);
    cbor_text(output, "input");
    cbor_text(output, &tool.input);
    cbor_text(output, "output");
    cbor_optional_text(output, tool.output.as_deref());
    cbor_text(output, "error");
    cbor_optional_text(output, tool.error.as_deref());
}

fn write_cbor_end(output: &mut Vec<u8>, end: &EpisodeEnd) {
    cbor_text(output, "reason");
    cbor_text(output, end_reason_name(&end.reason));
    cbor_text(output, "error");
    cbor_optional_text(output, end.error.as_deref());
    cbor_text(output, "finished_at_ms");
    cbor_optional_unsigned(output, end.finished_at_ms);
}

fn cbor_optional_text(output: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => cbor_text(output, value),
        None => output.push(0xf6),
    }
}

fn cbor_optional_unsigned(output: &mut Vec<u8>, value: Option<u64>) {
    match value {
        Some(value) => cbor_unsigned(output, value),
        None => output.push(0xf6),
    }
}

fn cbor_text(output: &mut Vec<u8>, value: &str) {
    cbor_major_length(output, 3, value.len() as u64);
    output.extend_from_slice(value.as_bytes());
}

fn cbor_map(output: &mut Vec<u8>, length: usize) {
    cbor_major_length(output, 5, length as u64);
}

fn cbor_unsigned(output: &mut Vec<u8>, value: u64) {
    cbor_major_length(output, 0, value);
}

fn cbor_major_length(output: &mut Vec<u8>, major: u8, value: u64) {
    debug_assert!(major <= 7);
    match value {
        0..=23 => output.push((major << 5) | value as u8),
        24..=255 => output.extend_from_slice(&[(major << 5) | 24, value as u8]),
        256..=65_535 => {
            output.push((major << 5) | 25);
            output.extend_from_slice(&(value as u16).to_be_bytes());
        }
        65_536..=4_294_967_295 => {
            output.push((major << 5) | 26);
            output.extend_from_slice(&(value as u32).to_be_bytes());
        }
        _ => {
            output.push((major << 5) | 27);
            output.extend_from_slice(&value.to_be_bytes());
        }
    }
}
