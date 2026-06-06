use super::*;

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
