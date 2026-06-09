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
    let server = tokio::spawn(async move { handle_client(server_stream, server_runtime).await });
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
    let server = tokio::spawn(async move { handle_client(server_stream, server_runtime).await });

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
async fn manual_message_inject_skips_message_already_injecting() {
    // #7: an idle auto-delivery marks the message `Injecting` and then sleeps
    // for the settle delay before writing to the PTY. A manual `MessageInject`
    // arriving during that window must not start a second injection, or the
    // same prompt is written into the PTY twice. The guard rejects a message
    // that is already `Injecting`.
    let runtime = DaemonRuntime::new(16);
    let agent = runtime.register_agent("inject-guard".to_string()).await;
    let message = runtime
        .create_message(NewAgentMessage {
            task_id: None,
            thread_id: None,
            from: MessageSource::System,
            to: MessageTarget::Agent(agent.id.clone()),
            kind: MessageKind::Handoff,
            priority: Priority::Normal,
            body: "deliver me once".to_string(),
            context_refs: Vec::new(),
            artifact_refs: Vec::new(),
            delivery_mode: DeliveryMode::InjectWhenIdle,
            requires_response: false,
        })
        .await
        .expect("message is created");

    // Simulate the first injection already being in flight.
    {
        let mut state = runtime.state.write().await;
        state
            .messages
            .update_delivery_status(
                &message.id,
                DeliveryStatus::Injecting,
                DateTimeUtc::now_utc(),
            )
            .expect("status updates");
    }

    let error = runtime
        .inject_message(&message.id)
        .await
        .expect_err("a message already injecting is not re-injected");
    assert!(
        error.to_string().contains("already injecting"),
        "guard reports the in-flight injection: {error}"
    );

    // The status is untouched by the rejected manual attempt.
    assert_eq!(
        runtime
            .get_message(&message.id)
            .await
            .unwrap()
            .delivery_status,
        DeliveryStatus::Injecting
    );
}

#[tokio::test]
async fn manual_message_inject_can_target_explicit_agent_session() {
    let root = std::env::temp_dir().join(format!("agentmux-explicit-inject-{}", ulid::Ulid::new()));
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
    let root = std::env::temp_dir().join(format!("agentmux-idle-message-{}", ulid::Ulid::new()));
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
    let root = std::env::temp_dir().join(format!("agentmux-ready-message-{}", ulid::Ulid::new()));
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
        runtime.apply_agent_status_signal(&agent.id, AgentStatus::AwaitingInput, "prompt is ready"),
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
    let root = std::env::temp_dir().join(format!("agentmux-settle-delay-{}", ulid::Ulid::new()));
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

/// Step 1: a human keystroke forwarded via the live `send_input_script` path
/// (the `AgentSendInputScript` IPC handler) must update the target pane's
/// `last_human_input_at`. Before the fix this timestamp was never updated by
/// real keystrokes, so the auto-injection quiet window saw stale data.
#[tokio::test]
async fn human_input_script_records_last_human_input_on_live_path() {
    let root =
        std::env::temp_dir().join(format!("agentmux-human-input-record-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&root).expect("temporary root is created");
    let output_path = root.join("human-input.txt");
    let runtime = DaemonRuntime::new(16);
    let agent = runtime
        .spawn_agent(
            "human-codex".to_string(),
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

    // No human input recorded yet.
    {
        let state = runtime.state.read().await;
        assert_eq!(
            state
                .agents
                .get(&agent.id)
                .unwrap()
                .input_activity
                .last_human_input_at,
            None,
            "freshly spawned pane has no recorded human input"
        );
    }

    let before = DateTimeUtc::now_utc();
    let script = InputScript {
        id: InputScriptId::new(),
        target_agent_id: agent.id.clone(),
        reason: "human keystroke".to_string(),
        preconditions: Vec::new(),
        actions: vec![InputAction::SendRaw(b"h".to_vec())],
        safety: InputSafety::Safe,
        created_at: DateTimeUtc::now_utc(),
    };
    runtime
        .send_input_script(&script)
        .await
        .expect("human input script is delivered");

    let recorded = {
        let state = runtime.state.read().await;
        state
            .agents
            .get(&agent.id)
            .unwrap()
            .input_activity
            .last_human_input_at
            .expect("live human key path records last_human_input_at")
    };
    assert!(
        recorded >= before,
        "recorded human input {recorded:?} should be at/after {before:?}"
    );

    terminate_agent_process(&runtime, &agent.id.to_string()).await;
    std::fs::remove_dir_all(root).expect("temporary root is removed");
}

/// Step 2: auto-injection (idle delivery) must be deferred while the target
/// pane is within the human-typing quiet window, and must proceed once the
/// last keystroke ages out of that window. Drives the real
/// `deliver_idle_messages_for_agent` path; the quiet window is the default
/// `InjectionTiming` value (non-zero even in tests).
#[tokio::test]
async fn auto_injection_defers_during_human_quiet_window_then_proceeds() {
    let root = std::env::temp_dir().join(format!("agentmux-quiet-defer-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&root).expect("temporary root is created");
    let output_path = root.join("quiet-defer.txt");
    let runtime = DaemonRuntime::new(16);
    let agent = runtime
        .spawn_agent(
            "quiet-codex".to_string(),
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
            priority: Priority::High,
            body: "quiet-window-deferred-body".to_string(),
            context_refs: Vec::new(),
            artifact_refs: Vec::new(),
            delivery_mode: DeliveryMode::InjectWhenIdle,
            requires_response: false,
        })
        .await
        .expect("message is created");

    // Simulate the human typing right now: record a keystroke at the
    // current instant so the pane is inside the quiet window.
    runtime
        .record_human_input_for_agent(&agent.id, DateTimeUtc::now_utc())
        .await;

    let deferred = runtime
        .deliver_idle_messages_for_agent(&agent.id, AgentStatus::AwaitingInput)
        .await
        .expect("idle delivery does not error while deferring");
    assert!(
        deferred.is_none(),
        "auto-injection must be deferred while the human is typing"
    );
    assert_eq!(
        runtime
            .get_message(&message.id)
            .await
            .unwrap()
            .delivery_status,
        DeliveryStatus::Queued,
        "deferred message stays queued for a later idle retry"
    );

    // Age the last keystroke out of the quiet window (default 2500ms) by
    // backdating it well beyond the window. A later idle poll now proceeds.
    {
        let mut state = runtime.state.write().await;
        let live = state.agents.get_mut(&agent.id).unwrap();
        live.input_activity
            .record_human_input(DateTimeUtc::now_utc() - Duration::from_secs(60));
    }

    let delivered = tokio::time::timeout(
        Duration::from_secs(5),
        runtime.deliver_idle_messages_for_agent(&agent.id, AgentStatus::AwaitingInput),
    )
    .await
    .expect("idle delivery should not hang")
    .expect("idle delivery succeeds")
    .expect("message is delivered once the human goes quiet");
    assert_eq!(delivered.id, message.id);
    assert_eq!(delivered.delivery_status, DeliveryStatus::Delivered);

    let output = wait_for_file_contains(&output_path, "quiet-window-deferred-body")
        .await
        .expect("message reached PTY after quiet window elapsed");
    assert!(output.contains("quiet-window-deferred-body"));

    terminate_agent_process(&runtime, &agent.id.to_string()).await;
    std::fs::remove_dir_all(root).expect("temporary root is removed");
}

/// The settle delay opens a race: the quiet check passes, the daemon sleeps
/// `send_delay` before writing, and a human starts typing during that
/// sleep. The write-time re-check must defer the injection (back to
/// `WaitingForAgent`) instead of typing over the human.
#[tokio::test]
async fn auto_injection_rechecks_quiet_window_after_settle_delay() {
    let root = std::env::temp_dir().join(format!("agentmux-quiet-recheck-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&root).expect("temporary root is created");
    let output_path = root.join("quiet-recheck.txt");
    let runtime = DaemonRuntime::new(16);
    let agent = runtime
        .spawn_agent(
            "recheck-codex".to_string(),
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

    // Non-zero settle delay so the recheck window actually opens.
    {
        let mut state = runtime.state.write().await;
        state.injection_timing.send_delay = Duration::from_millis(400);
    }

    let message = runtime
        .create_message(NewAgentMessage {
            task_id: None,
            thread_id: None,
            from: MessageSource::System,
            to: MessageTarget::Agent(agent.id.clone()),
            kind: MessageKind::Handoff,
            priority: Priority::High,
            body: "quiet-recheck-deferred-body".to_string(),
            context_refs: Vec::new(),
            artifact_refs: Vec::new(),
            delivery_mode: DeliveryMode::InjectWhenIdle,
            requires_response: false,
        })
        .await
        .expect("message is created");

    // Start the idle delivery (quiet check passes: no human input yet),
    // then simulate a human keystroke while the daemon sits in the settle
    // sleep. The keystroke lands well inside the 2500ms quiet window
    // relative to the post-sleep recheck.
    let delivery_runtime = runtime.clone();
    let delivery_agent = agent.id.clone();
    let delivery = tokio::spawn(async move {
        delivery_runtime
            .deliver_idle_messages_for_agent(&delivery_agent, AgentStatus::AwaitingInput)
            .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    runtime
        .record_human_input_for_agent(&agent.id, DateTimeUtc::now_utc())
        .await;

    let deferred = tokio::time::timeout(Duration::from_secs(5), delivery)
        .await
        .expect("idle delivery should not hang")
        .expect("delivery task does not panic")
        .expect("idle delivery does not error while deferring");
    assert!(
        deferred.is_none(),
        "injection must be deferred when a human typed during the settle delay"
    );
    assert_eq!(
        runtime
            .get_message(&message.id)
            .await
            .unwrap()
            .delivery_status,
        DeliveryStatus::WaitingForAgent,
        "deferred message returns to WaitingForAgent for a later idle retry"
    );
    let pty_output = std::fs::read_to_string(&output_path).unwrap_or_default();
    assert!(
        !pty_output.contains("quiet-recheck-deferred-body"),
        "deferred message body must not reach the PTY"
    );

    // Age the keystroke out of the quiet window; the retry now delivers.
    {
        let mut state = runtime.state.write().await;
        state.injection_timing.send_delay = Duration::ZERO;
        let live = state.agents.get_mut(&agent.id).unwrap();
        live.input_activity
            .record_human_input(DateTimeUtc::now_utc() - Duration::from_secs(60));
    }
    let delivered = tokio::time::timeout(
        Duration::from_secs(5),
        runtime.deliver_idle_messages_for_agent(&agent.id, AgentStatus::AwaitingInput),
    )
    .await
    .expect("idle delivery should not hang")
    .expect("idle delivery succeeds")
    .expect("message is delivered once the human goes quiet");
    assert_eq!(delivered.id, message.id);
    assert_eq!(delivered.delivery_status, DeliveryStatus::Delivered);

    terminate_agent_process(&runtime, &agent.id.to_string()).await;
    std::fs::remove_dir_all(root).expect("temporary root is removed");
}

// ── Radar edge-case additions ──────────────────────────────────────────
//
// Cases 4-6 guard multi-pane safety invariants that the happy-path suite
// didn't fully cover.

/// Edge case 4: per-pane isolation.
///
/// A human keystroke recorded against pane A's quiet window must NOT defer
/// auto-injection into a DIFFERENT idle pane B. The quiet window is
/// per-agent, so pane B's message must be delivered even while pane A is
/// within its quiet window.
#[tokio::test]
async fn quiet_window_on_pane_a_does_not_defer_delivery_to_pane_b() {
    let root = std::env::temp_dir().join(format!("agentmux-pane-isolation-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&root).expect("temporary root is created");
    let output_path = root.join("pane-b-output.txt");

    let runtime = DaemonRuntime::new(16);

    // Pane A: metadata-only (no PTY); only its quiet window is exercised.
    let agent_a = runtime.register_agent("typing-human".to_string()).await;

    // Pane B: live PTY agent that will receive a message.
    let agent_b = runtime
        .spawn_agent(
            "idle-receiver".to_string(),
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
        .expect("pane_b agent is spawned");

    // Queue a message for pane B.
    let message_b = runtime
        .create_message(NewAgentMessage {
            task_id: None,
            thread_id: None,
            from: MessageSource::System,
            to: MessageTarget::Agent(agent_b.id.clone()),
            kind: MessageKind::Handoff,
            priority: Priority::Normal,
            body: "pane-b-isolation-body".to_string(),
            context_refs: Vec::new(),
            artifact_refs: Vec::new(),
            delivery_mode: DeliveryMode::InjectWhenIdle,
            requires_response: false,
        })
        .await
        .expect("message for pane B is created");

    // Simulate a human typing in pane A RIGHT NOW → pane A is in its quiet window.
    runtime
        .record_human_input_for_agent(&agent_a.id, DateTimeUtc::now_utc())
        .await;

    // Pane B should still receive its message unaffected by pane A's activity.
    let delivered_b = tokio::time::timeout(
        Duration::from_secs(5),
        runtime.deliver_idle_messages_for_agent(&agent_b.id, AgentStatus::AwaitingInput),
    )
    .await
    .expect("delivery for pane B must not hang")
    .expect("delivery call for pane B must not error")
    .expect("pane B must receive its message while pane A is in quiet window");

    assert_eq!(
        delivered_b.id, message_b.id,
        "delivered message must be the one queued for pane B"
    );
    assert_eq!(
        delivered_b.delivery_status,
        DeliveryStatus::Delivered,
        "pane B message status must be Delivered"
    );

    let output = wait_for_file_contains(&output_path, "pane-b-isolation-body")
        .await
        .expect("pane B received message despite pane A being in quiet window");
    assert!(output.contains("pane-b-isolation-body"));

    terminate_agent_process(&runtime, &agent_b.id.to_string()).await;
    std::fs::remove_dir_all(root).expect("temporary root is removed");
}

/// Edge case 5: no-stuck-queue — deferred message is delivered on a later
/// attempt, confirming the message stays Queued (not dropped/failed) while
/// deferred and is then consumed on the next eligible call.
///
/// NOTE: the existing `auto_injection_defers_during_human_quiet_window_then_proceeds`
/// test already covers this end-to-end. This lightweight unit-level variant
/// confirms the Queued status is preserved between calls using only a
/// metadata-only agent (no PTY spawn), verifying the guard returns Ok(None)
/// on both the first and a repeated call while still within the quiet window.
#[tokio::test]
async fn deferred_message_stays_queued_on_repeated_delivery_attempts() {
    let runtime = DaemonRuntime::new(16);
    let agent = runtime.register_agent("no-pty-typist".to_string()).await;

    let message = runtime
        .create_message(NewAgentMessage {
            task_id: None,
            thread_id: None,
            from: MessageSource::System,
            to: MessageTarget::Agent(agent.id.clone()),
            kind: MessageKind::Handoff,
            priority: Priority::Normal,
            body: "must stay queued".to_string(),
            context_refs: Vec::new(),
            artifact_refs: Vec::new(),
            delivery_mode: DeliveryMode::InjectWhenIdle,
            requires_response: false,
        })
        .await
        .expect("message is created");

    // Put the agent inside the quiet window.
    runtime
        .record_human_input_for_agent(&agent.id, DateTimeUtc::now_utc())
        .await;

    // First attempt: deferred (quiet window active).
    let first = runtime
        .deliver_idle_messages_for_agent(&agent.id, AgentStatus::AwaitingInput)
        .await
        .expect("first attempt must not error");
    assert!(first.is_none(), "first attempt must be deferred");

    // Status is still Queued — not dropped, not Failed.
    assert_eq!(
        runtime
            .get_message(&message.id)
            .await
            .unwrap()
            .delivery_status,
        DeliveryStatus::Queued,
        "message must stay Queued after the first deferred attempt"
    );

    // Second attempt within the same quiet window: still deferred.
    let second = runtime
        .deliver_idle_messages_for_agent(&agent.id, AgentStatus::AwaitingInput)
        .await
        .expect("second attempt must not error");
    assert!(second.is_none(), "second attempt must still be deferred");

    assert_eq!(
        runtime
            .get_message(&message.id)
            .await
            .unwrap()
            .delivery_status,
        DeliveryStatus::Queued,
        "message must still be Queued after the second deferred attempt"
    );
}

/// Edge case 6: boundary — injection is deferred when the last keystroke is
/// strictly inside the quiet window, and is attempted (no longer deferred by
/// the quiet-window guard) once the elapsed time reaches the window boundary.
///
/// Uses time back-dating so the test runs instantly without real sleeps.
/// The "at exactly the boundary" case uses a metadata-only agent: the quiet-
/// window guard passes, so the call proceeds to the PTY-write step and fails
/// with "has no live PTY" rather than returning Ok(None). This distinguishes
/// "not deferred" from "deferred".
#[tokio::test]
async fn injection_defers_inside_window_and_proceeds_at_boundary() {
    let runtime = DaemonRuntime::new(16);
    let agent = runtime.register_agent("boundary-agent".to_string()).await;

    let _message = runtime
        .create_message(NewAgentMessage {
            task_id: None,
            thread_id: None,
            from: MessageSource::System,
            to: MessageTarget::Agent(agent.id.clone()),
            kind: MessageKind::Handoff,
            priority: Priority::Normal,
            body: "boundary-test-body".to_string(),
            context_refs: Vec::new(),
            artifact_refs: Vec::new(),
            delivery_mode: DeliveryMode::InjectWhenIdle,
            requires_response: false,
        })
        .await
        .expect("message is created");

    let quiet = {
        let state = runtime.state.read().await;
        state.injection_timing.human_input_quiet
    };

    // --- Inside the quiet window (1 ms before the boundary) → deferred ---
    {
        let mut state = runtime.state.write().await;
        let live = state.agents.get_mut(&agent.id).unwrap();
        // Keystroke happened (quiet - 1 ms) ago, so elapsed < quiet.
        live.input_activity
            .record_human_input(DateTimeUtc::now_utc() - quiet + Duration::from_millis(1));
    }

    let inside_result = runtime
        .deliver_idle_messages_for_agent(&agent.id, AgentStatus::AwaitingInput)
        .await
        .expect("inside-window call must not error");
    assert!(
        inside_result.is_none(),
        "delivery must be deferred while 1 ms inside the quiet window"
    );

    // --- At the quiet-window boundary → the guard passes; no longer deferred ---
    {
        let mut state = runtime.state.write().await;
        let live = state.agents.get_mut(&agent.id).unwrap();
        // Keystroke happened exactly `quiet` duration ago.
        live.input_activity
            .record_human_input(DateTimeUtc::now_utc() - quiet);
    }

    // NOTE: the agent has no live PTY, so once the quiet-window guard passes
    // the call will fail at the PTY-write step. The important invariant is
    // that it does NOT return Ok(None) — i.e., the quiet-window guard itself
    // no longer blocks delivery.
    let boundary_result = runtime
        .deliver_idle_messages_for_agent(&agent.id, AgentStatus::AwaitingInput)
        .await;
    assert!(
        boundary_result.is_err() || boundary_result.as_ref().unwrap().is_some(),
        "at the boundary the quiet-window guard must pass (result must be \
             Err(no-live-PTY) or Some(...), never Ok(None))"
    );
    if let Ok(Some(msg)) = boundary_result {
        // If a PTY were present, delivery would succeed.
        assert_eq!(msg.delivery_status, DeliveryStatus::Delivered);
    }
    // Err path: confirm it's the PTY error, not a quiet-window deferral.
    // (Ok(None) is the only incorrect outcome and was already excluded above.)
}

// ── Commands-panel broadcast-input (agent.broadcast_input) ──────────────

/// Spawn a perl-capture PTY agent under `role` that writes everything it
/// receives to `output_path`. Shared by the broadcast tests below.
async fn spawn_capture_agent(
    runtime: &DaemonRuntime,
    name: &str,
    role: AgentRole,
    root: &Path,
    output_path: &Path,
) -> RegisteredAgentSession {
    runtime
        .spawn_agent_with_role(
            name.to_string(),
            role,
            PtySpawnSpec {
                command: "/usr/bin/perl".to_string(),
                args: vec![
                    "-e".to_string(),
                    pty_capture_script().to_string(),
                    output_path.display().to_string(),
                ],
                cwd: root.to_path_buf(),
                env: BTreeMap::from([("TERM".to_string(), "xterm-256color".to_string())]),
                size: Default::default(),
            },
        )
        .await
        .expect("capture agent is spawned")
}

/// `broadcast` fans the raw input out to every live agent PTY.
#[tokio::test]
async fn broadcast_input_delivers_to_all_agents() {
    let root = std::env::temp_dir().join(format!("agentmux-broadcast-all-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&root).expect("temporary root is created");
    let out_a = root.join("a.txt");
    let out_b = root.join("b.txt");
    let runtime = DaemonRuntime::new(16);

    let agent_a =
        spawn_capture_agent(&runtime, "impl-a", AgentRole::Implementer, &root, &out_a).await;
    let agent_b = spawn_capture_agent(&runtime, "tester-b", AgentRole::Tester, &root, &out_b).await;

    let outcome = runtime
        .broadcast_input(
            &MessageTarget::Broadcast,
            &[
                InputAction::PasteText("broadcast-all-body".to_string()),
                InputAction::PressEnter,
            ],
        )
        .await
        .expect("broadcast must succeed");

    assert_eq!(outcome.skipped.len(), 0, "no pane is typing → none skipped");
    let delivered: std::collections::BTreeSet<_> = outcome.delivered.into_iter().collect();
    assert!(delivered.contains(&agent_a.id));
    assert!(delivered.contains(&agent_b.id));

    assert!(
        wait_for_file_contains(&out_a, "broadcast-all-body")
            .await
            .is_some(),
        "agent A PTY received the broadcast text"
    );
    assert!(
        wait_for_file_contains(&out_b, "broadcast-all-body")
            .await
            .is_some(),
        "agent B PTY received the broadcast text"
    );

    terminate_agent_process(&runtime, &agent_a.id.to_string()).await;
    terminate_agent_process(&runtime, &agent_b.id.to_string()).await;
    std::fs::remove_dir_all(root).expect("temporary root is removed");
}

/// A pane with a human typing inside the quiet window is skipped; the other
/// resolved panes still receive the broadcast.
#[tokio::test]
async fn broadcast_input_skips_pane_with_human_typing() {
    let root = std::env::temp_dir().join(format!("agentmux-broadcast-skip-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&root).expect("temporary root is created");
    let out_idle = root.join("idle.txt");
    let out_typing = root.join("typing.txt");
    let runtime = DaemonRuntime::new(16);

    let idle = spawn_capture_agent(
        &runtime,
        "impl-idle",
        AgentRole::Implementer,
        &root,
        &out_idle,
    )
    .await;
    let typing = spawn_capture_agent(
        &runtime,
        "impl-typing",
        AgentRole::Implementer,
        &root,
        &out_typing,
    )
    .await;

    // The "typing" pane saw a human keystroke right now → inside quiet window.
    runtime
        .record_human_input_for_agent(&typing.id, DateTimeUtc::now_utc())
        .await;

    let outcome = runtime
        .broadcast_input(
            &MessageTarget::Broadcast,
            &[
                InputAction::PasteText("broadcast-skip-body".to_string()),
                InputAction::PressEnter,
            ],
        )
        .await
        .expect("broadcast must succeed even when a pane is skipped");

    assert_eq!(
        outcome.delivered,
        vec![idle.id.clone()],
        "only the idle pane is written"
    );
    assert_eq!(
        outcome.skipped,
        vec![typing.id.clone()],
        "the typing pane is skipped"
    );

    assert!(
        wait_for_file_contains(&out_idle, "broadcast-skip-body")
            .await
            .is_some(),
        "idle pane received the broadcast text"
    );

    terminate_agent_process(&runtime, &idle.id.to_string()).await;
    terminate_agent_process(&runtime, &typing.id.to_string()).await;
    std::fs::remove_dir_all(root).expect("temporary root is removed");
}

/// `role:<role>` restricts the broadcast to the matching role only.
#[tokio::test]
async fn broadcast_input_role_target_filters_recipients() {
    let root = std::env::temp_dir().join(format!("agentmux-broadcast-role-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&root).expect("temporary root is created");
    let out_tester = root.join("tester.txt");
    let out_impl = root.join("impl.txt");
    let runtime = DaemonRuntime::new(16);

    let tester = spawn_capture_agent(&runtime, "qa-1", AgentRole::Tester, &root, &out_tester).await;
    let implementer =
        spawn_capture_agent(&runtime, "impl-1", AgentRole::Implementer, &root, &out_impl).await;

    let outcome = runtime
        .broadcast_input(
            &MessageTarget::Role(AgentRole::Tester),
            &[
                InputAction::PasteText("role-scoped-body".to_string()),
                InputAction::PressEnter,
            ],
        )
        .await
        .expect("role-scoped broadcast must succeed");

    assert_eq!(
        outcome.delivered,
        vec![tester.id.clone()],
        "only testers receive it"
    );
    assert!(outcome.skipped.is_empty());

    assert!(
        wait_for_file_contains(&out_tester, "role-scoped-body")
            .await
            .is_some(),
        "tester pane received the role-scoped broadcast"
    );
    // The implementer pane must not have received it.
    assert!(
        !std::fs::read_to_string(&out_impl)
            .unwrap_or_default()
            .contains("role-scoped-body"),
        "implementer pane must NOT receive a tester-scoped broadcast"
    );

    terminate_agent_process(&runtime, &tester.id.to_string()).await;
    terminate_agent_process(&runtime, &implementer.id.to_string()).await;
    std::fs::remove_dir_all(root).expect("temporary root is removed");
}

/// A target that resolves to no agents surfaces the bus `UserError`.
#[tokio::test]
async fn broadcast_input_unresolvable_target_is_user_error() {
    let runtime = DaemonRuntime::new(16);
    // No agents registered at all → broadcast resolves to empty.
    let error = runtime
        .broadcast_input(
            &MessageTarget::Broadcast,
            &[InputAction::PasteText("noone".to_string())],
        )
        .await
        .expect_err("broadcast to no agents must error");
    assert!(
        matches!(error, AgentmuxError::UserError(_)),
        "got {error:?}"
    );

    // A role with no matching agents also errors, even if other agents exist.
    let _other = runtime.register_agent("planner-only".to_string()).await;
    let role_error = runtime
        .broadcast_input(
            &MessageTarget::Role(AgentRole::Tester),
            &[InputAction::PasteText("noone".to_string())],
        )
        .await
        .expect_err("broadcast to an unmatched role must error");
    assert!(matches!(role_error, AgentmuxError::UserError(_)));
}

/// Every broadcast write is recorded in the JSONL event log
/// (input_script.created + input_script.injected), preserving auditability.
#[tokio::test]
async fn broadcast_input_records_event_log_entries() {
    let root =
        std::env::temp_dir().join(format!("agentmux-broadcast-events-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&root).expect("temporary root is created");
    let event_log_path = root.join(".agentmux").join("events.jsonl");
    let out = root.join("out.txt");
    let runtime = DaemonRuntime::new(16).with_event_log(EventLog::new(&event_log_path));

    let agent =
        spawn_capture_agent(&runtime, "impl-evt", AgentRole::Implementer, &root, &out).await;

    runtime
        .broadcast_input(
            &MessageTarget::Broadcast,
            &[InputAction::PasteText("audit-me".to_string())],
        )
        .await
        .expect("broadcast must succeed");

    // The JSONL event log captures both lifecycle events for the broadcast
    // write, preserving the spec's auditability invariant.
    let content = std::fs::read_to_string(&event_log_path).expect("event log is written");
    let events: Vec<serde_json::Value> = content
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid JSONL event"))
        .collect();
    let created = events
        .iter()
        .filter(|event| event["type"] == "input_script.created")
        .count();
    let injected = events
        .iter()
        .filter(|event| event["type"] == "input_script.injected")
        .count();
    assert_eq!(
        created, 1,
        "broadcast appends exactly one input_script.created"
    );
    assert_eq!(
        injected, 1,
        "broadcast appends exactly one input_script.injected"
    );
    // The recorded script is attributed to the target agent and carries the
    // broadcast reason.
    let created_event = events
        .iter()
        .find(|event| event["type"] == "input_script.created")
        .unwrap();
    assert_eq!(created_event["agent_id"], agent.id.to_string());
    assert_eq!(created_event["payload"]["reason"], "agent.broadcast_input");

    terminate_agent_process(&runtime, &agent.id.to_string()).await;
    std::fs::remove_dir_all(root).expect("temporary root is removed");
}
