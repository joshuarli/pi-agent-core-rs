//! Bounded menu geometry shared by command and picker surfaces.

/// A menu item with a stable semantic identifier and display label.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MenuItem {
    pub id: String,
    pub label: String,
}

/// Clamp a selected row to a visible menu window.
pub fn visible_selection(selected: usize, item_count: usize, max_rows: usize) -> Option<usize> {
    if item_count == 0 || max_rows == 0 {
        None
    } else {
        Some(selected.min(item_count - 1).min(max_rows - 1))
    }
}
