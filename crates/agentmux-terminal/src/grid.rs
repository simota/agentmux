//! Screen grid — stub.
//!
//! #TODO(agent): define Cell struct (character, style attributes)
//! #TODO(agent): implement resize, write_char, scroll_up

/// A 2-D grid of terminal cells.
///
/// Row-major layout: `cells[row][col]`.
pub struct ScreenGrid {
    pub rows: u16,
    pub cols: u16,
    // #TODO(agent): Vec<Vec<Cell>> once Cell is defined
}

impl ScreenGrid {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self { rows, cols }
    }
}
