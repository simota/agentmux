use super::*;
#[cfg(feature = "activity-feed")]
use agentmux_ipc::EVENT_SUBSCRIBE_PROTOCOL_VERSION;

    #[test]
    fn agent_resize_request_uses_resize_ipc_command() {
        let request = agent_resize_request(
            "req_resize_1".to_string(),
            "agent_001".to_string(),
            TuiTerminalSize { rows: 22, cols: 78 },
        );

        assert_eq!(request.id, "req_resize_1");
        assert_eq!(request.command, IpcCommand::AgentResize);
        assert_eq!(
            request.payload,
            json!({ "agent_id": "agent_001", "rows": 22, "cols": 78 })
        );
    }

    #[test]
    fn resize_pane_sizes_use_inner_pane_dimensions() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "agents": [
                {"id": "agent_a", "name": "a"},
                {"id": "agent_b", "name": "b"}
            ]
        }));

        let sizes = resize_pane_sizes(&state, 100, 24);

        assert_eq!(
            sizes,
            vec![
                (
                    "agent_a".to_string(),
                    TuiTerminalSize { rows: 22, cols: 48 }
                ),
                (
                    "agent_b".to_string(),
                    TuiTerminalSize { rows: 22, cols: 48 }
                ),
            ]
        );
    }

    #[test]
    fn resize_pane_sizes_ignore_local_conversation_list_pane() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "agents": [
                {"id": "agent_a", "name": "a"}
            ]
        }));
        state.open_conversation_list_pane();

        let sizes = resize_pane_sizes(&state, 100, 24);

        assert_eq!(
            sizes,
            vec![(
                "agent_a".to_string(),
                TuiTerminalSize { rows: 22, cols: 48 }
            )]
        );
    }

    #[test]
    fn pending_spawn_pane_size_matches_hypothetical_new_pane_inner_dimensions() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "agents": [
                {"id": "agent_a", "name": "a"}
            ]
        }));

        let size = pending_spawn_pane_size(&state, 100, 24);

        assert_eq!(size, Some(TuiTerminalSize { rows: 22, cols: 48 }));
    }

    #[test]
    fn pending_spawn_pane_size_uses_full_inner_area_when_first_pane() {
        let state = TuiSessionState::default();

        let size = pending_spawn_pane_size(&state, 100, 24);

        assert_eq!(size, Some(TuiTerminalSize { rows: 22, cols: 98 }));
    }

    #[test]
    fn resize_panes_for_terminal_updates_state_and_returns_resize_requests() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "agents": [
                {"id": "agent_a", "name": "a"}
            ]
        }));
        let mut sequence = 7;

        let requests = resize_panes_for_terminal(&mut state, 90, 30, &mut sequence);

        assert_eq!(sequence, 8);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].id, "req_resize_8");
        assert_eq!(
            requests[0].payload,
            json!({ "agent_id": "agent_a", "rows": 28, "cols": 88 })
        );
        let pane = state.pane("agent_a").expect("pane");
        assert_eq!(pane.grid().rows(), 28);
        assert_eq!(pane.grid().cols(), 88);
    }

    #[test]
    fn mouse_scroll_helpers_target_hovered_pane() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "agents": [
                {"id": "agent_a", "name": "a"},
                {"id": "agent_b", "name": "b"}
            ]
        }));

        assert_eq!(mouse_scroll_delta(MouseEventKind::ScrollUp), Some(3));
        assert_eq!(mouse_scroll_delta(MouseEventKind::ScrollDown), Some(-3));
        assert!(scroll_pane_at(&mut state, 100, 24, 75, 2, 3));
        assert_eq!(state.pane("agent_a").expect("pane a").scroll_offset(), 0);
        assert_eq!(state.pane("agent_b").expect("pane b").scroll_offset(), 3);

        assert!(scroll_pane_at(&mut state, 100, 24, 75, 2, -1));
        assert_eq!(state.pane("agent_b").expect("pane b").scroll_offset(), 2);
    }

    #[test]
    fn copy_mode_drag_targets_only_focused_pane_inner_area() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "agents": [
                {"id": "agent_a", "name": "a"},
                {"id": "agent_b", "name": "b"}
            ]
        }));
        assert!(state.layout_mut().focus("agent_b"));
        state.resize_pane("agent_b", TuiTerminalSize { rows: 3, cols: 8 });
        state.apply_event(&agentmux_ipc::DaemonEvent::new(
            agentmux_ipc::protocol::IpcEventKind::PtyOutputChunk,
            json!({ "agent_id": "agent_b", "text": "alpha\nbeta\n" }),
        ));

        let inner = focused_pane_inner_rect(&state, 20, 5).expect("focused inner rect");
        assert_eq!(inner, Rect::new(11, 1, 8, 3));
        let mut drag_start = None;

        assert_eq!(
            copy_mode_mouse_action(
                &mut state,
                20,
                5,
                MouseEventKind::Down(MouseButton::Left),
                1,
                1,
                &mut drag_start,
            ),
            None
        );
        assert!(state.copy_selection().is_none());

        assert_eq!(
            copy_mode_mouse_action(
                &mut state,
                20,
                5,
                MouseEventKind::Down(MouseButton::Left),
                inner.x + 1,
                inner.y,
                &mut drag_start,
            ),
            Some(CopyModeAction::Redraw)
        );
        assert_eq!(
            copy_mode_mouse_action(
                &mut state,
                20,
                5,
                MouseEventKind::Up(MouseButton::Left),
                inner.x + 3,
                inner.y + 1,
                &mut drag_start,
            ),
            Some(CopyModeAction::CopyAndExit("lpha\nbeta".to_string()))
        );
    }

    #[test]
    fn tui_stream_frame_seeds_status_and_applies_events() {
        let mut state = TuiSessionState::default();
        let status = DaemonResponse::ok(
            "req_tui_status",
            json!({
                "agents": [
                    {
                        "id": "agent_01KBKX3F4SPGZ1A0JMQJEFAV7B",
                        "name": "impl",
                        "status": "ready"
                    }
                ]
            }),
        );

        let response_id =
            apply_tui_stream_frame(&mut state, DaemonStreamFrame::Response(status)).unwrap();

        assert_eq!(response_id.as_deref(), Some("req_tui_status"));
        assert_eq!(
            state.layout().focused(),
            Some("agent_01KBKX3F4SPGZ1A0JMQJEFAV7B")
        );
        assert_eq!(
            state
                .pane("agent_01KBKX3F4SPGZ1A0JMQJEFAV7B")
                .and_then(|pane| pane.status()),
            Some("ready")
        );

        apply_tui_stream_frame(
            &mut state,
            DaemonStreamFrame::Event(agentmux_ipc::DaemonEvent::new(
                agentmux_ipc::IpcEventKind::PtyOutputChunk,
                json!({
                    "agent_id": "agent_01KBKX3F4SPGZ1A0JMQJEFAV7B",
                    "text": "hi"
                }),
            )),
        )
        .unwrap();

        assert_eq!(
            state
                .pane("agent_01KBKX3F4SPGZ1A0JMQJEFAV7B")
                .unwrap()
                .grid()
                .line_text(0)
                .unwrap()
                .trim_end(),
            "hi"
        );
    }

    #[test]
    fn tui_stream_frame_updates_message_bus_from_message_list_response() {
        let mut state = TuiSessionState::default();
        let response = DaemonResponse::ok(
            "req_message_list",
            json!({
                "messages": [
                    {
                        "message_id": "msg_1",
                        "created_at": "2026-06-04T02:00:00+00:00",
                        "delivery_status": "delivered",
                        "kind": "handoff",
                        "from": { "kind": "agent", "id": "planner" },
                        "to": { "kind": "agent", "id": "impl" },
                        "body": "continue"
                    }
                ]
            }),
        );

        let response_id =
            apply_tui_stream_frame(&mut state, DaemonStreamFrame::Response(response)).unwrap();

        assert_eq!(response_id.as_deref(), Some("req_message_list"));
        assert_eq!(state.messages().len(), 1);
        assert_eq!(state.messages()[0].message_id, "msg_1");
    }

    #[test]
    fn tui_stream_frame_adds_spawned_provider_agent() {
        let mut state = TuiSessionState::default();
        let response = DaemonResponse::ok(
            "req_agent_spawn_provider",
            json!({
                "agent_id": "agent_01KBQ4Y8T3BHQP4FPY6Y1VPD2K",
                "name": "codex",
                "process_id": 42
            }),
        );

        let frame = DaemonStreamFrame::Response(response);
        assert_eq!(
            spawned_agent_id_from_frame(&frame).as_deref(),
            Some("agent_01KBQ4Y8T3BHQP4FPY6Y1VPD2K")
        );

        let response_id = apply_tui_stream_frame(&mut state, frame).unwrap();

        assert_eq!(response_id.as_deref(), Some("req_agent_spawn_provider"));
        assert_eq!(
            state.layout().focused(),
            Some("agent_01KBQ4Y8T3BHQP4FPY6Y1VPD2K")
        );
        assert_eq!(
            state
                .pane("agent_01KBQ4Y8T3BHQP4FPY6Y1VPD2K")
                .expect("spawned pane")
                .name(),
            "codex"
        );
    }

    #[test]
    fn tui_stream_frame_restores_snapshot_response() {
        let mut state = TuiSessionState::default();
        let snapshot = DaemonResponse::ok(
            "req_snapshot",
            json!({
                "agent_id": "agent_01KBKX3F4SPGZ1A0JMQJEFAV7B",
                "name": "impl",
                "rows": 2,
                "cols": 4,
                "lines": ["done", ">   "]
            }),
        );

        let response_id =
            apply_tui_stream_frame(&mut state, DaemonStreamFrame::Response(snapshot)).unwrap();

        assert_eq!(response_id.as_deref(), Some("req_snapshot"));
        assert_eq!(
            state
                .pane("agent_01KBKX3F4SPGZ1A0JMQJEFAV7B")
                .unwrap()
                .grid()
                .line_text(0)
                .unwrap(),
            "done"
        );
    }

    #[test]
    fn tui_stream_frame_returns_daemon_response_errors() {
        let mut state = TuiSessionState::default();
        let response = DaemonResponse::error(
            "req_attach",
            agentmux_ipc::ErrorBody::new("not_found", "agent missing"),
        );

        let error =
            apply_tui_stream_frame(&mut state, DaemonStreamFrame::Response(response)).unwrap_err();

        assert!(error.to_string().contains("agent missing"));
        assert!(error.to_string().contains("not_found"));
    }

    #[test]
    fn runtime_input_failure_is_non_fatal() {
        // A keystroke forwarded to an agent with no live PTY returns an error
        // response; during the interactive loop this must be a soft notice, not
        // a session-killing error.
        let mut state = TuiSessionState::default();
        let response = DaemonResponse::error(
            "req_input_1",
            agentmux_ipc::ErrorBody::new("INPUT_SCRIPT_FAILED", "agent has no live PTY"),
        );

        let notice = apply_runtime_stream_frame(&mut state, DaemonStreamFrame::Response(response));

        let notice = notice.expect("runtime failure is surfaced as a notice");
        assert!(notice.contains("no live PTY"));
        assert!(
            state
                .runtime_notice()
                .is_some_and(|notice| notice.contains("no live PTY"))
        );
    }

    #[cfg(feature = "activity-feed")]
    #[test]
    fn activity_feed_subscribe_is_gated_by_daemon_protocol_version() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "protocol_version": EVENT_SUBSCRIBE_PROTOCOL_VERSION - 1,
            "agents": []
        }));

        assert!(!daemon_supports_event_subscribe(&state));

        state.set_runtime_notice("Activity Feed unsupported by this daemon");
        assert_eq!(
            state.runtime_notice(),
            Some("Activity Feed unsupported by this daemon")
        );

        state.apply_daemon_status(&json!({
            "protocol_version": EVENT_SUBSCRIBE_PROTOCOL_VERSION,
            "agents": []
        }));
        assert!(daemon_supports_event_subscribe(&state));
    }

    #[cfg(feature = "arena")]
    #[test]
    fn arena_actions_are_gated_by_daemon_protocol_version() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "protocol_version": ARENA_PROTOCOL_VERSION - 1,
            "agents": []
        }));

        assert!(!daemon_supports_arena_state(&state));

        state.set_runtime_notice("Arena unsupported by this daemon");
        assert_eq!(
            state.runtime_notice(),
            Some("Arena unsupported by this daemon")
        );

        state.apply_daemon_status(&json!({
            "protocol_version": ARENA_PROTOCOL_VERSION,
            "agents": []
        }));
        assert!(daemon_supports_arena_state(&state));
    }

    #[test]
    fn commands_input_key_maps_editor_actions() {
        use crossterm::event::KeyCode;
        assert_eq!(
            commands_input_key(KeyCode::Enter),
            Some(CommandsInputAction::Send)
        );
        assert_eq!(
            commands_input_key(KeyCode::Tab),
            Some(CommandsInputAction::CycleTarget)
        );
        assert_eq!(
            commands_input_key(KeyCode::Esc),
            Some(CommandsInputAction::Clear)
        );
        assert_eq!(
            commands_input_key(KeyCode::Backspace),
            Some(CommandsInputAction::Backspace)
        );
        assert_eq!(
            commands_input_key(KeyCode::Char('a')),
            Some(CommandsInputAction::Insert('a'))
        );
        assert_eq!(commands_input_key(KeyCode::Left), None);
    }

    #[test]
    fn tui_stream_frame_records_broadcast_response_in_commands_history() {
        let mut state = TuiSessionState::default();
        state.commands_input_push('h');
        state.begin_commands_broadcast("role:tester", "hi");

        let response = DaemonResponse::ok(
            "req_agent_broadcast_input",
            json!({
                "delivered": ["a1", "a2"],
                "skipped": ["a3"]
            }),
        );
        let response_id =
            apply_tui_stream_frame(&mut state, DaemonStreamFrame::Response(response)).unwrap();

        assert_eq!(response_id.as_deref(), Some("req_agent_broadcast_input"));
        let history = state.commands_history();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].target, "role:tester");
        assert_eq!(history[0].text, "hi");
        assert_eq!(
            history[0].kind,
            agentmux_tui::state::CommandsLogKind::Broadcast {
                delivered: 2,
                skipped: 1
            }
        );
    }

    #[test]
    fn tui_stream_frame_records_role_assign_response_in_commands_history() {
        let mut state = TuiSessionState::default();
        state.apply_event(&agentmux_ipc::DaemonEvent::new(
            agentmux_ipc::protocol::IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_foo", "name": "foo", "role": "implementer", "process_id": 7 }),
        ));
        // Point the broadcast target at the live session before assigning.
        while state.commands_target() != "agent:foo" {
            state.cycle_commands_target();
        }
        state.begin_commands_role_assign("qa-lead");

        let response = DaemonResponse::ok(
            "req_agent_set_role",
            json!({ "agent_id": "agent_foo", "role": "qa-lead" }),
        );
        let response_id =
            apply_tui_stream_frame(&mut state, DaemonStreamFrame::Response(response)).unwrap();

        assert_eq!(response_id.as_deref(), Some("req_agent_set_role"));
        let history = state.commands_history();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].target, "agent:foo");
        assert_eq!(
            history[0].kind,
            agentmux_tui::state::CommandsLogKind::RoleAssigned {
                role: "qa-lead".to_string()
            }
        );
    }
