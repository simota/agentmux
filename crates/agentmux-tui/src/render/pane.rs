//! Agent pane chrome and grid rendering.

use agentmux_terminal::{Cell, CellWidth, ScreenGrid};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
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
}

impl PaneChrome {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            focused: false,
        }
    }

    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }
}

/// Renders a terminal `ScreenGrid` into a ratatui `Buffer`.
#[derive(Clone, Debug, Default)]
pub struct AgentPaneRenderer;

impl AgentPaneRenderer {
    pub fn render(&self, area: Rect, grid: &ScreenGrid, chrome: &PaneChrome, buffer: &mut Buffer) {
        self.render_scrolled(area, grid, 0, chrome, buffer);
    }

    pub fn render_scrolled(
        &self,
        area: Rect,
        grid: &ScreenGrid,
        scroll_offset: usize,
        chrome: &PaneChrome,
        buffer: &mut Buffer,
    ) {
        self.render_scrolled_with_selection(area, grid, scroll_offset, chrome, None, buffer);
    }

    pub fn render_scrolled_with_selection(
        &self,
        area: Rect,
        grid: &ScreenGrid,
        scroll_offset: usize,
        chrome: &PaneChrome,
        selection: Option<&CopySelection>,
        buffer: &mut Buffer,
    ) {
        if area.width == 0 || area.height == 0 {
            return;
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

        self.render_grid_scrolled_with_selection(inner, grid, scroll_offset, selection, buffer);
    }

    pub fn render_grid(&self, area: Rect, grid: &ScreenGrid, buffer: &mut Buffer) {
        self.render_grid_scrolled(area, grid, 0, buffer);
    }

    pub fn render_grid_scrolled(
        &self,
        area: Rect,
        grid: &ScreenGrid,
        scroll_offset: usize,
        buffer: &mut Buffer,
    ) {
        self.render_grid_scrolled_with_selection(area, grid, scroll_offset, None, buffer);
    }

    pub fn render_grid_scrolled_with_selection(
        &self,
        area: Rect,
        grid: &ScreenGrid,
        scroll_offset: usize,
        selection: Option<&CopySelection>,
        buffer: &mut Buffer,
    ) {
        if scroll_offset == 0 {
            render_current_grid(area, grid, selection, buffer);
            return;
        }

        if area.width == 0 || area.height == 0 {
            return;
        }

        let cols = area.width.min(grid.cols());
        let total_rows = grid.scrollback().len() + usize::from(grid.rows());
        let rows = usize::from(area.height).min(total_rows);
        let start = total_rows
            .saturating_sub(rows)
            .saturating_sub(scroll_offset.min(total_rows.saturating_sub(rows)));

        for row in 0..rows {
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
                && let Some(cell) =
                    buffer.cell_mut((area.x + cursor.col, area.y + cursor_screen_row))
            {
                cell.set_style(Style::default().add_modifier(Modifier::REVERSED));
            }
        }
    }
}

fn render_current_grid(
    area: Rect,
    grid: &ScreenGrid,
    selection: Option<&CopySelection>,
    buffer: &mut Buffer,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let rows = area.height.min(grid.rows());
    let cols = area.width.min(grid.cols());

    for row in 0..rows {
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
    }
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
