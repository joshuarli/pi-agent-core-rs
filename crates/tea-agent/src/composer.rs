//! Local prompt buffer and cursor operations.

use std::fmt;

use crate::ui::visual_layout::{display_width, VisualLayout};

/// Errors from native prompt editing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComposerError {
    /// A line-only operation was given a newline.
    Newline,
}

impl fmt::Display for ComposerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Newline => formatter.write_str("line-only composer operation received a newline"),
        }
    }
}

impl std::error::Error for ComposerError {}

/// UTF-8-safe prompt buffer. Native editing supports newlines through an
/// explicit insertion method; the one-line `replace` operation remains useful
/// for command completion and recovery paths.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Composer {
    text: String,
    cursor: usize,
}

impl Composer {
    /// Create an empty composer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Borrow the complete buffer.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether this buffer contains editor-provided multiline text.
    pub fn is_multiline(&self) -> bool {
        self.text.contains(['\n', '\r'])
    }

    /// Return the cursor's UTF-8 byte offset.
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// Insert one scalar at the cursor.
    pub fn insert(&mut self, symbol: char) -> Result<(), ComposerError> {
        if symbol == '\n' || symbol == '\r' {
            return Err(ComposerError::Newline);
        }
        self.text.insert(self.cursor, symbol);
        self.cursor += symbol.len_utf8();
        Ok(())
    }

    /// Insert a native newline at the cursor.
    pub fn insert_newline(&mut self) {
        self.text.insert(self.cursor, '\n');
        self.cursor += 1;
    }

    /// Insert a string at the cursor.
    pub fn insert_str(&mut self, text: &str) -> Result<(), ComposerError> {
        if text.chars().any(|symbol| symbol == '\n' || symbol == '\r') {
            return Err(ComposerError::Newline);
        }
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
        Ok(())
    }

    /// Insert pasted or editor text, preserving line breaks.
    pub fn insert_str_multiline(&mut self, text: &str) {
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    /// Replace the whole buffer with text returned by the external editor.
    ///
    /// The native composer never inserts line breaks itself, but `$EDITOR` is
    /// expressly allowed to return a complete multiline prompt.
    pub fn replace_from_editor(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
    }

    /// Remove all local input.
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// Take the complete input and reset the composer to empty.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    /// Move one scalar to the left.
    pub fn move_left(&mut self) {
        self.cursor = previous_cluster_start(&self.text, self.cursor);
    }

    /// Move one scalar to the right.
    pub fn move_right(&mut self) {
        if let Some(character) = self.text[self.cursor..].chars().next() {
            self.cursor += character.len_utf8();
            while let Some(character) = self.text[self.cursor..].chars().next() {
                if !is_zero_width(character) {
                    break;
                }
                self.cursor += character.len_utf8();
            }
        }
    }

    /// Move over one shell-like word to the left.
    pub fn move_word_left(&mut self) {
        while self.cursor > 0 {
            let (start, character) = self.text[..self.cursor]
                .char_indices()
                .next_back()
                .expect("cursor is on a character boundary");
            if !character.is_whitespace() {
                break;
            }
            self.cursor = start;
        }
        while self.cursor > 0 {
            let (start, character) = self.text[..self.cursor]
                .char_indices()
                .next_back()
                .expect("cursor is on a character boundary");
            if character.is_whitespace() {
                break;
            }
            self.cursor = start;
        }
    }

    /// Move over one shell-like word to the right.
    pub fn move_word_right(&mut self) {
        while self.cursor < self.text.len() {
            let character = self.text[self.cursor..]
                .chars()
                .next()
                .expect("cursor is before a character");
            if !character.is_whitespace() {
                break;
            }
            self.cursor += character.len_utf8();
        }
        while self.cursor < self.text.len() {
            let character = self.text[self.cursor..]
                .chars()
                .next()
                .expect("cursor is before a character");
            if character.is_whitespace() {
                break;
            }
            self.cursor += character.len_utf8();
        }
    }

    /// Move to the same character column on the previous logical line.
    pub fn move_line_up(&mut self) {
        let current_start = self.text[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        if current_start == 0 {
            return;
        }
        let column = self.text[current_start..self.cursor].chars().count();
        let previous_end = current_start - 1;
        let previous_start = self.text[..previous_end]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        self.cursor =
            char_offset(&self.text[previous_start..previous_end], column) + previous_start;
    }

    /// Move to the same character column on the next logical line.
    pub fn move_line_down(&mut self) {
        let current_start = self.text[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        let current_end = self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |index| self.cursor + index);
        if current_end == self.text.len() {
            return;
        }
        let column = self.text[current_start..self.cursor].chars().count();
        let next_start = current_end + 1;
        let next_end = self.text[next_start..]
            .find('\n')
            .map_or(self.text.len(), |index| next_start + index);
        self.cursor = char_offset(&self.text[next_start..next_end], column) + next_start;
    }

    /// Move to the same display column on the previous wrapped or hard line.
    ///
    /// Unlike [`Self::move_line_up`], this follows the terminal's visual rows,
    /// including soft wrapping and wide scalars. It returns whether the cursor
    /// moved, allowing input handling to fall back to history at the boundary.
    pub fn move_visual_line_up(&mut self, width: u16) -> bool {
        self.move_visual_line(width, -1)
    }

    /// Move to the same display column on the next wrapped or hard line.
    pub fn move_visual_line_down(&mut self, width: u16) -> bool {
        self.move_visual_line(width, 1)
    }

    fn move_visual_line(&mut self, width: u16, direction: isize) -> bool {
        let layout = VisualLayout::measure(&self.text, self.cursor, width);
        let current = layout.cursor_row as isize;
        let target = current + direction;
        if target < 0 || target >= layout.rows.len() as isize {
            return false;
        }
        let current_row = &layout.rows[layout.cursor_row];
        let target_row = &layout.rows[target as usize];
        let desired_column = self.text[current_row.start..self.cursor]
            .chars()
            .map(display_width)
            .sum::<usize>();
        let mut column = 0;
        let mut cursor = target_row.start;
        for symbol in self.text[target_row.start..target_row.end].chars() {
            let symbol_width = display_width(symbol);
            if column + symbol_width > desired_column {
                break;
            }
            column += symbol_width;
            cursor += symbol.len_utf8();
        }
        self.cursor = cursor;
        true
    }

    /// Move to the beginning of the current logical line.
    pub fn home(&mut self) {
        self.cursor = self.text[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
    }

    /// Move to the end of the current logical line.
    pub fn end(&mut self) {
        self.cursor = self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |index| self.cursor + index);
    }

    /// Delete the scalar before the cursor.
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let cluster_start = previous_cluster_start(&self.text, self.cursor);
        self.text.drain(cluster_start..self.cursor);
        self.cursor = cluster_start;
    }

    /// Delete the scalar at the cursor.
    pub fn delete(&mut self) {
        let Some(character) = self.text[self.cursor..].chars().next() else {
            return;
        };
        let mut end = self.cursor + character.len_utf8();
        while let Some(next) = self.text[end..].chars().next() {
            if !is_zero_width(next) {
                break;
            }
            end += next.len_utf8();
        }
        self.text.drain(self.cursor..end);
    }

    /// Replace the complete line and place the cursor at its end.
    pub fn replace(&mut self, text: impl Into<String>) -> Result<(), ComposerError> {
        let text = text.into();
        if text.chars().any(|symbol| symbol == '\n' || symbol == '\r') {
            return Err(ComposerError::Newline);
        }
        self.text = text;
        self.cursor = self.text.len();
        Ok(())
    }

    /// Take the current submission and clear the buffer.
    pub fn submit(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }
}

fn char_offset(text: &str, target: usize) -> usize {
    text.char_indices()
        .nth(target)
        .map_or(text.len(), |(index, _)| index)
}

fn is_zero_width(character: char) -> bool {
    character == '\u{200d}'
        || matches!(character, '\u{fe0e}' | '\u{fe0f}')
        || ('\u{300}'..='\u{36f}').contains(&character)
        || ('\u{1ab0}'..='\u{1aff}').contains(&character)
        || ('\u{20d0}'..='\u{20ff}').contains(&character)
        || ('\u{fe20}'..='\u{fe2f}').contains(&character)
}

fn previous_cluster_start(text: &str, cursor: usize) -> usize {
    let mut iterator = text[..cursor].char_indices().rev();
    let Some((mut start, mut character)) = iterator.next() else {
        return 0;
    };
    while is_zero_width(character) {
        let Some((previous_start, previous_character)) = iterator.next() else {
            return 0;
        };
        start = previous_start;
        character = previous_character;
    }
    start
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_edits_remain_one_line_but_editor_text_may_be_multiline() {
        let mut composer = Composer::new();
        composer.insert_str("abc").expect("one line is valid");
        composer.move_left();
        composer.backspace();
        assert_eq!(composer.text(), "ac");
        assert!(matches!(composer.insert('\n'), Err(ComposerError::Newline)));

        composer.clear();
        composer.insert_str("ac").expect("one line is valid");
        composer.end();
        composer.insert_newline();
        composer.insert_str_multiline("second");
        assert_eq!(composer.text(), "ac\nsecond");
        composer.move_line_up();
        assert_eq!(composer.cursor(), 2);
        composer.move_line_down();
        assert_eq!(composer.text(), "ac\nsecond");

        composer.replace_from_editor("first\nsecond");
        assert!(composer.is_multiline());
        assert_eq!(composer.take(), "first\nsecond");
        assert!(composer.text().is_empty());
    }

    #[test]
    fn home_and_end_follow_the_current_logical_line() {
        let mut composer = Composer::new();
        composer.replace_from_editor("first\nsecond");
        composer.move_line_up();
        composer.home();
        assert_eq!(composer.cursor(), 0);
        composer.move_line_down();
        composer.home();
        assert_eq!(composer.cursor(), 6);
        composer.end();
        assert_eq!(composer.cursor(), composer.text().len());
    }

    #[test]
    fn scalar_cursor_edits_preserve_utf8_boundaries() {
        let mut composer = Composer::new();
        composer.insert_str("aé界").expect("one line is valid");
        composer.move_left();
        composer.backspace();
        assert_eq!(composer.text(), "a界");
        assert!(composer.text().is_char_boundary(composer.cursor()));
        composer.delete();
        assert_eq!(composer.text(), "a");
    }

    #[test]
    fn cursor_treats_combining_marks_as_part_of_the_previous_cluster() {
        let mut composer = Composer::new();
        composer.insert_str("e\u{301}x").expect("one line is valid");
        composer.move_left();
        composer.move_left();
        assert_eq!(composer.cursor(), 0);
        composer.move_right();
        assert_eq!(composer.cursor(), 3);
        composer.backspace();
        assert_eq!(composer.text(), "x");
    }

    #[test]
    fn visual_line_movement_follows_soft_wraps_and_wide_cells() {
        let mut composer = Composer::new();
        composer.replace_from_editor("a界bc");
        composer.end();
        assert!(composer.move_visual_line_up(5));
        assert_eq!(composer.cursor(), 1);
        assert!(composer.move_visual_line_down(5));
        assert_eq!(composer.cursor(), 5);
        assert!(!composer.move_visual_line_down(5));
    }
}
