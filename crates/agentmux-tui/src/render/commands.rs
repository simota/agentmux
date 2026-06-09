//! Commands (broadcast) panel rendering: a history log of sent broadcasts plus
//! an input field whose text is injected into every targeted PTY.

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

use crate::state::TuiSessionState;

use super::util::truncate_cell;

pub(crate) fn render_commands_panel(
    area: Rect,
    state: &TuiSessionState,
    focused: bool,
    buffer: &mut Buffer,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let content_width = usize::from(area.width.saturating_sub(4)).clamp(16, 120);
    let lines = commands_panel_lines(state, content_width, area.height);
    let text = lines.join("\n");

    Clear.render(area, buffer);
    let border_style = if focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Cyan)
    };
    let title = format!("Commands ({})", state.commands_target());
    let paragraph = Paragraph::new(text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(border_style),
        )
        .alignment(Alignment::Left)
        .style(Style::default().fg(Color::White).bg(Color::Black));
    paragraph.render(area, buffer);
}

/// Compose the panel body: hint, history log (oldest first), then the input
/// field pinned to the last visible row.
pub(crate) fn commands_panel_lines(
    state: &TuiSessionState,
    content_width: usize,
    area_height: u16,
) -> Vec<String> {
    // Interior rows available between the top/bottom borders.
    let interior_rows = usize::from(area_height.saturating_sub(2));
    if interior_rows == 0 {
        return Vec::new();
    }

    let hint = "Enter: send  Tab: target  Esc: clear  Ctrl-g x: close".to_string();
    let input_line = format!("> {}\u{2588}", state.commands_input_buffer());
    let input_line = truncate_cell(&input_line, content_width);

    // Reserve the last row for the input field and one row for the hint.
    let history_rows = interior_rows.saturating_sub(2);

    let mut history: Vec<String> = Vec::new();
    if state.commands_history().is_empty() {
        history.push("no commands sent yet".to_string());
    } else {
        for entry in state.commands_history() {
            let head = format!("[{}] {}", entry.target, entry.text);
            let outcome = format!(
                "  -> delivered {}, skipped {}",
                entry.delivered, entry.skipped
            );
            history.push(truncate_cell(&head, content_width));
            history.push(truncate_cell(&outcome, content_width));
        }
    }

    // Keep the newest history visible: drop the oldest lines when overflowing.
    if history.len() > history_rows {
        let overflow = history.len() - history_rows;
        history.drain(0..overflow);
    }

    let mut lines = Vec::with_capacity(interior_rows);
    lines.extend(history);
    // Pad so the hint and input field sit at the bottom of the pane.
    while lines.len() + 2 < interior_rows {
        lines.push(String::new());
    }
    lines.push(truncate_cell(&hint, content_width));
    lines.push(input_line);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_panel_shows_placeholder_and_input_field_at_bottom() {
        let state = TuiSessionState::default();
        let lines = commands_panel_lines(&state, 40, 8);

        // interior rows = 8 - 2 = 6; last two rows are the hint and input field.
        assert_eq!(lines.len(), 6);
        assert_eq!(lines[0], "no commands sent yet");
        assert!(lines[lines.len() - 2].starts_with("Enter: send"));
        assert!(lines[lines.len() - 1].starts_with("> "));
        assert!(lines[lines.len() - 1].contains('\u{2588}'));
    }

    #[test]
    fn input_buffer_and_history_render_into_the_panel() {
        let mut state = TuiSessionState::default();
        state.push_commands_history("broadcast", "run tests", 2, 1);
        state.commands_input_push('g');
        state.commands_input_push('o');

        let lines = commands_panel_lines(&state, 60, 10);
        assert!(lines.iter().any(|line| line.contains("[broadcast] run tests")));
        assert!(lines
            .iter()
            .any(|line| line.contains("delivered 2, skipped 1")));
        assert_eq!(lines.last().unwrap().as_str(), "> go\u{2588}");
    }

    #[test]
    fn zero_height_area_produces_no_lines() {
        let state = TuiSessionState::default();
        assert!(commands_panel_lines(&state, 40, 1).is_empty());
    }
}
