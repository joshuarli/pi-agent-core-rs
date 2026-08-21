//! Presentation projection from [`crate::app::AppState`] to the local cell grid.
//!
//! The renderer owns presentation semantics above the core event boundary.
//! Core events remain lossless; this layer decides how a user, assistant,
//! tool, notice, or Markdown table occupies terminal rows.

use crate::app::{AppState, ToolState, TranscriptKind};
use crate::composer::Composer;
use crate::grid::{Cell, Grid, Rect, Style};
use crossterm::style::Color;
use hi_lite::{Highlighter, Kind, Language};
use tea_core::provider::ProviderRegistry;
use tea_protocol::JsonValue;

#[derive(Clone, Debug, Eq, PartialEq)]
struct RenderLine {
    text: String,
    style: Style,
    character_styles: Option<Vec<Style>>,
}

impl RenderLine {
    fn plain(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
            character_styles: None,
        }
    }

    fn styled(text: impl Into<String>, style: Style, character_styles: Vec<Style>) -> Self {
        Self {
            text: text.into(),
            style,
            character_styles: Some(character_styles),
        }
    }
}

/// Fixed regions of a rendered frame.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Layout {
    /// Startup identity/header region.
    pub header: Rect,
    /// Transcript region.
    pub transcript: Rect,
    /// Multiline composer region.
    pub composer: Rect,
    /// Compact hint/status region.
    pub status: Rect,
}

/// Compute the default empty-composer layout.
pub fn layout(width: u16, height: u16) -> Layout {
    layout_for_composer(width, height, 1, 0)
}

/// Compute a frame layout that gives the composer enough rows for its current
/// visual content while retaining at least one transcript row when possible.
pub fn layout_for(state: &AppState, width: u16, height: u16) -> Layout {
    let desired = composer_lines(state.composer(), width).len().max(1) as u16;
    let header_height = welcome_lines(state, width).len() as u16;
    let status_height = if height >= 2 {
        2
    } else {
        u16::from(height > 0)
    };
    let available = height.saturating_sub(status_height + header_height);
    let max_composer = (available / 2).max(1);
    layout_for_composer(
        width,
        height,
        desired.min(max_composer),
        header_height.min(height.saturating_sub(status_height)),
    )
}

fn layout_for_composer(
    width: u16,
    height: u16,
    composer_height: u16,
    header_height: u16,
) -> Layout {
    let status_height = if height >= 2 {
        2
    } else {
        u16::from(height > 0)
    };
    let header_height = header_height.min(height.saturating_sub(status_height));
    let composer_height = composer_height.min(height.saturating_sub(status_height + header_height));
    let transcript_height = height.saturating_sub(composer_height + status_height + header_height);
    Layout {
        header: Rect {
            x: 0,
            y: 0,
            width,
            height: header_height,
        },
        transcript: Rect {
            x: 0,
            y: header_height + composer_height,
            width,
            height: transcript_height,
        },
        composer: Rect {
            x: 0,
            y: header_height,
            width,
            height: composer_height,
        },
        status: Rect {
            x: 0,
            y: header_height + composer_height + transcript_height,
            width,
            height: status_height,
        },
    }
}

/// Render the current presentation state into a fresh frame.
pub fn render(state: &AppState, registry: &ProviderRegistry, width: u16, height: u16) -> Grid {
    let mut grid = Grid::new(width, height);
    let regions = layout_for(state, width, height);
    for (row, line) in welcome_lines(state, regions.header.width)
        .into_iter()
        .enumerate()
    {
        if row >= regions.header.height as usize {
            break;
        }
        put_line(
            &mut grid,
            regions.header.x,
            regions.header.y + row as u16,
            regions.header.width,
            &line,
        );
    }
    let transcript = wrapped_transcript(state, regions.transcript.width);
    let visible_rows = regions.transcript.height as usize;
    let start = if state.follows_output() {
        transcript.len().saturating_sub(visible_rows)
    } else {
        state.viewport_offset().min(transcript.len())
    };
    for (row, line) in transcript.iter().skip(start).enumerate() {
        if row >= visible_rows {
            break;
        }
        put_line(
            &mut grid,
            regions.transcript.x,
            regions.transcript.y + row as u16,
            regions.transcript.width,
            line,
        );
    }

    let composer = composer_lines(state.composer(), regions.composer.width);
    let composer_start = composer_view_start(
        state.composer(),
        regions.composer.height,
        regions.composer.width,
        &composer,
    );
    for (row, line) in composer.into_iter().skip(composer_start).enumerate() {
        if row >= regions.composer.height as usize {
            break;
        }
        put_text(
            &mut grid,
            regions.composer.x,
            regions.composer.y + row as u16,
            regions.composer.width,
            &line,
            Style {
                foreground: Some(Color::White),
                bold: true,
                ..Style::default()
            },
        );
    }

    if regions.status.height != 0 {
        for (row, status) in state.footer_lines(registry).into_iter().enumerate() {
            if row >= regions.status.height as usize {
                break;
            }
            put_text(
                &mut grid,
                regions.status.x,
                regions.status.y + row as u16,
                regions.status.width,
                &status,
                Style {
                    foreground: Some(Color::DarkGrey),
                    ..Style::default()
                },
            );
        }
    }

    if let Some(lines) = state.picker_lines_visible(registry, regions.transcript.height as usize) {
        for (row, line) in lines.into_iter().enumerate() {
            if row >= regions.transcript.height as usize {
                break;
            }
            put_text(
                &mut grid,
                regions.transcript.x,
                regions.transcript.y + row as u16,
                regions.transcript.width,
                &line,
                Style {
                    foreground: Some(Color::White),
                    ..Style::default()
                },
            );
        }
    }
    grid
}

/// Return the count and available row count used by scrolling calculations.
pub fn transcript_metrics(state: &AppState, width: u16, height: u16) -> (usize, usize) {
    let regions = layout_for(state, width, height);
    (
        wrapped_transcript(state, regions.transcript.width).len(),
        regions.transcript.height as usize,
    )
}

/// Return the number of visual rows occupied by the composer.
pub fn composer_height(state: &AppState, width: u16) -> u16 {
    composer_lines(state.composer(), width).len().max(1) as u16
}

/// Return the native cursor location for the visible composer.
pub fn composer_cursor_position(state: &AppState, width: u16, height: u16) -> Option<(u16, u16)> {
    let regions = layout_for(state, width, height);
    if regions.composer.height == 0 {
        return None;
    }
    let (row, column) = composer_visual_cursor(state.composer(), width);
    let composer = composer_lines(state.composer(), width);
    let composer_start =
        composer_view_start(state.composer(), regions.composer.height, width, &composer);
    let row = row.saturating_sub(composer_start as u16);
    Some((
        column.min(width.saturating_sub(1)),
        regions.composer.y + row.min(regions.composer.height.saturating_sub(1)),
    ))
}

fn composer_visual_cursor(composer: &Composer, width: u16) -> (u16, u16) {
    let mut row = 0_u16;
    let mut column = 2_u16;
    let budget = width.saturating_sub(2).max(1);
    for symbol in composer.text()[..composer.cursor()].chars() {
        if symbol == '\n' {
            row = row.saturating_add(1);
            column = 2;
            continue;
        }
        let symbol_width = char_width(symbol) as u16;
        if column.saturating_sub(2).saturating_add(symbol_width) > budget {
            row = row.saturating_add(1);
            column = 2;
        }
        column = column.saturating_add(symbol_width);
    }
    (column, row)
}

fn put_text(grid: &mut Grid, x: u16, y: u16, width: u16, text: &str, style: Style) {
    let mut column = 0_u16;
    for symbol in text.chars() {
        if symbol == '\r' {
            continue;
        }
        if symbol == '\n' {
            break;
        }
        let symbol_width = char_width(symbol);
        if symbol_width == 0 {
            continue;
        }
        let symbol_width = symbol_width as u16;
        if column.saturating_add(symbol_width) > width {
            break;
        }
        let _ = grid.set(x.saturating_add(column), y, Cell { symbol, style });
        if symbol_width == 2 && column + 1 < width {
            let _ = grid.set(x.saturating_add(column + 1), y, Cell { symbol: ' ', style });
        }
        column = column.saturating_add(symbol_width);
    }
}

fn put_line(grid: &mut Grid, x: u16, y: u16, width: u16, line: &RenderLine) {
    let mut column = 0_u16;
    for (index, symbol) in line.text.chars().enumerate() {
        if symbol == '\r' {
            continue;
        }
        if symbol == '\n' {
            break;
        }
        let symbol_width = char_width(symbol);
        if symbol_width == 0 {
            continue;
        }
        let symbol_width = symbol_width as u16;
        if column.saturating_add(symbol_width) > width {
            break;
        }
        let style = line
            .character_styles
            .as_ref()
            .and_then(|styles| styles.get(index).copied())
            .unwrap_or(line.style);
        let _ = grid.set(x.saturating_add(column), y, Cell { symbol, style });
        if symbol_width == 2 && column + 1 < width {
            let _ = grid.set(x.saturating_add(column + 1), y, Cell { symbol: ' ', style });
        }
        column = column.saturating_add(symbol_width);
    }
}

fn wrapped_transcript(state: &AppState, width: u16) -> Vec<RenderLine> {
    let mut output = Vec::new();
    let mut emitted = false;
    for (index, line) in state.transcript().iter().enumerate() {
        if matches!(&line.kind, TranscriptKind::Welcome) {
            continue;
        }
        if emitted {
            output.push(RenderLine::plain(String::new(), Style::default()));
        }
        output.extend(entry_lines_for_state(
            line,
            width,
            state.is_streaming_transcript(index),
            state.tool_output_expanded(),
        ));
        emitted = true;
    }
    for text in state.queued_lines() {
        output.push(RenderLine::plain(String::new(), Style::default()));
        output.extend(wrap_lines(
            &text,
            width,
            Style {
                foreground: Some(Color::DarkGrey),
                ..Style::default()
            },
        ));
    }
    output
}

fn welcome_lines(state: &AppState, width: u16) -> Vec<RenderLine> {
    state
        .transcript()
        .iter()
        .find(|line| matches!(&line.kind, TranscriptKind::Welcome))
        .map_or_else(Vec::new, |line| entry_lines(line, width))
}

fn entry_lines(line: &crate::app::TranscriptLine, width: u16) -> Vec<RenderLine> {
    entry_lines_for_state(line, width, false, true)
}

fn entry_lines_for_state(
    line: &crate::app::TranscriptLine,
    width: u16,
    streaming: bool,
    tool_output_expanded: bool,
) -> Vec<RenderLine> {
    match &line.kind {
        TranscriptKind::Welcome => wrap_lines(
            &line.text,
            width,
            Style {
                foreground: Some(Color::DarkGrey),
                bold: true,
                ..Style::default()
            },
        ),
        TranscriptKind::User => rail_lines(strip_prefix(&line.text, "you: "), width),
        TranscriptKind::Assistant => {
            markdown_lines(strip_prefix(&line.text, "assistant: "), width, !streaming)
        }
        TranscriptKind::Tool { name, state } => {
            tool_lines(name, *state, &line.text, width, tool_output_expanded)
        }
        TranscriptKind::Error => wrap_lines(
            strip_prefix(&line.text, "assistant error: "),
            width,
            Style {
                foreground: Some(Color::Red),
                ..Style::default()
            },
        ),
        TranscriptKind::Notice => wrap_lines(
            &line.text,
            width,
            Style {
                foreground: Some(Color::DarkGrey),
                ..Style::default()
            },
        ),
    }
}

fn rail_lines(text: &str, width: u16) -> Vec<RenderLine> {
    let budget = width.saturating_sub(2);
    wrap_raw_text(text, budget)
        .into_iter()
        .map(|line| {
            RenderLine::plain(
                format!("┃ {line}"),
                Style {
                    foreground: Some(Color::White),
                    bold: true,
                    ..Style::default()
                },
            )
        })
        .collect()
}

fn tool_lines(
    name: &str,
    state: ToolState,
    raw: &str,
    width: u16,
    expanded: bool,
) -> Vec<RenderLine> {
    let marker = match state {
        ToolState::Started => '⏺',
        ToolState::Progress => '…',
        ToolState::Completed => '✓',
        ToolState::Failed => '✗',
    };
    let (phase, payload) = tool_phase_and_payload(name, raw);
    let mut detail = payload.lines().next().unwrap_or_default().trim();
    let summary = (phase == "started")
        .then(|| tool_argument_summary(name, payload))
        .flatten();
    if let Some(summary) = summary.as_deref() {
        detail = summary;
    }
    let label = if detail.is_empty() {
        format!("{marker} {name}")
    } else {
        format!("{marker} {name}: {}", compact_tool_detail(detail))
    };
    let style = Style {
        foreground: Some(match state {
            ToolState::Failed => Color::Red,
            ToolState::Completed => Color::Green,
            ToolState::Started | ToolState::Progress => Color::DarkGrey,
        }),
        bold: state == ToolState::Failed,
        ..Style::default()
    };
    let mut output = wrap_lines(&label, width, style);
    // Keep a multiline result readable without letting its first line hide the
    // lifecycle card. Standard tools can return source files, command output,
    // or search matches; each continuation line gets a low-contrast body rail.
    if phase != "started" && expanded {
        let continuation = payload
            .split_once('\n')
            .map(|(_, continuation)| continuation)
            .unwrap_or_default();
        let body_style = Style {
            foreground: Some(if state == ToolState::Failed {
                Color::Red
            } else {
                Color::DarkGrey
            }),
            ..Style::default()
        };
        output.extend(tool_body_lines(continuation, width, body_style));
    } else if phase != "started" && payload.lines().count() > 1 {
        output.push(RenderLine::plain(
            "  └ … (Ctrl+O to expand)",
            Style {
                foreground: Some(Color::DarkGrey),
                ..Style::default()
            },
        ));
    }
    output
}

fn tool_phase_and_payload<'a>(name: &str, raw: &'a str) -> (&'a str, &'a str) {
    let prefix = format!("tool {name} — ");
    let body = raw.strip_prefix(&prefix).unwrap_or(raw);
    body.split_once(": ").unwrap_or(("", body))
}

fn tool_body_lines(text: &str, width: u16, style: Style) -> Vec<RenderLine> {
    if text.is_empty() {
        return Vec::new();
    }
    let body_width = width.saturating_sub(4);
    wrap_raw_text_preserving_indentation(text, body_width)
        .into_iter()
        .map(|line| RenderLine::plain(format!("  │ {line}"), style))
        .collect()
}

/// Render stable, human-oriented summaries for the pinned coding tools. The
/// tool event still owns the raw JSON; parsing it here keeps presentation from
/// becoming another semantic tool contract and preserves a generic fallback
/// for extension-defined tools.
fn tool_argument_summary(name: &str, payload: &str) -> Option<String> {
    let object = JsonValue::parse(payload).ok()?.as_object()?.clone();
    let value = |key: &str| object.get(key).and_then(JsonValue::as_str);
    match name {
        "bash" | "shell" => {
            value("command").map(|command| format!("$ {}", truncate_display(command.trim(), 72)))
        }
        "read" => value("path").map(|path| {
            let range = match (json_u64(&object, "offset"), json_u64(&object, "limit")) {
                (Some(offset), Some(limit)) => format!(
                    " lines {offset}–{}",
                    offset.saturating_add(limit.saturating_sub(1))
                ),
                (Some(offset), None) => format!(" from line {offset}"),
                _ => String::new(),
            };
            format!("{}{}", truncate_display(path, 64), range)
        }),
        "write" => value("path").map(|path| {
            let bytes = value("content").map_or(0, str::len);
            format!("{} ({bytes} bytes)", truncate_display(path, 56))
        }),
        "edit" => value("path").map(|path| {
            let count = object
                .get("edits")
                .and_then(JsonValue::as_array)
                .map_or(0, |edits| edits.len());
            let noun = if count == 1 {
                "replacement"
            } else {
                "replacements"
            };
            format!("{} ({count} {noun})", truncate_display(path, 56))
        }),
        "grep" => value("pattern").map(|pattern| {
            let location = value("path").or_else(|| value("glob"));
            match location {
                Some(location) => format!(
                    "/{}/ in {}",
                    truncate_display(pattern, 36),
                    truncate_display(location, 28)
                ),
                None => format!("/{}/", truncate_display(pattern, 64)),
            }
        }),
        "find" => value("pattern").map(|pattern| {
            let location = value("path").map_or(".", |path| path);
            format!(
                "{} in {}",
                truncate_display(pattern, 44),
                truncate_display(location, 24)
            )
        }),
        "ls" => Some(value("path").map_or_else(|| ".".into(), |path| truncate_display(path, 72))),
        _ => None,
    }
}

fn json_u64(object: &std::collections::BTreeMap<String, JsonValue>, key: &str) -> Option<u64> {
    object
        .get(key)
        .and_then(JsonValue::as_f64)
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|value| value as u64)
}

fn compact_tool_detail(detail: &str) -> String {
    let detail = detail.trim();
    if detail.starts_with('{') {
        for key in ["command", "path", "query", "pattern"] {
            let marker = format!("\"{key}\":");
            if let Some(index) = detail.find(&marker) {
                let value = detail[index + marker.len()..].trim_start();
                let value = value.trim_matches(['"', '}', ' ']);
                return truncate_display(value, 72);
            }
        }
    }
    truncate_display(detail, 72)
}

fn markdown_lines(text: &str, width: u16, style_diffs: bool) -> Vec<RenderLine> {
    let mut output = Vec::new();
    let raw_lines: Vec<&str> = text.split('\n').collect();
    let mut index = 0;
    let mut markdown = Highlighter::new(Language::Markdown);
    let mut markdown_scratch = Vec::new();
    let mut code_highlighter = None;
    let mut code_scratch = Vec::new();
    let mut code_is_diff = false;
    let mut code_is_complete = false;
    let mut in_code = false;
    while index < raw_lines.len() {
        let raw = raw_lines[index];
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") {
            if in_code {
                in_code = false;
                code_highlighter = None;
                code_is_diff = false;
                code_is_complete = false;
                code_scratch.clear();
                markdown.reset();
                output.push(RenderLine::plain(
                    "└",
                    Style {
                        foreground: Some(Color::DarkGrey),
                        bold: true,
                        ..Style::default()
                    },
                ));
            } else {
                let _ = markdown.highlight_into(raw.as_bytes(), &mut markdown_scratch);
                markdown.reset();
                let info = trimmed.trim_start_matches('`').trim();
                let language_name = info
                    .split(|character: char| character.is_ascii_whitespace() || character == ',')
                    .find(|name| !name.is_empty())
                    .unwrap_or_default();
                code_is_diff = matches!(
                    language_name.to_ascii_lowercase().as_str(),
                    "diff" | "patch" | "udiff"
                );
                code_is_complete = !code_is_diff
                    || style_diffs
                    || raw_lines[index + 1..]
                        .iter()
                        .any(|line| line.trim_start().starts_with("```"));
                code_highlighter = Language::from_name(language_name).map(Highlighter::new);
                in_code = true;
                let label = if info.is_empty() { "code" } else { info };
                output.push(RenderLine::plain(
                    format!("┌ {label}"),
                    Style {
                        foreground: Some(Color::DarkGrey),
                        bold: true,
                        ..Style::default()
                    },
                ));
            }
            index += 1;
            continue;
        }
        if in_code {
            if code_is_diff {
                if code_is_complete {
                    output.extend(diff_code_lines(raw, width));
                } else {
                    output.extend(code_lines(
                        &RenderLine::plain(
                            raw,
                            Style {
                                foreground: Some(Color::DarkGrey),
                                ..Style::default()
                            },
                        ),
                        width,
                    ));
                }
            } else if let Some(highlighter) = code_highlighter.as_mut() {
                let highlighted = highlighted_line(
                    raw,
                    highlighter,
                    &mut code_scratch,
                    Style {
                        foreground: Some(Color::DarkGrey),
                        ..Style::default()
                    },
                );
                output.extend(code_lines(&highlighted, width));
            } else {
                output.extend(code_lines(
                    &RenderLine::plain(
                        raw,
                        Style {
                            foreground: Some(Color::DarkGrey),
                            ..Style::default()
                        },
                    ),
                    width,
                ));
            }
            index += 1;
            continue;
        }

        let highlighted =
            highlighted_line(raw, &mut markdown, &mut markdown_scratch, Style::default());
        if is_table_header(
            raw_lines.get(index).copied(),
            raw_lines.get(index + 1).copied(),
        ) {
            let start = index;
            index += 2;
            while index < raw_lines.len() && raw_lines[index].contains('|') {
                index += 1;
            }
            output.extend(render_table(&raw_lines[start..index], width));
            continue;
        }
        if let Some(heading) = trimmed.strip_prefix('#') {
            output.extend(wrap_lines(
                heading.trim_start_matches('#').trim_start(),
                width,
                Style {
                    foreground: Some(Color::White),
                    bold: true,
                    ..Style::default()
                },
            ));
        } else if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            output.extend(wrap_lines(&format!("• {item}"), width, Style::default()));
        } else if let Some((marker, item)) = ordered_list_item(trimmed) {
            output.extend(wrap_lines(
                &format!("{marker} {item}"),
                width,
                Style::default(),
            ));
        } else if let Some(quote) = trimmed.strip_prefix('>') {
            output.extend(wrap_lines(
                &format!("│ {}", quote.trim_start()),
                width,
                Style {
                    foreground: Some(Color::DarkGrey),
                    ..Style::default()
                },
            ));
        } else {
            output.extend(wrap_styled_line(&highlighted, width, false));
        }
        index += 1;
    }
    output
}

fn ordered_list_item(line: &str) -> Option<(&str, &str)> {
    let boundary = line.find(". ")?;
    let (number, item) = line.split_at(boundary);
    if !number.is_empty() && number.chars().all(|character| character.is_ascii_digit()) {
        Some((number, item.trim_start_matches(". ")))
    } else {
        None
    }
}

fn highlighted_line(
    text: &str,
    highlighter: &mut Highlighter,
    scratch: &mut Vec<Kind>,
    base: Style,
) -> RenderLine {
    let kinds = highlighter.highlight_into(text.as_bytes(), scratch);
    let styles = text
        .char_indices()
        .map(|(index, _)| style_for_kind(kinds.get(index).copied().unwrap_or_default(), base))
        .collect();
    RenderLine::styled(text, base, styles)
}

fn style_for_kind(kind: Kind, base: Style) -> Style {
    let mut style = base;
    style.foreground = match kind {
        Kind::Normal => base.foreground,
        Kind::Keyword => Some(Color::Blue),
        Kind::Type => Some(Color::Cyan),
        Kind::String => Some(Color::Green),
        Kind::Comment => Some(Color::DarkGrey),
        Kind::Number => Some(Color::Yellow),
        Kind::Bracket => Some(Color::White),
        Kind::Operator => Some(Color::Magenta),
        Kind::Function => Some(Color::Blue),
        Kind::Constant => Some(Color::Yellow),
        Kind::Macro => Some(Color::Magenta),
    };
    style
}

fn wrap_styled_line(line: &RenderLine, width: u16, preserve_indentation: bool) -> Vec<RenderLine> {
    if width == 0 {
        return Vec::new();
    }
    let styles = line
        .character_styles
        .as_ref()
        .cloned()
        .unwrap_or_else(|| vec![line.style; line.text.chars().count()]);
    let mut characters = line.text.chars().zip(styles).collect::<Vec<_>>();
    if !preserve_indentation {
        let trim = characters
            .iter()
            .take_while(|(character, _)| character.is_whitespace())
            .count();
        characters.drain(..trim);
    }
    if characters.is_empty() {
        return vec![RenderLine::plain(String::new(), line.style)];
    }

    let mut output = Vec::new();
    let mut start = 0;
    while start < characters.len() {
        let mut used = 0;
        let mut end = start;
        let mut last_space = None;
        while end < characters.len() {
            let symbol = characters[end].0;
            let symbol_width = char_width(symbol);
            if used + symbol_width > usize::from(width) {
                break;
            }
            used += symbol_width;
            if symbol.is_whitespace() {
                last_space = Some(end);
            }
            end += 1;
        }
        if end == start {
            end += 1;
        }
        let cut = if end < characters.len() {
            last_space.filter(|space| *space >= start).unwrap_or(end)
        } else {
            end
        };
        let (chunk, next_start) =
            if cut > start && cut < characters.len() && characters[cut].0.is_whitespace() {
                (&characters[start..cut], cut + 1)
            } else {
                (&characters[start..cut], cut)
            };
        let text = chunk
            .iter()
            .map(|(character, _)| *character)
            .collect::<String>();
        let styles = chunk.iter().map(|(_, style)| *style).collect();
        output.push(RenderLine::styled(text, line.style, styles));
        start = next_start.max(start + 1);
    }
    output
}

fn code_lines(line: &RenderLine, width: u16) -> Vec<RenderLine> {
    let available = width.saturating_sub(2);
    let chunks = wrap_styled_line(line, available, true);
    if chunks.is_empty() {
        return vec![RenderLine::plain(
            "│ ",
            Style {
                foreground: Some(Color::DarkGrey),
                ..Style::default()
            },
        )];
    }
    chunks
        .into_iter()
        .map(prepend_code_rail)
        .collect()
}

fn prepend_code_rail(line: RenderLine) -> RenderLine {
    let rail_style = Style {
        foreground: Some(Color::DarkGrey),
        ..Style::default()
    };
    let mut text = String::from("│ ");
    text.push_str(&line.text);
    let mut styles = vec![rail_style; 2];
    styles.extend(
        line.character_styles
            .unwrap_or_else(|| vec![line.style; line.text.chars().count()]),
    );
    RenderLine::styled(text, line.style, styles)
}

fn diff_code_lines(raw: &str, width: u16) -> Vec<RenderLine> {
    let style = if raw.starts_with('+') && !raw.starts_with("+++") {
        Style {
            foreground: Some(Color::Green),
            ..Style::default()
        }
    } else if raw.starts_with('-') && !raw.starts_with("---") {
        Style {
            foreground: Some(Color::Red),
            ..Style::default()
        }
    } else if raw.starts_with("@@") || raw.starts_with("diff ") {
        Style {
            foreground: Some(Color::Cyan),
            bold: true,
            ..Style::default()
        }
    } else {
        Style {
            foreground: Some(Color::DarkGrey),
            ..Style::default()
        }
    };
    code_lines(&RenderLine::plain(raw, style), width)
}

fn is_table_header(header: Option<&str>, separator: Option<&str>) -> bool {
    let Some(header) = header else { return false };
    let Some(separator) = separator else {
        return false;
    };
    header.contains('|')
        && separator.contains('|')
        && split_table_row(separator).iter().all(|cell| {
            !cell.is_empty()
                && cell
                    .chars()
                    .filter(|character| *character != ':')
                    .all(|character| character == '-')
        })
}

fn split_table_row(row: &str) -> Vec<String> {
    row.trim()
        .trim_matches('|')
        .split('|')
        .map(|cell| cell.trim().trim_matches(':').to_owned())
        .collect()
}

fn render_table(rows: &[&str], width: u16) -> Vec<RenderLine> {
    let cells = rows
        .iter()
        .filter(|row| !is_separator_table_row(row))
        .map(|row| split_table_row(row))
        .collect::<Vec<_>>();
    let columns = cells.iter().map(Vec::len).max().unwrap_or(0);
    if columns == 0 {
        return Vec::new();
    }
    let mut widths = (0..columns)
        .map(|column| {
            cells
                .iter()
                .map(|row| row.get(column).map_or(0, |cell| display_width(cell)))
                .max()
                .unwrap_or(1)
                .max(1)
        })
        .collect::<Vec<_>>();
    let max_width = usize::from(width).max(columns + 3);
    let budget = max_width.saturating_sub(columns + 1);
    while widths.iter().sum::<usize>() > budget {
        if let Some((index, _)) = widths.iter().enumerate().max_by_key(|(_, value)| **value) {
            if widths[index] <= 3 {
                break;
            }
            widths[index] -= 1;
        } else {
            break;
        }
    }
    let border = |left: char, middle: char, right: char| {
        format!(
            "{left}{}{right}",
            widths
                .iter()
                .map(|width| "─".repeat(width + 2))
                .collect::<Vec<_>>()
                .join(&middle.to_string())
        )
    };
    let mut output = vec![RenderLine::plain(
        border('┌', '┬', '┐'),
        Style {
            foreground: Some(Color::Cyan),
            ..Style::default()
        },
    )];
    for (row_index, row) in cells.iter().enumerate() {
        let content = widths
            .iter()
            .enumerate()
            .map(|(column, width)| {
                let value = row.get(column).map_or("", String::as_str);
                format!(" {} ", pad_display(value, *width))
            })
            .collect::<Vec<_>>()
            .join("│");
        output.push(RenderLine::plain(format!("│{content}│"), Style::default()));
        if row_index == 0 {
            output.push(RenderLine::plain(
                border('├', '┼', '┤'),
                Style {
                    foreground: Some(Color::Cyan),
                    ..Style::default()
                },
            ));
        }
    }
    output.push(RenderLine::plain(
        border('└', '┴', '┘'),
        Style {
            foreground: Some(Color::Cyan),
            ..Style::default()
        },
    ));
    output
}

fn is_separator_table_row(row: &str) -> bool {
    split_table_row(row)
        .iter()
        .all(|cell| !cell.is_empty() && cell.chars().all(|character| character == '-'))
}

fn composer_lines(composer: &Composer, width: u16) -> Vec<String> {
    let budget = width.saturating_sub(2);
    let mut output = Vec::new();
    for logical in composer.text().split('\n') {
        let wrapped = wrap_raw_text_preserving_indentation(logical, budget);
        if wrapped.is_empty() {
            output.push("┃ ".into());
        } else {
            output.extend(wrapped.into_iter().map(|line| format!("┃ {line}")));
        }
    }
    if output.is_empty() {
        output.push("┃ ".into());
    }
    output
}

fn composer_view_start(
    composer: &Composer,
    visible_rows: u16,
    width: u16,
    lines: &[String],
) -> usize {
    if visible_rows == 0 || lines.len() <= usize::from(visible_rows) {
        return 0;
    }
    let (_, cursor_row) = composer_visual_cursor(composer, width);
    let visible = usize::from(visible_rows);
    usize::from(cursor_row)
        .saturating_sub(visible - 1)
        .min(lines.len() - visible)
}

fn wrap_lines(text: &str, width: u16, style: Style) -> Vec<RenderLine> {
    wrap_raw_text(text, width)
        .into_iter()
        .map(|text| RenderLine::plain(text, style))
        .collect()
}

fn wrap_raw_text(text: &str, width: u16) -> Vec<String> {
    wrap_raw_text_inner(text, width, false)
}

fn wrap_raw_text_preserving_indentation(text: &str, width: u16) -> Vec<String> {
    wrap_raw_text_inner(text, width, true)
}

fn wrap_raw_text_inner(text: &str, width: u16, preserve_indentation: bool) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut output = Vec::new();
    for logical in text.split('\n') {
        if logical.is_empty() {
            output.push(String::new());
            continue;
        }
        let mut remaining = if preserve_indentation {
            logical.to_owned()
        } else {
            logical.trim_start().to_owned()
        };
        while !remaining.is_empty() {
            let mut used = 0;
            let mut end = 0;
            let mut last_space = None;
            for (index, symbol) in remaining.char_indices() {
                let symbol_width = char_width(symbol);
                if used + symbol_width > usize::from(width) {
                    break;
                }
                used += symbol_width;
                end = index + symbol.len_utf8();
                if symbol.is_whitespace() {
                    last_space = Some(index);
                }
            }
            if end == 0 {
                let symbol = remaining.chars().next().expect("remaining is non-empty");
                end = symbol.len_utf8();
            }
            let cut = if end < remaining.len() {
                last_space.filter(|space| *space > 0).unwrap_or(end)
            } else {
                end
            };
            output.push(remaining[..cut].trim_end().to_owned());
            remaining = remaining[cut..].trim_start().to_owned();
        }
    }
    output
}

fn strip_prefix<'a>(text: &'a str, prefix: &str) -> &'a str {
    text.strip_prefix(prefix).unwrap_or(text)
}

fn truncate_display(text: &str, width: usize) -> String {
    let mut output = String::new();
    let mut used = 0;
    for symbol in text.chars() {
        let symbol_width = char_width(symbol);
        if used + symbol_width > width {
            break;
        }
        output.push(symbol);
        used += symbol_width;
    }
    output
}

fn pad_display(text: &str, width: usize) -> String {
    let value = truncate_display(text, width);
    let padding = width.saturating_sub(display_width(&value));
    format!("{value}{}", " ".repeat(padding))
}

fn display_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

fn char_width(symbol: char) -> usize {
    if symbol == '\u{200d}'
        || matches!(symbol, '\u{fe0e}' | '\u{fe0f}')
        || ('\u{300}'..='\u{36f}').contains(&symbol)
        || ('\u{1ab0}'..='\u{1aff}').contains(&symbol)
        || ('\u{20d0}'..='\u{20ff}').contains(&symbol)
        || ('\u{fe20}'..='\u{fe2f}').contains(&symbol)
    {
        0
    } else if matches!(
        symbol,
        '\u{1100}'..='\u{115f}'
            | '\u{2329}'..='\u{232a}'
            | '\u{2e80}'..='\u{a4cf}'
            | '\u{ac00}'..='\u{d7a3}'
            | '\u{f900}'..='\u{faff}'
            | '\u{fe10}'..='\u{fe19}'
            | '\u{fe30}'..='\u{fe6f}'
            | '\u{ff00}'..='\u{ff60}'
            | '\u{ffe0}'..='\u{ffe6}'
            | '\u{1f300}'..='\u{1faff}'
    ) {
        2
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::TranscriptLine;

    #[test]
    fn markdown_table_has_unicode_borders_and_header_rule() {
        let lines = markdown_lines("| Name | Value |\n| --- | --- |\n| foo | bar |", 40, true);
        assert_eq!(lines[0].text, "┌──────┬───────┐");
        assert_eq!(lines[1].text, "│ Name │ Value │");
        assert_eq!(lines[2].text, "├──────┼───────┤");
        assert_eq!(lines[3].text, "│ foo  │ bar   │");
        assert_eq!(lines[4].text, "└──────┴───────┘");
    }

    #[test]
    fn user_entries_render_as_connected_rails() {
        let line = TranscriptLine {
            sequence: None,
            text: "you: hello world".into(),
            kind: TranscriptKind::User,
        };
        let lines = entry_lines(&line, 20);
        assert_eq!(lines[0].text, "┃ hello world");
    }

    #[test]
    fn wide_characters_consume_two_terminal_cells() {
        assert_eq!(display_width("界"), 2);
        assert_eq!(wrap_raw_text("a界b", 3), ["a界", "b"]);
    }

    #[test]
    fn markdown_table_pads_wide_cells_by_display_width() {
        let lines = markdown_lines("| Name | Value |\n| --- | --- |\n| 界 | ok |", 30, true);
        assert_eq!(lines[1].text, "│ Name │ Value │");
        assert_eq!(lines[3].text, "│ 界   │ ok    │");
    }

    #[test]
    fn markdown_inline_tokens_receive_hi_lite_styles() {
        let lines = markdown_lines("**bold** and `inline`", 40, true);
        let styles = lines[0]
            .character_styles
            .as_ref()
            .expect("highlighted markdown line");
        assert_eq!(styles[0].foreground, Some(Color::Blue));
        let inline_start = lines[0].text.find('`').expect("inline code");
        assert_eq!(styles[inline_start].foreground, Some(Color::Green));
    }

    #[test]
    fn markdown_ordered_lists_and_quotes_get_bounded_structure() {
        let lines = markdown_lines("1. first\n2. second\n> quoted", 40, true);
        let text = lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>();
        assert_eq!(text, ["1 first", "2 second", "│ quoted"]);
    }

    #[test]
    fn fenced_code_uses_the_declared_language_and_preserves_the_rail() {
        let lines = markdown_lines("```rust\nfn main() { return 1; }\n```", 40, true);
        assert_eq!(lines[0].text, "┌ rust");
        assert_eq!(lines[1].text, "│ fn main() { return 1; }");
        let styles = lines[1]
            .character_styles
            .as_ref()
            .expect("highlighted code line");
        assert_eq!(styles[0].foreground, Some(Color::DarkGrey));
        assert_eq!(styles[2].foreground, Some(Color::Blue));
        assert_eq!(lines[2].text, "└");
    }

    #[test]
    fn unknown_fenced_languages_remain_visible_without_syntax_rules() {
        let lines = markdown_lines("```made-up\ncontent\n```", 40, true);
        assert_eq!(lines[1].text, "│ content");
        assert_eq!(
            lines[1]
                .character_styles
                .as_ref()
                .expect("neutral code styles")[2]
                .foreground,
            Some(Color::DarkGrey)
        );
    }

    #[test]
    fn streaming_diffs_stay_neutral_until_the_diff_block_ends() {
        let streaming = markdown_lines("```diff\n+added\n-removed", 40, false);
        let finished = markdown_lines("```diff\n+added\n-removed\n```", 40, false);
        assert_eq!(
            streaming[1]
                .character_styles
                .as_ref()
                .expect("streaming diff styles")[2]
                .foreground,
            Some(Color::DarkGrey)
        );
        assert_eq!(
            finished[1]
                .character_styles
                .as_ref()
                .expect("finished diff styles")[2]
                .foreground,
            Some(Color::Green)
        );
        assert_eq!(
            finished[2]
                .character_styles
                .as_ref()
                .expect("finished diff styles")[2]
                .foreground,
            Some(Color::Red)
        );
    }

    #[test]
    fn standard_tool_arguments_render_as_type_specific_cards() {
        let bash = tool_lines(
            "bash",
            ToolState::Started,
            r#"tool bash — started: {"command":"cargo test -p tea-agent","timeout":30}"#,
            80,
            true,
        );
        assert_eq!(bash[0].text, "⏺ bash: $ cargo test -p tea-agent");

        let edit = tool_lines(
            "edit",
            ToolState::Started,
            r#"tool edit — started: {"path":"src/render.rs","edits":[{"oldText":"a","newText":"b"},{"oldText":"c","newText":"d"}]}"#,
            80,
            true,
        );
        assert_eq!(edit[0].text, "⏺ edit: src/render.rs (2 replacements)");
    }

    #[test]
    fn multiline_tool_results_render_a_body_rail() {
        let lines = tool_lines(
            "read",
            ToolState::Completed,
            "tool read — completed: first line\n  second line\nthird line",
            30,
            true,
        );
        assert_eq!(lines[0].text, "✓ read: first line");
        assert_eq!(lines[1].text, "  │   second line");
        assert_eq!(lines[2].text, "  │ third line");
        assert_eq!(lines[0].style.foreground, Some(Color::Green));

        let collapsed = tool_lines(
            "read",
            ToolState::Completed,
            "tool read — completed: first line\nsecond line",
            30,
            false,
        );
        assert_eq!(collapsed[1].text, "  └ … (Ctrl+O to expand)");
    }

    #[test]
    fn composer_preserves_indentation_and_scrolls_to_the_cursor() {
        let mut composer = Composer::new();
        composer.replace_from_editor("  first\n    second\n      third");
        let lines = composer_lines(&composer, 20);
        assert_eq!(lines[0], "┃   first");
        assert_eq!(lines[2], "┃       third");
        assert_eq!(composer_view_start(&composer, 2, 20, &lines), 1);
    }

    #[test]
    fn empty_composer_starts_at_the_top_of_the_frame() {
        let regions = layout(80, 24);
        assert_eq!(regions.header.height, 0);
        assert_eq!(regions.composer.y, 0);
        assert!(regions.transcript.y > regions.composer.y);
    }
}
