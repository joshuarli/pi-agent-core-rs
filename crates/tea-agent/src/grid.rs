//! Small cell grid used by the future renderer.

use std::fmt;

/// ANSI terminal colors used by the renderer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Color {
    Black,
    DarkGrey,
    Red,
    DarkRed,
    Green,
    DarkGreen,
    Yellow,
    DarkYellow,
    Blue,
    DarkBlue,
    Magenta,
    DarkMagenta,
    Cyan,
    DarkCyan,
    White,
    Grey,
}

/// Deliberately small cell style; terminal policy stays in the TUI.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Style {
    /// Optional foreground color.
    pub foreground: Option<Color>,
    /// Optional background color.
    pub background: Option<Color>,
    /// Whether the cell should be bold.
    pub bold: bool,
}

/// One printable scalar and its style.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cell {
    /// The scalar painted at this position.
    pub symbol: char,
    /// The style applied to the scalar.
    pub style: Style,
}

impl Cell {
    /// Construct a blank cell.
    pub const fn blank() -> Self {
        Self {
            symbol: ' ',
            style: Style {
                foreground: None,
                background: None,
                bold: false,
            },
        }
    }
}

impl Default for Cell {
    fn default() -> Self {
        Self::blank()
    }
}

/// A fixed terminal rectangle.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Rect {
    /// Left column.
    pub x: u16,
    /// Top row.
    pub y: u16,
    /// Width in terminal columns.
    pub width: u16,
    /// Height in terminal rows.
    pub height: u16,
}

/// A changed cell in a frame diff.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CellChange {
    /// Column of the changed cell.
    pub x: u16,
    /// Row of the changed cell.
    pub y: u16,
    /// New cell value.
    pub cell: Cell,
}

/// A current-versus-previous frame difference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameDiff {
    /// Whether every current cell must be repainted.
    pub full_redraw: bool,
    /// Changed cells in row-major order.
    pub changes: Vec<CellChange>,
}

/// Errors from addressing a grid cell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GridError {
    /// Requested column.
    pub x: u16,
    /// Requested row.
    pub y: u16,
    /// Grid width.
    pub width: u16,
    /// Grid height.
    pub height: u16,
}

impl fmt::Display for GridError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "grid coordinate ({}, {}) is outside {}x{}",
            self.x, self.y, self.width, self.height
        )
    }
}

impl std::error::Error for GridError {}

/// Fixed-size cell storage for one rendered frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Grid {
    width: u16,
    height: u16,
    cells: Vec<Cell>,
}

impl Grid {
    /// Create a blank frame.
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            cells: vec![Cell::blank(); width as usize * height as usize],
        }
    }

    /// Grid width in columns.
    pub const fn width(&self) -> u16 {
        self.width
    }

    /// Grid height in rows.
    pub const fn height(&self) -> u16 {
        self.height
    }

    /// Resize and clear the frame.
    pub fn resize(&mut self, width: u16, height: u16) {
        self.width = width;
        self.height = height;
        self.cells = vec![Cell::blank(); width as usize * height as usize];
    }

    /// Clear all cells to blanks.
    pub fn clear(&mut self) {
        self.cells.fill(Cell::blank());
    }

    /// Read one cell, returning `None` outside the frame.
    pub fn get(&self, x: u16, y: u16) -> Option<Cell> {
        self.index(x, y).map(|index| self.cells[index])
    }

    /// Set one cell.
    pub fn set(&mut self, x: u16, y: u16, cell: Cell) -> Result<(), GridError> {
        let index = self.index(x, y).ok_or(GridError {
            x,
            y,
            width: self.width,
            height: self.height,
        })?;
        self.cells[index] = cell;
        Ok(())
    }

    /// Compare this frame against a previous frame.
    pub fn diff(&self, previous: Option<&Grid>) -> FrameDiff {
        let dimensions_changed = previous
            .map(|frame| frame.width != self.width || frame.height != self.height)
            .unwrap_or(true);
        let mut changes = Vec::new();
        for y in 0..self.height {
            for x in 0..self.width {
                let cell = self.get(x, y).expect("coordinates are generated in bounds");
                if dimensions_changed || previous.and_then(|frame| frame.get(x, y)) != Some(cell) {
                    changes.push(CellChange { x, y, cell });
                }
            }
        }
        FrameDiff {
            full_redraw: dimensions_changed,
            changes,
        }
    }

    fn index(&self, x: u16, y: u16) -> Option<usize> {
        (x < self.width && y < self.height).then_some(y as usize * self.width as usize + x as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diff_is_empty_for_equal_frames_and_tracks_style_changes() {
        let mut before = Grid::new(2, 1);
        let mut after = before.clone();
        assert!(after.diff(Some(&before)).changes.is_empty());

        after
            .set(
                1,
                0,
                Cell {
                    symbol: 'x',
                    style: Style {
                        foreground: Some(Color::Blue),
                        ..Style::default()
                    },
                },
            )
            .expect("in bounds");
        let diff = after.diff(Some(&before));
        assert!(!diff.full_redraw);
        assert_eq!(diff.changes.len(), 1);

        before.resize(1, 1);
        assert!(after.diff(Some(&before)).full_redraw);
    }
}
