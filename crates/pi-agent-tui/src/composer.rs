//! One-line local composer buffer and cursor operations.

use std::fmt;

/// Errors from the deliberately single-line composer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComposerError {
    /// Newline characters belong in the external editor, not this buffer.
    Newline,
}

impl fmt::Display for ComposerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Newline => {
                formatter.write_str("composer accepts one line; use the editor for multiline text")
            }
        }
    }
}

impl std::error::Error for ComposerError {}

/// UTF-8-safe one-line text buffer.
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

    /// Insert a string at the cursor.
    pub fn insert_str(&mut self, text: &str) -> Result<(), ComposerError> {
        if text.chars().any(|symbol| symbol == '\n' || symbol == '\r') {
            return Err(ComposerError::Newline);
        }
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
        Ok(())
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
        self.cursor = self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(index, _)| index)
            .unwrap_or(0);
    }

    /// Move one scalar to the right.
    pub fn move_right(&mut self) {
        self.cursor = self.text[self.cursor..]
            .char_indices()
            .nth(1)
            .map(|(index, _)| self.cursor + index)
            .unwrap_or(self.text.len());
    }

    /// Move to the beginning of the line.
    pub const fn home(&mut self) {
        self.cursor = 0;
    }

    /// Move to the end of the line.
    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }

    /// Delete the scalar before the cursor.
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(index, _)| index)
            .unwrap_or(0);
        self.text.drain(start..self.cursor);
        self.cursor = start;
    }

    /// Delete the scalar at the cursor.
    pub fn delete(&mut self) {
        let Some(end) = self.text[self.cursor..]
            .char_indices()
            .nth(1)
            .map(|(index, _)| self.cursor + index)
        else {
            return;
        };
        self.text.drain(self.cursor..end);
    }

    /// Replace the complete line and place the cursor at its end.
    pub fn replace(&mut self, text: impl Into<String>) -> Result<(), ComposerError> {
        let text = text.into();
        if text.chars().any(|symbol| symbol == '\n' || symbol == '\r') {
            return Err(ComposerError::Newline);
        }
        self.text = text;
        self.end();
        Ok(())
    }

    /// Take the current submission and clear the buffer.
    pub fn submit(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }
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

        composer.replace_from_editor("first\nsecond");
        assert!(composer.is_multiline());
        assert_eq!(composer.take(), "first\nsecond");
        assert!(composer.text().is_empty());
    }
}
