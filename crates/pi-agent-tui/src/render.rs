//! Pure projection from [`crate::app::AppState`] to the local cell grid.

use crate::app::AppState;
use crate::grid::{Cell, Grid, Rect, Style};
use crossterm::style::Color;
use pi_agent_core::provider::ProviderRegistry;

#[derive(Clone, Debug, Eq, PartialEq)]
struct RenderLine {
    text: String,
    style: Style,
}

/// Fixed regions of the initial screen.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Layout {
    /// Transcript region.
    pub transcript: Rect,
    /// One-line composer region.
    pub composer: Rect,
    /// Status region.
    pub status: Rect,
}

/// Compute the fixed transcript/composer/status regions.
pub fn layout(width: u16, height: u16) -> Layout {
    let composer_height = u16::from(height >= 2);
    let status_height = if height >= 3 { 2 } else { u16::from(height >= 1) };
    let transcript_height = height.saturating_sub(composer_height + status_height);
    Layout {
        transcript: Rect {
            x: 0,
            y: 0,
            width,
            height: transcript_height,
        },
        composer: Rect {
            x: 0,
            y: transcript_height,
            width,
            height: composer_height,
        },
        status: Rect {
            x: 0,
            y: transcript_height + composer_height,
            width,
            height: status_height,
        },
    }
}

/// Render the current presentation state into a fresh frame.
pub fn render(state: &AppState, registry: &ProviderRegistry, width: u16, height: u16) -> Grid {
    let mut grid = Grid::new(width, height);
    let regions = layout(width, height);
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

    if regions.composer.height != 0 {
        put_text(
            &mut grid,
            regions.composer.x,
            regions.composer.y,
            regions.composer.width,
            &composer_display(state),
            Style::default(),
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
                Style::default(),
            );
        }
    }
    if let Some(lines) = state.picker_lines(registry) {
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
                Style::default(),
            );
        }
    }
    grid
}

/// Return the count and available row count used by scrolling calculations.
pub fn transcript_metrics(state: &AppState, width: u16, height: u16) -> (usize, usize) {
    let regions = layout(width, height);
    (
        wrapped_transcript(state, regions.transcript.width).len(),
        regions.transcript.height as usize,
    )
}

fn composer_display(state: &AppState) -> String {
    if state.composer().is_multiline() {
        format!(
            "> [multiline prompt: {} bytes; Ctrl+G to edit]",
            state.composer().text().len()
        )
    } else {
        format!("> {}", state.composer().text())
    }
}

fn put_text(grid: &mut Grid, x: u16, y: u16, width: u16, text: &str, style: Style) {
    for (offset, symbol) in text
        .chars()
        .map(|symbol| {
            if matches!(symbol, '\n' | '\r') {
                '↵'
            } else {
                symbol
            }
        })
        .take(width as usize)
        .enumerate()
    {
        let _ = grid.set(x.saturating_add(offset as u16), y, Cell { symbol, style });
    }
}

fn wrapped_transcript(state: &AppState, width: u16) -> Vec<RenderLine> {
    state
        .transcript()
        .iter()
        .flat_map(|line| block_lines(&line.text))
        .flat_map(|line| wrap_render_line(line, width))
        .chain(
            state
                .queued_lines()
                .into_iter()
                .map(|text| RenderLine {
                    text,
                    style: Style::default(),
                })
                .flat_map(|line| wrap_render_line(line, width)),
        )
        .collect()
}

/// Apply the small, stable block vocabulary the v0 renderer owns locally.
fn block_lines(text: &str) -> Vec<RenderLine> {
    let mut in_code = false;
    let mut lines = Vec::new();
    for raw in text.split('\n') {
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") {
            let label = trimmed.trim_start_matches('`').trim();
            if in_code {
                lines.push(RenderLine {
                    text: "└".into(),
                    style: Style {
                        foreground: Some(Color::Cyan),
                        ..Style::default()
                    },
                });
                in_code = false;
            } else {
                lines.push(RenderLine {
                    text: if label.is_empty() {
                        "┌ code".into()
                    } else {
                        format!("┌ code: {label}")
                    },
                    style: Style {
                        foreground: Some(Color::Cyan),
                        bold: true,
                        ..Style::default()
                    },
                });
                in_code = true;
            }
            continue;
        }
        if in_code {
            lines.push(RenderLine {
                text: format!("  {raw}"),
                style: Style {
                    foreground: Some(Color::Cyan),
                    ..Style::default()
                },
            });
            continue;
        }
        if let Some(heading) = trimmed.strip_prefix('#') {
            lines.push(RenderLine {
                text: heading.trim_start_matches('#').trim_start().to_owned(),
                style: Style {
                    bold: true,
                    ..Style::default()
                },
            });
            continue;
        }
        if let Some(item) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
            lines.push(RenderLine {
                text: format!("• {item}"),
                style: Style::default(),
            });
            continue;
        }
        let error = trimmed.starts_with("assistant error:")
            || trimmed.contains(" — failed:")
            || trimmed.starts_with("turn failed");
        lines.push(RenderLine {
            text: raw.to_owned(),
            style: Style {
                foreground: error.then_some(Color::Red),
                ..Style::default()
            },
        });
    }
    lines
}

fn wrap_render_line(line: RenderLine, width: u16) -> Vec<RenderLine> {
    wrap_raw_text(&line.text, width)
        .into_iter()
        .map(|text| RenderLine {
            text,
            style: line.style,
        })
        .collect()
}

/// Wrap raw text by scalar count; wide-cell support is intentionally deferred until tested.
fn wrap_raw_text(text: &str, width: u16) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines = vec![String::new()];
    for symbol in text.chars() {
        if symbol == '\r' {
            continue;
        }
        if symbol == '\n'
            || lines
                .last()
                .is_some_and(|line| line.chars().count() == width as usize)
        {
            lines.push(String::new());
            if symbol == '\n' {
                continue;
            }
        }
        lines
            .last_mut()
            .expect("one output line exists")
            .push(symbol);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_text_wraps_without_a_text_layout_dependency() {
        assert_eq!(wrap_raw_text("abcdef", 3), ["abc", "def"]);
        assert_eq!(wrap_raw_text("a\nb", 3), ["a", "b"]);
    }

    #[test]
    fn bounded_blocks_keep_markdown_code_and_errors_legible() {
        let lines = block_lines("# title\n- item\n```rust\nlet ok = true;\n```\nassistant error: bad");
        assert_eq!(lines[0].text, "title");
        assert!(lines[0].style.bold);
        assert_eq!(lines[1].text, "• item");
        assert_eq!(lines[2].text, "┌ code: rust");
        assert_eq!(lines[3].text, "  let ok = true;");
        assert_eq!(lines[4].text, "└");
        assert_eq!(lines[5].style.foreground, Some(Color::Red));
    }
}
