//! Display-width-aware composer geometry.

/// A visible composer row and the byte range it represents in the source buffer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VisualRow {
    pub text: String,
    pub start: usize,
    pub end: usize,
}

/// Measured composer layout. Rows include the `❯ ` prompt prefix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VisualLayout {
    pub rows: Vec<VisualRow>,
    pub cursor_row: usize,
    pub cursor_column: usize,
    pub cursor_byte: usize,
}

impl VisualLayout {
    /// Measure hard newlines and soft wrapping at `width` terminal columns.
    pub fn measure(text: &str, cursor: usize, width: u16) -> Self {
        let width = usize::from(width).max(1);
        let content_width = width.saturating_sub(2).max(1);
        let cursor = cursor.min(text.len());
        let mut rows = Vec::new();
        let mut row_start = 0;
        let mut used = 0;
        let mut row_text = String::new();
        let mut cursor_row = 0;
        let mut cursor_column = 2;
        let mut offset = 0;

        for symbol in text.chars() {
            let next = offset + symbol.len_utf8();
            if symbol == '\n' {
                if cursor == offset {
                    cursor_row = rows.len();
                    cursor_column = 2 + used;
                }
                rows.push(VisualRow {
                    text: format!("❯ {row_text}"),
                    start: row_start,
                    end: offset,
                });
                row_start = next;
                row_text.clear();
                used = 0;
                offset = next;
                continue;
            }
            let symbol_width = display_width(symbol);
            if used != 0 && used + symbol_width > content_width {
                rows.push(VisualRow {
                    text: format!("❯ {row_text}"),
                    start: row_start,
                    end: offset,
                });
                row_start = offset;
                row_text.clear();
                used = 0;
            }
            if cursor == offset {
                cursor_row = rows.len();
                cursor_column = 2 + used;
            }
            row_text.push(symbol);
            used += symbol_width;
            offset = next;
        }
        if cursor == offset {
            cursor_row = rows.len();
            cursor_column = 2 + used;
        }
        rows.push(VisualRow {
            text: format!("❯ {row_text}"),
            start: row_start,
            end: offset,
        });
        Self {
            rows,
            cursor_row,
            cursor_column: cursor_column.min(width.saturating_sub(1)),
            cursor_byte: cursor,
        }
    }
}

/// Return the number of terminal cells occupied by a scalar.
pub fn display_width(symbol: char) -> usize {
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

    #[test]
    fn layout_wraps_wide_scalars_and_keeps_cursor_visible() {
        let layout = VisualLayout::measure("a界b", 5, 5);
        assert_eq!(layout.rows[0].text, "❯ a界");
        assert_eq!(layout.rows[1].text, "❯ b");
        assert_eq!(layout.cursor_row, 1);
        assert_eq!(layout.cursor_column, 3);
    }

    #[test]
    fn layout_preserves_hard_empty_lines_and_combining_marks() {
        let layout = VisualLayout::measure("e\u{301}\n\nnext", 4, 20);
        assert_eq!(layout.rows.len(), 3);
        assert_eq!(layout.rows[1].text, "❯ ");
        assert_eq!(display_width('\u{301}'), 0);
    }
}
