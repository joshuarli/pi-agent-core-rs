//! Crossterm ownership and terminal-mode restoration.

use crate::grid::{FrameDiff, Style};
use crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event};
use crossterm::execute;
use crossterm::queue;
use crossterm::style::{
    Attribute, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
};
use crossterm::terminal::{self, Clear, ClearType};
use crossterm::{cursor, terminal::EnterAlternateScreen, terminal::LeaveAlternateScreen};
use std::fmt;
use std::io::{self, stdout, Stdout, Write};
use std::time::Duration;

/// Errors from terminal setup, input, output, or restoration.
#[derive(Debug)]
pub enum TerminalError {
    /// The underlying terminal operation failed.
    Io(io::Error),
    /// A suspended guard was asked to resume after it was already active.
    AlreadyActive,
    /// A suspended guard was asked to resume after it had been permanently restored.
    Inactive,
}

impl fmt::Display for TerminalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "terminal I/O failed: {error}"),
            Self::AlreadyActive => formatter.write_str("terminal is already active"),
            Self::Inactive => formatter.write_str("terminal guard is inactive"),
        }
    }
}

impl std::error::Error for TerminalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::AlreadyActive | Self::Inactive => None,
        }
    }
}

impl From<io::Error> for TerminalError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// RAII owner of raw mode, the alternate screen, cursor visibility, and bracketed paste.
pub struct TerminalGuard {
    output: Stdout,
    active: bool,
    cursor_position: Option<(u16, u16)>,
}

impl fmt::Debug for TerminalGuard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalGuard")
            .field("active", &self.active)
            .finish_non_exhaustive()
    }
}

impl TerminalGuard {
    /// Enter the terminal modes owned by the application.
    pub fn enter() -> Result<Self, TerminalError> {
        let mut guard = Self {
            output: stdout(),
            active: false,
            cursor_position: None,
        };
        guard.activate()?;
        Ok(guard)
    }

    fn activate(&mut self) -> Result<(), TerminalError> {
        terminal::enable_raw_mode()?;
        if let Err(error) = execute!(
            self.output,
            EnterAlternateScreen,
            cursor::Hide,
            EnableBracketedPaste
        ) {
            let _ = terminal::disable_raw_mode();
            return Err(TerminalError::Io(error));
        }
        self.active = true;
        self.cursor_position = None;
        Ok(())
    }

    /// Restore all owned terminal modes.  Dropping the guard performs the same best-effort action.
    pub fn restore(&mut self) -> Result<(), TerminalError> {
        if !self.active {
            return Ok(());
        }
        let command_result = execute!(
            self.output,
            DisableBracketedPaste,
            cursor::Show,
            LeaveAlternateScreen
        );
        let raw_result = terminal::disable_raw_mode();
        self.active = false;
        self.cursor_position = None;
        command_result.map_err(TerminalError::Io)?;
        raw_result.map_err(TerminalError::Io)
    }

    /// Temporarily restore the user's normal terminal for an external program.
    pub fn suspend(&mut self) -> Result<(), TerminalError> {
        self.restore()
    }

    /// Re-enter the exact modes suspended by [`Self::suspend`].
    pub fn resume(&mut self) -> Result<(), TerminalError> {
        if self.active {
            return Err(TerminalError::AlreadyActive);
        }
        self.activate()
    }

    /// Whether this guard currently owns terminal modes.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Poll for a synchronous Crossterm input event.
    pub fn poll_event(&self, timeout: Duration) -> Result<Option<Event>, TerminalError> {
        if !self.active {
            return Err(TerminalError::Inactive);
        }
        if event::poll(timeout)? {
            Ok(Some(event::read()?))
        } else {
            Ok(None)
        }
    }

    /// Return the currently available terminal dimensions.
    pub fn size(&self) -> Result<(u16, u16), TerminalError> {
        terminal::size().map_err(TerminalError::Io)
    }

    /// Flush changed cells and leave the native cursor at the local composer cursor.
    pub fn draw(
        &mut self,
        diff: &FrameDiff,
        cursor_position: Option<(u16, u16)>,
    ) -> Result<(), TerminalError> {
        if !self.active {
            return Err(TerminalError::Inactive);
        }
        if diff.changes.is_empty() && self.cursor_position == cursor_position {
            return Ok(());
        }
        if diff.full_redraw {
            queue!(self.output, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
        }
        for change in &diff.changes {
            if diff.full_redraw && change.cell == crate::grid::Cell::blank() {
                continue;
            }
            queue!(self.output, cursor::MoveTo(change.x, change.y))?;
            apply_style(&mut self.output, change.cell.style)?;
            queue!(self.output, Print(change.cell.symbol))?;
        }
        queue!(self.output, ResetColor, SetAttribute(Attribute::Reset))?;
        if let Some((x, y)) = cursor_position {
            queue!(self.output, cursor::MoveTo(x, y), cursor::Show)?;
        } else {
            queue!(self.output, cursor::Hide)?;
        }
        self.flush()?;
        self.cursor_position = cursor_position;
        Ok(())
    }

    /// Flush output owned by the guard.
    pub fn flush(&mut self) -> Result<(), TerminalError> {
        self.output.flush().map_err(TerminalError::Io)
    }
}

fn apply_style(output: &mut Stdout, style: Style) -> Result<(), io::Error> {
    queue!(output, ResetColor, SetAttribute(Attribute::Reset))?;
    if let Some(foreground) = style.foreground {
        queue!(output, SetForegroundColor(foreground))?;
    }
    if let Some(background) = style.background {
        queue!(output, SetBackgroundColor(background))?;
    }
    if style.bold {
        queue!(output, SetAttribute(Attribute::Bold))?;
    }
    Ok(())
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}
