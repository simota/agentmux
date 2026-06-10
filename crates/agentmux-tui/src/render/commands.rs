//! Commands (broadcast) panel rendering: a history log of sent broadcasts plus
//! an input field whose text is injected into every targeted PTY.

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Position, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};
use unicode_width::UnicodeWidthChar;

use crate::state::{CommandsLogKind, TuiSessionState};

use super::util::truncate_cell;

/// Cursor block rendered inside the Commands input field.
const CURSOR_BLOCK: char = '\u{2588}';

/// Display width of the `"> "` input prefix.
const INPUT_PREFIX_WIDTH: usize = 2;

/// Render the commands panel. When `focused`, returns the hardware cursor
/// [`Position`] at the input-field caret so the real terminal cursor sits in
/// the input area (keeping typing and IME preedit anchored there); returns
/// `None` when unfocused or the pane is too small to host the input row.
pub(crate) fn render_commands_panel(
    area: Rect,
    state: &TuiSessionState,
    focused: bool,
    buffer: &mut Buffer,
) -> Option<Position> {
    if area.width == 0 || area.height == 0 {
        return None;
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

    // Focused: place the real cursor on the input-field caret. The input line
    // is pinned to the last interior row by `commands_panel_lines` and rendered
    // by `commands_input_field_line`; the hardware cursor X is derived from the
    // same window computation so the visible `█` and the terminal cursor (and
    // IME preedit anchor) can never drift apart.
    if !focused || area.height < 4 || area.width < 4 {
        return None;
    }
    let (_, cursor_offset) = commands_input_field_line(state, content_width);
    let max_x = area.x + area.width.saturating_sub(2); // last column before the right border
    let cursor_x = (area.x + 1)
        .saturating_add(u16::try_from(cursor_offset).unwrap_or(u16::MAX))
        .min(max_x);
    let cursor_y = area.y + area.height.saturating_sub(2); // last interior row (input field)
    Some(Position {
        x: cursor_x,
        y: cursor_y,
    })
}

/// Compose the panel body:
///
/// 1. Targets/Sessions section (broadcast + roles + per-session agent lines,
///    with a `▸ ` marker on the currently selected target).
/// 2. A separator line.
/// 3. History log (oldest first, newest visible when overflow).
/// 4. Hint line.
/// 5. Input field pinned to the last interior row.
///
/// When total content exceeds `interior_rows` the history section is truncated
/// first; the targets section is always shown in full.
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

    let current_target = state.commands_target();
    let options = state.commands_target_options();

    // ── Targets/Sessions section ──────────────────────────────────────────
    // Each option renders as one of:
    //   "▸ broadcast"              (active, plain target)
    //   "  role:implementer"       (inactive role target)
    //   "▸ impl-1 (implementer)"   (active agent, shows name + role)
    //   "  rev-1"                  (inactive agent without role)
    //
    // NOTE: truncate_cell normalises whitespace and strips leading spaces, so
    // marker lines are truncated with `truncate_marker_line` instead.
    let mut targets_lines: Vec<String> = Vec::with_capacity(options.len().max(1));

    if options.is_empty() || (options.len() == 1 && options[0] == "broadcast") {
        // No live sessions — still show broadcast option + placeholder.
        let marker = if current_target == "broadcast" {
            "▸ "
        } else {
            "  "
        };
        targets_lines.push(truncate_marker_line(marker, "broadcast", content_width));
        targets_lines.push(truncate_marker_line("  ", "(no sessions)", content_width));
    } else {
        // Build a lookup: agent:<name> → role label for annotating agent rows.
        let name_to_role: std::collections::HashMap<String, Option<String>> = state
            .panes()
            .filter(|pane| pane.process_id().is_some())
            .map(|pane| (pane.name().to_string(), pane.role().map(ToOwned::to_owned)))
            .collect();

        for option in &options {
            let is_active = option == current_target;
            let marker = if is_active { "▸ " } else { "  " };

            let body = if let Some(name) = option.strip_prefix("agent:") {
                match name_to_role.get(name).and_then(|r| r.as_deref()) {
                    Some(role) => format!("{name} ({role})"),
                    None => name.to_string(),
                }
            } else {
                option.clone()
            };
            targets_lines.push(truncate_marker_line(marker, &body, content_width));
        }
    }

    // ── Fixed bottom elements ─────────────────────────────────────────────
    let separator = "─".repeat(content_width.min(40));
    let hint = "/send \"text\"  /role <r>  /keys <seq>  Tab: target  Esc: clear".to_string();
    let (input_line, _) = commands_input_field_line(state, content_width);

    // Fixed rows: separator + hint + input = 3
    let fixed_rows = 3;
    // Targets section rows (always shown in full):
    let targets_rows = targets_lines.len();
    // Remaining rows for history:
    let history_rows = interior_rows
        .saturating_sub(fixed_rows)
        .saturating_sub(targets_rows);

    // ── History section ───────────────────────────────────────────────────
    let mut history: Vec<String> = Vec::new();
    if !state.commands_history().is_empty() {
        for entry in state.commands_history() {
            let head = format!("[{}] {}", entry.target, entry.text);
            let outcome = match &entry.kind {
                CommandsLogKind::Broadcast { delivered, skipped } => {
                    format!("  -> delivered {delivered}, skipped {skipped}")
                }
                CommandsLogKind::RoleAssigned { role } => format!("  -> set role {role}"),
                CommandsLogKind::Error => "  -> error".to_string(),
            };
            history.push(truncate_cell(&head, content_width));
            history.push(truncate_cell(&outcome, content_width));
        }
        // Keep the newest history visible: drop the oldest lines when overflowing.
        if history.len() > history_rows {
            let overflow = history.len() - history_rows;
            history.drain(0..overflow);
        }
    }

    // ── Assemble ──────────────────────────────────────────────────────────
    let used = targets_rows + 1 /* separator */ + history.len() + fixed_rows - 1 /* separator already counted */;
    let padding_rows = interior_rows.saturating_sub(used + 2); // +2 for hint+input

    let mut lines = Vec::with_capacity(interior_rows);
    lines.extend(targets_lines);
    lines.push(truncate_cell(&separator, content_width));
    lines.extend(history);
    // Pad so hint and input field sit at the bottom.
    for _ in 0..padding_rows {
        lines.push(String::new());
    }
    // Clamp to leave exactly 2 rows for hint + input.
    while lines.len() + 2 > interior_rows {
        // Remove the oldest non-empty lines first (just before the separator area).
        if lines.len() > targets_rows + 1 {
            lines.remove(targets_rows + 1);
        } else if lines.pop().is_none() {
            // interior_rows < 2 (pane height of exactly 3): even an empty body
            // cannot satisfy the clamp, so stop instead of spinning forever —
            // the paragraph clips the overflowing hint/input rows.
            break;
        }
    }
    while lines.len() + 2 < interior_rows {
        lines.push(String::new());
    }
    lines.push(truncate_cell(&hint, content_width));
    lines.push(input_line);
    lines
}

/// Produce a target-list row: `marker` (2 chars: `"▸ "` or `"  "`) followed by
/// `body`, truncated so the total does not exceed `max_chars`.
///
/// Unlike `truncate_cell`, this preserves the leading marker exactly — the two
/// spaces of the inactive marker must not be collapsed by whitespace normalisation.
fn truncate_marker_line(marker: &str, body: &str, max_chars: usize) -> String {
    let marker_chars = marker.chars().count();
    let body_budget = max_chars.saturating_sub(marker_chars);
    let truncated_body = truncate_cell(body, body_budget);
    format!("{marker}{truncated_body}")
}

/// Render the Commands input row (`"> "` prefix plus a sliding window over the
/// input buffer with the `█` caret) and the caret's display-cell offset from
/// the start of the row.
///
/// Unlike history/targets rows this must NOT go through `truncate_cell`: its
/// whitespace normalisation would collapse consecutive spaces and turn U+3000
/// the user actually typed into a single ASCII space. The window is computed
/// with unicode display widths and always keeps the caret visible — anchored
/// left while the content fits, scrolling with the caret once it passes the
/// right edge. `render_commands_panel` derives the hardware cursor X from the
/// same computation so the rendered caret and the terminal cursor stay in sync.
pub(crate) fn commands_input_field_line(
    state: &TuiSessionState,
    content_width: usize,
) -> (String, usize) {
    let budget = content_width.saturating_sub(INPUT_PREFIX_WIDTH);
    let (window, cursor_offset) = input_window(
        state.commands_input_buffer(),
        state.commands_input_cursor(),
        budget,
    );
    (format!("> {window}"), INPUT_PREFIX_WIDTH + cursor_offset)
}

/// Compute the visible window of the input buffer (caret block inserted at the
/// `cursor` char index) limited to `budget` display cells, plus the caret's
/// cell offset within that window.
///
/// Whitespace is preserved verbatim. When the content overflows, the window
/// starts at cell 0 until the caret reaches the right edge and then follows
/// the caret, so the caret block is always visible. A wide character that
/// straddles the window's left edge is skipped whole (char-boundary safe).
fn input_window(buffer: &str, cursor: usize, budget: usize) -> (String, usize) {
    if budget == 0 {
        return (String::new(), 0);
    }

    // Display items: the buffer's chars with the caret block (1 cell wide)
    // inserted at the cursor's char index (end of buffer when past the text).
    let mut items: Vec<(char, usize)> = Vec::with_capacity(buffer.chars().count() + 1);
    let mut block_index = None;
    for (index, ch) in buffer.chars().enumerate() {
        if index == cursor {
            block_index = Some(items.len());
            items.push((CURSOR_BLOCK, 1));
        }
        items.push((ch, UnicodeWidthChar::width(ch).unwrap_or(0)));
    }
    let block_index = block_index.unwrap_or_else(|| {
        items.push((CURSOR_BLOCK, 1));
        items.len() - 1
    });

    let cursor_cell: usize = items[..block_index].iter().map(|(_, width)| width).sum();
    // First window cell: 0 while everything left of the caret fits; otherwise
    // scroll just enough that the caret block sits at the right edge.
    let start_cell = (cursor_cell + 1).saturating_sub(budget);

    let mut window = String::new();
    let mut first_visible_cell = None;
    let mut cell = 0usize;
    let mut used = 0usize;
    for (ch, width) in items {
        let begins = cell;
        cell += width;
        if begins < start_cell {
            continue;
        }
        if used + width > budget {
            break;
        }
        first_visible_cell.get_or_insert(begins);
        window.push(ch);
        used += width;
    }

    let cursor_offset = cursor_cell - first_visible_cell.unwrap_or(cursor_cell);
    (window, cursor_offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_panel_shows_broadcast_placeholder_and_input_field_at_bottom() {
        let state = TuiSessionState::default();
        let lines = commands_panel_lines(&state, 40, 8);

        // interior rows = 8 - 2 = 6; last two rows are the hint and input field.
        assert_eq!(lines.len(), 6);
        // No sessions: targets section shows "▸ broadcast" + "(no sessions)".
        assert_eq!(lines[0], "▸ broadcast");
        assert_eq!(lines[1], "  (no sessions)");
        assert!(lines[lines.len() - 2].starts_with("/send"));
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
        assert!(
            lines
                .iter()
                .any(|line| line.contains("[broadcast] run tests"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("delivered 2, skipped 1"))
        );
        assert_eq!(lines.last().unwrap().as_str(), "> go\u{2588}");
    }

    /// The input row must not be whitespace-normalised: consecutive spaces and
    /// U+3000 (ideographic space) are preserved exactly as typed.
    #[test]
    fn input_line_preserves_consecutive_and_ideographic_spaces() {
        let mut state = TuiSessionState::default();
        for ch in ['a', ' ', ' ', 'b', '\u{3000}', 'c'] {
            state.commands_input_push(ch);
        }

        let lines = commands_panel_lines(&state, 40, 8);
        assert_eq!(
            lines.last().unwrap().as_str(),
            "> a  b\u{3000}c\u{2588}",
            "input row must keep raw whitespace"
        );
    }

    /// When the input is wider than the field, the window scrolls so the caret
    /// (at the end of the buffer) stays visible, and the hardware cursor X is
    /// derived from the same window.
    #[test]
    fn input_line_overflow_keeps_caret_visible() {
        let mut state = TuiSessionState::default();
        for ch in "abcdefghijklmnopqrstuvwxy".chars() {
            state.commands_input_push(ch); // 25 chars
        }

        let content_width = 16; // budget after "> " prefix: 14 cells
        let (line, cursor_offset) = commands_input_field_line(&state, content_width);
        // 13 tail chars + the caret block fill the 14-cell budget.
        assert_eq!(line, "> mnopqrstuvwxy\u{2588}");
        assert_eq!(cursor_offset, 15); // prefix (2) + 13 tail chars
    }

    /// With the caret in the middle of the buffer, the `█` block is rendered at
    /// the caret position and the hardware cursor X matches it exactly.
    #[test]
    fn input_line_renders_caret_mid_buffer_and_cursor_matches() {
        let mut state = TuiSessionState::default();
        for ch in "abcd".chars() {
            state.commands_input_push(ch);
        }
        state.commands_input_move_left();
        state.commands_input_move_left(); // caret between 'b' and 'c'

        let lines = commands_panel_lines(&state, 40, 8);
        assert_eq!(lines.last().unwrap().as_str(), "> ab\u{2588}cd");

        let area = Rect::new(0, 0, 40, 8);
        let mut buffer = Buffer::empty(area);
        let cursor = render_commands_panel(area, &state, true, &mut buffer);
        // interior x (1) + "> " (2) + "ab" (2 cells)
        assert_eq!(
            cursor,
            Some(Position {
                x: 1 + 2 + 2,
                y: 8 - 2
            })
        );
    }

    /// When the caret scrolls back from an overflowing tail, the window follows
    /// it left so the caret block stays visible.
    #[test]
    fn input_line_overflow_window_follows_caret_left() {
        let mut state = TuiSessionState::default();
        for ch in "abcdefghijklmnopqrstuvwxy".chars() {
            state.commands_input_push(ch); // 25 chars
        }
        state.commands_input_move_home();

        let (line, cursor_offset) = commands_input_field_line(&state, 16);
        // Caret at the start: the head of the buffer is visible again.
        assert_eq!(line, "> \u{2588}abcdefghijklm");
        assert_eq!(cursor_offset, 2);
    }

    /// Wide characters never straddle the window's left edge: the straddling
    /// char is dropped whole and the caret offset shrinks accordingly.
    #[test]
    fn input_line_overflow_skips_straddling_wide_char() {
        let mut state = TuiSessionState::default();
        for ch in "あいうえおかきくけ".chars() {
            state.commands_input_push(ch); // 9 wide chars = 18 cells
        }

        let content_width = 12; // budget: 10 cells; caret cell = 18
        let (line, cursor_offset) = commands_input_field_line(&state, content_width);
        // start_cell = 9 falls inside 'お' (cells 8..10), so the window starts
        // at 'か' (cell 10): 4 wide chars (8 cells) + caret block.
        assert_eq!(line, "> かきくけ\u{2588}");
        assert_eq!(cursor_offset, 2 + 8);
    }

    /// Tiny widths never panic: with no budget after the prefix the row
    /// degrades to the bare `"> "` and a 1-cell budget still shows the caret.
    #[test]
    fn input_line_tiny_widths_degrade_without_panic() {
        let mut state = TuiSessionState::default();
        for ch in "あい".chars() {
            state.commands_input_push(ch); // 2 wide chars = 4 cells, caret at 4
        }

        // content_width 0..=2 leaves no budget after the prefix.
        for content_width in 0..=2 {
            let (line, cursor_offset) = commands_input_field_line(&state, content_width);
            assert_eq!(line, "> ", "width {content_width}: prefix only");
            assert_eq!(cursor_offset, 2, "width {content_width}");
        }
        // Budget 1: only the caret block fits.
        assert_eq!(
            commands_input_field_line(&state, 3),
            ("> \u{2588}".to_string(), 2)
        );
        // Budget 2: 'い' would straddle the left edge → caret block only.
        assert_eq!(
            commands_input_field_line(&state, 4),
            ("> \u{2588}".to_string(), 2)
        );
        // Budget 3: 'い' (2 cells) + caret block fit exactly.
        assert_eq!(
            commands_input_field_line(&state, 5),
            ("> い\u{2588}".to_string(), 4)
        );
    }

    /// Exact-fit boundary: content plus caret block exactly filling the budget
    /// does not scroll; one more char tips the window by exactly one cell.
    #[test]
    fn input_line_exact_fit_boundary() {
        let mut state = TuiSessionState::default();
        for ch in "abcdefghijklm".chars() {
            state.commands_input_push(ch); // 13 chars + caret = 14 cells
        }

        let (line, cursor_offset) = commands_input_field_line(&state, 16); // budget 14
        assert_eq!(line, "> abcdefghijklm\u{2588}");
        assert_eq!(cursor_offset, 15);

        state.commands_input_push('n'); // 15 cells: scroll by exactly one
        let (line, cursor_offset) = commands_input_field_line(&state, 16);
        assert_eq!(line, "> bcdefghijklmn\u{2588}");
        assert_eq!(cursor_offset, 15);
    }

    /// Window boundaries on a half/full-width mix: the scroll start can land
    /// on either kind of char and the caret offset shrinks by the cells
    /// actually skipped.
    #[test]
    fn input_line_mixed_width_window_boundary() {
        let mut state = TuiSessionState::default();
        for ch in "aあbいc".chars() {
            state.commands_input_push(ch); // 7 cells, caret at cell 7
        }

        // budget 4: start_cell 4 lands exactly on 'い'.
        let (line, cursor_offset) = commands_input_field_line(&state, 6);
        assert_eq!(line, "> いc\u{2588}");
        assert_eq!(cursor_offset, 2 + 3);

        // budget 5: start_cell 3 lands exactly on 'b'.
        let (line, cursor_offset) = commands_input_field_line(&state, 7);
        assert_eq!(line, "> bいc\u{2588}");
        assert_eq!(cursor_offset, 2 + 4);
    }

    /// Zero-width combining marks occupy no cells: they stay attached to their
    /// base char in the window and never advance the caret offset.
    #[test]
    fn input_line_combining_mark_takes_no_cells() {
        let mut state = TuiSessionState::default();
        for ch in "e\u{301}x".chars() {
            state.commands_input_push(ch); // e + combining acute + x
        }

        let (line, cursor_offset) = commands_input_field_line(&state, 40);
        assert_eq!(line, "> e\u{301}x\u{2588}");
        assert_eq!(cursor_offset, 2 + 2); // 'e' (1) + mark (0) + 'x' (1)
    }

    /// Regression: a pane height of exactly 3 (one interior row) used to spin
    /// forever in the bottom-clamp loop — `pop()` on the already-empty body
    /// never made `lines.len() + 2 > 1` false. The function must terminate and
    /// still keep the input row as the last line.
    #[test]
    fn one_interior_row_terminates_and_keeps_input_line_last() {
        let mut state = TuiSessionState::default();
        state.commands_input_push('x');

        let lines = commands_panel_lines(&state, 16, 3);
        assert_eq!(lines.last().unwrap().as_str(), "> x\u{2588}");
    }

    /// Degenerate focused areas render without panicking and report no cursor
    /// position (the pane cannot host the input row).
    #[test]
    fn render_commands_panel_degenerate_areas_are_safe() {
        let mut state = TuiSessionState::default();
        for ch in "abc".chars() {
            state.commands_input_push(ch);
        }

        for (width, height) in [(1u16, 1u16), (3, 3), (2, 8), (40, 1), (3, 8), (40, 3)] {
            let area = Rect::new(0, 0, width, height);
            let mut buffer = Buffer::empty(area);
            assert_eq!(
                render_commands_panel(area, &state, true, &mut buffer),
                None,
                "area {width}x{height} must not host a cursor"
            );
        }
    }

    #[test]
    fn zero_height_area_produces_no_lines() {
        let state = TuiSessionState::default();
        assert!(commands_panel_lines(&state, 40, 1).is_empty());
    }

    #[test]
    fn focused_panel_reports_cursor_at_end_of_input_field() {
        let mut state = TuiSessionState::default();
        state.commands_input_push('g');
        state.commands_input_push('o');
        let area = Rect::new(0, 0, 40, 8);
        let mut buffer = Buffer::empty(area);

        // Cursor sits after the "> " prefix (2 cols) + "go" (2 cols) on the last
        // interior row (area.y + height - 2).
        let cursor = render_commands_panel(area, &state, true, &mut buffer);
        assert_eq!(
            cursor,
            Some(Position {
                x: 1 + 2 + 2,
                y: 8 - 2
            })
        );

        // Unfocused panes do not own the hardware cursor.
        let mut unfocused = Buffer::empty(area);
        assert_eq!(
            render_commands_panel(area, &state, false, &mut unfocused),
            None
        );
    }

    #[test]
    fn focused_panel_cursor_accounts_for_wide_input_chars() {
        let mut state = TuiSessionState::default();
        // Two full-width characters occupy 4 display columns.
        state.commands_input_push('あ');
        state.commands_input_push('い');
        let area = Rect::new(0, 0, 40, 8);
        let mut buffer = Buffer::empty(area);

        let cursor = render_commands_panel(area, &state, true, &mut buffer);
        assert_eq!(
            cursor,
            Some(Position {
                x: 1 + 2 + 4,
                y: 8 - 2
            })
        );
    }

    /// Targets section shows all broadcast options; active target gets the ▸ marker.
    #[test]
    fn targets_section_lists_sessions_and_marks_active_target() {
        use agentmux_ipc::protocol::{DaemonEvent, IpcEventKind};
        use serde_json::json;

        let mut state = TuiSessionState::default();
        state.apply_event(&DaemonEvent::new(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "a1", "name": "impl-1", "role": "implementer", "process_id": 1 }),
        ));
        state.apply_event(&DaemonEvent::new(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "r1", "name": "rev-1", "role": "reviewer", "process_id": 2 }),
        ));

        // Default target is "broadcast" — its line should carry the ▸ marker.
        let lines = commands_panel_lines(&state, 60, 14);
        let broadcast_line = lines.iter().find(|l| l.contains("broadcast")).unwrap();
        assert!(
            broadcast_line.starts_with('▸'),
            "broadcast line should have active marker, got: {broadcast_line:?}"
        );
        // Role lines must be present.
        assert!(
            lines.iter().any(|l| l.contains("role:implementer")),
            "role:implementer line missing"
        );
        assert!(
            lines.iter().any(|l| l.contains("role:reviewer")),
            "role:reviewer line missing"
        );
        // Agent lines must be present and annotated with role.
        assert!(
            lines
                .iter()
                .any(|l| l.contains("impl-1") && l.contains("implementer")),
            "agent:impl-1 line missing or not annotated"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("rev-1") && l.contains("reviewer")),
            "agent:rev-1 line missing or not annotated"
        );

        // Cycle to "agent:impl-1" and verify the marker moves.
        state.cycle_commands_target(); // → role:implementer
        state.cycle_commands_target(); // → role:reviewer
        state.cycle_commands_target(); // → agent:impl-1
        assert_eq!(state.commands_target(), "agent:impl-1");
        let lines2 = commands_panel_lines(&state, 60, 14);
        let impl_line = lines2
            .iter()
            .find(|l| l.contains("impl-1"))
            .expect("impl-1 line");
        assert!(
            impl_line.starts_with('▸'),
            "agent:impl-1 line should be active, got: {impl_line:?}"
        );
        let broadcast_inactive = lines2
            .iter()
            .find(|l| l.contains("broadcast"))
            .expect("broadcast line");
        assert!(
            broadcast_inactive.starts_with("  "),
            "broadcast should be inactive, got: {broadcast_inactive:?}"
        );
    }

    /// A session without a role appears in the agent section without annotation.
    #[test]
    fn agent_without_role_appears_without_annotation() {
        use agentmux_ipc::protocol::{DaemonEvent, IpcEventKind};
        use serde_json::json;

        let mut state = TuiSessionState::default();
        state.apply_event(&DaemonEvent::new(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "x1", "name": "solo", "process_id": 1 }),
        ));

        let lines = commands_panel_lines(&state, 60, 10);
        // No role line should appear (no role set).
        assert!(
            !lines.iter().any(|l| l.contains("role:")),
            "unexpected role line when session has no role"
        );
        // Agent line for "solo" must exist (no parenthesised annotation).
        let solo_line = lines
            .iter()
            .find(|l| l.contains("solo"))
            .expect("solo line");
        assert!(
            !solo_line.contains('('),
            "solo agent line should not have role annotation, got: {solo_line:?}"
        );
    }

    /// When the current target has been removed (e.g. agent exited), the next
    /// cycle_commands_target resolves to the first entry safely.
    #[test]
    fn stale_target_resolves_safely_on_next_cycle() {
        use agentmux_ipc::protocol::{DaemonEvent, IpcEventKind};
        use serde_json::json;

        let mut state = TuiSessionState::default();
        state.apply_event(&DaemonEvent::new(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "s1", "name": "worker", "process_id": 1 }),
        ));
        // Cycle to the agent target.
        state.cycle_commands_target(); // → agent:worker
        assert_eq!(state.commands_target(), "agent:worker");

        // Simulate the session exiting.
        state.apply_event(&DaemonEvent::new(
            IpcEventKind::AgentExited,
            json!({ "agent_id": "s1" }),
        ));
        // Target is now stale (agent:worker no longer in options).
        assert_eq!(state.commands_target(), "agent:worker");

        // Next Tab must not panic and must resolve to broadcast.
        state.cycle_commands_target();
        assert_eq!(state.commands_target(), "broadcast");
    }
}
