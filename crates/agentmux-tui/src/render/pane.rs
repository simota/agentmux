//! Agent pane chrome and grid rendering.

use agentmux_terminal::{Cell, CellWidth, ScreenGrid};
use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Widget},
};

use crate::state::CopySelection;

use super::util::to_ratatui_style;

/// Border/status metadata for an agent pane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaneChrome {
    pub title: String,
    pub focused: bool,
    /// When `true`, `AGENTMUX_RESULT:` marker blocks are blanked out during
    /// rendering. This is purely a display filter — the daemon still detects
    /// the marker from the raw PTY byte stream, so orchestration is unaffected.
    pub hide_result_marker: bool,
}

impl PaneChrome {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            focused: false,
            hide_result_marker: false,
        }
    }

    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    pub fn hide_result_marker(mut self, hide: bool) -> Self {
        self.hide_result_marker = hide;
        self
    }
}

/// Renders a terminal `ScreenGrid` into a ratatui `Buffer`.
#[derive(Clone, Debug, Default)]
pub struct AgentPaneRenderer;

impl AgentPaneRenderer {
    /// Renders the pane and returns the visible cursor's screen [`Position`] if
    /// the cursor is visible within the rendered area, or [`None`] otherwise.
    pub fn render(
        &self,
        area: Rect,
        grid: &ScreenGrid,
        chrome: &PaneChrome,
        buffer: &mut Buffer,
    ) -> Option<Position> {
        self.render_scrolled(area, grid, 0, chrome, buffer)
    }

    /// Renders the pane with scroll offset and returns the visible cursor
    /// screen [`Position`], or [`None`] if the cursor is not visible.
    pub fn render_scrolled(
        &self,
        area: Rect,
        grid: &ScreenGrid,
        scroll_offset: usize,
        chrome: &PaneChrome,
        buffer: &mut Buffer,
    ) -> Option<Position> {
        self.render_scrolled_with_selection(area, grid, scroll_offset, chrome, None, buffer)
    }

    /// Renders the pane with scroll offset and optional copy selection, and
    /// returns the visible cursor screen [`Position`], or [`None`] if the
    /// cursor is not visible.
    pub fn render_scrolled_with_selection(
        &self,
        area: Rect,
        grid: &ScreenGrid,
        scroll_offset: usize,
        chrome: &PaneChrome,
        selection: Option<&CopySelection>,
        buffer: &mut Buffer,
    ) -> Option<Position> {
        if area.width == 0 || area.height == 0 {
            return None;
        }

        let block_style = if chrome.focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(chrome.title.as_str())
            .border_style(block_style);
        let inner = block.inner(area);
        block.render(area, buffer);

        render_grid_scrolled_filtered(
            inner,
            grid,
            scroll_offset,
            selection,
            chrome.hide_result_marker,
            buffer,
        )
    }

    /// Renders only the grid (no border chrome) and returns the visible cursor
    /// screen [`Position`], or [`None`] if the cursor is not visible.
    pub fn render_grid(
        &self,
        area: Rect,
        grid: &ScreenGrid,
        buffer: &mut Buffer,
    ) -> Option<Position> {
        self.render_grid_scrolled(area, grid, 0, buffer)
    }

    /// Renders the grid with scroll offset and returns the visible cursor
    /// screen [`Position`], or [`None`] if the cursor is not visible.
    pub fn render_grid_scrolled(
        &self,
        area: Rect,
        grid: &ScreenGrid,
        scroll_offset: usize,
        buffer: &mut Buffer,
    ) -> Option<Position> {
        self.render_grid_scrolled_with_selection(area, grid, scroll_offset, None, buffer)
    }

    /// Renders the grid with scroll offset and optional copy selection, and
    /// returns the visible cursor screen [`Position`], or [`None`] if the
    /// cursor is not visible.
    pub fn render_grid_scrolled_with_selection(
        &self,
        area: Rect,
        grid: &ScreenGrid,
        scroll_offset: usize,
        selection: Option<&CopySelection>,
        buffer: &mut Buffer,
    ) -> Option<Position> {
        render_grid_scrolled_filtered(area, grid, scroll_offset, selection, false, buffer)
    }
}

/// Renders the grid (scrolled or current) with an optional copy selection and
/// `AGENTMUX_RESULT:` marker suppression, returning the visible cursor screen
/// [`Position`] or [`None`].
fn render_grid_scrolled_filtered(
    area: Rect,
    grid: &ScreenGrid,
    scroll_offset: usize,
    selection: Option<&CopySelection>,
    hide_result_marker: bool,
    buffer: &mut Buffer,
) -> Option<Position> {
    if scroll_offset == 0 {
        return render_current_grid(area, grid, selection, hide_result_marker, buffer);
    }

    if area.width == 0 || area.height == 0 {
        return None;
    }

    let cols = area.width.min(grid.cols());
    let total_rows = grid.scrollback().len() + usize::from(grid.rows());
    let rows = usize::from(area.height).min(total_rows);
    let start = total_rows
        .saturating_sub(rows)
        .saturating_sub(scroll_offset.min(total_rows.saturating_sub(rows)));

    let suppressed = if hide_result_marker {
        let lines: Vec<String> = (0..rows)
            .map(|row| history_row_text(grid, start + row, cols))
            .collect();
        result_marker_suppressed_rows(&lines)
    } else {
        vec![false; rows]
    };

    for row in 0..rows {
        if suppressed.get(row).copied().unwrap_or(false) {
            blank_row(area, row, cols, buffer);
            continue;
        }
        for col in 0..cols {
            let Some(cell) = history_cell(grid, start + row, col) else {
                continue;
            };

            let x = area.x + col;
            let y = area.y + u16::try_from(row).unwrap_or(u16::MAX);
            if let Some(target) = buffer.cell_mut((x, y)) {
                if cell.width == CellWidth::WideContinuation {
                    target.set_symbol(" ");
                } else {
                    target.set_char(cell.ch);
                }
                target.set_style(to_ratatui_style(&cell.style));
                if selection.is_some_and(|selection| selection.contains(row as u16, col)) {
                    target.set_style(target.style().add_modifier(Modifier::REVERSED));
                }
            }
        }
    }

    let cursor = grid.cursor();
    let cursor_history_row = grid.scrollback().len() + usize::from(cursor.row);
    if cursor.visible && cursor.col < cols {
        let cursor_screen_row = cursor_history_row
            .checked_sub(start)
            .filter(|row| *row < rows);
        if let Some(cursor_screen_row) = cursor_screen_row
            && let Ok(cursor_screen_row) = u16::try_from(cursor_screen_row)
            && let Some(cell) = buffer.cell_mut((area.x + cursor.col, area.y + cursor_screen_row))
        {
            cell.set_style(Style::default().add_modifier(Modifier::REVERSED));
            return Some(Position {
                x: area.x + cursor.col,
                y: area.y + cursor_screen_row,
            });
        }
    }
    None
}

/// Renders the current (non-scrolled) grid and returns the visible cursor
/// screen [`Position`], or [`None`] if the cursor is hidden or out of bounds.
fn render_current_grid(
    area: Rect,
    grid: &ScreenGrid,
    selection: Option<&CopySelection>,
    hide_result_marker: bool,
    buffer: &mut Buffer,
) -> Option<Position> {
    if area.width == 0 || area.height == 0 {
        return None;
    }

    let rows = area.height.min(grid.rows());
    let cols = area.width.min(grid.cols());

    let suppressed = if hide_result_marker {
        let lines: Vec<String> = (0..rows)
            .map(|row| grid_row_text(grid, row, cols))
            .collect();
        result_marker_suppressed_rows(&lines)
    } else {
        vec![false; usize::from(rows)]
    };

    for row in 0..rows {
        if suppressed.get(usize::from(row)).copied().unwrap_or(false) {
            blank_row(area, usize::from(row), cols, buffer);
            continue;
        }
        for col in 0..cols {
            let Some(cell) = grid.cell(row, col) else {
                continue;
            };

            let x = area.x + col;
            let y = area.y + row;
            if let Some(target) = buffer.cell_mut((x, y)) {
                if cell.width == CellWidth::WideContinuation {
                    target.set_symbol(" ");
                } else {
                    target.set_char(cell.ch);
                }
                target.set_style(to_ratatui_style(&cell.style));
                if selection.is_some_and(|selection| selection.contains(row, col)) {
                    target.set_style(target.style().add_modifier(Modifier::REVERSED));
                }
            }
        }
    }

    let cursor = grid.cursor();
    if cursor.visible
        && cursor.row < rows
        && cursor.col < cols
        && let Some(cell) = buffer.cell_mut((area.x + cursor.col, area.y + cursor.row))
    {
        cell.set_style(Style::default().add_modifier(Modifier::REVERSED));
        return Some(Position {
            x: area.x + cursor.col,
            y: area.y + cursor.row,
        });
    }
    None
}

fn history_cell(grid: &ScreenGrid, history_row: usize, col: u16) -> Option<&Cell> {
    let scrollback_rows = grid.scrollback().len();
    if history_row < scrollback_rows {
        return grid.scrollback()[history_row].cells().get(usize::from(col));
    }
    let grid_row = history_row.checked_sub(scrollback_rows)?;
    let grid_row = u16::try_from(grid_row).ok()?;
    grid.cell(grid_row, col)
}

/// Build the text of a current-grid row by concatenating cell chars across
/// `0..cols`, with trailing whitespace trimmed. Wide-continuation cells map to
/// a single space so column counting stays aligned with rendering.
fn grid_row_text(grid: &ScreenGrid, row: u16, cols: u16) -> String {
    let mut text = String::new();
    for col in 0..cols {
        match grid.cell(row, col) {
            Some(cell) if cell.width == CellWidth::WideContinuation => text.push(' '),
            Some(cell) => text.push(cell.ch),
            None => text.push(' '),
        }
    }
    text.trim_end().to_string()
}

/// Build the text of a history (scrollback or grid) row for scroll rendering,
/// trimming trailing whitespace.
fn history_row_text(grid: &ScreenGrid, history_row: usize, cols: u16) -> String {
    let mut text = String::new();
    for col in 0..cols {
        match history_cell(grid, history_row, col) {
            Some(cell) if cell.width == CellWidth::WideContinuation => text.push(' '),
            Some(cell) => text.push(cell.ch),
            None => text.push(' '),
        }
    }
    text.trim_end().to_string()
}

/// Blank a single rendered row (`0..cols`) to spaces with the default style,
/// preserving layout (the row is overwritten, not removed).
fn blank_row(area: Rect, row: usize, cols: u16, buffer: &mut Buffer) {
    let y = area.y + u16::try_from(row).unwrap_or(u16::MAX);
    for col in 0..cols {
        if let Some(target) = buffer.cell_mut((area.x + col, y)) {
            target.set_char(' ');
            target.set_style(Style::default());
        }
    }
}

/// The marker that prefixes an `AGENTMUX_RESULT` turn-status block.
const RESULT_MARKER_PREFIX: &str = "AGENTMUX_RESULT:";

/// Given the trailing-trimmed text of each visible row, return a parallel
/// `Vec<bool>` flagging which rows belong to an `AGENTMUX_RESULT:` block and
/// should be blanked during rendering.
///
/// Detection rules (v1):
/// - A row whose trimmed text starts with `AGENTMUX_RESULT:` opens a block and
///   is always suppressed.
/// - From the marker, the JSON body is followed by simple brace-depth counting:
///   text after `AGENTMUX_RESULT:` on the marker row, then subsequent rows, are
///   scanned for `{`/`}`. Every row scanned while depth `> 0` (and the row that
///   returns depth to `0`) is suppressed.
/// - Brace characters inside JSON string literals are NOT specially handled in
///   v1 (plain depth counting); this is an accepted simplification.
/// - If the marker row has no `{` and the next non-empty row does not begin a
///   brace, only the marker row is suppressed.
/// - If the block's braces never close within the visible rows (a JSON value
///   still streaming/dripping in), every row from the marker to the end of the
///   visible window is suppressed so partial/garbled JSON is never shown.
/// - Multiple blocks are each detected independently; non-marker ordinary rows
///   are never suppressed.
fn result_marker_suppressed_rows(lines: &[String]) -> Vec<bool> {
    let mut suppressed = vec![false; lines.len()];
    let mut index = 0;
    while index < lines.len() {
        let trimmed = lines[index].trim_start();
        if !trimmed.starts_with(RESULT_MARKER_PREFIX) {
            index += 1;
            continue;
        }

        // Suppress the marker row itself.
        suppressed[index] = true;

        // Begin brace-depth tracking from any JSON that trails the marker.
        let after_marker = &trimmed[RESULT_MARKER_PREFIX.len()..];
        let mut depth = brace_delta(after_marker);
        let mut seen_open = depth > 0;

        if depth > 0 {
            // JSON started on the marker row; scan following rows until it closes.
            let mut cursor = index + 1;
            while cursor < lines.len() && depth > 0 {
                suppressed[cursor] = true;
                depth += brace_delta(&lines[cursor]);
                cursor += 1;
            }
            index = cursor;
            continue;
        }

        // No JSON on the marker row: look for the block opening on a later row.
        let mut cursor = index + 1;
        // Skip blank rows between the marker and the JSON body.
        while cursor < lines.len() && lines[cursor].trim().is_empty() {
            cursor += 1;
        }

        if cursor < lines.len() && lines[cursor].trim_start().starts_with('{') {
            // Suppress the rows we skipped plus the JSON block.
            for row in suppressed.iter_mut().take(cursor).skip(index + 1) {
                *row = true;
            }
            while cursor < lines.len() {
                suppressed[cursor] = true;
                depth += brace_delta(&lines[cursor]);
                seen_open = seen_open || depth > 0;
                cursor += 1;
                if seen_open && depth <= 0 {
                    break;
                }
            }
            index = cursor;
            continue;
        }

        // Marker with no JSON body: only the marker row is suppressed.
        index += 1;
    }
    suppressed
}

/// Net brace depth change for a line: `count('{') - count('}')`. String-literal
/// escaping is intentionally ignored in v1 (see `result_marker_suppressed_rows`).
fn brace_delta(line: &str) -> i32 {
    line.chars().fold(0, |acc, ch| match ch {
        '{' => acc + 1,
        '}' => acc - 1,
        _ => acc,
    })
}

#[cfg(test)]
mod marker_tests {
    use super::*;
    use agentmux_terminal::CellStyle;

    fn lines(rows: &[&str]) -> Vec<String> {
        rows.iter().map(|row| row.to_string()).collect()
    }

    #[test]
    fn single_line_inline_block_suppresses_only_that_row() {
        let input = lines(&[
            "regular output",
            r#"AGENTMUX_RESULT: {"status":"completed"}"#,
            "more output",
        ]);
        assert_eq!(
            result_marker_suppressed_rows(&input),
            vec![false, true, false]
        );
    }

    #[test]
    fn multi_line_pretty_block_suppresses_full_block() {
        let input = lines(&[
            "before",
            "AGENTMUX_RESULT:",
            "{",
            r#"  "status": "completed","#,
            r#"  "summary": "did work""#,
            "}",
            "after",
        ]);
        assert_eq!(
            result_marker_suppressed_rows(&input),
            vec![false, true, true, true, true, true, false]
        );
    }

    #[test]
    fn unclosed_block_suppresses_through_end_of_window() {
        // JSON is still dripping in: braces never balance within the window.
        let input = lines(&[
            "before",
            "AGENTMUX_RESULT:",
            "{",
            r#"  "status": "completed","#,
        ]);
        assert_eq!(
            result_marker_suppressed_rows(&input),
            vec![false, true, true, true]
        );
    }

    #[test]
    fn unclosed_inline_block_suppresses_through_end_of_window() {
        let input = lines(&[
            "before",
            r#"AGENTMUX_RESULT: {"status":"#,
            r#"  "completed""#,
        ]);
        assert_eq!(
            result_marker_suppressed_rows(&input),
            vec![false, true, true]
        );
    }

    #[test]
    fn marker_substring_not_at_line_start_is_not_suppressed() {
        let input = lines(&[
            "log: emitted AGENTMUX_RESULT: now",
            "see AGENTMUX_RESULT below",
        ]);
        assert_eq!(result_marker_suppressed_rows(&input), vec![false, false]);
    }

    #[test]
    fn marker_with_no_json_body_suppresses_only_marker() {
        let input = lines(&["AGENTMUX_RESULT:", "next regular line", "another"]);
        assert_eq!(
            result_marker_suppressed_rows(&input),
            vec![true, false, false]
        );
    }

    #[test]
    fn multiple_blocks_each_detected() {
        let input = lines(&[
            "a",
            r#"AGENTMUX_RESULT: {"status":"ok"}"#,
            "b",
            "AGENTMUX_RESULT:",
            "{",
            "}",
            "c",
        ]);
        assert_eq!(
            result_marker_suppressed_rows(&input),
            vec![false, true, false, true, true, true, false]
        );
    }

    #[test]
    fn leading_indented_marker_is_detected() {
        let input = lines(&[r#"    AGENTMUX_RESULT: {"x":1}"#]);
        assert_eq!(result_marker_suppressed_rows(&input), vec![true]);
    }

    #[test]
    fn render_current_grid_blanks_marker_row_when_hidden() {
        let mut grid = ScreenGrid::new(3, 40);
        for ch in "hello".chars() {
            grid.write_char(ch, CellStyle::default());
        }
        grid.write_char('\n', CellStyle::default());
        for ch in r#"AGENTMUX_RESULT: {"status":"ok"}"#.chars() {
            grid.write_char(ch, CellStyle::default());
        }

        let area = Rect::new(0, 0, 40, 3);

        // With hiding on, the marker row is blanked but the normal row stays.
        let mut hidden = Buffer::empty(area);
        render_current_grid(area, &grid, None, true, &mut hidden);
        let row0: String = (0..40)
            .map(|c| hidden.cell((c, 0)).unwrap().symbol())
            .collect();
        let row1: String = (0..40)
            .map(|c| hidden.cell((c, 1)).unwrap().symbol())
            .collect();
        assert!(row0.starts_with("hello"));
        assert_eq!(row1.trim(), "");

        // With hiding off, the marker row renders verbatim.
        let mut shown = Buffer::empty(area);
        render_current_grid(area, &grid, None, false, &mut shown);
        let shown_row1: String = (0..40)
            .map(|c| shown.cell((c, 1)).unwrap().symbol())
            .collect();
        assert!(shown_row1.contains("AGENTMUX_RESULT:"));
    }
}
