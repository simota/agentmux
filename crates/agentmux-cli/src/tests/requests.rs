use super::*;

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
        // No role is sent: the daemon assigns the initial role "default".
        assert!(request.payload.get("role").is_none());
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
        // No role is sent: the daemon assigns the initial role "default".
        assert!(request.payload.get("role").is_none());
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
            parse_start_layout(args.providers.as_deref()).unwrap().panes,
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
    fn agent_set_role_request_carries_agent_id_and_role() {
        let request =
            agent_set_role_request("agent_001".to_string(), "qa-lead".to_string()).unwrap();

        assert_eq!(request.command, IpcCommand::AgentSetRole);
        assert_eq!(request.payload["agent_id"], "agent_001");
        assert_eq!(request.payload["role"], "qa-lead");
    }

    #[test]
    fn agent_set_role_request_rejects_empty_inputs() {
        assert!(agent_set_role_request("  ".to_string(), "reviewer".to_string()).is_err());
        assert!(agent_set_role_request("agent_001".to_string(), "  ".to_string()).is_err());
    }

    #[test]
    fn agent_send_keys_request_targets_single_session_with_actions_json() {
        let request = agent_send_keys_request("agent_001", "C-c|enter").unwrap();

        assert_eq!(request.command, IpcCommand::AgentBroadcastInput);
        assert_eq!(request.payload["target"], "agent:agent_001");
        let actions = request.payload["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0], json!({ "press_ctrl": "c" }));
        assert_eq!(actions[1], json!("press_enter"));
    }

    #[test]
    fn agent_send_keys_request_emits_send_raw_byte_arrays() {
        let request = agent_send_keys_request("agent_xyz", "raw:1b5b41").unwrap();
        let actions = request.payload["actions"].as_array().unwrap();
        assert_eq!(actions[0], json!({ "send_raw": [0x1b, 0x5b, 0x41] }));
    }

    #[test]
    fn agent_send_keys_request_rejects_empty_and_malformed_input() {
        assert!(agent_send_keys_request("  ", "C-c").is_err());
        assert!(agent_send_keys_request("agent_001", "   ").is_err());
        assert!(agent_send_keys_request("agent_001", "bogus").is_err());
        assert!(agent_send_keys_request("agent_001", "raw:1b5").is_err());
    }
