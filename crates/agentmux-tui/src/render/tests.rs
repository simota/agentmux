use super::*;
use agentmux_terminal::{CellStyle, TerminalColor, TerminalParser};
use ratatui::layout::Position;
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

#[cfg(feature = "activity-feed")]
#[test]
fn feed_time_extracts_clock_and_falls_back_on_malformed_timestamps() {
    use super::messages::feed_time;

    // Well-formed RFC3339-like timestamp: keep only HH:MM:SS.
    assert_eq!(feed_time("2026-06-10T12:34:56Z"), "12:34:56");
    // The ts arrives from an unvalidated daemon frame: multi-byte characters
    // around the slice range used to panic on a char boundary and must fall
    // back to the full string instead.
    assert_eq!(feed_time("2026-06-10Tあいうえお"), "2026-06-10Tあいうえお");
    assert_eq!(feed_time("not a timestamp"), "not a timestamp");
    assert_eq!(feed_time(""), "");
}

#[cfg(feature = "activity-feed")]
#[test]
fn render_activity_feed_with_empty_state_does_not_panic() {
    let state = TuiSessionState::default();
    let area = Rect::new(0, 0, 50, 10);
    let mut buffer = Buffer::empty(area);

    render_activity_feed(area, &state, &mut buffer);

    let rendered = buffer
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Activity Feed"));
}

#[cfg(feature = "arena")]
#[test]
fn render_arena_overlay_with_empty_state_does_not_panic() {
    let mut state = TuiSessionState::default();
    state.apply_command(crate::keymap::TuiCommand::ShowArenaOverlay);
    let area = Rect::new(0, 0, 80, 12);
    let mut buffer = Buffer::empty(area);

    render_arena_overlay(area, &state, &mut buffer);

    let rendered = buffer
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Arena Candidates"));
    assert!(rendered.contains("no arena candidates"));
}

#[cfg(feature = "arena")]
#[test]
fn render_session_draws_arena_overlay() {
    let mut state = TuiSessionState::default();
    state.apply_daemon_status(&json!({
        "protocol_version": 3,
        "agents": [],
        "arena_candidates": [
            {
                "worktree_id": "wt_001",
                "name": "agentmux/task-a",
                "provider": "codex",
                "diff_stat": "1 file changed",
                "test_status": "passed",
                "summary": "ready"
            }
        ]
    }));
    state.apply_command(crate::keymap::TuiCommand::ShowArenaOverlay);
    let area = Rect::new(0, 0, 120, 18);
    let mut buffer = Buffer::empty(area);

    TuiSessionRenderer::default().render(area, &state, &mut buffer);

    let rendered = buffer
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Arena Candidates"));
    assert!(rendered.contains("codex"));
    assert!(rendered.contains("passed"));
    assert!(rendered.contains("wt_001"));
}

#[cfg(feature = "activity-feed")]
#[test]
fn render_activity_feed_keeps_selected_row_in_visible_window() {
    let mut state = TuiSessionState::default();
    for index in 0..8 {
        state.apply_event(&agentmux_ipc::DaemonEvent::new(
            agentmux_ipc::IpcEventKind::TaskCreated,
            json!({ "task_id": format!("task_{index:03}") }),
        ));
    }
    for _ in 0..5 {
        state.apply_command(crate::keymap::TuiCommand::ActivityFeedPrevious);
    }
    let area = Rect::new(0, 0, 80, 7);
    let mut buffer = Buffer::empty(area);

    render_activity_feed(area, &state, &mut buffer);

    let rendered = buffer
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("> [-] task_002  created  task_002"));
    assert!(!rendered.contains("task_007"));
}

#[cfg(feature = "activity-feed")]
#[test]
fn render_session_draws_activity_feed_pane() {
    let mut state = TuiSessionState::default();
    state.open_activity_feed_pane();
    state.apply_event(&agentmux_ipc::DaemonEvent::new(
        agentmux_ipc::IpcEventKind::AgentStatusChanged,
        json!({ "agent_id": "agent_001", "status": "awaiting_input" }),
    ));

    let area = Rect::new(0, 0, 80, 12);
    let mut buffer = Buffer::empty(area);

    TuiSessionRenderer::default().render(area, &state, &mut buffer);

    let rendered = buffer
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Activity Feed"));
    assert!(rendered.contains("agent_001"));
    assert!(rendered.contains("awaiting_input"));
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

fn row_text(buffer: &Buffer, y: u16, width: u16) -> String {
    (0..width)
        .filter_map(|x| buffer.cell((x, y)).map(|cell| cell.symbol().to_string()))
        .collect()
}

#[test]
fn status_bar_shows_prefix_indicator_and_focus_label() {
    let mut state = TuiSessionState::default();
    state.apply_daemon_status(&json!({
        "agents": [
            {"id": "left", "name": "planner"},
            {"id": "right", "name": "impl"}
        ]
    }));
    assert!(state.layout_mut().focus("right"));
    state.set_prefix_active(true);

    let area = Rect::new(0, 0, 60, 6);
    let mut buffer = Buffer::empty(area);
    TuiSessionRenderer::default().render(area, &state, &mut buffer);

    let status = row_text(&buffer, 5, 60);
    assert!(status.contains("PREFIX (Ctrl-g)"), "status was {status:?}");
    assert!(
        status.contains("focus: impl [2/2]"),
        "status was {status:?}"
    );
    // Prefix segment is rendered on a highlighted background.
    let cell = buffer.cell((0, 5)).expect("status cell");
    assert_eq!(cell.bg, Color::Yellow);
}

#[test]
fn status_bar_hides_prefix_segment_when_not_armed() {
    let mut state = TuiSessionState::default();
    state.apply_daemon_status(&json!({
        "agents": [{"id": "solo", "name": "solo"}]
    }));

    let area = Rect::new(0, 0, 40, 5);
    let mut buffer = Buffer::empty(area);
    TuiSessionRenderer::default().render(area, &state, &mut buffer);

    let status = row_text(&buffer, 4, 40);
    assert!(!status.contains("PREFIX"), "status was {status:?}");
    assert!(
        status.contains("focus: solo [1/1]"),
        "status was {status:?}"
    );
}

#[test]
fn pane_title_marks_focus_glyph_and_unseen_output() {
    let mut state = TuiSessionState::default();
    state.apply_daemon_status(&json!({
        "agents": [
            {"id": "left", "name": "planner"},
            {"id": "right", "name": "impl"}
        ]
    }));
    assert!(state.layout_mut().focus("left"));
    // Output to the unfocused pane raises its attention marker.
    state.apply_event(&agentmux_ipc::protocol::DaemonEvent::new(
        agentmux_ipc::protocol::IpcEventKind::PtyOutputChunk,
        json!({ "agent_id": "right", "text": "x" }),
    ));

    let area = Rect::new(0, 0, 40, 6);
    let mut buffer = Buffer::empty(area);
    TuiSessionRenderer::default().render(area, &state, &mut buffer);

    // The focus glyph sits on the focused (left) pane's title row.
    let left_title = row_text(&buffer, 0, 20);
    assert!(left_title.contains('▶'), "left title was {left_title:?}");
    // The unseen marker sits on the unfocused (right) pane's title row.
    let right_title = row_text(&buffer, 0, 40);
    assert!(right_title.contains('●'), "right title was {right_title:?}");
}

// --- Cursor position tests ---

/// Focused agent pane with a visible cursor returns the correct screen Position.
#[test]
fn render_grid_returns_cursor_position_when_visible() {
    // Grid with one row and enough columns; write one ASCII char so cursor is
    // at col=1.
    let mut grid = ScreenGrid::new(1, 10);
    grid.write_char('x', CellStyle::default());
    // cursor.col == 1, cursor.row == 0, cursor.visible == true (default)

    // Render at an offset to verify the area origin is included.
    let area = Rect::new(5, 3, 10, 1);
    let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 10));
    let pos = AgentPaneRenderer.render_grid(area, &grid, &mut buffer);

    assert_eq!(
        pos,
        Some(Position {
            x: 5 + 1, // area.x + cursor.col
            y: 3,     // area.y + cursor.row (row 0)
        }),
        "expected cursor at (6, 3)"
    );
}

/// Wide (full-width) characters advance the cursor by 2 cells; the returned X
/// must reflect the display-width-based column, not a character count.
#[test]
fn render_grid_returns_cursor_position_after_wide_chars() {
    // Write two wide characters: each occupies 2 columns, so after both the
    // cursor is at col=4.
    let mut grid = ScreenGrid::new(1, 10);
    grid.write_char('あ', CellStyle::default()); // col advances to 2
    grid.write_char('い', CellStyle::default()); // col advances to 4

    let area = Rect::new(2, 1, 10, 1);
    let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 5));
    let pos = AgentPaneRenderer.render_grid(area, &grid, &mut buffer);

    assert_eq!(
        pos,
        Some(Position {
            x: 2 + 4, // area.x + 4 (two wide chars)
            y: 1,
        }),
        "expected cursor at (6, 1) after two wide chars"
    );
}

/// When the cursor is not visible the function returns None.
#[test]
fn render_grid_returns_none_when_cursor_hidden() {
    let mut grid = ScreenGrid::new(1, 6);
    grid.write_char('A', CellStyle::default());
    grid.set_cursor_visible(false);

    let area = Rect::new(0, 0, 6, 1);
    let mut buffer = Buffer::empty(area);
    let pos = AgentPaneRenderer.render_grid(area, &grid, &mut buffer);

    assert_eq!(pos, None, "hidden cursor should return None");
}

/// The focused agent pane propagates its cursor position through
/// TuiSessionRenderer::render, and a non-focused pane does not.
#[test]
fn session_render_returns_cursor_for_focused_agent_pane_only() {
    let mut state = TuiSessionState::default();
    state.apply_daemon_status(&json!({
        "agents": [
            {"id": "left", "name": "planner"},
            {"id": "right", "name": "impl"}
        ]
    }));
    // Focus the right pane.
    assert!(state.layout_mut().focus("right"));

    // Push a character into the right (focused) pane so its cursor is at col=1.
    state.apply_event(&agentmux_ipc::protocol::DaemonEvent::new(
        agentmux_ipc::protocol::IpcEventKind::PtyOutputChunk,
        json!({ "agent_id": "right", "text": "z" }),
    ));

    // A wide area so both panes get a border + interior.
    let area = Rect::new(0, 0, 40, 6);
    let mut buffer = Buffer::empty(area);
    let pos = TuiSessionRenderer::default().render(area, &state, &mut buffer);

    // pos must be Some and within the right pane's inner area (x >= 21 for a
    // 20-col left split; y must be between 1 and 4 inclusive for the inner rows).
    let pos = pos.expect("focused agent pane should yield a cursor position");
    assert!(
        pos.x >= 20,
        "cursor x={} should be inside the right pane (x>=20)",
        pos.x
    );
    assert!(pos.y >= 1 && pos.y <= 4, "cursor y={} out of range", pos.y);
}

/// When the focused pane is a conversation-list pane (not an agent pane),
/// render must return None so the hardware cursor is hidden.
#[test]
fn session_render_returns_none_for_conversation_list_focus() {
    let mut state = TuiSessionState::default();
    state.open_conversation_list_pane();

    let area = Rect::new(0, 0, 40, 6);
    let mut buffer = Buffer::empty(area);
    let pos = TuiSessionRenderer::default().render(area, &state, &mut buffer);

    assert_eq!(
        pos, None,
        "conversation-list pane focus should not set hardware cursor"
    );
}

/// When multiple panes exist but only the focused one has a visible cursor,
/// the non-focused pane's cursor does not influence the return value.
#[test]
fn session_render_ignores_non_focused_pane_cursor() {
    let mut state = TuiSessionState::default();
    state.apply_daemon_status(&json!({
        "agents": [
            {"id": "left", "name": "planner"},
            {"id": "right", "name": "impl"}
        ]
    }));
    // Focus the left pane.
    assert!(state.layout_mut().focus("left"));

    // Output to the unfocused right pane (cursor would be at col=1 there).
    state.apply_event(&agentmux_ipc::protocol::DaemonEvent::new(
        agentmux_ipc::protocol::IpcEventKind::PtyOutputChunk,
        json!({ "agent_id": "right", "text": "q" }),
    ));
    // No output to left pane: its cursor stays at col=0.

    let area = Rect::new(0, 0, 40, 6);
    let mut buffer = Buffer::empty(area);
    let pos = TuiSessionRenderer::default().render(area, &state, &mut buffer);

    // The result must be from the focused (left) pane, not the right pane.
    // The left pane occupies x=0..20; inner x starts at 1.
    let pos = pos.expect("focused agent pane should yield a cursor position");
    assert!(
        pos.x < 20,
        "cursor x={} should be inside the left pane (x<20)",
        pos.x
    );
}
