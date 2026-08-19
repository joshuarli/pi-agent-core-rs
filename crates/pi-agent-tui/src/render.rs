//! Presentation projection from [`crate::app::AppState`] to the local cell grid.
//!
//! The renderer owns presentation semantics above the core event boundary.
//! Core events remain lossless; this layer decides how a user, assistant,
//! tool, notice, or Markdown table occupies terminal rows.

use crate::app::{AppState, ToolState, TranscriptKind};
use crate::composer::Composer;
use crate::grid::{Cell, Grid, Rect, Style};
use crossterm::style::Color;
use pi_agent_core::provider::ProviderRegistry;

#[derive(Clone, Debug, Eq, PartialEq)]
struct RenderLine {
    text: String,
    style: Style,
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
        put_text(
            &mut grid,
            regions.header.x,
            regions.header.y + row as u16,
            regions.header.width,
            &line.text,
            line.style,
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
        put_text(
            &mut grid,
            regions.transcript.x,
            regions.transcript.y + row as u16,
            regions.transcript.width,
            &line.text,
            line.style,
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

fn wrapped_transcript(state: &AppState, width: u16) -> Vec<RenderLine> {
    let mut output = Vec::new();
    let mut entries = state
        .transcript()
        .iter()
        .filter(|line| !matches!(&line.kind, TranscriptKind::Welcome));
    for (index, line) in entries.by_ref().enumerate() {
        if index > 0 {
            output.push(RenderLine {
                text: String::new(),
                style: Style::default(),
            });
        }
        output.extend(entry_lines(line, width));
    }
    for text in state.queued_lines() {
        output.push(RenderLine {
            text: String::new(),
            style: Style::default(),
        });
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
        TranscriptKind::Assistant => markdown_lines(strip_prefix(&line.text, "assistant: "), width),
        TranscriptKind::Tool { name, state } => tool_lines(name, *state, &line.text, width),
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
        .map(|line| RenderLine {
            text: format!("┃ {line}"),
            style: Style {
                foreground: Some(Color::White),
                bold: true,
                ..Style::default()
            },
        })
        .collect()
}

fn tool_lines(name: &str, state: ToolState, raw: &str, width: u16) -> Vec<RenderLine> {
    let marker = match state {
        ToolState::Started => '⏺',
        ToolState::Progress => '…',
        ToolState::Completed => '✓',
        ToolState::Failed => '✗',
    };
    let detail = raw
        .split_once(": ")
        .map(|(_, detail)| compact_tool_detail(detail))
        .unwrap_or_default();
    let label = if detail.is_empty() {
        format!("{marker} {name}")
    } else {
        format!("{marker} {name}: {detail}")
    };
    wrap_lines(
        &label,
        width,
        Style {
            foreground: Some(if state == ToolState::Failed {
                Color::Red
            } else {
                Color::DarkGrey
            }),
            ..Style::default()
        },
    )
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

fn markdown_lines(text: &str, width: u16) -> Vec<RenderLine> {
    let mut output = Vec::new();
    let raw_lines: Vec<&str> = text.split('\n').collect();
    let mut index = 0;
    let mut in_code = false;
    while index < raw_lines.len() {
        let raw = raw_lines[index];
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") {
            in_code = !in_code;
            output.push(RenderLine {
                text: if in_code {
                    format!("┌ {}", trimmed.trim_start_matches('`').trim())
                } else {
                    "└".into()
                },
                style: Style {
                    foreground: Some(Color::DarkGrey),
                    bold: true,
                    ..Style::default()
                },
            });
            index += 1;
            continue;
        }
        if in_code {
            output.extend(wrap_lines(
                &format!("│ {raw}"),
                width,
                Style {
                    foreground: Some(Color::DarkGrey),
                    ..Style::default()
                },
            ));
            index += 1;
            continue;
        }
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
        } else {
            output.extend(wrap_lines(raw, width, Style::default()));
        }
        index += 1;
    }
    output
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
    let mut output = vec![RenderLine {
        text: border('┌', '┬', '┐'),
        style: Style {
            foreground: Some(Color::Cyan),
            ..Style::default()
        },
    }];
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
        output.push(RenderLine {
            text: format!("│{content}│"),
            style: Style::default(),
        });
        if row_index == 0 {
            output.push(RenderLine {
                text: border('├', '┼', '┤'),
                style: Style {
                    foreground: Some(Color::Cyan),
                    ..Style::default()
                },
            });
        }
    }
    output.push(RenderLine {
        text: border('└', '┴', '┘'),
        style: Style {
            foreground: Some(Color::Cyan),
            ..Style::default()
        },
    });
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
        .map(|text| RenderLine { text, style })
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
        let lines = markdown_lines("| Name | Value |\n| --- | --- |\n| foo | bar |", 40);
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
        let lines = markdown_lines("| Name | Value |\n| --- | --- |\n| 界 | ok |", 30);
        assert_eq!(lines[1].text, "│ Name │ Value │");
        assert_eq!(lines[3].text, "│ 界   │ ok    │");
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
