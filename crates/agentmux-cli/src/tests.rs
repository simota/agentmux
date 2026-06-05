//! Unit tests for the `agentmux` CLI (moved verbatim from `main.rs`).

use std::path::PathBuf;

use agentmux_core::{AgentmuxConfig, AgentmuxError, error::Result};
use agentmux_ipc::{ClientRequest, DaemonResponse, DaemonStreamFrame, IpcCommand, PROTOCOL_VERSION};
use agentmux_tui::layout::Rect;
use agentmux_tui::state::{
    AgentProviderChoice, CommandEffect, TerminalSize as TuiTerminalSize, TuiSessionState,
};
use crossterm::event::{MouseButton, MouseEventKind};
use serde_json::{Value, json};

use crate::*;

fn bare_session_spawn_request() -> ClientRequest {
    agent_spawn_for_provider_request(AgentProviderChoice::Codex)
}

fn agent_spawn_for_provider_request(provider: AgentProviderChoice) -> ClientRequest {
    agent_spawn_for_provider_request_with_size(provider, None)
}

fn agent_id_from_spawn_response(response: DaemonResponse) -> Result<String> {
    if !response.ok {
        return Err(response_error("agent.spawn", response));
    }

    response
        .payload
        .and_then(|payload| {
            payload
                .get("agent_id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .ok_or_else(|| AgentmuxError::IpcError("agent.spawn response missing agent_id".to_string()))
}


    #[test]
    fn daemon_pid_path_sits_next_to_socket() {
        let socket = PathBuf::from("/tmp/agentmux-test/agentmux.sock");
        assert_eq!(
            daemon_pid_path(&socket),
            Some(PathBuf::from("/tmp/agentmux-test/agentmux.pid"))
        );
    }

    #[test]
    fn resolve_daemon_binary_falls_back_to_bare_name() {
        // Always returns a non-empty program name (sibling path or PATH fallback).
        assert!(!resolve_daemon_binary().is_empty());
    }

    #[test]
    fn task_run_request_matches_spec_payload() {
        #[cfg(not(feature = "arena"))]
        let request = task_run_request(
            "refresh token bugを修正".to_string(),
            Some("claude-codex".to_string()),
        )
        .unwrap();
        #[cfg(feature = "arena")]
        let request = task_run_request(
            "refresh token bugを修正".to_string(),
            Some("claude-codex".to_string()),
            None,
            None,
        )
        .unwrap();

        assert_eq!(request.version, PROTOCOL_VERSION);
        assert_eq!(request.command, IpcCommand::TaskRun);
        assert_eq!(request.payload["body"], "refresh token bugを修正");
        assert_eq!(request.payload["team"], "claude-codex");
        assert!(request.payload["project_path"].as_str().is_some());
    }

    #[test]
    fn task_run_defaults_to_claude_codex_team() {
        #[cfg(not(feature = "arena"))]
        let request = task_run_request("fix failing tests".to_string(), None).unwrap();
        #[cfg(feature = "arena")]
        let request = task_run_request("fix failing tests".to_string(), None, None, None).unwrap();

        assert_eq!(request.payload["team"], "claude-codex");
    }

    #[cfg(feature = "arena")]
    #[test]
    fn arena_task_run_request_sets_runner_and_providers() {
        let request = task_run_request(
            "compare implementations".to_string(),
            None,
            Some("claude,codex".to_string()),
            Some("main".to_string()),
        )
        .unwrap();

        assert_eq!(request.command, IpcCommand::TaskRun);
        assert_eq!(request.payload["runner"], "arena");
        assert_eq!(request.payload["providers"][0], "claude");
        assert_eq!(request.payload["providers"][1], "codex");
        assert_eq!(request.payload["base_branch"], "main");
    }

    #[cfg(feature = "arena")]
    #[test]
    fn arena_task_run_request_rejects_empty_provider_list() {
        let error = task_run_request(
            "compare implementations".to_string(),
            None,
            Some(" , , ".to_string()),
            None,
        )
        .expect_err("empty arena provider list is rejected");

        assert!(error.to_string().contains("at least one provider"));
    }

    #[cfg(feature = "arena")]
    #[test]
    fn arena_task_run_request_allows_single_provider() {
        let request = task_run_request(
            "compare implementation".to_string(),
            None,
            Some("claude".to_string()),
            None,
        )
        .unwrap();

        assert_eq!(request.payload["runner"], "arena");
        assert_eq!(request.payload["providers"].as_array().unwrap().len(), 1);
        assert_eq!(request.payload["providers"][0], "claude");
    }

    #[cfg(feature = "arena")]
    #[test]
    fn arena_adopt_request_targets_worktree_adopt_ipc() {
        let request = worktree_adopt_request("wt_01HX".to_string());

        assert_eq!(request.command, IpcCommand::WorktreeAdopt);
        assert_eq!(request.payload["worktree_id"], "wt_01HX");
    }

    #[test]
    fn attach_request_targets_agent_session_for_daemon_ipc() {
        let request = attach_request("agent_01HX".to_string());

        assert_eq!(request.command, IpcCommand::ClientAttach);
        assert_eq!(request.payload["agent_id"], "agent_01HX");
    }

    #[test]
    fn tui_bootstrap_requests_status_attach_and_snapshot() {
        let status = tui_daemon_status_request();
        let attach = attach_request("agent_01HX".to_string());
        let snapshot = snapshot_request("agent_01HX".to_string());

        assert_eq!(status.id, "req_tui_status");
        assert_eq!(status.command, IpcCommand::DaemonStatus);
        assert_eq!(status.payload, json!({}));
        assert_eq!(attach.id, "req_attach");
        assert_eq!(attach.command, IpcCommand::ClientAttach);
        assert_eq!(attach.payload["agent_id"], "agent_01HX");
        assert_eq!(snapshot.id, "req_snapshot");
        assert_eq!(snapshot.command, IpcCommand::AgentSnapshot);
        assert_eq!(snapshot.payload["agent_id"], "agent_01HX");
    }

    #[test]
    fn bare_session_spawn_request_registers_default_coding_agent() {
        let request = bare_session_spawn_request();

        assert_eq!(request.id, "req_agent_spawn_provider");
        assert_eq!(request.command, IpcCommand::AgentSpawn);
        assert_eq!(request.payload["provider"], "codex");
        assert_eq!(request.payload["role"], "implementer");
        let name = request.payload["name"].as_str().unwrap();
        assert!(name.starts_with("codex-"));
        assert_eq!(name.len(), "codex-".len() + 6);
    }

    #[test]
    fn provider_spawn_request_registers_selected_coding_agent() {
        let request = agent_spawn_for_provider_request(AgentProviderChoice::Agy);

        assert_eq!(request.id, "req_agent_spawn_provider");
        assert_eq!(request.command, IpcCommand::AgentSpawn);
        assert_eq!(request.payload["provider"], "agy");
        assert_eq!(request.payload["role"], "implementer");
        let name = request.payload["name"].as_str().unwrap();
        assert!(name.starts_with("agy-"));
        assert_eq!(name.len(), "agy-".len() + 6);
    }

    #[test]
    fn provider_spawn_request_can_include_initial_pty_size() {
        let request = agent_spawn_for_provider_request_with_size(
            AgentProviderChoice::Codex,
            Some(TuiTerminalSize { rows: 28, cols: 88 }),
        );

        assert_eq!(request.id, "req_agent_spawn_provider");
        assert_eq!(request.command, IpcCommand::AgentSpawn);
        assert_eq!(request.payload["provider"], "codex");
        assert_eq!(request.payload["size"], json!({ "rows": 28, "cols": 88 }));
    }

    #[test]
    fn start_command_accepts_comma_separated_providers() {
        let cli = Cli::try_parse_from(["agentmux", "start", "agy,messages,codex"]).unwrap();
        let Some(Commands::Start(args)) = cli.command else {
            panic!("expected start command");
        };

        assert_eq!(
            parse_start_panes(args.providers.as_deref()).unwrap(),
            vec![
                StartupPaneChoice::Agent(AgentProviderChoice::Agy),
                StartupPaneChoice::Messages,
                StartupPaneChoice::Agent(AgentProviderChoice::Codex)
            ]
        );
    }

    #[test]
    fn startup_spawn_request_uses_trackable_response_id() {
        let request = agent_spawn_for_provider_request_with_id(
            "req_start_agent_spawn_0",
            AgentProviderChoice::Agy,
            None,
        );

        assert_eq!(request.id, "req_start_agent_spawn_0");
        assert!(is_agent_spawn_response_id(&request.id));
        assert_eq!(request.command, IpcCommand::AgentSpawn);
        assert_eq!(request.payload["provider"], "agy");

        let frame = DaemonStreamFrame::Response(DaemonResponse::ok(
            "req_start_agent_spawn_0",
            json!({ "agent_id": "agent_01KBQ4Y8T3BHQP4FPY6Y1VPD2K" }),
        ));
        assert_eq!(
            spawned_agent_id_from_frame(&frame).as_deref(),
            Some("agent_01KBQ4Y8T3BHQP4FPY6Y1VPD2K")
        );
    }

    #[test]
    fn bare_spawn_response_yields_agent_id_for_tui_attach() {
        let agent_id = agent_id_from_spawn_response(DaemonResponse::ok(
            "req_bare_agent_spawn",
            json!({ "agent_id": "agent_01KBQ4Y8T3BHQP4FPY6Y1VPD2K" }),
        ))
        .unwrap();

        assert_eq!(agent_id, "agent_01KBQ4Y8T3BHQP4FPY6Y1VPD2K");
    }

    #[test]
    fn bare_spawn_response_requires_agent_id() {
        let error =
            agent_id_from_spawn_response(DaemonResponse::ok("req_bare_agent_spawn", json!({})))
                .unwrap_err();

        assert!(error.to_string().contains("missing agent_id"));
    }

    #[test]
    fn detach_request_uses_client_detach_ipc_command() {
        let request = detach_request();

        assert_eq!(request.id, "req_detach");
        assert_eq!(request.command, IpcCommand::ClientDetach);
        assert_eq!(request.payload, json!({}));
    }

    #[test]
    fn sigint_requests_tui_detach_for_terminal_restoring_shutdown_path() {
        assert_eq!(tui_signal_effect(TuiSignal::Sigint), CommandEffect::Detach);
    }

    #[test]
    fn quit_closes_tui_client_without_stopping_agent_sessions() {
        let request = tui_close_request(CommandEffect::Quit).expect("close request");

        assert_eq!(request.command, IpcCommand::ClientDetach);
        assert_eq!(request.payload, json!({}));
    }

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
    fn message_requests_target_daemon_ipc() {
        let list = message_list_request();
        assert_eq!(list.command, IpcCommand::MessageList);

        let show = message_show_request("msg_01HX".to_string());
        assert_eq!(show.command, IpcCommand::MessageShow);
        assert_eq!(show.payload["message_id"], "msg_01HX");

        let send = message_send_request("agent_01HX".to_string(), "hello".to_string(), None, None)
            .unwrap();
        assert_eq!(send.command, IpcCommand::MessageCreate);
        assert_eq!(send.payload["to"], "agent_01HX");
        assert_eq!(send.payload["body"], "hello");
        assert_eq!(send.payload["kind"], "handoff");
        assert_eq!(send.payload["delivery_mode"], "inject_when_idle");

        let inject = message_inject_request("msg_01HX".to_string());
        assert_eq!(inject.command, IpcCommand::MessageInject);
        assert_eq!(inject.payload["message_id"], "msg_01HX");
    }

    #[test]
    fn send_commands_default_to_inject_and_accept_override_flags() {
        let cli =
            Cli::try_parse_from(["agentmux", "agent", "send", "agent_01HX", "hello"]).unwrap();
        let Some(Commands::Agent(args)) = cli.command else {
            panic!("expected agent command");
        };
        let AgentAction::Send {
            inject, no_inject, ..
        } = args.action
        else {
            panic!("expected agent send action");
        };
        assert!(should_inject_message(inject, no_inject));

        let cli = Cli::try_parse_from([
            "agentmux",
            "agent",
            "send",
            "--inject",
            "agent_01HX",
            "hello",
        ])
        .unwrap();
        let Some(Commands::Agent(args)) = cli.command else {
            panic!("expected agent command");
        };
        let AgentAction::Send {
            inject, no_inject, ..
        } = args.action
        else {
            panic!("expected agent send action");
        };
        assert!(should_inject_message(inject, no_inject));

        let cli = Cli::try_parse_from([
            "agentmux",
            "message",
            "send",
            "--inject",
            "--to",
            "agent:agent_01HX",
            "hello",
        ])
        .unwrap();
        let Some(Commands::Message(args)) = cli.command else {
            panic!("expected message command");
        };
        let MessageAction::Send {
            inject, no_inject, ..
        } = args.action
        else {
            panic!("expected message send action");
        };
        assert!(should_inject_message(inject, no_inject));

        let cli = Cli::try_parse_from([
            "agentmux",
            "message",
            "send",
            "--no-inject",
            "--to",
            "agent:agent_01HX",
            "hello",
        ])
        .unwrap();
        let Some(Commands::Message(args)) = cli.command else {
            panic!("expected message command");
        };
        let MessageAction::Send {
            inject, no_inject, ..
        } = args.action
        else {
            panic!("expected message send action");
        };
        assert!(!should_inject_message(inject, no_inject));
    }

    #[test]
    fn message_send_accepts_kind_and_priority_flags() {
        let cli = Cli::try_parse_from([
            "agentmux",
            "message",
            "send",
            "--to",
            "role:tester",
            "--kind",
            "Question",
            "--priority",
            "high",
            "ask something",
        ])
        .unwrap();
        let Some(Commands::Message(args)) = cli.command else {
            panic!("expected message command");
        };
        let MessageAction::Send {
            to,
            kind,
            priority,
            body,
            ..
        } = args.action
        else {
            panic!("expected message send action");
        };
        assert_eq!(to.as_deref(), Some("role:tester"));
        assert_eq!(kind.as_deref(), Some("Question"));
        assert_eq!(priority.as_deref(), Some("high"));

        let request = message_send_request(to.unwrap(), body, kind, priority).unwrap();
        assert_eq!(request.payload["kind"], "question");
        assert_eq!(request.payload["priority"], "high");
    }

    #[test]
    fn message_send_accepts_thread_flag_without_to() {
        let cli = Cli::try_parse_from([
            "agentmux",
            "message",
            "send",
            "--thread",
            "thread_01HXAMPLE0000000000000000",
            "--kind",
            "Finding",
            "私の意見です",
        ])
        .unwrap();
        let Some(Commands::Message(args)) = cli.command else {
            panic!("expected message command");
        };
        let MessageAction::Send { to, thread, .. } = args.action else {
            panic!("expected message send action");
        };
        assert_eq!(to, None);
        assert_eq!(thread.as_deref(), Some("thread_01HXAMPLE0000000000000000"));

        // --to も --thread も無い場合はパースエラー。
        assert!(
            Cli::try_parse_from(["agentmux", "message", "send", "orphan body"]).is_err(),
            "send without --to/--thread must be rejected"
        );
    }

    #[test]
    fn meeting_open_request_builds_participants_and_limits() {
        let request = meeting_open_request(
            "X の設計方針".to_string(),
            "claude-a, codex-b ,agy-c".to_string(),
            Some(3),
            None,
            None,
            None,
        )
        .unwrap();

        assert_eq!(request.command, IpcCommand::MeetingOpen);
        assert_eq!(request.payload["topic"], "X の設計方針");
        assert_eq!(
            request.payload["participants"],
            json!(["claude-a", "codex-b", "agy-c"])
        );
        assert_eq!(request.payload["max_messages_per_participant"], 3);
        assert_eq!(request.payload["kind"], "question");
        assert_eq!(request.payload["priority"], "normal");

        // 参加者 2 名未満は拒否。
        assert!(
            meeting_open_request(
                "topic".to_string(),
                "solo-agent".to_string(),
                None,
                None,
                None,
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn message_send_defaults_kind_handoff_and_priority_normal() {
        let request =
            message_send_request("role:tester".to_string(), "body".to_string(), None, None)
                .unwrap();
        assert_eq!(request.payload["kind"], "handoff");
        assert_eq!(request.payload["priority"], "normal");
    }

    #[test]
    fn message_send_maps_protocol_kind_names_to_wire_values() {
        assert_eq!(
            normalize_message_kind("TaskAssignment").unwrap(),
            "task_assignment"
        );
        assert_eq!(
            normalize_message_kind("PatchProposal").unwrap(),
            "patch_proposal"
        );
        assert_eq!(
            normalize_message_kind("StatusProbe").unwrap(),
            "status_probe"
        );
        // Case-insensitive.
        assert_eq!(normalize_message_kind("handoff").unwrap(), "handoff");
    }

    #[test]
    fn message_send_rejects_invalid_kind_and_priority() {
        let kind_err = message_send_request(
            "role:tester".to_string(),
            "body".to_string(),
            Some("Greeting".to_string()),
            None,
        )
        .unwrap_err();
        assert!(
            kind_err
                .to_string()
                .contains("invalid message kind 'Greeting'")
        );

        let prio_err = message_send_request(
            "role:tester".to_string(),
            "body".to_string(),
            None,
            Some("critical".to_string()),
        )
        .unwrap_err();
        assert!(prio_err.to_string().contains("invalid priority 'critical'"));
    }

    #[test]
    fn message_send_attributes_agent_when_env_present() {
        // Serialize env mutation across tests in this binary to avoid a race on
        // the process-global `AGENTMUX_AGENT_ID`.
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        // SAFETY: env access is serialized by ENV_LOCK for the duration below.
        unsafe {
            std::env::set_var("AGENTMUX_AGENT_ID", "agent_01HXSENDER");
        }
        let request =
            message_send_request("role:tester".to_string(), "body".to_string(), None, None);
        // Remove before asserting so a failed assertion cannot leak the var.
        unsafe {
            std::env::remove_var("AGENTMUX_AGENT_ID");
        }
        let request = request.unwrap();
        assert_eq!(request.payload["from_agent_id"], "agent_01HXSENDER");

        let request =
            message_send_request("role:tester".to_string(), "body".to_string(), None, None)
                .unwrap();
        assert!(
            request.payload.get("from_agent_id").is_none(),
            "no from_agent_id without env"
        );
    }

    #[test]
    fn format_message_history_payload_lists_messages_newest_first() {
        let payload = json!({
            "messages": [
                {
                    "message_id": "msg_old",
                    "task_id": "task_a",
                    "from": { "kind": "agent", "id": "planner" },
                    "to": { "kind": "role", "id": "tester" },
                    "kind": "handoff",
                    "body": "older handoff",
                    "delivery_status": "queued",
                    "created_at": "2026-06-04T01:00:00+00:00"
                },
                {
                    "message_id": "msg_new",
                    "task_id": "task_a",
                    "from": { "kind": "orchestrator" },
                    "to": { "kind": "agent", "id": "impl-codex" },
                    "kind": "test_result",
                    "body": "newer test result",
                    "delivery_status": "delivered",
                    "created_at": "2026-06-04T02:00:00+00:00"
                }
            ]
        });

        let output = format_message_history_payload(
            &payload,
            &MessageHistoryFilter {
                limit: 50,
                ..MessageHistoryFilter::default()
            },
        );

        assert!(output.starts_with("CREATED"));
        assert!(output.contains("msg_new"));
        assert!(output.contains("agent:impl-codex"));
        assert!(output.find("msg_new").unwrap() < output.find("msg_old").unwrap());
    }

    #[test]
    fn format_message_history_payload_filters_and_limits_messages() {
        let payload = json!({
            "messages": [
                {
                    "message_id": "msg_a",
                    "task_id": "task_a",
                    "from": { "kind": "agent", "id": "planner" },
                    "to": { "kind": "agent", "id": "impl-codex" },
                    "kind": "handoff",
                    "body": "first",
                    "delivery_status": "queued",
                    "created_at": "2026-06-04T01:00:00+00:00"
                },
                {
                    "message_id": "msg_b",
                    "task_id": "task_b",
                    "from": { "kind": "agent", "id": "tester" },
                    "to": { "kind": "agent", "id": "reviewer" },
                    "kind": "test_result",
                    "body": "second",
                    "delivery_status": "delivered",
                    "created_at": "2026-06-04T02:00:00+00:00"
                }
            ]
        });

        let output = format_message_history_payload(
            &payload,
            &MessageHistoryFilter {
                limit: 1,
                task: None,
                thread: None,
                agent: Some("impl-codex".to_string()),
                kind: Some("handoff".to_string()),
                status: Some("queued".to_string()),
            },
        );

        assert!(output.contains("msg_a"));
        assert!(!output.contains("msg_b"));
    }

    #[test]
    fn format_message_history_payload_reports_empty_history() {
        assert_eq!(
            format_message_history_payload(
                &json!({ "messages": [] }),
                &MessageHistoryFilter {
                    limit: 50,
                    ..MessageHistoryFilter::default()
                },
            ),
            "no messages\n"
        );
    }

    #[test]
    fn message_send_rejects_empty_body_before_ipc() {
        let error = message_send_request("agent_01HX".to_string(), "  ".to_string(), None, None)
            .unwrap_err();

        assert!(error.to_string().contains("message body must not be empty"));
    }

    #[test]
    fn context_requests_target_daemon_ipc() {
        let add = context_add_request("decision log".to_string()).unwrap();
        assert_eq!(add.command, IpcCommand::ContextCreate);
        assert_eq!(add.payload["title"], "decision log");
        assert_eq!(add.payload["kind"], "handoff_summary");

        let list = context_list_request();
        assert_eq!(list.command, IpcCommand::ContextSearch);
        assert_eq!(list.payload, json!({}));

        let show = context_show_request("ctx_01HX".to_string());
        assert_eq!(show.command, IpcCommand::ContextSearch);
        assert_eq!(show.payload["context_id"], "ctx_01HX");

        let search = context_search_request("risk".to_string()).unwrap();
        assert_eq!(search.command, IpcCommand::ContextSearch);
        assert_eq!(search.payload["query"], "risk");

        let attach = context_attach_request("ctx_01HX".to_string(), "msg_01HX".to_string());
        assert_eq!(attach.command, IpcCommand::ContextAttach);
        assert_eq!(attach.payload["context_id"], "ctx_01HX");
        assert_eq!(attach.payload["message_id"], "msg_01HX");

        let inject = context_inject_request("ctx_01HX".to_string(), "agent_01HX".to_string());
        assert_eq!(inject.command, IpcCommand::ContextInject);
        assert_eq!(inject.payload["context_id"], "ctx_01HX");
        assert_eq!(inject.payload["agent_id"], "agent_01HX");

        let export = context_export_request("contexts.json".to_string());
        assert_eq!(export.command, IpcCommand::ContextExport);
        assert_eq!(export.payload["output"], "contexts.json");
    }

    #[test]
    fn context_request_builders_reject_empty_text_before_ipc() {
        let add_error = context_add_request("  ".to_string()).unwrap_err();
        assert!(
            add_error
                .to_string()
                .contains("context title must not be empty")
        );

        let search_error = context_search_request("  ".to_string()).unwrap_err();
        assert!(
            search_error
                .to_string()
                .contains("context search query must not be empty")
        );
    }

    #[test]
    fn worktree_requests_target_daemon_ipc() {
        let list = worktree_list_request();
        assert_eq!(list.command, IpcCommand::WorktreeList);
        assert_eq!(list.payload, json!({}));

        let diff = worktree_diff_request("wt_01HX".to_string());
        assert_eq!(diff.command, IpcCommand::WorktreeDiff);
        assert_eq!(diff.payload["worktree_id"], "wt_01HX");

        let test = worktree_test_request("wt_01HX".to_string());
        assert_eq!(test.command, IpcCommand::WorktreeTest);
        assert_eq!(test.payload["worktree_id"], "wt_01HX");

        let promote = worktree_promote_request("wt_01HX".to_string());
        assert_eq!(promote.command, IpcCommand::WorktreePromote);
        assert_eq!(promote.payload["worktree_id"], "wt_01HX");

        let archive = worktree_archive_request("wt_01HX".to_string());
        assert_eq!(archive.command, IpcCommand::WorktreeArchive);
        assert_eq!(archive.payload["worktree_id"], "wt_01HX");
    }

    #[test]
    fn approval_requests_target_daemon_ipc() {
        let list = approval_list_request();
        assert_eq!(list.command, IpcCommand::ApprovalList);
        assert_eq!(list.payload, json!({}));

        let approve = approval_approve_request("appr_01HX".to_string());
        assert_eq!(approve.command, IpcCommand::ApprovalApprove);
        assert_eq!(approve.payload["approval_id"], "appr_01HX");

        let reject = approval_reject_request("appr_01HY".to_string());
        assert_eq!(reject.command, IpcCommand::ApprovalReject);
        assert_eq!(reject.payload["approval_id"], "appr_01HY");
    }

    #[test]
    fn agent_requests_target_daemon_ipc() {
        let list = agent_ls_request();
        assert_eq!(list.command, IpcCommand::DaemonStatus);
        assert_eq!(list.payload, json!({}));

        let spawn = agent_spawn_request("codex".to_string(), "implementer".to_string()).unwrap();
        assert_eq!(spawn.command, IpcCommand::AgentSpawn);
        assert_eq!(spawn.payload["provider"], "codex");
        assert_eq!(spawn.payload["role"], "implementer");
        let spawn_name = spawn.payload["name"].as_str().unwrap();
        assert!(spawn_name.starts_with("implementer-"));
        assert_eq!(spawn_name.len(), "implementer-".len() + 6);

        let second_spawn =
            agent_spawn_request("codex".to_string(), "implementer".to_string()).unwrap();
        assert_ne!(spawn.payload["name"], second_spawn.payload["name"]);

        let stop = agent_stop_request("agent_01HX".to_string());
        assert_eq!(stop.command, IpcCommand::AgentStop);
        assert_eq!(stop.payload["agent_id"], "agent_01HX");

        let send = agent_send_request("agent_01HX".to_string(), "hello".to_string()).unwrap();
        assert_eq!(send.command, IpcCommand::MessageCreate);
        assert_eq!(send.payload["to"], "agent:agent_01HX");
        assert_eq!(send.payload["body"], "hello");

        let send_by_name =
            agent_send_request("codex-a1b2c3".to_string(), "hello".to_string()).unwrap();
        assert_eq!(send_by_name.payload["to"], "agent:codex-a1b2c3");

        let inject = agent_inject_request("msg_01HX".to_string(), "agent_01HX".to_string());
        assert_eq!(inject.command, IpcCommand::MessageInject);
        assert_eq!(inject.payload["message_id"], "msg_01HX");
        assert_eq!(inject.payload["agent_id"], "agent_01HX");

        let focus = agent_focus_request("agent_01HX".to_string());
        assert_eq!(focus.command, IpcCommand::AgentFocus);
        assert_eq!(focus.payload["agent_id"], "agent_01HX");

        let interrupt = agent_interrupt_request("agent_01HX".to_string());
        assert_eq!(interrupt.command, IpcCommand::AgentInterrupt);
        assert_eq!(interrupt.payload["agent_id"], "agent_01HX");
    }

    #[test]
    fn sessions_list_request_targets_daemon_status() {
        let request = sessions_list_request();

        assert_eq!(request.id, "req_sessions_list");
        assert_eq!(request.command, IpcCommand::DaemonStatus);
        assert_eq!(request.payload, json!({}));
    }

    #[test]
    fn format_sessions_payload_lists_only_running_sessions() {
        let payload = json!({
            "agents": [
                {
                    "id": "agent_live",
                    "name": "shell",
                    "role": "tester",
                    "status": "awaiting_input",
                    "input_ready": true,
                    "process_id": 1234,
                    "has_process": true,
                    "attached_clients": ["csess_1", "csess_2"]
                },
                {
                    "id": "agent_restored",
                    "name": "restored",
                    "process_id": null,
                    "has_process": false,
                    "attached_clients": []
                }
            ]
        });

        assert_eq!(
            format_sessions_payload(&payload),
            "ID NAME ROLE STATUS INPUT PID CLIENTS\nagent_live shell tester awaiting_input ready 1234 2\n"
        );
    }

    #[test]
    fn format_sessions_payload_reports_empty_running_sessions() {
        assert_eq!(
            format_sessions_payload(&json!({ "agents": [] })),
            "no running sessions\n"
        );
        assert_eq!(format_sessions_payload(&json!({})), "no running sessions\n");
    }

    #[test]
    fn agent_request_builders_reject_empty_values_before_ipc() {
        let provider_error =
            agent_spawn_request(" ".to_string(), "implementer".to_string()).unwrap_err();
        assert!(provider_error.to_string().contains("provider"));

        let role_error = agent_spawn_request("codex".to_string(), " ".to_string()).unwrap_err();
        assert!(role_error.to_string().contains("role"));

        let body_error = agent_send_request("agent_01HX".to_string(), " ".to_string()).unwrap_err();
        assert!(body_error.to_string().contains("agent message body"));
    }

    #[test]
    fn layout_requests_target_daemon_ipc() {
        let save = layout_save_request("default".to_string()).unwrap();
        assert_eq!(save.command, IpcCommand::LayoutSet);
        assert_eq!(save.payload["name"], "default");

        let load = layout_load_request("default".to_string());
        assert_eq!(load.command, IpcCommand::LayoutGet);
        assert_eq!(load.payload["name"], "default");

        let list = layout_list_request();
        assert_eq!(list.command, IpcCommand::LayoutGet);
        assert_eq!(list.payload, json!({}));
    }

    #[test]
    fn layout_save_rejects_empty_name_before_ipc() {
        let error = layout_save_request("  ".to_string()).unwrap_err();

        assert!(error.to_string().contains("layout name must not be empty"));
    }

    #[test]
    fn project_init_creates_agentmux_config_without_overwriting_existing_file() {
        let root = std::env::temp_dir().join(format!("agentmux-cli-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let project_dir = init_project(&root).unwrap();
        let config_path = project_dir.join(".agentmux/config.toml");
        let contents = std::fs::read_to_string(&config_path).unwrap();
        let config = AgentmuxConfig::parse_str(&contents).unwrap();
        assert_eq!(config.project.name, "example");

        std::fs::write(
            &config_path,
            DEFAULT_PROJECT_CONFIG.replace("example", "custom"),
        )
        .unwrap();
        init_project(&root).unwrap();
        assert!(
            std::fs::read_to_string(&config_path)
                .unwrap()
                .contains("custom")
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn result_protocol_install_updates_existing_local_instruction_files_once() {
        let root = std::env::temp_dir().join(format!(
            "agentmux-result-protocol-local-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let agents_path = root.join("AGENTS.md");
        let claude_path = root.join("CLAUDE.md");
        let gemini_path = root.join("GEMINI.md");
        std::fs::write(&agents_path, "# Agents\n").unwrap();
        std::fs::write(&claude_path, "# Claude\n").unwrap();

        let first = install_result_protocol(&root, false).unwrap();
        assert_eq!(first.len(), 3);
        assert_eq!(first[0].status, ResultProtocolInstallStatus::Added);
        assert_eq!(first[1].status, ResultProtocolInstallStatus::Added);
        assert_eq!(first[2].status, ResultProtocolInstallStatus::Missing);
        assert!(!gemini_path.exists());

        let second = install_result_protocol(&root, false).unwrap();
        assert_eq!(
            second[0].status,
            ResultProtocolInstallStatus::AlreadyPresent
        );
        assert_eq!(
            second[1].status,
            ResultProtocolInstallStatus::AlreadyPresent
        );
        assert_eq!(second[2].status, ResultProtocolInstallStatus::Missing);

        let contents = std::fs::read_to_string(&agents_path).unwrap();
        assert_eq!(contents.matches(RESULT_PROTOCOL_MARKER_START).count(), 1);
        assert!(contents.contains("AGENTMUX_RESULT:"));
        assert!(contents.contains("messages[]"));
        assert!(contents.contains("Always send messages with inject delivery"));
        assert!(contents.contains("never pass `delivery_mode: inbox_only`"));
        assert!(contents.contains("Manual injection is a fallback"));
        // AGENTMUX_RESULT is now a turn-status notification; the CLI is the
        // first-choice message channel and messages[] is the shell-less fallback.
        assert!(contents.contains("turn-status notification"));
        assert!(contents.contains("first choice is the CLI"));
        assert!(contents.contains("agentmux message send --to <target> --kind <Kind>"));
        assert!(contents.contains("fallback for agents that have no shell access"));
        assert!(contents.contains(
            "Do not ask the user for confirmation before sending normal message replies"
        ));
        assert!(contents.contains("back-and-forth turns"));
        assert!(contents.contains(MESSAGE_CONFIRM_AFTER_TURNS_ENV));
        assert!(contents.contains("Allowed message kind values"));
        assert!(contents.contains("AGENTMUX_AGENT_NAME"));
        assert!(contents.contains("agentmux message inject <message_id>"));
        assert!(contents.contains("agentmux agent inject <message_id> <agent_id>"));
        assert!(contents.contains("agentmux start \"agy,messages,codex\""));
        assert!(contents.contains("Conversation List"));
        assert!(contents.contains("Injection is asynchronous"));
        assert!(contents.contains("Two-session exchange example"));
        assert!(contents.contains("agentmux message list"));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn message_confirm_after_turns_parses_env_value_or_defaults() {
        assert_eq!(
            message_confirm_after_turns(None),
            DEFAULT_MESSAGE_CONFIRM_AFTER_TURNS
        );
        assert_eq!(message_confirm_after_turns(Some("5")), 5);
        assert_eq!(
            message_confirm_after_turns(Some("0")),
            DEFAULT_MESSAGE_CONFIRM_AFTER_TURNS
        );
        assert_eq!(
            message_confirm_after_turns(Some("not-a-number")),
            DEFAULT_MESSAGE_CONFIRM_AFTER_TURNS
        );

        let block = result_protocol_block_with_threshold(5);
        assert!(block.contains("5 or more back-and-forth turns"));
        assert!(block.contains(MESSAGE_CONFIRM_AFTER_TURNS_ENV));
    }

    #[test]
    fn result_protocol_install_refreshes_stale_managed_block() {
        let root = std::env::temp_dir().join(format!(
            "agentmux-result-protocol-refresh-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let agents_path = root.join("AGENTS.md");
        std::fs::write(
            &agents_path,
            "# Agents\n\n<!-- agentmux-result-protocol:start -->\nold instructions\n<!-- agentmux-result-protocol:end -->\n",
        )
        .unwrap();

        let report = install_result_protocol(&root, false).unwrap();

        assert_eq!(report[0].status, ResultProtocolInstallStatus::Updated);
        let contents = std::fs::read_to_string(&agents_path).unwrap();
        assert_eq!(contents.matches(RESULT_PROTOCOL_MARKER_START).count(), 1);
        assert!(!contents.contains("old instructions"));
        assert!(contents.contains("Two-session exchange example"));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn result_protocol_install_can_create_global_style_instruction_file() {
        let root = std::env::temp_dir().join(format!(
            "agentmux-result-protocol-global-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let target = root.join(".codex/AGENTS.md");

        let first = install_result_protocol_to_file(&target, true).unwrap();
        assert_eq!(first.status, ResultProtocolInstallStatus::Added);
        assert!(target.exists());

        let second = install_result_protocol_to_file(&target, true).unwrap();
        assert_eq!(second.status, ResultProtocolInstallStatus::AlreadyPresent);
        let contents = std::fs::read_to_string(&target).unwrap();
        assert_eq!(contents.matches(RESULT_PROTOCOL_MARKER_START).count(), 1);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn config_parser_accepts_docs_example_and_rejects_invalid_values() {
        let config = AgentmuxConfig::parse_str(DEFAULT_PROJECT_CONFIG).unwrap();
        assert_eq!(config.team["claude-codex"].agents.len(), 5);

        let invalid =
            DEFAULT_PROJECT_CONFIG.replace("prefix_key = \"Ctrl-g\"", "prefix_key = \"F12\"");
        let error = AgentmuxConfig::parse_str(&invalid).unwrap_err();
        assert!(error.to_string().contains("tui.prefix_key"));
    }

    #[test]
    fn command_lookup_searches_path_entries() {
        let root =
            std::env::temp_dir().join(format!("agentmux-cli-path-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let command_path = root.join("codex");
        std::fs::write(&command_path, "").unwrap();

        assert_eq!(
            find_command_in_path("codex", Some(root.as_os_str())),
            Some(command_path)
        );
        assert_eq!(find_command_in_path("claude", Some(root.as_os_str())), None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn doctor_report_includes_required_v0_1_checks() {
        let root =
            std::env::temp_dir().join(format!("agentmux-cli-doctor-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".agentmux")).unwrap();
        std::fs::write(root.join(".agentmux/config.toml"), DEFAULT_PROJECT_CONFIG).unwrap();

        let report = doctor_report(&root.join("agentmux.sock"), &root);
        let names = report.iter().map(|check| check.name).collect::<Vec<_>>();

        assert_eq!(
            names,
            vec![
                "daemon socket",
                "config parse",
                "SQLite access",
                "claude",
                "codex",
                "PTY creation",
                "git worktree"
            ]
        );
        assert!(report.iter().any(|check| {
            check.name == "config parse"
                && check.status == DoctorStatus::Ok
                && check.detail == "project=example"
        }));

        std::fs::remove_dir_all(&root).unwrap();
    }
