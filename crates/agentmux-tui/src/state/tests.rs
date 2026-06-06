//! Unit tests for the TUI session state module.

use super::*;
use crate::keymap::{FocusDirection, TuiCommand};
use agentmux_ipc::protocol::{DaemonEvent, IpcEventKind};
use serde_json::json;


    fn event(kind: IpcEventKind, payload: serde_json::Value) -> DaemonEvent {
        DaemonEvent::new(kind, payload)
    }

    #[test]
    fn spawned_agent_adds_pane_and_initial_focus() {
        let mut state = TuiSessionState::default();

        let change = state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({
                "agent_id": "agent_001",
                "name": "impl-codex",
                "role": "implementer",
                "process_id": 42
            }),
        ));

        assert_eq!(change, StateChange::AddedPane("agent_001".to_string()));
        assert_eq!(state.layout().panes(), &["agent_001".to_string()]);
        assert_eq!(state.layout().focused(), Some("agent_001"));
        let pane = state.pane("agent_001").expect("pane");
        assert_eq!(pane.name(), "impl-codex");
        assert_eq!(pane.role(), Some("implementer"));
        assert_eq!(pane.process_id(), Some(42));
        assert_eq!(pane.chrome_title(), "impl-codex (implementer)");
    }

    #[test]
    fn duplicate_spawn_updates_existing_pane_without_reordering_layout() {
        let mut state = TuiSessionState::default();
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_001", "name": "old" }),
        ));

        let change = state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_001", "name": "new", "process_id": 7 }),
        ));

        assert_eq!(change, StateChange::UpdatedPane("agent_001".to_string()));
        assert_eq!(state.layout().panes(), &["agent_001".to_string()]);
        let pane = state.pane("agent_001").expect("pane");
        assert_eq!(pane.name(), "new");
        assert_eq!(pane.process_id(), Some(7));
    }

    #[test]
    fn client_attached_focuses_existing_pane() {
        let mut state = TuiSessionState::default();
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_a", "name": "a" }),
        ));
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_b", "name": "b" }),
        ));

        let change = state.apply_event(&event(
            IpcEventKind::ClientAttached,
            json!({ "client_id": "client_001", "agent_id": "agent_b" }),
        ));

        assert_eq!(change, StateChange::FocusedPane("agent_b".to_string()));
        assert_eq!(state.focused_pane().expect("focused").agent_id(), "agent_b");
    }

    #[test]
    fn status_event_updates_chrome_title() {
        let mut state = TuiSessionState::default();
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_001", "name": "impl" }),
        ));

        let change = state.apply_event(&event(
            IpcEventKind::AgentStatusChanged,
            json!({ "agent_id": "agent_001", "status": "awaiting_input" }),
        ));

        assert_eq!(change, StateChange::UpdatedPane("agent_001".to_string()));
        let pane = state.pane("agent_001").expect("pane");
        assert_eq!(pane.status(), Some("awaiting_input"));
        assert_eq!(pane.chrome_title(), "impl | awaiting_input");
    }

    #[cfg(feature = "activity-feed")]
    #[test]
    fn feed_entry_from_agent_status_changed_event() {
        let entry = FeedEntry::from_event(&event(
            IpcEventKind::AgentStatusChanged,
            json!({ "agent_id": "agent_001", "status": "awaiting_input" }),
        ))
        .expect("entry");

        assert_eq!(entry.actor, "agent_001");
        assert_eq!(entry.action, "status awaiting_input");
        assert_eq!(entry.target, "agent_001");
        assert_eq!(entry.kind, "agent.status_changed");
        assert_eq!(entry.focus_agent_id.as_deref(), Some("agent_001"));
    }

    #[cfg(feature = "activity-feed")]
    #[test]
    fn feed_entry_from_message_created_event_includes_delivery_status() {
        let entry = FeedEntry::from_event(&event(
            IpcEventKind::MessageCreated,
            json!({
                "message_id": "msg_001",
                "from": {"kind": "user", "id": "client_001"},
                "to": {"kind": "agent", "id": "agent_001"},
                "delivery_status": "pending",
                "created_at": "2026-06-04T12:34:56+00:00"
            }),
        ))
        .expect("entry");

        assert_eq!(entry.ts, "2026-06-04T12:34:56+00:00");
        assert_eq!(entry.actor, "user:client_001");
        assert_eq!(entry.action, "message pending");
        assert_eq!(entry.target, "agent:agent_001");
        assert_eq!(entry.kind, "message.created");
        assert_eq!(entry.focus_agent_id.as_deref(), Some("agent_001"));
    }

    #[cfg(feature = "activity-feed")]
    #[test]
    fn feed_entry_from_approval_created_event() {
        let entry = FeedEntry::from_event(&event(
            IpcEventKind::ApprovalCreated,
            json!({
                "approval_id": "approval_001",
                "kind": "tool",
                "risk": "medium",
                "title": "Run command"
            }),
        ))
        .expect("entry");

        assert_eq!(entry.actor, "policy");
        assert_eq!(entry.action, "approval requested");
        assert_eq!(entry.target, "approval_001");
        assert_eq!(entry.kind, "approval.created");
        assert_eq!(entry.focus_agent_id, None);
    }

    #[cfg(feature = "activity-feed")]
    #[test]
    fn feed_entry_from_daemon_event_uses_sensible_daemon_actor() {
        let entry = FeedEntry::from_event(&event(
            IpcEventKind::DaemonStopped,
            json!({ "socket_path": "/tmp/agentmux.sock" }),
        ))
        .expect("entry");

        assert_eq!(entry.actor, "daemon");
        assert_eq!(entry.action, "stopped");
        assert_eq!(entry.target, "-");
        assert_eq!(entry.kind, "daemon.stopped");
        assert_eq!(entry.focus_agent_id, None);
    }

    #[cfg(feature = "activity-feed")]
    #[test]
    fn activity_feed_ignores_high_frequency_output_events() {
        assert!(
            FeedEntry::from_event(&event(
                IpcEventKind::PtyOutputChunk,
                json!({ "agent_id": "agent_001", "text": "hello" }),
            ))
            .is_none()
        );
        assert!(
            FeedEntry::from_event(&event(
                IpcEventKind::ScreenDiff,
                json!({ "agent_id": "agent_001", "text": "hello" }),
            ))
            .is_none()
        );
    }

    #[cfg(feature = "activity-feed")]
    #[test]
    fn sitrep_sorts_agents_needing_attention_first() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "agents": [
                {"id": "agent_ready", "name": "ready", "status": "ready"},
                {"id": "agent_waiting", "name": "waiting", "status": "awaiting_input"}
            ]
        }));

        assert_eq!(state.sitrep()[0].agent_id, "agent_waiting");
        assert!(state.sitrep()[0].needs_attention);
        assert_eq!(state.sitrep()[1].agent_id, "agent_ready");
    }

    #[cfg(feature = "activity-feed")]
    #[test]
    fn agent_exit_removes_sitrep_entry_that_needed_attention() {
        let mut state = TuiSessionState::default();
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_001", "name": "impl" }),
        ));
        state.apply_event(&event(
            IpcEventKind::AgentStatusChanged,
            json!({ "agent_id": "agent_001", "status": "awaiting_input" }),
        ));

        let change = state.apply_event(&event(
            IpcEventKind::AgentExited,
            json!({ "agent_id": "agent_001", "name": "impl" }),
        ));

        assert_eq!(change, StateChange::RemovedPane("agent_001".to_string()));
        assert!(state.sitrep().is_empty());
    }

    #[cfg(feature = "activity-feed")]
    #[test]
    fn feed_caps_at_500_entries_and_keeps_indices_valid() {
        let mut state = TuiSessionState::default();

        for index in 0..501 {
            state.apply_event(&event(
                IpcEventKind::TaskCreated,
                json!({ "task_id": format!("task_{index:03}") }),
            ));
        }

        assert_eq!(state.feed_entries().len(), 500);
        assert_eq!(
            state.feed_entries().front().expect("front").target,
            "task_001"
        );
        assert_eq!(
            state.feed_entries().back().expect("back").target,
            "task_500"
        );
        assert!(state.activity_feed_selected_index() < state.feed_entries().len());
        assert!(state.feed_scroll() <= state.feed_entries().len());
    }

    #[cfg(feature = "activity-feed")]
    #[test]
    fn feed_navigation_on_empty_feed_is_noop() {
        let mut state = TuiSessionState::default();

        assert_eq!(
            state.apply_command(TuiCommand::ActivityFeedNext),
            CommandEffect::Continue
        );
        assert_eq!(
            state.apply_command(TuiCommand::ActivityFeedPrevious),
            CommandEffect::Continue
        );
        assert_eq!(
            state.apply_command(TuiCommand::FocusFeedEntry),
            CommandEffect::Continue
        );
        assert_eq!(state.activity_feed_selected_index(), 0);
    }

    #[cfg(feature = "activity-feed")]
    #[test]
    fn activity_feed_navigation_updates_scroll_to_keep_selection_visible() {
        let mut state = TuiSessionState::default();
        for index in 0..8 {
            state.apply_event(&event(
                IpcEventKind::TaskCreated,
                json!({ "task_id": format!("task_{index:03}") }),
            ));
        }

        for _ in 0..5 {
            state.apply_command(TuiCommand::ActivityFeedPrevious);
        }

        assert_eq!(state.activity_feed_selected_index(), 2);
        assert_eq!(state.feed_scroll(), 5);
        assert_eq!(state.activity_feed_window_start(5), 0);

        state.apply_command(TuiCommand::ActivityFeedNext);

        assert_eq!(state.activity_feed_selected_index(), 3);
        assert_eq!(state.feed_scroll(), 4);
        assert_eq!(state.activity_feed_window_start(5), 0);
    }

    #[cfg(feature = "activity-feed")]
    #[test]
    fn incoming_feed_event_does_not_steal_non_tail_selection() {
        let mut state = TuiSessionState::default();
        for index in 0..3 {
            state.apply_event(&event(
                IpcEventKind::TaskCreated,
                json!({ "task_id": format!("task_{index:03}") }),
            ));
        }
        state.apply_command(TuiCommand::ActivityFeedPrevious);

        state.apply_event(&event(
            IpcEventKind::TaskCreated,
            json!({ "task_id": "task_003" }),
        ));

        assert_eq!(state.activity_feed_selected_index(), 1);
        assert_eq!(state.feed_scroll(), 2);
    }

    #[cfg(feature = "activity-feed")]
    #[test]
    fn focus_feed_entry_for_removed_agent_is_noop() {
        let mut state = TuiSessionState::default();
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_001", "name": "impl" }),
        ));
        state.apply_event(&event(
            IpcEventKind::AgentExited,
            json!({ "agent_id": "agent_001", "name": "impl" }),
        ));

        assert!(state.pane("agent_001").is_none());
        assert_eq!(
            state.apply_command(TuiCommand::FocusFeedEntry),
            CommandEffect::Continue
        );
    }

    #[cfg(feature = "activity-feed")]
    #[test]
    fn focus_feed_entry_returns_focus_pane_effect() {
        let mut state = TuiSessionState::default();
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_001", "name": "impl" }),
        ));
        state.apply_event(&event(
            IpcEventKind::AgentStatusChanged,
            json!({ "agent_id": "agent_001", "status": "awaiting_input" }),
        ));

        assert_eq!(
            state.apply_command(TuiCommand::FocusFeedEntry),
            CommandEffect::FocusPaneById("agent_001".to_string())
        );
    }

    #[cfg(feature = "arena")]
    #[test]
    fn arena_events_update_candidates_and_adopt_effect() {
        let mut state = TuiSessionState::default();
        state.apply_event(&event(
            IpcEventKind::WorktreeCreated,
            json!({
                "worktree": {
                    "worktree_id": "wt_001",
                    "branch_name": "agentmux/task-a"
                },
                "provider": "codex"
            }),
        ));
        state.apply_event(&event(
            IpcEventKind::WorktreeDiffCaptured,
            json!({ "worktree_id": "wt_001", "stat": "1 file changed" }),
        ));
        state.apply_event(&event(
            IpcEventKind::WorktreeTestCompleted,
            json!({ "worktree_id": "wt_001", "status": "passed" }),
        ));

        assert_eq!(state.arena_candidates().len(), 1);
        assert_eq!(state.arena_candidates()[0].provider, "codex");
        assert_eq!(state.arena_candidates()[0].diff_stat, "1 file changed");
        assert_eq!(state.arena_candidates()[0].test_status, "passed");
        assert_eq!(
            state.apply_command(TuiCommand::ArenaAdopt),
            CommandEffect::ArenaAdopt("wt_001".to_string())
        );
    }

    #[cfg(feature = "arena")]
    #[test]
    fn arena_adopt_with_empty_selection_is_noop() {
        let mut state = TuiSessionState::default();

        assert_eq!(
            state.apply_command(TuiCommand::ShowArenaOverlay),
            CommandEffect::Continue
        );
        assert!(state.arena_overlay_visible());
        assert_eq!(
            state.apply_command(TuiCommand::ArenaAdopt),
            CommandEffect::Continue
        );
        assert_eq!(state.arena_selected_index(), 0);
    }

    #[cfg(feature = "arena")]
    #[test]
    fn arena_candidate_refresh_clamps_selection_while_overlay_is_open() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "protocol_version": 3,
            "agents": [],
            "arena_candidates": [
                { "worktree_id": "wt_001", "provider": "claude" },
                { "worktree_id": "wt_002", "provider": "codex" }
            ]
        }));
        state.apply_command(TuiCommand::ShowArenaOverlay);
        state.apply_command(TuiCommand::ArenaPrevious);

        assert_eq!(state.arena_selected_index(), 1);

        state.apply_daemon_status(&json!({
            "protocol_version": 3,
            "agents": [],
            "arena_candidates": [
                { "worktree_id": "wt_001", "provider": "claude" }
            ]
        }));

        assert_eq!(state.arena_selected_index(), 0);
        assert_eq!(
            state.apply_command(TuiCommand::ArenaAdopt),
            CommandEffect::ArenaAdopt("wt_001".to_string())
        );
    }

    #[cfg(feature = "arena")]
    #[test]
    fn daemon_status_seeds_arena_candidates() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "protocol_version": 3,
            "agents": [],
            "arena_candidates": [
                {
                    "worktree_id": "wt_001",
                    "agent_id": "agent_001",
                    "provider": "claude",
                    "diff_stat": "2 files changed",
                    "test_status": "failed"
                }
            ]
        }));

        assert_eq!(state.arena_candidates().len(), 1);
        assert_eq!(state.arena_candidates()[0].worktree_id, "wt_001");
        assert_eq!(state.arena_candidates()[0].test_status, "failed");
    }

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

    #[test]
    fn exited_event_removes_pane_and_moves_focus() {
        let mut state = TuiSessionState::default();
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_001", "name": "impl", "process_id": 42 }),
        ));
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_002", "name": "shell", "process_id": 43 }),
        ));

        let change = state.apply_event(&event(
            IpcEventKind::AgentExited,
            json!({ "agent_id": "agent_001", "exit_status": 0 }),
        ));

        assert_eq!(change, StateChange::RemovedPane("agent_001".to_string()));
        assert!(state.pane("agent_001").is_none());
        assert_eq!(state.layout().panes(), &["agent_002".to_string()]);
        assert_eq!(state.layout().focused(), Some("agent_002"));
    }

    #[test]
    fn malformed_or_unknown_events_do_not_mutate_panes() {
        let mut state = TuiSessionState::default();

        assert_eq!(
            state.apply_event(&event(
                IpcEventKind::AgentSpawned,
                json!({ "name": "missing" })
            )),
            StateChange::Ignored
        );
        assert_eq!(
            state.apply_event(&event(
                IpcEventKind::MessageCreated,
                json!({ "body": "missing id" })
            )),
            StateChange::Ignored
        );

        assert_eq!(state.layout().panes(), &Vec::<String>::new());
    }

    #[test]
    fn focus_next_previous_and_zoom_delegate_to_layout_state() {
        let mut state = TuiSessionState::default();
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_a", "name": "a" }),
        ));
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_b", "name": "b" }),
        ));
        state.layout_mut().focus("agent_a");

        state.focus_next();
        state.focus_previous();
        state.toggle_zoom();

        assert_eq!(state.layout().focused(), Some("agent_a"));
        assert!(state.layout().is_zoomed());
    }

    #[test]
    fn apply_prefix_commands_updates_state_or_returns_session_effect() {
        let mut state = TuiSessionState::default();
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_a", "name": "a" }),
        ));
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_b", "name": "b" }),
        ));
        state.layout_mut().focus("agent_a");

        assert_eq!(
            state.apply_command(TuiCommand::Focus(FocusDirection::Right)),
            CommandEffect::Continue
        );
        assert_eq!(state.layout().focused(), Some("agent_b"));

        assert_eq!(
            state.apply_command(TuiCommand::Focus(FocusDirection::Left)),
            CommandEffect::Continue
        );
        assert_eq!(state.layout().focused(), Some("agent_a"));

        assert_eq!(
            state.apply_command(TuiCommand::ToggleZoom),
            CommandEffect::Continue
        );
        assert!(state.layout().is_zoomed());
        assert_eq!(
            state.apply_command(TuiCommand::SplitVertical),
            CommandEffect::Continue
        );
        assert!(state.provider_picker_visible());
        assert_eq!(
            state.apply_command(TuiCommand::SelectProvider),
            CommandEffect::SpawnAgentPane(AgentProviderChoice::Claude)
        );
        assert!(!state.provider_picker_visible());
        assert_eq!(
            state.provider_options()[3].choice,
            NewPaneChoice::ConversationList
        );
        assert_eq!(
            state.apply_command(TuiCommand::ClosePane),
            CommandEffect::StopPane("agent_a".to_string())
        );
        assert_eq!(
            state.apply_command(TuiCommand::RotateLayout),
            CommandEffect::Continue
        );
        assert_eq!(
            state.layout().split_direction(),
            crate::layout::SplitDirection::Horizontal
        );
        assert_eq!(
            state.apply_command(TuiCommand::Detach),
            CommandEffect::Detach
        );
        assert_eq!(state.apply_command(TuiCommand::Quit), CommandEffect::Quit);
        assert_eq!(
            state.apply_command(TuiCommand::Help),
            CommandEffect::Continue
        );
        assert!(state.keybinding_help_visible());
        assert_eq!(
            state.apply_command(TuiCommand::Help),
            CommandEffect::Continue
        );
        assert!(!state.keybinding_help_visible());
        assert_eq!(
            state.apply_command(TuiCommand::ShowSessionList),
            CommandEffect::Continue
        );
        assert!(state.session_list_visible());
        assert!(!state.keybinding_help_visible());
        assert_eq!(
            state.apply_command(TuiCommand::ShowSessionList),
            CommandEffect::Continue
        );
        assert!(!state.session_list_visible());
        assert_eq!(
            state.apply_command(TuiCommand::ShowMessageBus),
            CommandEffect::RefreshMessages
        );
        assert!(state.message_bus_visible());
        assert_eq!(
            state.apply_command(TuiCommand::CloseOverlay),
            CommandEffect::Continue
        );
        assert!(!state.message_bus_visible());
    }

    #[test]
    fn provider_picker_can_open_and_close_conversation_list_pane() {
        let mut state = TuiSessionState::default();
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_a", "name": "a" }),
        ));
        state.layout_mut().focus("agent_a");
        state.open_provider_picker();
        state.apply_command(TuiCommand::ProviderPrevious);

        assert_eq!(
            state.apply_command(TuiCommand::SelectProvider),
            CommandEffect::OpenConversationListPane
        );
        assert!(!state.provider_picker_visible());
        assert!(state.is_conversation_list_pane(CONVERSATION_LIST_PANE_ID));
        assert_eq!(state.layout().focused(), Some(CONVERSATION_LIST_PANE_ID));

        assert!(!state.message_details_visible());
        assert_eq!(
            state.apply_command(TuiCommand::ToggleMessageDetails),
            CommandEffect::Continue
        );
        assert!(state.message_details_visible());

        assert_eq!(
            state.apply_command(TuiCommand::ClosePane),
            CommandEffect::Continue
        );
        assert!(!state.is_conversation_list_pane(CONVERSATION_LIST_PANE_ID));
        assert_eq!(state.layout().focused(), Some("agent_a"));
    }

    #[test]
    fn session_list_selection_focuses_selected_running_session() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "agents": [
                {
                    "id": "agent_a",
                    "name": "a",
                    "process_id": 100
                },
                {
                    "id": "agent_b",
                    "name": "b",
                    "process_id": 200
                },
                {
                    "id": "agent_restored",
                    "name": "restored",
                    "process_id": null
                }
            ]
        }));
        state.layout_mut().focus("agent_a");

        assert_eq!(
            state.apply_command(TuiCommand::ShowSessionList),
            CommandEffect::Continue
        );
        assert!(state.session_list_visible());
        assert_eq!(state.session_list_selected_index(), 0);

        assert_eq!(
            state.apply_command(TuiCommand::SessionListNext),
            CommandEffect::Continue
        );
        assert_eq!(state.session_list_selected_index(), 1);
        assert_eq!(
            state.apply_command(TuiCommand::FocusSelectedSession),
            CommandEffect::Continue
        );

        assert_eq!(state.layout().focused(), Some("agent_b"));
        assert!(!state.session_list_visible());
    }

    #[test]
    fn session_list_selection_wraps_and_closes() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "agents": [
                {
                    "id": "agent_a",
                    "name": "a",
                    "process_id": 100
                },
                {
                    "id": "agent_b",
                    "name": "b",
                    "process_id": 200
                }
            ]
        }));

        state.apply_command(TuiCommand::ShowSessionList);
        state.apply_command(TuiCommand::SessionListPrevious);
        assert_eq!(state.session_list_selected_index(), 1);

        state.apply_command(TuiCommand::CloseOverlay);
        assert!(!state.session_list_visible());
    }

    #[test]
    fn daemon_status_payload_seeds_agent_panes_in_daemon_order() {
        let mut state = TuiSessionState::default();

        let applied = state.apply_daemon_status(&json!({
            "agents": [
                {
                    "id": "agent-a",
                    "name": "planner",
                    "process_id": 7,
                    "status": "interactive_ready"
                },
                {
                    "id": "agent-b",
                    "name": "impl",
                    "process_id": null
                },
                {
                    "name": "malformed"
                }
            ]
        }));

        assert_eq!(applied, 2);
        assert_eq!(
            state.layout().panes(),
            &["agent-a".to_owned(), "agent-b".to_owned()]
        );
        assert_eq!(state.layout().focused(), Some("agent-a"));

        let first = state.pane("agent-a").expect("first pane exists");
        assert_eq!(first.name(), "planner");
        assert_eq!(first.process_id(), Some(7));
        assert_eq!(first.status(), Some("interactive_ready"));
        assert_eq!(first.last_event(), None);

        let second = state.pane("agent-b").expect("second pane exists");
        assert_eq!(second.name(), "impl");
        assert_eq!(second.process_id(), None);
    }

    #[test]
    fn daemon_status_payload_updates_existing_panes_without_reordering() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "agents": [
                {"id": "a", "name": "old-a"},
                {"id": "b", "name": "old-b"}
            ]
        }));

        let applied = state.apply_daemon_status(&json!({
            "agents": [
                {"id": "b", "name": "new-b", "process_id": 9},
                {"id": "a", "name": "new-a", "status": "busy"}
            ]
        }));

        assert_eq!(applied, 2);
        assert_eq!(state.layout().panes(), &["a".to_owned(), "b".to_owned()]);
        assert_eq!(state.pane("a").expect("a pane").name(), "new-a");
        assert_eq!(state.pane("a").expect("a pane").status(), Some("busy"));
        assert_eq!(state.pane("b").expect("b pane").name(), "new-b");
        assert_eq!(state.pane("b").expect("b pane").process_id(), Some(9));
    }

    #[test]
    fn message_list_payload_updates_message_bus_state_newest_first() {
        let mut state = TuiSessionState::default();

        let applied = state.apply_message_list_payload(&json!({
            "messages": [
                {
                    "message_id": "msg_old",
                    "created_at": "2026-06-04T01:00:00+00:00",
                    "delivery_status": "queued",
                    "kind": "handoff",
                    "from": { "kind": "agent", "id": "planner" },
                    "to": { "kind": "agent", "id": "impl" },
                    "body": "old"
                },
                {
                    "message_id": "msg_new",
                    "created_at": "2026-06-04T02:00:00+00:00",
                    "delivery_status": "delivered",
                    "kind": "test_result",
                    "from": { "kind": "orchestrator" },
                    "to": { "kind": "role", "id": "tester" },
                    "body": "new"
                }
            ]
        }));

        assert_eq!(applied, 2);
        assert_eq!(state.messages()[0].message_id, "msg_new");
        assert_eq!(state.messages()[0].from, "orchestrator");
        assert_eq!(state.messages()[0].to, "role:tester");
    }

    #[test]
    fn message_events_upsert_message_bus_state() {
        let mut state = TuiSessionState::default();

        assert_eq!(
            state.apply_event(&event(
                IpcEventKind::MessageCreated,
                json!({
                    "message_id": "msg_1",
                    "created_at": "2026-06-04T01:00:00+00:00",
                    "delivery_status": "queued",
                    "kind": "handoff",
                    "from": { "kind": "agent", "id": "planner" },
                    "to": { "kind": "agent", "id": "impl" },
                    "body": "review this"
                })
            )),
            StateChange::UpdatedMessages
        );
        assert_eq!(state.messages()[0].delivery_status, "queued");

        assert_eq!(
            state.apply_event(&event(
                IpcEventKind::MessageDelivered,
                json!({
                    "message_id": "msg_1",
                    "created_at": "2026-06-04T01:00:00+00:00",
                    "delivery_status": "delivered",
                    "kind": "handoff",
                    "from": { "kind": "agent", "id": "planner" },
                    "to": { "kind": "agent", "id": "impl" },
                    "body": "review this"
                })
            )),
            StateChange::UpdatedMessages
        );
        assert_eq!(state.messages().len(), 1);
        assert_eq!(state.messages()[0].delivery_status, "delivered");
    }
