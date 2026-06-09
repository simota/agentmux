use super::*;

#[test]
fn pty_output_chunk_advances_terminal_grid() {
    let mut state =
        TuiSessionState::default().with_terminal_size(TerminalSize { rows: 2, cols: 8 });
    state.apply_event(&event(
        IpcEventKind::AgentSpawned,
        json!({ "agent_id": "agent_001", "name": "impl" }),
    ));

    let change = state.apply_event(&event(
        IpcEventKind::PtyOutputChunk,
        json!({ "agent_id": "agent_001", "text": "hello" }),
    ));

    assert_eq!(change, StateChange::UpdatedPane("agent_001".to_string()));
    let line = state
        .pane("agent_001")
        .expect("pane")
        .grid()
        .line_text(0)
        .expect("line");
    assert_eq!(line, "hello   ");
}

#[test]
fn focused_pane_scroll_offset_tracks_mouse_history_navigation() {
    let mut state =
        TuiSessionState::default().with_terminal_size(TerminalSize { rows: 2, cols: 4 });
    state.apply_event(&event(
        IpcEventKind::AgentSpawned,
        json!({ "agent_id": "agent_001", "name": "impl" }),
    ));
    state.apply_event(&event(
        IpcEventKind::PtyOutputChunk,
        json!({ "agent_id": "agent_001", "text": "aaaa\nbbbb\ncccc\n" }),
    ));

    let change = state.scroll_focused_pane(3);

    assert_eq!(change, StateChange::UpdatedPane("agent_001".to_string()));
    assert_eq!(state.pane("agent_001").expect("pane").scroll_offset(), 3);

    state.apply_event(&event(
        IpcEventKind::PtyOutputChunk,
        json!({ "agent_id": "agent_001", "text": "dddd\n" }),
    ));

    let pane = state.pane("agent_001").expect("pane");
    assert_eq!(pane.scroll_offset(), pane.grid().scrollback().len());

    let previous = pane.scroll_offset();
    state.scroll_focused_pane(-1);
    assert_eq!(
        state.pane("agent_001").expect("pane").scroll_offset(),
        previous.saturating_sub(1)
    );
}

#[test]
fn resize_pane_updates_terminal_grid_dimensions() {
    let mut state =
        TuiSessionState::default().with_terminal_size(TerminalSize { rows: 2, cols: 8 });
    state.apply_event(&event(
        IpcEventKind::AgentSpawned,
        json!({ "agent_id": "agent_001", "name": "impl" }),
    ));
    state.apply_event(&event(
        IpcEventKind::PtyOutputChunk,
        json!({ "agent_id": "agent_001", "text": "hello" }),
    ));

    let change = state.resize_pane("agent_001", TerminalSize { rows: 4, cols: 12 });

    assert_eq!(change, StateChange::UpdatedPane("agent_001".to_string()));
    let pane = state.pane("agent_001").expect("pane");
    assert_eq!(pane.grid().rows(), 4);
    assert_eq!(pane.grid().cols(), 12);
    assert_eq!(pane.grid().line_text(0).as_deref(), Some("hello       "));
}

#[test]
fn output_bytes_payload_is_supported() {
    let mut state =
        TuiSessionState::default().with_terminal_size(TerminalSize { rows: 1, cols: 3 });
    state.apply_event(&event(
        IpcEventKind::AgentSpawned,
        json!({ "agent_id": "agent_001", "name": "impl" }),
    ));

    state.apply_event(&event(
        IpcEventKind::ScreenDiff,
        json!({ "pane_id": "agent_001", "bytes": [65, 66, 300, 67] }),
    ));

    assert_eq!(
        state
            .pane("agent_001")
            .expect("pane")
            .grid()
            .line_text(0)
            .expect("line"),
        "ABC"
    );
}

#[test]
fn output_bytes_preserve_split_utf8_sequences() {
    let mut state =
        TuiSessionState::default().with_terminal_size(TerminalSize { rows: 1, cols: 4 });
    state.apply_event(&event(
        IpcEventKind::AgentSpawned,
        json!({ "agent_id": "agent_001", "name": "impl" }),
    ));

    state.apply_event(&event(
        IpcEventKind::PtyOutputChunk,
        json!({ "agent_id": "agent_001", "bytes": [0xE2] }),
    ));
    state.apply_event(&event(
        IpcEventKind::PtyOutputChunk,
        json!({ "agent_id": "agent_001", "bytes": [0x94, 0x80] }),
    ));

    assert_eq!(
        state
            .pane("agent_001")
            .expect("pane")
            .grid()
            .line_text(0)
            .expect("line"),
        "─   "
    );
}

#[test]
fn full_snapshot_restores_existing_pane_grid() {
    let mut state =
        TuiSessionState::default().with_terminal_size(TerminalSize { rows: 1, cols: 4 });
    state.apply_daemon_status(&json!({
        "agents": [
            {"id": "agent_001", "name": "impl", "process_id": 7}
        ]
    }));

    let change = state.apply_snapshot(&json!({
        "agent_id": "agent_001",
        "name": "impl",
        "process_id": 7,
        "rows": 2,
        "cols": 5,
        "lines": ["hello", "bye  "]
    }));

    assert_eq!(change, StateChange::UpdatedPane("agent_001".to_string()));
    let pane = state.pane("agent_001").expect("pane");
    assert_eq!(pane.grid().rows(), 2);
    assert_eq!(pane.grid().cols(), 5);
    assert_eq!(pane.grid().line_text(0).as_deref(), Some("hello"));
    assert_eq!(pane.grid().line_text(1).as_deref(), Some("bye  "));
    assert_eq!(
        pane.last_event(),
        Some(&IpcEventKind::TerminalSnapshotSaved)
    );
}

#[test]
fn snapshot_restore_clips_lines_by_display_width() {
    let mut state =
        TuiSessionState::default().with_terminal_size(TerminalSize { rows: 1, cols: 3 });
    state.apply_daemon_status(&json!({
        "agents": [
            {"id": "agent_001", "name": "impl", "process_id": 7}
        ]
    }));

    let change = state.apply_snapshot(&json!({
        "agent_id": "agent_001",
        "name": "impl",
        "process_id": 7,
        "rows": 1,
        "cols": 3,
        "lines": ["A変B"]
    }));

    assert_eq!(change, StateChange::UpdatedPane("agent_001".to_string()));
    let pane = state.pane("agent_001").expect("pane");
    assert_eq!(pane.grid().line_text(0).as_deref(), Some("A変"));
    assert_eq!(pane.grid().cursor().row, 0);
    // The wide glyph fills up to the right margin; the cursor parks on the
    // last column (wrap pending) instead of going past the grid.
    assert_eq!(pane.grid().cursor().col, 2);
}
