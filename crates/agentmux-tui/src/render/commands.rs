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
        let marker = if current_target == "broadcast" { "▸ " } else { "  " };
        targets_lines.push(truncate_marker_line(marker, "broadcast", content_width));
        targets_lines.push(truncate_marker_line("  ", "(no sessions)", content_width));
    } else {
        // Build a lookup: agent:<name> → role label for annotating agent rows.
        let name_to_role: std::collections::HashMap<String, Option<String>> = state
            .panes()
            .filter(|pane| pane.process_id().is_some())
            .map(|pane| {
                (
                    pane.name().to_string(),
                    pane.role().map(ToOwned::to_owned),
                )
            })
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
    let hint = "Enter: send  Tab: target  Esc: clear  Ctrl-g x: close".to_string();
    let input_line = truncate_cell(
        &format!("> {}\u{2588}", state.commands_input_buffer()),
        content_width,
    );

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
            let outcome = format!(
                "  -> delivered {}, skipped {}",
                entry.delivered, entry.skipped
            );
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
        } else {
            lines.pop();
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
            lines.iter().any(|l| l.contains("impl-1") && l.contains("implementer")),
            "agent:impl-1 line missing or not annotated"
        );
        assert!(
            lines.iter().any(|l| l.contains("rev-1") && l.contains("reviewer")),
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
        let solo_line = lines.iter().find(|l| l.contains("solo")).expect("solo line");
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
