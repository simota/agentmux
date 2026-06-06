use super::*;

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

