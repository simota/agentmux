use super::*;

    #[tokio::test]
    async fn message_create_auto_injects_into_idle_pty_agent() {
        let root =
            std::env::temp_dir().join(format!("agentmux-msg-create-deliver-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&root).expect("temporary root is created");
        let output_path = root.join("tester-output.txt");
        let runtime = DaemonRuntime::new(16);

        let tester = runtime
            .spawn_agent_with_role(
                "send-tester".to_string(),
                AgentRole::Tester,
                PtySpawnSpec {
                    command: "/usr/bin/perl".to_string(),
                    args: vec![
                        "-e".to_string(),
                        pty_capture_script().to_string(),
                        output_path.display().to_string(),
                    ],
                    cwd: root.clone(),
                    env: BTreeMap::from([("TERM".to_string(), "xterm-256color".to_string())]),
                    size: Default::default(),
                },
            )
            .await
            .expect("tester agent is spawned");

        // Mark the tester idle so it is eligible for immediate injection.
        {
            let mut state = runtime.state.write().await;
            if let Some(live) = state.agents.get_mut(&tester.id) {
                live.metadata.status = Some(AgentStatus::AwaitingInput);
            }
        }

        // Drive the message through the real IPC `MessageCreate` dispatch arm.
        let (client_stream, server_stream) = UnixStream::pair().unwrap();
        let server_runtime = runtime.clone();
        let server =
            tokio::spawn(async move { handle_client(server_stream, server_runtime).await });
        let (reader, writer) = client_stream.into_split();
        let mut reader = JsonlReader::new(BufReader::new(reader));
        let mut writer = JsonlWriter::new(writer);
        writer.write(&ClientHello::new("0.1.0")).await.unwrap();
        writer
            .write(&ClientRequest::new(
                "req_message_send",
                IpcCommand::MessageCreate,
                json!({
                    "to": tester.id.to_string(),
                    "body": "message-send-auto-inject-body",
                    "kind": "handoff",
                    "priority": "normal",
                    "delivery_mode": "inject_when_idle",
                }),
            ))
            .await
            .unwrap();
        let create_response = read_response(&mut reader).await;
        assert!(
            create_response.ok,
            "message.create response was {create_response:?}"
        );

        // No manual `message inject` is issued; delivery must happen on its own.
        let output = wait_for_file_contains(&output_path, "message-send-auto-inject-body")
            .await
            .expect("handoff prompt reached tester PTY without manual inject");
        assert!(output.contains("message-send-auto-inject-body"));

        let messages_after = runtime.list_messages().await;
        assert_eq!(messages_after.len(), 1);
        assert_eq!(
            messages_after[0].delivery_status,
            DeliveryStatus::Delivered,
            "message must be Delivered after auto-injection"
        );

        terminate_agent_process(&runtime, &tester.id.to_string()).await;
        server.abort();
        std::fs::remove_dir_all(root).expect("temporary root is removed");
    }

    /// When the output tail contains an AGENTMUX_RESULT block with invalid
    /// JSON, parse_agent_result_marker returns NeedsStatusProbe. The daemon
    /// must not silently drop this — `persist_live_agent_result` surfaces a
    /// `NeedsProbe` outcome (with the reason) so the forwarder can emit a
    /// deduplicated Error event. It must never be reported as `Persisted`.
    #[test]
    fn message_create_payload_attributes_agent_from_from_agent_id() {
        let agent_id = AgentSessionId::new();
        let payload = json!({
            "to": "role:tester",
            "body": "from a live agent session",
            "kind": "question",
            "priority": "high",
            "delivery_mode": "inject_when_idle",
            "from_agent_id": agent_id.to_string(),
        });

        let message = message_create_payload(&payload).expect("payload parses");
        assert_eq!(message.from, MessageSource::Agent(agent_id));
        assert_eq!(message.kind, MessageKind::Question);
        assert_eq!(message.priority, Priority::High);
        assert_eq!(message.to, MessageTarget::Role(AgentRole::Tester));
    }

    #[test]
    fn message_create_payload_falls_back_to_user_when_from_agent_id_absent_or_invalid() {
        // Absent -> User source.
        let absent = json!({ "to": "role:tester", "body": "no sender id" });
        let message = message_create_payload(&absent).expect("payload parses");
        assert!(matches!(message.from, MessageSource::User(_)));

        // Malformed id -> User source (not an error).
        let invalid = json!({
            "to": "role:tester",
            "body": "bad sender id",
            "from_agent_id": "not-a-valid-ulid-session-id",
        });
        let message = message_create_payload(&invalid).expect("payload parses");
        assert!(matches!(message.from, MessageSource::User(_)));
    }

    /// Regression: when the message target's agent has status RunningTurn (not
    /// eligible per agent_accepts_idle_injection), the message must stay Queued
    /// and must NOT cause a panic or an error — the daemon silently defers until
    /// the agent becomes idle.
    #[tokio::test]
    async fn ipc_message_commands_create_list_show_and_inject() {
        let runtime = DaemonRuntime::new(16);
        let root = std::env::temp_dir().join(format!("agentmux-message-ipc-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&root).expect("temporary root is created");
        let output_path = root.join("message.txt");
        let (client_stream, server_stream) = UnixStream::pair().unwrap();
        let server_runtime = runtime.clone();
        let server =
            tokio::spawn(async move { handle_client(server_stream, server_runtime).await });

        let (reader, writer) = client_stream.into_split();
        let mut reader = JsonlReader::new(BufReader::new(reader));
        let mut writer = JsonlWriter::new(writer);

        writer.write(&ClientHello::new("0.1.0")).await.unwrap();
        writer
            .write(&ClientRequest::new(
                "req_spawn",
                IpcCommand::AgentSpawn,
                json!({
                    "name": "impl-codex",
                    "command": "/usr/bin/perl",
                    "args": ["-e", pty_capture_script(), output_path],
                    "cwd": root,
                    "env": { "TERM": "xterm-256color" },
                }),
            ))
            .await
            .unwrap();
        let spawn_response = read_response(&mut reader).await;
        assert!(spawn_response.ok);
        let agent_id = spawn_response.payload.unwrap()["agent_id"]
            .as_str()
            .unwrap()
            .to_string();
        writer
            .write(&ClientRequest::new(
                "req_attach",
                IpcCommand::ClientAttach,
                json!({ "agent_id": agent_id }),
            ))
            .await
            .unwrap();
        let (attach_response, attach_event) = read_response_and_event(&mut reader).await;
        assert!(attach_response.ok);
        assert_eq!(attach_event.kind, IpcEventKind::ClientAttached);

        // This test exercises the *manual* `message inject` command path.
        // Mark the agent as RunningTurn so the auto idle-delivery triggered by
        // `message create` is a no-op here and delivery happens only via the
        // explicit inject below (the auto-delivery path is covered by
        // `message_create_auto_injects_into_idle_pty_agent`).
        {
            let parsed = parse_agent_session_id(&agent_id).unwrap();
            let mut state = runtime.state.write().await;
            if let Some(live) = state.agents.get_mut(&parsed) {
                live.metadata.status = Some(AgentStatus::RunningTurn);
            }
        }

        writer
            .write(&ClientRequest::new(
                "req_message_create",
                IpcCommand::MessageCreate,
                json!({
                    "to": agent_id,
                    "body": "please review the diff",
                    "kind": "handoff",
                    "priority": "normal",
                    "delivery_mode": "inject_when_idle",
                }),
            ))
            .await
            .unwrap();
        let (create_response, created_event) = read_response_and_event(&mut reader).await;
        assert!(create_response.ok);
        assert_eq!(created_event.kind, IpcEventKind::MessageCreated);
        let create_payload = create_response.payload.unwrap();
        let message_id = create_payload["message_id"].as_str().unwrap().to_string();
        assert_eq!(create_payload["delivery_status"], "queued");

        writer
            .write(&ClientRequest::new(
                "req_message_create_by_name",
                IpcCommand::MessageCreate,
                json!({
                    "to": "impl-codex",
                    "body": "message by session name",
                    "kind": "handoff",
                    "priority": "normal",
                    "delivery_mode": "inject_when_idle",
                }),
            ))
            .await
            .unwrap();
        let (create_by_name_response, created_by_name_event) =
            read_response_and_event(&mut reader).await;
        assert!(create_by_name_response.ok);
        assert_eq!(created_by_name_event.kind, IpcEventKind::MessageCreated);
        let create_by_name_payload = create_by_name_response.payload.unwrap();
        let message_by_name_id = create_by_name_payload["message_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            create_by_name_payload["to"],
            json!({ "kind": "agent_name", "id": "impl-codex" })
        );

        writer
            .write(&ClientRequest::new(
                "req_message_list",
                IpcCommand::MessageList,
                json!({}),
            ))
            .await
            .unwrap();
        let list_response: DaemonResponse = reader.read().await.unwrap().unwrap();
        assert!(list_response.ok);
        let list_payload = list_response.payload.unwrap();
        let listed_messages = list_payload["messages"].as_array().unwrap();
        assert_eq!(listed_messages.len(), 2);
        assert!(
            listed_messages
                .iter()
                .any(|message| message["message_id"] == message_id)
        );
        assert!(
            listed_messages
                .iter()
                .any(|message| message["message_id"] == message_by_name_id)
        );

        writer
            .write(&ClientRequest::new(
                "req_message_show",
                IpcCommand::MessageShow,
                json!({ "message_id": message_id }),
            ))
            .await
            .unwrap();
        let show_response: DaemonResponse = reader.read().await.unwrap().unwrap();
        assert!(show_response.ok);
        let show_payload = show_response.payload.unwrap();
        assert_eq!(show_payload["body"], "please review the diff");

        writer
            .write(&ClientRequest::new(
                "req_message_inject",
                IpcCommand::MessageInject,
                json!({ "message_id": show_payload["message_id"] }),
            ))
            .await
            .unwrap();
        let inject_response = read_response(&mut reader).await;
        assert!(inject_response.ok);
        assert_eq!(
            inject_response.payload.as_ref().unwrap()["delivery_status"],
            "delivered"
        );
        let first_event: DaemonEvent = reader.read().await.unwrap().unwrap();
        let second_event: DaemonEvent = reader.read().await.unwrap().unwrap();
        assert_eq!(first_event.kind, IpcEventKind::InputInjected);
        assert_eq!(second_event.kind, IpcEventKind::MessageDelivered);

        std::thread::sleep(Duration::from_millis(1200));
        let delivered = std::fs::read_to_string(&output_path).expect("message reached PTY");
        assert!(delivered.contains("[agentmux handoff]"));
        assert!(delivered.contains("message:\nplease review the diff"));

        terminate_agent_process(&runtime, &agent_id).await;
        server.abort();
    }

    #[tokio::test]
    async fn meeting_open_fans_out_kickoff_to_participants_except_opener() {
        let runtime = DaemonRuntime::new(16);
        let opener = runtime.register_agent("claude-a".to_string()).await;
        let second = runtime.register_agent("codex-b".to_string()).await;
        let third = runtime.register_agent("agy-c".to_string()).await;

        let (thread, kickoff) = runtime
            .open_meeting(OpenMeetingInput {
                topic: "X の設計方針".to_string(),
                // Mixed name/id participant references must both resolve.
                participants: vec![
                    "claude-a".to_string(),
                    second.id.to_string(),
                    "agy-c".to_string(),
                ],
                opened_by: MessageSource::Agent(opener.id.clone()),
                max_messages_per_participant: Some(2),
                kind: MessageKind::Question,
                priority: Priority::Normal,
                body: None,
            })
            .await
            .expect("meeting opens");

        assert_eq!(thread.participants.len(), 3);
        assert_eq!(thread.max_messages_per_participant, 2);
        assert_eq!(kickoff.thread_id, Some(thread.id.clone()));
        assert!(kickoff.body.contains("X の設計方針"));

        {
            let state = runtime.state.read().await;
            assert!(
                state.messages.inbox(&opener.id).unwrap().is_empty(),
                "opener must not receive its own kickoff"
            );
            assert_eq!(state.messages.inbox(&second.id).unwrap().len(), 1);
            assert_eq!(state.messages.inbox(&third.id).unwrap().len(), 1);
        }

        let listed = runtime.list_meetings().await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].1, 1, "kickoff counts as a thread message");

        runtime
            .close_meeting(&thread.id)
            .await
            .expect("meeting closes");
        let error = runtime
            .create_message(NewAgentMessage {
                task_id: None,
                thread_id: None,
                from: MessageSource::Agent(second.id.clone()),
                to: MessageTarget::Thread(thread.id.clone()),
                kind: MessageKind::Finding,
                priority: Priority::Normal,
                body: "too late".to_string(),
                context_refs: Vec::new(),
                artifact_refs: Vec::new(),
                delivery_mode: DeliveryMode::InjectWhenIdle,
                requires_response: false,
            })
            .await
            .expect_err("closed thread rejects messages");
        assert!(error.to_string().contains("is closed"));
    }

    #[tokio::test]
    async fn manual_message_inject_fails_without_live_pty() {
        let runtime = DaemonRuntime::new(16);
        let agent = runtime.register_agent("metadata-only".to_string()).await;
        let message = runtime
            .create_message(NewAgentMessage {
                task_id: None,
                thread_id: None,
                from: MessageSource::System,
                to: MessageTarget::Agent(agent.id.clone()),
                kind: MessageKind::Handoff,
                priority: Priority::Normal,
                body: "deliver me".to_string(),
                context_refs: Vec::new(),
                artifact_refs: Vec::new(),
                delivery_mode: DeliveryMode::InjectImmediately,
                requires_response: false,
            })
            .await
            .expect("message is created");

        let error = runtime
            .inject_message(&message.id)
            .await
            .expect_err("metadata-only agent cannot receive PTY input");

        assert!(error.to_string().contains("has no live PTY"));
        assert_eq!(
            runtime
                .get_message(&message.id)
                .await
                .unwrap()
                .delivery_status,
            DeliveryStatus::Failed
        );
    }

    #[tokio::test]
    async fn manual_message_inject_can_target_explicit_agent_session() {
        let root =
            std::env::temp_dir().join(format!("agentmux-explicit-inject-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&root).expect("temporary root is created");
        let first_output = root.join("first.txt");
        let second_output = root.join("second.txt");
        let runtime = DaemonRuntime::new(16);
        let first = runtime
            .spawn_agent_with_role(
                "tester-first".to_string(),
                AgentRole::Tester,
                PtySpawnSpec {
                    command: "/usr/bin/perl".to_string(),
                    args: vec![
                        "-e".to_string(),
                        pty_capture_script().to_string(),
                        first_output.display().to_string(),
                    ],
                    cwd: root.clone(),
                    env: BTreeMap::from([("TERM".to_string(), "xterm-256color".to_string())]),
                    size: Default::default(),
                },
            )
            .await
            .expect("first tester is spawned");
        let second = runtime
            .spawn_agent_with_role(
                "tester-second".to_string(),
                AgentRole::Tester,
                PtySpawnSpec {
                    command: "/usr/bin/perl".to_string(),
                    args: vec![
                        "-e".to_string(),
                        pty_capture_script().to_string(),
                        second_output.display().to_string(),
                    ],
                    cwd: root.clone(),
                    env: BTreeMap::from([("TERM".to_string(), "xterm-256color".to_string())]),
                    size: Default::default(),
                },
            )
            .await
            .expect("second tester is spawned");
        let message = runtime
            .create_message(NewAgentMessage {
                task_id: None,
                thread_id: None,
                from: MessageSource::System,
                to: MessageTarget::Role(AgentRole::Tester),
                kind: MessageKind::Handoff,
                priority: Priority::Normal,
                body: "explicit session injection".to_string(),
                context_refs: Vec::new(),
                artifact_refs: Vec::new(),
                delivery_mode: DeliveryMode::InjectWhenIdle,
                requires_response: true,
            })
            .await
            .expect("role-targeted message is created");

        let delivered = runtime
            .inject_message_to_agent(&message.id, &second.id)
            .await
            .expect("explicit agent injection succeeds");

        assert_eq!(delivered.delivery_status, DeliveryStatus::Delivered);
        assert!(
            runtime.inject_message(&message.id).await.is_err(),
            "role-targeted manual injection without an explicit agent remains ambiguous"
        );
        let output = wait_for_file_contains(&second_output, "message:\nexplicit session injection")
            .await
            .expect("message reached explicitly targeted PTY");
        assert!(output.contains("message:\nexplicit session injection"));
        assert!(
            std::fs::read_to_string(&first_output)
                .unwrap_or_default()
                .is_empty(),
            "non-targeted tester should not receive the injected message"
        );

        terminate_agent_process(&runtime, &first.id.to_string()).await;
        terminate_agent_process(&runtime, &second.id.to_string()).await;
        std::fs::remove_dir_all(root).expect("temporary root is removed");
    }

    #[tokio::test]
    async fn idle_delivery_injects_rendered_prompt_with_context_and_mailbox_path() {
        let root =
            std::env::temp_dir().join(format!("agentmux-idle-message-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&root).expect("temporary root is created");
        let output_path = root.join("idle-message.txt");
        let runtime = DaemonRuntime::new(16);
        let agent = runtime
            .spawn_agent(
                "idle-codex".to_string(),
                PtySpawnSpec {
                    command: "/usr/bin/perl".to_string(),
                    args: vec![
                        "-e".to_string(),
                        pty_capture_script().to_string(),
                        output_path.display().to_string(),
                    ],
                    cwd: root.clone(),
                    env: BTreeMap::from([("TERM".to_string(), "xterm-256color".to_string())]),
                    size: Default::default(),
                },
            )
            .await
            .expect("agent is spawned");
        let project_id = runtime.state.read().await.default_project_id.clone();
        let short = runtime
            .create_context(NewContextItem {
                project_id: project_id.clone(),
                task_id: None,
                scope: ContextScope::Project,
                kind: ContextKind::Decision,
                title: "routing rule".to_string(),
                body: "Use MessageBus before PTY injection.".to_string(),
                source: ContextSource::System,
                visibility: Visibility::Internal,
                confidence: 1.0,
                tags: Vec::new(),
                related_files: Vec::new(),
                artifact_refs: Vec::new(),
            })
            .await
            .expect("short context is created");
        let long = runtime
            .create_context(NewContextItem {
                project_id,
                task_id: None,
                scope: ContextScope::Project,
                kind: ContextKind::ErrorLog,
                title: "large log".to_string(),
                body: "x".repeat(3000),
                source: ContextSource::System,
                visibility: Visibility::Internal,
                confidence: 1.0,
                tags: Vec::new(),
                related_files: Vec::new(),
                artifact_refs: Vec::new(),
            })
            .await
            .expect("long context is created");
        let message = runtime
            .create_message(NewAgentMessage {
                task_id: None,
                thread_id: None,
                from: MessageSource::System,
                to: MessageTarget::Agent(agent.id.clone()),
                kind: MessageKind::Handoff,
                priority: Priority::High,
                body: "handle queued idle work".to_string(),
                context_refs: vec![short.id.clone(), long.id.clone()],
                artifact_refs: Vec::new(),
                delivery_mode: DeliveryMode::InjectWhenIdle,
                requires_response: true,
            })
            .await
            .expect("message is created");

        let delivered = tokio::time::timeout(
            Duration::from_secs(5),
            runtime.deliver_idle_messages_for_agent(&agent.id, AgentStatus::AwaitingInput),
        )
        .await
        .expect("idle delivery should not hang")
        .expect("idle delivery succeeds")
        .expect("message is delivered");

        assert_eq!(delivered.id, message.id);
        assert_eq!(delivered.delivery_status, DeliveryStatus::Delivered);
        let output = wait_for_file_contains(&output_path, "message:\nhandle queued idle work")
            .await
            .expect("message reached PTY");
        assert!(output.contains("message:\nhandle queued idle work"));
        assert!(output.contains("- routing rule: Use MessageBus before PTY injection."));
        assert!(output.contains(".agentmux/inbox/idle-codex/ctx-"));

        let mailbox_dir = std::env::current_dir()
            .unwrap()
            .join(".agentmux/inbox/idle-codex");
        assert!(
            std::fs::read_dir(&mailbox_dir)
                .expect("mailbox directory exists")
                .any(|entry| entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("ctx-"))
        );
        terminate_agent_process(&runtime, &agent.id.to_string()).await;
        std::fs::remove_dir_all(&mailbox_dir).expect("mailbox directory is cleaned");
        std::fs::remove_dir_all(root).expect("temporary root is removed");
    }

    #[tokio::test]
    async fn ready_status_signal_delivers_next_idle_message_to_live_pty() {
        let root =
            std::env::temp_dir().join(format!("agentmux-ready-message-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&root).expect("temporary root is created");
        let output_path = root.join("ready-message.txt");
        let runtime = DaemonRuntime::new(16);
        let agent = runtime
            .spawn_agent(
                "ready-codex".to_string(),
                PtySpawnSpec {
                    command: "/usr/bin/perl".to_string(),
                    args: vec![
                        "-e".to_string(),
                        pty_capture_script().to_string(),
                        output_path.display().to_string(),
                    ],
                    cwd: root.clone(),
                    env: BTreeMap::from([("TERM".to_string(), "xterm-256color".to_string())]),
                    size: Default::default(),
                },
            )
            .await
            .expect("agent is spawned");
        let message = runtime
            .create_message(NewAgentMessage {
                task_id: None,
                thread_id: None,
                from: MessageSource::System,
                to: MessageTarget::Agent(agent.id.clone()),
                kind: MessageKind::Handoff,
                priority: Priority::Normal,
                body: "status driven work".to_string(),
                context_refs: Vec::new(),
                artifact_refs: Vec::new(),
                delivery_mode: DeliveryMode::InjectWhenIdle,
                requires_response: true,
            })
            .await
            .expect("message is created");
        let mut events = runtime.subscribe();

        let delivered = tokio::time::timeout(
            Duration::from_secs(5),
            runtime.apply_agent_status_signal(
                &agent.id,
                AgentStatus::AwaitingInput,
                "prompt is ready",
            ),
        )
        .await
        .expect("ready status delivery should not hang")
        .expect("ready status triggers idle delivery")
        .expect("idle message is delivered");

        assert_eq!(delivered.id, message.id);
        assert_eq!(delivered.delivery_status, DeliveryStatus::Delivered);
        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.push(
                events
                    .recv()
                    .await
                    .expect("delivery event is published")
                    .kind,
            );
        }
        assert!(seen.contains(&IpcEventKind::AgentStatusSignal));
        assert!(seen.contains(&IpcEventKind::AgentStatusChanged));
        assert!(seen.contains(&IpcEventKind::InputInjected));
        assert!(seen.contains(&IpcEventKind::MessageDelivered));

        let status = runtime.status_payload().await;
        assert_eq!(status["agents"][0]["status"], "awaiting_input");
        assert_eq!(status["agents"][0]["input_ready"], true);

        let output = wait_for_file_contains(&output_path, "message:\nstatus driven work")
            .await
            .expect("message reached PTY");
        assert!(output.contains("message:\nstatus driven work"));

        terminate_agent_process(&runtime, &agent.id.to_string()).await;
        std::fs::remove_dir_all(root).expect("temporary root is removed");
    }

    /// Regression test for the settle-delay fix in `deliver_idle_messages_for_agent`.
    ///
    /// After the fix, `deliver_idle_messages_for_agent` awaits the settle delay
    /// (`InjectionTiming::send_delay`, Duration::ZERO under cfg(test)) before writing to the
    /// PTY.  This test confirms that the added sleep does NOT break delivery: the message still reaches the live PTY and
    /// is returned with DeliveryStatus::Delivered.
    #[tokio::test]
    async fn deliver_idle_messages_settle_delay_does_not_break_delivery() {
        let root = std::env::temp_dir()
            .join(format!("agentmux-settle-delay-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&root).expect("temporary root is created");
        let output_path = root.join("settle-delay.txt");
        let runtime = DaemonRuntime::new(16);
        let agent = runtime
            .spawn_agent(
                "settle-codex".to_string(),
                PtySpawnSpec {
                    command: "/usr/bin/perl".to_string(),
                    args: vec![
                        "-e".to_string(),
                        pty_capture_script().to_string(),
                        output_path.display().to_string(),
                    ],
                    cwd: root.clone(),
                    env: BTreeMap::from([("TERM".to_string(), "xterm-256color".to_string())]),
                    size: Default::default(),
                },
            )
            .await
            .expect("agent is spawned");
        let message = runtime
            .create_message(NewAgentMessage {
                task_id: None,
                thread_id: None,
                from: MessageSource::System,
                to: MessageTarget::Agent(agent.id.clone()),
                kind: MessageKind::Handoff,
                priority: Priority::Normal,
                body: "settle delay regression check".to_string(),
                context_refs: Vec::new(),
                artifact_refs: Vec::new(),
                delivery_mode: DeliveryMode::InjectWhenIdle,
                requires_response: false,
            })
            .await
            .expect("message is created");

        // Directly call `deliver_idle_messages_for_agent` (the auto idle-delivery path).
        // In cfg(test) `InjectionTiming::send_delay` == Duration::ZERO, so the settle sleep
        // is a no-op and delivery must still succeed end-to-end.
        let delivered = tokio::time::timeout(
            Duration::from_secs(5),
            runtime.deliver_idle_messages_for_agent(&agent.id, AgentStatus::AwaitingInput),
        )
        .await
        .expect("idle delivery should not hang")
        .expect("idle delivery succeeds")
        .expect("message is delivered");

        assert_eq!(delivered.id, message.id);
        assert_eq!(delivered.delivery_status, DeliveryStatus::Delivered);

        let output = wait_for_file_contains(&output_path, "message:\nsettle delay regression check")
            .await
            .expect("message reached PTY");
        assert!(output.contains("message:\nsettle delay regression check"));

        terminate_agent_process(&runtime, &agent.id.to_string()).await;
        std::fs::remove_dir_all(root).expect("temporary root is removed");
    }
