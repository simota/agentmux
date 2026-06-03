use std::collections::VecDeque;

/// One styled terminal cell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cell {
    pub ch: char,
    pub style: CellStyle,
    pub width: CellWidth,
}

impl Cell {
    pub fn blank() -> Self {
        Self {
            ch: ' ',
            style: CellStyle::default(),
            width: CellWidth::Narrow,
        }
    }
}

impl Default for Cell {
    fn default() -> Self {
        Self::blank()
    }
}

/// Display style carried by a terminal cell.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CellStyle {
    pub fg: Option<TerminalColor>,
    pub bg: Option<TerminalColor>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
    pub dim: bool,
}

/// Terminal color value used by SGR attributes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalColor {
    Indexed(u8),
    Rgb { r: u8, g: u8, b: u8 },
}

/// Width marker for rendering code points into terminal cells.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CellWidth {
    Narrow,
    Wide,
    WideContinuation,
}

/// Cursor position and visibility in zero-based grid coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CursorState {
    pub row: u16,
    pub col: u16,
    pub visible: bool,
}

impl Default for CursorState {
    fn default() -> Self {
        Self {
            row: 0,
            col: 0,
            visible: true,
        }
    }
}

/// Inclusive-exclusive dirty rectangle in grid coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirtyRegion {
    pub row: u16,
    pub col: u16,
    pub rows: u16,
    pub cols: u16,
}

impl DirtyRegion {
    fn full(rows: u16, cols: u16) -> Option<Self> {
        if rows == 0 || cols == 0 {
            None
        } else {
            Some(Self {
                row: 0,
                col: 0,
                rows,
                cols,
            })
        }
    }
}

/// A single logical row captured for scrollback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Line {
    cells: Vec<Cell>,
}

impl Line {
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    pub fn text(&self) -> String {
        self.cells.iter().map(|cell| cell.ch).collect()
    }
}

/// A 2-D terminal screen grid with bounded scrollback and dirty tracking.
///
/// Row-major layout: `cells[row][col]`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScreenGrid {
    rows: u16,
    cols: u16,
    cells: Vec<Cell>,
    cursor: CursorState,
    scrollback: VecDeque<Line>,
    max_scrollback_lines: usize,
    dirty_regions: Vec<DirtyRegion>,
}

impl ScreenGrid {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self::with_scrollback(rows, cols, 1_000)
    }

    pub fn with_scrollback(rows: u16, cols: u16, max_scrollback_lines: usize) -> Self {
        let len = usize::from(rows) * usize::from(cols);
        let mut grid = Self {
            rows,
            cols,
            cells: vec![Cell::blank(); len],
            cursor: CursorState::default(),
            scrollback: VecDeque::new(),
            max_scrollback_lines,
            dirty_regions: Vec::new(),
        };
        grid.mark_full_dirty();
        grid
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn cursor(&self) -> CursorState {
        self.cursor
    }

    pub fn scrollback(&self) -> &VecDeque<Line> {
        &self.scrollback
    }

    pub fn dirty_regions(&self) -> &[DirtyRegion] {
        &self.dirty_regions
    }

    pub fn clear_dirty(&mut self) {
        self.dirty_regions.clear();
    }

    pub fn cell(&self, row: u16, col: u16) -> Option<&Cell> {
        self.index(row, col).map(|index| &self.cells[index])
    }

    pub fn line_text(&self, row: u16) -> Option<String> {
        if row >= self.rows {
            return None;
        }

        Some(
            (0..self.cols)
                .filter_map(|col| self.cell(row, col))
                .map(|cell| cell.ch)
                .collect(),
        )
    }

    pub fn set_cursor(&mut self, row: u16, col: u16) {
        self.cursor.row = row.min(self.rows.saturating_sub(1));
        self.cursor.col = col.min(self.cols.saturating_sub(1));
    }

    pub fn move_cursor(&mut self, row_delta: i16, col_delta: i16) {
        let row = self
            .cursor
            .row
            .saturating_add_signed(row_delta)
            .min(self.rows.saturating_sub(1));
        let col = self
            .cursor
            .col
            .saturating_add_signed(col_delta)
            .min(self.cols.saturating_sub(1));
        self.set_cursor(row, col);
    }

    pub fn set_cursor_visible(&mut self, visible: bool) {
        self.cursor.visible = visible;
    }

    pub fn write_char(&mut self, ch: char, style: CellStyle) {
        if self.rows == 0 || self.cols == 0 {
            return;
        }

        if ch == '\n' {
            self.newline();
            return;
        }

        if ch == '\r' {
            self.cursor.col = 0;
            return;
        }

        if ch == '\x08' {
            self.cursor.col = self.cursor.col.saturating_sub(1);
            return;
        }

        if self.cursor.col >= self.cols {
            self.newline();
        }

        let row = self.cursor.row;
        let col = self.cursor.col;
        if let Some(index) = self.index(row, col) {
            self.cells[index] = Cell {
                ch,
                style,
                width: CellWidth::Narrow,
            };
            self.mark_dirty(row, col, 1, 1);
        }

        self.cursor.col += 1;
        if self.cursor.col >= self.cols {
            self.cursor.col = self.cols;
        }
    }

    pub fn clear_line(&mut self, row: u16) {
        if row >= self.rows {
            return;
        }

        for col in 0..self.cols {
            if let Some(index) = self.index(row, col) {
                self.cells[index] = Cell::blank();
            }
        }
        self.mark_dirty(row, 0, 1, self.cols);
    }

    pub fn clear_line_from_cursor(&mut self) {
        let row = self.cursor.row;
        if row >= self.rows {
            return;
        }

        for col in self.cursor.col..self.cols {
            if let Some(index) = self.index(row, col) {
                self.cells[index] = Cell::blank();
            }
        }
        self.mark_dirty(
            row,
            self.cursor.col,
            1,
            self.cols.saturating_sub(self.cursor.col),
        );
    }

    pub fn clear_line_to_cursor(&mut self) {
        let row = self.cursor.row;
        if row >= self.rows {
            return;
        }

        for col in 0..=self.cursor.col.min(self.cols.saturating_sub(1)) {
            if let Some(index) = self.index(row, col) {
                self.cells[index] = Cell::blank();
            }
        }
        self.mark_dirty(row, 0, 1, self.cursor.col.saturating_add(1).min(self.cols));
    }

    pub fn clear_screen(&mut self) {
        for cell in &mut self.cells {
            *cell = Cell::blank();
        }
        self.set_cursor(0, 0);
        self.mark_full_dirty();
    }

    pub fn clear_screen_from_cursor(&mut self) {
        if self.rows == 0 || self.cols == 0 {
            return;
        }

        for row in self.cursor.row..self.rows {
            let start_col = if row == self.cursor.row {
                self.cursor.col
            } else {
                0
            };
            for col in start_col..self.cols {
                if let Some(index) = self.index(row, col) {
                    self.cells[index] = Cell::blank();
                }
            }
        }
        self.mark_full_dirty();
    }

    pub fn clear_screen_to_cursor(&mut self) {
        if self.rows == 0 || self.cols == 0 {
            return;
        }

        for row in 0..=self.cursor.row.min(self.rows.saturating_sub(1)) {
            let end_col = if row == self.cursor.row {
                self.cursor.col.min(self.cols.saturating_sub(1))
            } else {
                self.cols.saturating_sub(1)
            };
            for col in 0..=end_col {
                if let Some(index) = self.index(row, col) {
                    self.cells[index] = Cell::blank();
                }
            }
        }
        self.mark_full_dirty();
    }

    pub fn scroll_up(&mut self, lines: u16) {
        if self.rows == 0 || self.cols == 0 {
            return;
        }

        let lines = lines.min(self.rows);
        for _ in 0..lines {
            self.push_scrollback_line(0);
            self.cells.drain(0..usize::from(self.cols));
            self.cells
                .extend(std::iter::repeat_with(Cell::blank).take(usize::from(self.cols)));
        }
        self.mark_full_dirty();
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        if self.rows == rows && self.cols == cols {
            return;
        }

        let mut resized = vec![Cell::blank(); usize::from(rows) * usize::from(cols)];
        let rows_to_copy = self.rows.min(rows);
        let cols_to_copy = self.cols.min(cols);

        for row in 0..rows_to_copy {
            for col in 0..cols_to_copy {
                let Some(old_index) = self.index(row, col) else {
                    continue;
                };
                let new_index = usize::from(row) * usize::from(cols) + usize::from(col);
                resized[new_index] = self.cells[old_index].clone();
            }
        }

        self.rows = rows;
        self.cols = cols;
        self.cells = resized;
        self.set_cursor(self.cursor.row, self.cursor.col);
        self.mark_full_dirty();
    }

    fn newline(&mut self) {
        self.cursor.col = 0;
        if self.cursor.row + 1 >= self.rows {
            self.scroll_up(1);
        } else {
            self.cursor.row += 1;
        }
    }

    fn push_scrollback_line(&mut self, row: u16) {
        let start = usize::from(row) * usize::from(self.cols);
        let end = start + usize::from(self.cols);
        self.scrollback.push_back(Line {
            cells: self.cells[start..end].to_vec(),
        });

        while self.scrollback.len() > self.max_scrollback_lines {
            self.scrollback.pop_front();
        }
    }

    fn index(&self, row: u16, col: u16) -> Option<usize> {
        if row < self.rows && col < self.cols {
            Some(usize::from(row) * usize::from(self.cols) + usize::from(col))
        } else {
            None
        }
    }

    fn mark_full_dirty(&mut self) {
        if let Some(region) = DirtyRegion::full(self.rows, self.cols) {
            self.dirty_regions.push(region);
        }
    }

    fn mark_dirty(&mut self, row: u16, col: u16, rows: u16, cols: u16) {
        if rows == 0 || cols == 0 {
            return;
        }

        self.dirty_regions.push(DirtyRegion {
            row,
            col,
            rows,
            cols,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_grid_starts_blank_with_cursor_at_origin() {
        let grid = ScreenGrid::new(2, 3);

        assert_eq!(grid.rows(), 2);
        assert_eq!(grid.cols(), 3);
        assert_eq!(grid.cursor(), CursorState::default());
        assert_eq!(grid.line_text(0).as_deref(), Some("   "));
        assert_eq!(
            grid.dirty_regions(),
            &[DirtyRegion {
                row: 0,
                col: 0,
                rows: 2,
                cols: 3,
            }]
        );
    }

    #[test]
    fn write_char_updates_cell_cursor_and_dirty_region() {
        let mut grid = ScreenGrid::new(2, 4);
        grid.clear_dirty();
        let style = CellStyle {
            bold: true,
            fg: Some(TerminalColor::Indexed(2)),
            ..CellStyle::default()
        };

        grid.write_char('A', style.clone());

        assert_eq!(grid.cell(0, 0).map(|cell| cell.ch), Some('A'));
        assert_eq!(grid.cell(0, 0).map(|cell| &cell.style), Some(&style));
        assert_eq!(grid.cursor().col, 1);
        assert_eq!(
            grid.dirty_regions(),
            &[DirtyRegion {
                row: 0,
                col: 0,
                rows: 1,
                cols: 1,
            }]
        );
    }

    #[test]
    fn newline_at_bottom_scrolls_into_bounded_scrollback() {
        let mut grid = ScreenGrid::with_scrollback(2, 3, 1);

        for ch in "abcdefghijkl".chars() {
            grid.write_char(ch, CellStyle::default());
        }

        assert_eq!(grid.scrollback().len(), 1);
        assert_eq!(grid.scrollback()[0].text(), "def");
        assert_eq!(grid.line_text(0).as_deref(), Some("ghi"));
        assert_eq!(grid.line_text(1).as_deref(), Some("jkl"));
    }

    #[test]
    fn resize_preserves_visible_cells_and_clamps_cursor() {
        let mut grid = ScreenGrid::new(2, 3);
        for ch in "abcd".chars() {
            grid.write_char(ch, CellStyle::default());
        }
        grid.set_cursor(1, 2);
        grid.clear_dirty();

        grid.resize(1, 2);

        assert_eq!(grid.rows(), 1);
        assert_eq!(grid.cols(), 2);
        assert_eq!(grid.line_text(0).as_deref(), Some("ab"));
        assert_eq!(
            grid.cursor(),
            CursorState {
                row: 0,
                col: 1,
                visible: true,
            }
        );
        assert_eq!(
            grid.dirty_regions(),
            &[DirtyRegion {
                row: 0,
                col: 0,
                rows: 1,
                cols: 2,
            }]
        );
    }

    #[test]
    fn clear_line_blanks_only_target_row() {
        let mut grid = ScreenGrid::new(2, 3);
        for ch in "abcxyz".chars() {
            grid.write_char(ch, CellStyle::default());
        }
        grid.clear_dirty();

        grid.clear_line(0);

        assert_eq!(grid.line_text(0).as_deref(), Some("   "));
        assert_eq!(grid.line_text(1).as_deref(), Some("xyz"));
        assert_eq!(
            grid.dirty_regions(),
            &[DirtyRegion {
                row: 0,
                col: 0,
                rows: 1,
                cols: 3,
            }]
        );
    }
}
