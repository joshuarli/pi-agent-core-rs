//! Explicit JSON Lines and CBOR sinks for compact trajectory records.
//!
//! The trace contract deliberately does not choose a storage location. These
//! sinks only serialize each owned [`TraceEvent`](crate::TraceEvent) to a
//! caller-supplied writer; they do not open files, read clocks, buffer an
//! episode, or make any runtime decision. JSON Lines is convenient for human
//! inspection and streaming pipelines. CBOR is a compact, self-delimiting
//! sequence of the same records for machine-oriented archives.

use crate::event::{EndReason, EpisodeEnd, EpisodeHeader, Tool, TraceEvent, Turn};
use crate::sink::TraceSink;
use std::collections::BTreeMap;
use std::io::{self, Write};

/// A [`TraceSink`] that writes one JSON object followed by a newline per event.
///
/// The JSON shape is an explicit V0 wire format. Every object carries
/// `schema_version` and a `type` discriminator. Its key order is stable, so
/// deterministic traces stay diff-friendly without an external serializer.
pub struct JsonLinesSink<W> {
    writer: W,
}

impl<W> JsonLinesSink<W> {
    /// Wrap a caller-owned writer.
    pub const fn new(writer: W) -> Self {
        Self { writer }
    }

    /// Return the underlying writer.
    pub fn into_inner(self) -> W {
        self.writer
    }

    /// Borrow the underlying writer.
    pub const fn inner(&self) -> &W {
        &self.writer
    }

    /// Mutably borrow the underlying writer.
    pub fn inner_mut(&mut self) -> &mut W {
        &mut self.writer
    }
}

impl<W: Write> TraceSink for JsonLinesSink<W> {
    type Error = io::Error;

    fn append(&mut self, event: TraceEvent) -> Result<(), Self::Error> {
        let mut record = String::new();
        write_json_event(&mut record, &event);
        self.writer.write_all(record.as_bytes())?;
        self.writer.write_all(b"\n")
    }
}

/// A [`TraceSink`] that appends one self-delimiting CBOR map per event.
///
/// The wire shape mirrors [`JsonLinesSink`]. Values use only definite-length
/// CBOR major types for maps, arrays, text, unsigned integers, booleans, and null;
/// no indefinite lengths, tags, floats, or host-specific extensions are used.
/// Concatenated values are intentionally valid CBOR sequence framing, which
/// allows a caller to stream records without a precomputed episode length.
pub struct CborSink<W> {
    writer: W,
}

impl<W> CborSink<W> {
    /// Wrap a caller-owned writer.
    pub const fn new(writer: W) -> Self {
        Self { writer }
    }

    /// Return the underlying writer.
    pub fn into_inner(self) -> W {
        self.writer
    }

    /// Borrow the underlying writer.
    pub const fn inner(&self) -> &W {
        &self.writer
    }

    /// Mutably borrow the underlying writer.
    pub fn inner_mut(&mut self) -> &mut W {
        &mut self.writer
    }
}

impl<W: Write> TraceSink for CborSink<W> {
    type Error = io::Error;

    fn append(&mut self, event: TraceEvent) -> Result<(), Self::Error> {
        let mut bytes = Vec::new();
        write_cbor_event(&mut bytes, &event);
        self.writer.write_all(&bytes)
    }
}

fn write_json_event(output: &mut String, event: &TraceEvent) {
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

fn write_cbor_event(output: &mut Vec<u8>, event: &TraceEvent) {
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

fn event_type(event: &TraceEvent) -> &'static str {
    match event {
        TraceEvent::EpisodeHeader(_) => "episode_header",
        TraceEvent::Turn(_) => "turn",
        TraceEvent::Tool(_) => "tool",
        TraceEvent::EpisodeEnd(_) => "episode_end",
    }
}

fn end_reason_name(reason: &EndReason) -> &str {
    match reason {
        EndReason::Completed => "completed",
        EndReason::Cancelled => "cancelled",
        EndReason::Failed => "failed",
        EndReason::Aborted => "aborted",
        EndReason::Other(value) => value,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EpisodeHeader, TraceEvent, Turn};

    #[test]
    fn json_lines_is_one_stable_escaped_object_per_record() {
        let mut sink = JsonLinesSink::new(Vec::new());
        sink.append(TraceEvent::from(
            EpisodeHeader::new("episode\n1").with_metadata("z", "\u{0001}\""),
        ))
        .expect("in-memory write succeeds");
        sink.append(TraceEvent::from(
            Turn::new(3, "input").with_output("output"),
        ))
        .expect("in-memory write succeeds");
        let text = String::from_utf8(sink.into_inner()).expect("JSON is UTF-8");
        assert_eq!(text.lines().count(), 2);
        assert_eq!(
            text.lines().next(),
            Some(
                r#"{"schema_version":0,"type":"episode_header","episode_id":"episode\n1","metadata":{"z":"\u0001\""},"started_at_ms":null}"#
            ),
        );
        assert!(text.contains(r#""type":"turn"#));
    }

    #[test]
    fn cbor_uses_definite_maps_and_has_no_json_line_delimiter() {
        let mut sink = CborSink::new(Vec::new());
        sink.append(TraceEvent::from(EpisodeHeader::new("episode")))
            .expect("in-memory write succeeds");
        let bytes = sink.into_inner();
        // Major type 5 (map), length 5. The record is one CBOR value, not a
        // newline-delimited JSON encoding.
        assert_eq!(bytes.first(), Some(&0xa5));
        assert!(!bytes.contains(&b'\n'));
        assert!(
            bytes
                .windows("episode_header".len())
                .any(|window| window == b"episode_header")
        );
    }
}
