//! Pane and view rendering.

use agentmux_terminal::{Cell, CellStyle, CellWidth, ScreenGrid, TerminalColor};
use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

use crate::state::{CopySelection, MessageListItem, TuiSessionState};

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

/// Renders all daemon-backed panes from client-side TUI state.
#[derive(Clone, Debug, Default)]
pub struct TuiSessionRenderer {
    pane_renderer: AgentPaneRenderer,
}

impl TuiSessionRenderer {
    pub fn render(&self, area: Rect, state: &TuiSessionState, buffer: &mut Buffer) {
        for (pane_id, rect) in state.layout().pane_rects(area) {
            if state.is_conversation_list_pane(&pane_id) {
                let focused = state.layout().focused() == Some(pane_id.as_str());
                render_message_list_panel(rect, state, "Conversation List", focused, buffer);
                continue;
            }

            let Some(pane) = state.pane(&pane_id) else {
                continue;
            };
            let chrome = PaneChrome::new(pane.chrome_title())
                .focused(state.layout().focused() == Some(pane.agent_id()));
            let selection = state
                .copy_selection()
                .filter(|selection| selection.agent_id == pane.agent_id());
            self.pane_renderer.render_scrolled_with_selection(
                rect,
                pane.grid(),
                pane.scroll_offset(),
                &chrome,
                selection,
                buffer,
            );
        }

        if state.keybinding_help_visible() {
            render_keybinding_help(area, buffer);
        }

        if state.session_list_visible() {
            render_session_list(area, state, buffer);
        }

        if state.provider_picker_visible() {
            render_provider_picker(area, state, buffer);
        }

        if state.message_bus_visible() {
            render_message_bus(area, state, buffer);
        }
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

const KEYBINDING_HELP_LINES: &[&str] = &[
    "Prefix: Ctrl-g",
    "",
    "Ctrl-g ?      Toggle this help",
    "Ctrl-g d      Detach session",
    "Ctrl-g q      Quit session",
    "Ctrl-g s      List running sessions",
    "Ctrl-g m      Message bus",
    "Ctrl-g x      Close focused pane",
    "Ctrl-g z      Toggle pane zoom",
    "Ctrl-g [      Copy/scroll focused pane",
    "Ctrl-g arrows Move focus",
    "Ctrl-g %      Split vertical + choose agent",
    "Ctrl-g \"      Split horizontal + choose agent",
    "Msg pane      Enter/Space/d details",
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
        "  ID NAME ROLE PID".to_string(),
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
        let role = pane.role().unwrap_or("-");
        let marker = if index == state.session_list_selected_index() {
            ">"
        } else {
            " "
        };
        lines.push(format!(
            "{marker} {} {} {} {}",
            pane.agent_id(),
            pane.name(),
            role,
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

fn render_provider_picker(area: Rect, state: &TuiSessionState, buffer: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let mut lines = vec![
        "Use Up/Down or j/k, Enter to start, Esc to close".to_string(),
        "".to_string(),
    ];
    for (index, option) in state.provider_options().iter().enumerate() {
        let marker = if index == state.provider_picker_selected_index() {
            ">"
        } else {
            " "
        };
        lines.push(format!(
            "{marker} {}  {}",
            option.choice.label(),
            option.hint
        ));
    }

    let height = u16::try_from(lines.len() + 2).unwrap_or(u16::MAX).min(12);
    let popup = centered_rect(area, 72, height);
    Clear.render(popup, buffer);
    let paragraph = Paragraph::new(lines.join("\n"))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("New Coding Agent")
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .alignment(Alignment::Left)
        .style(Style::default().fg(Color::White).bg(Color::Black));
    paragraph.render(popup, buffer);
}

fn render_message_bus(area: Rect, state: &TuiSessionState, buffer: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let max_height = area.height.saturating_sub(2).max(8);
    let height = u16::try_from(message_list_lines(state, area.width, true).len() + 2)
        .unwrap_or(u16::MAX)
        .min(max_height);
    let popup = centered_rect(area, area.width.saturating_sub(4).max(40), height);
    render_message_list_panel(popup, state, "Message Bus", false, buffer);
}

fn render_message_list_panel(
    area: Rect,
    state: &TuiSessionState,
    title: &'static str,
    focused: bool,
    buffer: &mut Buffer,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let overlay = title == "Message Bus";
    let lines = message_list_lines(state, area.width, overlay);
    let visible_lines = area.height.saturating_sub(2) as usize;
    let text = lines
        .into_iter()
        .take(visible_lines)
        .collect::<Vec<_>>()
        .join("\n");

    Clear.render(area, buffer);
    let border_style = if focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Cyan)
    };
    let paragraph = Paragraph::new(text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(message_list_title(title, state.message_details_visible()))
                .border_style(border_style),
        )
        .alignment(Alignment::Left)
        .style(Style::default().fg(Color::White).bg(Color::Black));
    paragraph.render(area, buffer);
}

fn message_list_title(title: &'static str, details_visible: bool) -> String {
    let mode = if details_visible {
        "details"
    } else {
        "compact"
    };
    format!("{title} [{mode}]")
}

fn message_list_lines(
    state: &TuiSessionState,
    area_width: u16,
    include_overlay_hint: bool,
) -> Vec<String> {
    let mut lines = Vec::new();
    let next_mode = if state.message_details_visible() {
        "compact"
    } else {
        "details"
    };
    if include_overlay_hint {
        lines.push(format!("Enter/Space/d {next_mode} | Esc/q close"));
    } else {
        lines.push(format!("Enter/Space/d {next_mode} | Ctrl-g x close"));
    }
    lines.push("".to_string());

    let content_width = usize::from(area_width.saturating_sub(4)).clamp(24, 120);

    for message in state.messages().iter() {
        if state.message_details_visible() {
            lines.extend(message_detail_lines(message, content_width));
        } else {
            lines.extend(message_compact_lines(message, content_width));
        }
    }

    if state.messages().is_empty() {
        lines.push("no messages".to_string());
    }

    lines
}

fn message_compact_lines(message: &MessageListItem, content_width: usize) -> Vec<String> {
    let meta = format!(
        "{} / {} / {} / {}",
        message.delivery_status,
        message.kind,
        message.message_id,
        compact_timestamp(&message.created_at)
    );
    let route = format!("{} -> {}", message.from, message.to);
    vec![
        truncate_cell(&meta, content_width),
        truncate_cell(&route, content_width),
        truncate_cell(&message.body, content_width),
        "".to_string(),
    ]
}

fn message_detail_lines(message: &MessageListItem, content_width: usize) -> Vec<String> {
    vec![
        truncate_cell(
            &format!("{} / {}", message.delivery_status, message.kind),
            content_width,
        ),
        truncate_cell(&format!("id: {}", message.message_id), content_width),
        truncate_cell(
            &format!("created: {}", compact_timestamp(&message.created_at)),
            content_width,
        ),
        truncate_cell(&format!("from: {}", message.from), content_width),
        truncate_cell(&format!("to: {}", message.to), content_width),
        truncate_cell(&format!("body: {}", message.body), content_width),
        "".to_string(),
    ]
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

fn compact_timestamp(value: &str) -> String {
    value
        .strip_suffix("+00:00")
        .unwrap_or(value)
        .replace('T', " ")
}

fn truncate_cell(value: &str, max_chars: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    let mut truncated = normalized
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    truncated.push('…');
    truncated
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
    use agentmux_terminal::{CellStyle, TerminalColor, TerminalParser};
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
    fn render_grid_respects_wide_character_continuation_cells() {
        let mut grid = ScreenGrid::new(1, 4);
        grid.write_char('変', CellStyle::default());
        grid.write_char('A', CellStyle::default());

        let mut buffer = Buffer::empty(Rect::new(0, 0, 4, 1));
        AgentPaneRenderer.render_grid(Rect::new(0, 0, 4, 1), &grid, &mut buffer);

        assert_eq!(buffer.cell((0, 0)).expect("wide head").symbol(), "変");
        assert_eq!(
            buffer.cell((1, 0)).expect("wide continuation").symbol(),
            " "
        );
        assert_eq!(buffer.cell((2, 0)).expect("next cell").symbol(), "A");
    }

    #[test]
    fn render_grid_scrolled_reads_from_scrollback_history() {
        let mut parser = TerminalParser::new(2, 4);
        parser.advance(b"1111\n2222\n3333\n");
        let grid = parser.grid();

        let mut buffer = Buffer::empty(Rect::new(0, 0, 4, 2));
        AgentPaneRenderer.render_grid_scrolled(Rect::new(0, 0, 4, 2), grid, 1, &mut buffer);

        let rendered = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("2222"));
        assert!(rendered.contains("3333"));
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
    fn render_session_draws_provider_picker_overlay_when_visible() {
        let mut state = TuiSessionState::default();
        state.open_provider_picker();

        let area = Rect::new(0, 0, 100, 20);
        let mut buffer = Buffer::empty(area);

        TuiSessionRenderer::default().render(area, &state, &mut buffer);

        let rendered = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("New Coding Agent"));
        assert!(rendered.contains("> Claude Code"));
        assert!(rendered.contains("Codex"));
        assert!(rendered.contains("Antigravity"));
        assert!(rendered.contains("Conversation List"));
    }

    #[test]
    fn render_session_draws_message_bus_overlay_when_visible() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "agents": [{"id": "shell", "name": "shell"}]
        }));
        state.apply_message_list_payload(&json!({
            "messages": [
                {
                    "message_id": "msg_001",
                    "created_at": "2026-06-04T02:00:00+00:00",
                    "delivery_status": "delivered",
                    "kind": "handoff",
                    "from": { "kind": "agent", "id": "planner" },
                    "to": { "kind": "agent", "id": "impl" },
                    "body": "please continue"
                }
            ]
        }));
        state.apply_command(crate::keymap::TuiCommand::ShowMessageBus);

        let area = Rect::new(0, 0, 160, 20);
        let mut buffer = Buffer::empty(area);

        TuiSessionRenderer::default().render(area, &state, &mut buffer);

        let rendered = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Message Bus"));
        assert!(rendered.contains("compact"));
        assert!(rendered.contains("msg_001"));
        assert!(rendered.contains("agent:planner"));
        assert!(rendered.contains("please continue"));
    }

    #[test]
    fn render_session_draws_conversation_list_pane() {
        let mut state = TuiSessionState::default();
        state.open_conversation_list_pane();
        state.apply_message_list_payload(&json!({
            "messages": [
                {
                    "message_id": "msg_002",
                    "created_at": "2026-06-04T02:00:00+00:00",
                    "delivery_status": "pending",
                    "kind": "handoff",
                    "from": { "kind": "agent", "id": "planner" },
                    "to": { "kind": "agent", "id": "impl" },
                    "body": "review this"
                }
            ]
        }));

        let area = Rect::new(0, 0, 160, 20);
        let mut buffer = Buffer::empty(area);

        TuiSessionRenderer::default().render(area, &state, &mut buffer);

        let rendered = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Conversation List"));
        assert!(rendered.contains("compact"));
        assert!(rendered.contains("msg_002"));
        assert!(rendered.contains("review this"));
    }

    #[test]
    fn render_session_draws_message_details_when_toggled() {
        let mut state = TuiSessionState::default();
        state.open_conversation_list_pane();
        state.apply_command(crate::keymap::TuiCommand::ToggleMessageDetails);
        state.apply_message_list_payload(&json!({
            "messages": [
                {
                    "message_id": "msg_003",
                    "created_at": "2026-06-04T02:00:00+00:00",
                    "delivery_status": "pending",
                    "kind": "handoff",
                    "from": { "kind": "agent", "id": "planner" },
                    "to": { "kind": "agent", "id": "impl" },
                    "body": "inspect carefully"
                }
            ]
        }));

        let area = Rect::new(0, 0, 120, 20);
        let mut buffer = Buffer::empty(area);

        TuiSessionRenderer::default().render(area, &state, &mut buffer);

        let rendered = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("details"));
        assert!(rendered.contains("id: msg_003"));
        assert!(rendered.contains("from: agent:planner"));
        assert!(rendered.contains("body: inspect carefully"));
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
