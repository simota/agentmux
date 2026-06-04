//! Pane and view rendering.

use agentmux_terminal::{CellStyle, CellWidth, ScreenGrid, TerminalColor};
use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

use crate::state::TuiSessionState;

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

        self.render_grid(inner, grid, buffer);
    }

    pub fn render_grid(&self, area: Rect, grid: &ScreenGrid, buffer: &mut Buffer) {
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
                }
            }
        }

        let cursor = grid.cursor();
        if cursor.visible && cursor.row < rows && cursor.col < cols {
            if let Some(cell) = buffer.cell_mut((area.x + cursor.col, area.y + cursor.row)) {
                cell.set_style(Style::default().add_modifier(Modifier::REVERSED));
            }
        }
    }
}

/// Renders all daemon-backed panes from client-side TUI state.
#[derive(Clone, Debug, Default)]
pub struct TuiSessionRenderer {
    pane_renderer: AgentPaneRenderer,
}

impl TuiSessionRenderer {
    pub fn render(&self, area: Rect, state: &TuiSessionState, buffer: &mut Buffer) {
        for (pane_id, rect) in state.layout().pane_rects(area) {
            let Some(pane) = state.pane(&pane_id) else {
                continue;
            };
            let chrome = PaneChrome::new(pane.chrome_title())
                .focused(state.layout().focused() == Some(pane.agent_id()));
            self.pane_renderer
                .render(rect, pane.grid(), &chrome, buffer);
        }

        if state.keybinding_help_visible() {
            render_keybinding_help(area, buffer);
        }

        if state.session_list_visible() {
            render_session_list(area, state, buffer);
        }
    }
}

const KEYBINDING_HELP_LINES: &[&str] = &[
    "Prefix: Ctrl-g",
    "",
    "Ctrl-g ?      Toggle this help",
    "Ctrl-g d      Detach session",
    "Ctrl-g q      Quit session",
    "Ctrl-g s      List running sessions",
    "Ctrl-g x      Close focused pane",
    "Ctrl-g z      Toggle pane zoom",
    "Ctrl-g arrows Move focus",
    "Ctrl-g %      Split vertical",
    "Ctrl-g \"      Split horizontal",
    "Ctrl-g Space  Rotate split direction",
    "Ctrl-g :      Command palette",
];

fn render_session_list(area: Rect, state: &TuiSessionState, buffer: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let mut lines = vec![
        "Use Up/Down or j/k, Enter to focus, Esc to close".to_string(),
        "".to_string(),
        "  ID NAME PID".to_string(),
    ];
    for (index, pane) in state
        .panes()
        .filter(|pane| pane.process_id().is_some())
        .enumerate()
    {
        let pid = pane
            .process_id()
            .map(|pid| pid.to_string())
            .unwrap_or_else(|| "-".to_string());
        let marker = if index == state.session_list_selected_index() {
            ">"
        } else {
            " "
        };
        lines.push(format!(
            "{marker} {} {} {}",
            pane.agent_id(),
            pane.name(),
            pid
        ));
    }

    if lines.len() == 3 {
        lines.push("no running sessions".to_string());
    }

    let height = u16::try_from(lines.len() + 2).unwrap_or(u16::MAX).min(18);
    let popup = centered_rect(area, 70, height);
    Clear.render(popup, buffer);
    let paragraph = Paragraph::new(lines.join("\n"))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Running Sessions")
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .alignment(Alignment::Left)
        .style(Style::default().fg(Color::White).bg(Color::Black));
    paragraph.render(popup, buffer);
}

fn render_keybinding_help(area: Rect, buffer: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let popup = centered_rect(area, 46, 15);
    Clear.render(popup, buffer);
    let paragraph = Paragraph::new(KEYBINDING_HELP_LINES.join("\n"))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Key Bindings")
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .alignment(Alignment::Left)
        .style(Style::default().fg(Color::White).bg(Color::Black));
    paragraph.render(popup, buffer);
}

fn centered_rect(area: Rect, max_width: u16, max_height: u16) -> Rect {
    let width = area.width.min(max_width);
    let height = area.height.min(max_height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

pub fn to_ratatui_style(style: &CellStyle) -> Style {
    let mut out = Style::default();

    if let Some(fg) = style.fg {
        out = out.fg(to_ratatui_color(fg));
    }
    if let Some(bg) = style.bg {
        out = out.bg(to_ratatui_color(bg));
    }

    let mut modifiers = Modifier::empty();
    if style.bold {
        modifiers |= Modifier::BOLD;
    }
    if style.italic {
        modifiers |= Modifier::ITALIC;
    }
    if style.underline {
        modifiers |= Modifier::UNDERLINED;
    }
    if style.reverse {
        modifiers |= Modifier::REVERSED;
    }
    if style.dim {
        modifiers |= Modifier::DIM;
    }

    out.add_modifier(modifiers)
}

pub fn to_ratatui_color(color: TerminalColor) -> Color {
    match color {
        TerminalColor::Indexed(index) => Color::Indexed(index),
        TerminalColor::Rgb { r, g, b } => Color::Rgb(r, g, b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentmux_terminal::{CellStyle, TerminalColor};
    use serde_json::json;

    use crate::state::TuiSessionState;

    #[test]
    fn render_grid_copies_characters_and_styles() {
        let mut grid = ScreenGrid::new(2, 4);
        let style = CellStyle {
            fg: Some(TerminalColor::Indexed(2)),
            bg: Some(TerminalColor::Rgb { r: 1, g: 2, b: 3 }),
            bold: true,
            ..CellStyle::default()
        };
        grid.write_char('A', style);
        grid.write_char('B', CellStyle::default());

        let mut buffer = Buffer::empty(Rect::new(0, 0, 4, 2));
        AgentPaneRenderer.render_grid(Rect::new(0, 0, 4, 2), &grid, &mut buffer);

        let first = buffer.cell((0, 0)).expect("first cell");
        assert_eq!(first.symbol(), "A");
        assert_eq!(first.fg, Color::Indexed(2));
        assert_eq!(first.bg, Color::Rgb(1, 2, 3));
        assert!(first.modifier.contains(Modifier::BOLD));
        assert_eq!(buffer.cell((1, 0)).expect("second cell").symbol(), "B");
    }

    #[test]
    fn render_pane_draws_border_title_and_inner_grid() {
        let mut grid = ScreenGrid::new(1, 3);
        for ch in "abc".chars() {
            grid.write_char(ch, CellStyle::default());
        }

        let area = Rect::new(0, 0, 8, 3);
        let mut buffer = Buffer::empty(area);
        let chrome = PaneChrome::new("impl-codex | AwaitingInput").focused(true);

        AgentPaneRenderer.render(area, &grid, &chrome, &mut buffer);

        assert_eq!(buffer.cell((0, 0)).expect("top-left border").symbol(), "┌");
        assert_eq!(buffer.cell((1, 1)).expect("inner a").symbol(), "a");
        assert_eq!(buffer.cell((2, 1)).expect("inner b").symbol(), "b");
        assert_eq!(buffer.cell((3, 1)).expect("inner c").symbol(), "c");
        assert!(
            buffer
                .content()
                .iter()
                .any(|cell| cell.symbol() == "i" && cell.fg == Color::Cyan)
        );
    }

    #[test]
    fn render_session_splits_daemon_agent_panes_and_marks_focus() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "agents": [
                {"id": "left", "name": "planner", "status": "ready"},
                {"id": "right", "name": "impl"}
            ]
        }));
        assert!(state.layout_mut().focus("right"));

        let area = Rect::new(0, 0, 20, 5);
        let mut buffer = Buffer::empty(area);

        TuiSessionRenderer::default().render(area, &state, &mut buffer);

        assert_eq!(buffer.cell((0, 0)).expect("left border").symbol(), "┌");
        assert_eq!(
            buffer.cell((10, 0)).expect("right border starts").symbol(),
            "┌"
        );
        assert!(
            buffer
                .content()
                .iter()
                .any(|cell| cell.symbol() == "p" && cell.fg == Color::DarkGray)
        );
        assert!(
            buffer
                .content()
                .iter()
                .any(|cell| cell.symbol() == "i" && cell.fg == Color::Cyan)
        );
    }

    #[test]
    fn render_session_draws_keybinding_help_overlay_when_visible() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "agents": [{"id": "shell", "name": "shell"}]
        }));
        state.apply_command(crate::keymap::TuiCommand::Help);

        let area = Rect::new(0, 0, 60, 20);
        let mut buffer = Buffer::empty(area);

        TuiSessionRenderer::default().render(area, &state, &mut buffer);

        let rendered = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Key Bindings"));
        assert!(rendered.contains("Ctrl-g ?"));
        assert!(rendered.contains("Toggle this help"));
    }

    #[test]
    fn render_session_draws_session_list_overlay_when_visible() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "agents": [
                {
                    "id": "agent_live",
                    "name": "shell",
                    "process_id": 1234
                },
                {
                    "id": "agent_restored",
                    "name": "restored",
                    "process_id": null
                }
            ]
        }));
        state.apply_command(crate::keymap::TuiCommand::ShowSessionList);

        let area = Rect::new(0, 0, 80, 20);
        let mut buffer = Buffer::empty(area);

        TuiSessionRenderer::default().render(area, &state, &mut buffer);

        let rendered = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Running Sessions"));
        assert!(rendered.contains("Enter to focus"));
        assert!(rendered.contains("> agent_live"));
        assert!(rendered.contains("agent_live"));
        assert!(rendered.contains("shell"));
        assert!(rendered.contains("1234"));
        assert!(!rendered.contains("agent_restored"));
    }

    #[test]
    fn visible_cursor_is_rendered_as_reversed_cell() {
        let mut grid = ScreenGrid::new(1, 3);
        grid.write_char('x', CellStyle::default());

        let mut buffer = Buffer::empty(Rect::new(0, 0, 3, 1));
        AgentPaneRenderer.render_grid(Rect::new(0, 0, 3, 1), &grid, &mut buffer);

        assert!(
            buffer
                .cell((1, 0))
                .expect("cursor cell")
                .modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn terminal_style_conversion_maps_flags_and_colors() {
        let style = CellStyle {
            fg: Some(TerminalColor::Rgb { r: 9, g: 8, b: 7 }),
            bg: Some(TerminalColor::Indexed(12)),
            italic: true,
            underline: true,
            reverse: true,
            dim: true,
            ..CellStyle::default()
        };

        let converted = to_ratatui_style(&style);

        assert_eq!(converted.fg, Some(Color::Rgb(9, 8, 7)));
        assert_eq!(converted.bg, Some(Color::Indexed(12)));
        assert!(converted.add_modifier.contains(Modifier::ITALIC));
        assert!(converted.add_modifier.contains(Modifier::UNDERLINED));
        assert!(converted.add_modifier.contains(Modifier::REVERSED));
        assert!(converted.add_modifier.contains(Modifier::DIM));
    }
}
