use super::*;

    #[tokio::test]
    async fn event_subscribe_empty_filter_forwards_every_event() {
        let runtime = DaemonRuntime::new(8);
        let event = DaemonEvent::new(
            IpcEventKind::MessageCreated,
            json!({ "task_id": "task_001", "role": "implementer" }),
        );
        let filter = EventSubscribeFilter {
            task_id: None,
            roles: Vec::new(),
            kinds: Vec::new(),
        };

        assert!(should_forward_event(&runtime, Some(&filter), &event).await);
    }

    #[tokio::test]
    async fn event_subscribe_filter_ands_fields_and_ors_values() {
        let runtime = DaemonRuntime::new(8);
        let event = DaemonEvent::new(
            IpcEventKind::MessageCreated,
            json!({ "task_id": "task_001", "role": "implementer" }),
        );
        let matching = EventSubscribeFilter {
            task_id: Some("task_001".to_string()),
            roles: vec!["tester".to_string(), "implementer".to_string()],
            kinds: vec![
                "agent.status_changed".to_string(),
                "message.created".to_string(),
            ],
        };
        let wrong_task = EventSubscribeFilter {
            task_id: Some("task_other".to_string()),
            ..matching.clone()
        };
        let wrong_role = EventSubscribeFilter {
            roles: vec!["tester".to_string(), "reviewer".to_string()],
            ..matching.clone()
        };
        let wrong_kind = EventSubscribeFilter {
            kinds: vec!["agent.status_changed".to_string()],
            ..matching.clone()
        };

        assert!(should_forward_event(&runtime, Some(&matching), &event).await);
        assert!(!should_forward_event(&runtime, Some(&wrong_task), &event).await);
        assert!(!should_forward_event(&runtime, Some(&wrong_role), &event).await);
        assert!(!should_forward_event(&runtime, Some(&wrong_kind), &event).await);
    }

    #[tokio::test]
    async fn event_subscribe_role_filter_uses_agent_role_when_payload_role_is_missing() {
        let runtime = DaemonRuntime::new(8);
        let agent = runtime
            .register_agent_with_role("tester-a1b2c3".to_string(), AgentRole::Tester)
            .await;
        let event = DaemonEvent::new(
            IpcEventKind::AgentStatusChanged,
            json!({ "agent_id": agent.id.to_string(), "status": "awaiting_input" }),
        );
        let filter = EventSubscribeFilter {
            task_id: None,
            roles: vec!["tester".to_string()],
            kinds: vec!["agent.status_changed".to_string()],
        };

        assert!(should_forward_event(&runtime, Some(&filter), &event).await);
    }

    #[tokio::test]
    async fn ipc_client_can_spawn_attach_detach_and_receive_events() {
        let runtime = DaemonRuntime::new(16);
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
                json!({ "name": "impl-codex" }),
            ))
            .await
            .unwrap();

        let spawn_response = read_response(&mut reader).await;
        assert!(spawn_response.ok);
        let spawn_payload = spawn_response.payload.unwrap();
        // No explicit role was requested, so the session starts at the default
        // role rather than inferring one from the "impl-codex" name. Runtime
        // role assignment happens later via `agent.set_role`.
        assert_eq!(spawn_payload["role"], "default");
        let agent_id = spawn_payload["agent_id"].as_str().unwrap().to_string();
        assert_no_frame(&mut reader).await;

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

        writer
            .write(&ClientRequest::new(
                "req_spawn_after_attach",
                IpcCommand::AgentSpawn,
                json!({ "name": "tester" }),
            ))
            .await
            .unwrap();
        let (second_spawn_response, second_spawn_event) =
            read_response_and_event(&mut reader).await;
        assert!(second_spawn_response.ok);
        assert_eq!(second_spawn_event.kind, IpcEventKind::AgentSpawned);

        writer
            .write(&ClientRequest::new(
                "req_detach",
                IpcCommand::ClientDetach,
                json!({}),
            ))
            .await
            .unwrap();
        let detach_response = read_response(&mut reader).await;
        assert!(detach_response.ok);

        runtime.register_agent("after-detach".to_string()).await;
        assert_no_frame(&mut reader).await;

        server.abort();
    }

    #[tokio::test]
    async fn ipc_agent_snapshot_restores_existing_terminal_buffer() {
        let runtime = DaemonRuntime::new(16);
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
                    "name": "snapshot-shell",
                    "command": "/bin/sh",
                    "args": ["-c", "printf snapshot-ready; sleep 1"],
                    "cwd": std::env::current_dir().unwrap(),
                    "env": { "TERM": "xterm-256color" },
                    "size": { "rows": 2, "cols": 20 },
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

        let mut snapshot_response = None;
        for _ in 0..40 {
            writer
                .write(&ClientRequest::new(
                    "req_snapshot",
                    IpcCommand::AgentSnapshot,
                    json!({ "agent_id": agent_id }),
                ))
                .await
                .unwrap();
            let response = read_response(&mut reader).await;
            assert!(response.ok, "snapshot response was {response:?}");
            let contains_output = response
                .payload
                .as_ref()
                .and_then(|payload| payload["lines"].as_array())
                .is_some_and(|lines| {
                    lines.iter().any(|line| {
                        line.as_str()
                            .is_some_and(|text| text.contains("snapshot-ready"))
                    })
                });
            if contains_output {
                snapshot_response = Some(response);
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let snapshot_response = snapshot_response.expect("snapshot output should be captured");

        let payload = snapshot_response.payload.unwrap();
        assert_eq!(payload["agent_id"], agent_id);
        assert_eq!(payload["rows"], 2);
        assert_eq!(payload["cols"], 20);

        terminate_agent_process(&runtime, &agent_id).await;
        server.abort();
    }

    #[tokio::test]
    async fn ipc_client_disconnect_detaches_without_dropping_live_agent_process() {
        let runtime = DaemonRuntime::new(16);
        let (client_stream, server_stream) = UnixStream::pair().unwrap();
        let server_runtime = runtime.clone();
        let first_server =
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
                    "name": "long-running-shell",
                    "command": "/bin/sh",
                    "args": ["-c", "while :; do sleep 1; done"],
                    "cwd": std::env::current_dir().unwrap(),
                    "env": { "TERM": "xterm-256color" },
                    "size": { "rows": 24, "cols": 80 },
                }),
            ))
            .await
            .unwrap();

        let spawn_response = read_response(&mut reader).await;
        assert!(spawn_response.ok);
        let spawn_payload = spawn_response.payload.unwrap();
        let agent_id = spawn_payload["agent_id"].as_str().unwrap().to_string();
        assert!(spawn_payload["process_id"].as_u64().is_some());

        writer
            .write(&ClientRequest::new(
                "req_attach",
                IpcCommand::ClientAttach,
                json!({ "agent_id": agent_id }),
            ))
            .await
            .unwrap();
        let (attach_response, _) = read_response_and_event(&mut reader).await;
        assert!(attach_response.ok);

        drop(writer);
        drop(reader);
        let _ = first_server.await;

        let status_after_disconnect = runtime.status_payload().await;
        assert_eq!(status_after_disconnect["agent_count"], 1);
        assert_eq!(status_after_disconnect["agents"][0]["id"], agent_id);
        assert_eq!(status_after_disconnect["agents"][0]["has_process"], true);
        assert_eq!(
            status_after_disconnect["agents"][0]["attached_clients"]
                .as_array()
                .unwrap()
                .len(),
            0
        );

        let (client_stream, server_stream) = UnixStream::pair().unwrap();
        let server_runtime = runtime.clone();
        let second_server =
            tokio::spawn(async move { handle_client(server_stream, server_runtime).await });
        let (reader, writer) = client_stream.into_split();
        let mut reader = JsonlReader::new(BufReader::new(reader));
        let mut writer = JsonlWriter::new(writer);

        writer.write(&ClientHello::new("0.1.0")).await.unwrap();
        writer
            .write(&ClientRequest::new(
                "req_reattach",
                IpcCommand::ClientAttach,
                json!({ "agent_id": agent_id }),
            ))
            .await
            .unwrap();
        let (reattach_response, reattach_event) = read_response_and_event(&mut reader).await;
        assert!(reattach_response.ok);
        assert_eq!(reattach_event.kind, IpcEventKind::ClientAttached);

        terminate_agent_process(&runtime, &agent_id).await;
        second_server.abort();
    }

    #[tokio::test]
    async fn ipc_message_show_reports_unknown_message() {
        let runtime = DaemonRuntime::new(16);
        let (client_stream, server_stream) = UnixStream::pair().unwrap();
        let server_runtime = runtime.clone();
        let server =
            tokio::spawn(async move { handle_client(server_stream, server_runtime).await });

        let (reader, writer) = client_stream.into_split();
        let mut reader = JsonlReader::new(BufReader::new(reader));
        let mut writer = JsonlWriter::new(writer);

        writer.write(&ClientHello::new("0.1.0")).await.unwrap();
        let missing = MessageId::new();
        writer
            .write(&ClientRequest::new(
                "req_message_show",
                IpcCommand::MessageShow,
                json!({ "message_id": missing.to_string() }),
            ))
            .await
            .unwrap();

        let response: DaemonResponse = reader.read().await.unwrap().unwrap();
        assert!(!response.ok);
        assert_eq!(response.error.unwrap().code, "MESSAGE_NOT_FOUND");

        server.abort();
    }

    #[tokio::test]
    async fn ipc_context_commands_create_search_attach_inject_and_export() {
        let runtime = DaemonRuntime::new(16);
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
                json!({ "name": "implementer" }),
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

        // This test focuses on context attach/inject, not message delivery.
        // Mark the agent RunningTurn so the auto idle-delivery triggered by the
        // `message create` below is a no-op and does not emit extra events that
        // would desync the response/event frame sequencing here.
        {
            let parsed = parse_agent_session_id(&agent_id).unwrap();
            let mut state = runtime.state.write().await;
            if let Some(live) = state.agents.get_mut(&parsed) {
                live.metadata.status = Some(AgentStatus::RunningTurn);
            }
        }

        writer
            .write(&ClientRequest::new(
                "req_context_create",
                IpcCommand::ContextCreate,
                json!({
                    "title": "review decision",
                    "body": "Use daemon IPC for shared context.",
                    "kind": "decision",
                    "visibility": "internal",
                    "tags": ["ipc"],
                }),
            ))
            .await
            .unwrap();
        let (create_response, created_event) = read_response_and_event(&mut reader).await;
        assert!(create_response.ok);
        assert_eq!(created_event.kind, IpcEventKind::ContextCreated);
        let create_payload = create_response.payload.unwrap();
        let context_id = create_payload["context_id"].as_str().unwrap().to_string();
        assert_eq!(create_payload["title"], "review decision");

        writer
            .write(&ClientRequest::new(
                "req_context_list",
                IpcCommand::ContextSearch,
                json!({}),
            ))
            .await
            .unwrap();
        let list_response: DaemonResponse = reader.read().await.unwrap().unwrap();
        assert!(list_response.ok);
        let list_payload = list_response.payload.unwrap();
        assert_eq!(list_payload["contexts"].as_array().unwrap().len(), 1);

        writer
            .write(&ClientRequest::new(
                "req_context_show",
                IpcCommand::ContextSearch,
                json!({ "context_id": context_id }),
            ))
            .await
            .unwrap();
        let show_response: DaemonResponse = reader.read().await.unwrap().unwrap();
        assert!(show_response.ok);
        let show_payload = show_response.payload.unwrap();
        assert_eq!(show_payload["body"], "Use daemon IPC for shared context.");

        writer
            .write(&ClientRequest::new(
                "req_context_search",
                IpcCommand::ContextSearch,
                json!({ "query": "daemon ipc" }),
            ))
            .await
            .unwrap();
        let search_response: DaemonResponse = reader.read().await.unwrap().unwrap();
        assert!(search_response.ok);
        assert_eq!(
            search_response.payload.unwrap()["contexts"]
                .as_array()
                .unwrap()
                .len(),
            1
        );

        writer
            .write(&ClientRequest::new(
                "req_message_create",
                IpcCommand::MessageCreate,
                json!({
                    "to": agent_id,
                    "body": "please use the attached context",
                }),
            ))
            .await
            .unwrap();
        let (message_response, _) = read_response_and_event(&mut reader).await;
        let message_id = message_response.payload.unwrap()["message_id"]
            .as_str()
            .unwrap()
            .to_string();

        writer
            .write(&ClientRequest::new(
                "req_context_attach",
                IpcCommand::ContextAttach,
                json!({ "context_id": show_payload["context_id"], "message_id": message_id }),
            ))
            .await
            .unwrap();
        let attach_response: DaemonResponse = reader.read().await.unwrap().unwrap();
        assert!(attach_response.ok);
        assert_eq!(
            attach_response.payload.unwrap()["context_refs"][0],
            show_payload["context_id"]
        );

        writer
            .write(&ClientRequest::new(
                "req_context_inject",
                IpcCommand::ContextInject,
                json!({ "context_id": show_payload["context_id"], "agent_id": agent_id }),
            ))
            .await
            .unwrap();
        let (inject_response, injected_event) = read_response_and_event(&mut reader).await;
        assert!(inject_response.ok);
        assert_eq!(injected_event.kind, IpcEventKind::ContextInjected);

        let output = std::env::temp_dir().join(format!(
            "agentmux-context-export-{}.json",
            ulid::Ulid::new()
        ));
        writer
            .write(&ClientRequest::new(
                "req_context_export",
                IpcCommand::ContextExport,
                json!({ "output": output }),
            ))
            .await
            .unwrap();
        let export_response: DaemonResponse = reader.read().await.unwrap().unwrap();
        assert!(export_response.ok);
        assert_eq!(export_response.payload.unwrap()["context_count"], 1);
        let exported = std::fs::read_to_string(&output).unwrap();
        assert!(exported.contains("review decision"));
        std::fs::remove_file(output).unwrap();

        server.abort();
    }

    #[tokio::test]
    async fn ipc_context_show_reports_unknown_context() {
        let runtime = DaemonRuntime::new(16);
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
                "req_context_show",
                IpcCommand::ContextSearch,
                json!({ "context_id": ContextItemId::new().to_string() }),
            ))
            .await
            .unwrap();

        let response: DaemonResponse = reader.read().await.unwrap().unwrap();
        assert!(!response.ok);
        assert_eq!(response.error.unwrap().code, "CONTEXT_NOT_FOUND");

        server.abort();
    }

    #[tokio::test]
    async fn ipc_context_show_rejects_invalid_context_id() {
        let runtime = DaemonRuntime::new(16);
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
                "req_context_show",
                IpcCommand::ContextSearch,
                json!({ "context_id": "not-a-context-id" }),
            ))
            .await
            .unwrap();

        let response: DaemonResponse = reader.read().await.unwrap().unwrap();
        assert!(!response.ok);
        assert_eq!(response.error.unwrap().code, "INVALID_CONTEXT_SEARCH");

        server.abort();
    }

    #[tokio::test]
    async fn ipc_approval_commands_list_approve_and_reject() {
        let runtime = DaemonRuntime::new(16);
        let attached_agent = runtime
            .register_agent("approval-observer".to_string())
            .await;
        let approve_request = runtime
            .submit_approval_request(ApprovalRequest::command(
                agentmux_core::ApprovalKind::ShellCommand,
                "cargo test",
                "test command requires approval",
            ))
            .await;
        let reject_request = runtime
            .submit_approval_request(ApprovalRequest::command(
                agentmux_core::ApprovalKind::GitCommit,
                "git commit -m fix",
                "git commit requires approval",
            ))
            .await;

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
                "req_attach",
                IpcCommand::ClientAttach,
                json!({ "agent_id": attached_agent.id.to_string() }),
            ))
            .await
            .unwrap();
        let (attach_response, attach_event) = read_response_and_event(&mut reader).await;
        assert!(attach_response.ok);
        assert_eq!(attach_event.kind, IpcEventKind::ClientAttached);

        writer
            .write(&ClientRequest::new(
                "req_approval_list",
                IpcCommand::ApprovalList,
                json!({}),
            ))
            .await
            .unwrap();
        let list_response: DaemonResponse = reader.read().await.unwrap().unwrap();
        assert!(list_response.ok);
        let list_payload = list_response.payload.unwrap();
        assert_eq!(list_payload["approvals"].as_array().unwrap().len(), 2);
        assert_eq!(
            list_payload["approvals"][0]["status"],
            serde_json::json!("pending")
        );

        writer
            .write(&ClientRequest::new(
                "req_approval_approve",
                IpcCommand::ApprovalApprove,
                json!({ "approval_id": approve_request.id.to_string() }),
            ))
            .await
            .unwrap();
        let (approve_response, approve_event) = read_response_and_event(&mut reader).await;
        assert!(approve_response.ok);
        assert_eq!(approve_event.kind, IpcEventKind::ApprovalDecided);
        assert_eq!(approve_response.payload.unwrap()["status"], "approved");

        writer
            .write(&ClientRequest::new(
                "req_approval_reject",
                IpcCommand::ApprovalReject,
                json!({ "approval_id": reject_request.id.to_string() }),
            ))
            .await
            .unwrap();
        let (reject_response, reject_event) = read_response_and_event(&mut reader).await;
        assert!(reject_response.ok);
        assert_eq!(reject_event.kind, IpcEventKind::ApprovalDecided);
        assert_eq!(reject_response.payload.unwrap()["status"], "rejected");

        writer
            .write(&ClientRequest::new(
                "req_approval_list_after_decisions",
                IpcCommand::ApprovalList,
                json!({}),
            ))
            .await
            .unwrap();
        let list_response: DaemonResponse = reader.read().await.unwrap().unwrap();
        assert!(list_response.ok);
        assert!(
            list_response.payload.unwrap()["approvals"]
                .as_array()
                .unwrap()
                .is_empty()
        );

        server.abort();
    }

    #[tokio::test]
    async fn ipc_approval_commands_report_invalid_and_unknown_ids() {
        let runtime = DaemonRuntime::new(16);
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
                "req_approval_invalid",
                IpcCommand::ApprovalApprove,
                json!({ "approval_id": "not-an-approval-id" }),
            ))
            .await
            .unwrap();
        let invalid_response: DaemonResponse = reader.read().await.unwrap().unwrap();
        assert!(!invalid_response.ok);
        assert_eq!(invalid_response.error.unwrap().code, "INVALID_APPROVAL_ID");

        writer
            .write(&ClientRequest::new(
                "req_approval_unknown",
                IpcCommand::ApprovalReject,
                json!({ "approval_id": ApprovalId::new().to_string() }),
            ))
            .await
            .unwrap();
        let unknown_response: DaemonResponse = reader.read().await.unwrap().unwrap();
        assert!(!unknown_response.ok);
        let error = unknown_response.error.unwrap();
        assert_eq!(error.code, "APPROVAL_DECISION_FAILED");
        assert!(error.message.contains("unknown approval"));

        server.abort();
    }

    #[tokio::test]
    async fn ipc_agent_commands_focus_stop_and_report_errors() {
        let runtime = DaemonRuntime::new(16);
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
                // No provider/command → metadata-only agent (no live PTY), which is
                // exactly what the interrupt-failure path below needs to exercise.
                json!({ "name": "reviewer", "role": "reviewer" }),
            ))
            .await
            .unwrap();
        let spawn_response = read_response(&mut reader).await;
        assert!(spawn_response.ok);
        let agent_id = spawn_response.payload.unwrap()["agent_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_no_frame(&mut reader).await;

        writer
            .write(&ClientRequest::new(
                "req_focus",
                IpcCommand::AgentFocus,
                json!({ "agent_id": agent_id }),
            ))
            .await
            .unwrap();
        let (focus_response, focus_event) = read_response_and_event(&mut reader).await;
        assert!(focus_response.ok);
        assert_eq!(focus_response.payload.unwrap()["focused"], true);
        assert_eq!(focus_event.kind, IpcEventKind::ClientAttached);

        writer
            .write(&ClientRequest::new(
                "req_interrupt",
                IpcCommand::AgentInterrupt,
                json!({ "agent_id": agent_id }),
            ))
            .await
            .unwrap();
        let interrupt_response: DaemonResponse = reader.read().await.unwrap().unwrap();
        assert!(!interrupt_response.ok);
        assert_eq!(
            interrupt_response.error.unwrap().code,
            "AGENT_INTERRUPT_FAILED"
        );

        writer
            .write(&ClientRequest::new(
                "req_stop",
                IpcCommand::AgentStop,
                json!({ "agent_id": agent_id }),
            ))
            .await
            .unwrap();
        let (stop_response, exited_event) = read_response_and_event(&mut reader).await;
        assert!(stop_response.ok);
        assert_eq!(stop_response.payload.unwrap()["stopped"], true);
        assert_eq!(exited_event.kind, IpcEventKind::AgentExited);

        writer
            .write(&ClientRequest::new(
                "req_stop_unknown",
                IpcCommand::AgentStop,
                json!({ "agent_id": AgentSessionId::new().to_string() }),
            ))
            .await
            .unwrap();
        let unknown_response: DaemonResponse = reader.read().await.unwrap().unwrap();
        assert!(!unknown_response.ok);
        assert_eq!(unknown_response.error.unwrap().code, "AGENT_STOP_FAILED");

        writer
            .write(&ClientRequest::new(
                "req_focus_invalid",
                IpcCommand::AgentFocus,
                json!({ "agent_id": "not-an-agent-id" }),
            ))
            .await
            .unwrap();
        let invalid_response: DaemonResponse = reader.read().await.unwrap().unwrap();
        assert!(!invalid_response.ok);
        assert_eq!(invalid_response.error.unwrap().code, "INVALID_AGENT_ID");

        server.abort();
    }

    #[tokio::test]
    async fn ipc_layout_commands_save_list_load_and_report_unknown_layout() {
        let runtime = DaemonRuntime::new(16);
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
                "req_layout_save",
                IpcCommand::LayoutSet,
                json!({
                    "name": "default",
                    "layout": { "panes": ["planner", "implementer"] },
                }),
            ))
            .await
            .unwrap();
        let save_response: DaemonResponse = reader.read().await.unwrap().unwrap();
        assert!(save_response.ok);
        assert_eq!(save_response.payload.unwrap()["saved"], true);

        writer
            .write(&ClientRequest::new(
                "req_layout_list",
                IpcCommand::LayoutGet,
                json!({}),
            ))
            .await
            .unwrap();
        let list_response: DaemonResponse = reader.read().await.unwrap().unwrap();
        assert!(list_response.ok);
        assert_eq!(list_response.payload.unwrap()["layouts"][0], "default");

        writer
            .write(&ClientRequest::new(
                "req_layout_load",
                IpcCommand::LayoutGet,
                json!({ "name": "default" }),
            ))
            .await
            .unwrap();
        let load_response: DaemonResponse = reader.read().await.unwrap().unwrap();
        assert!(load_response.ok);
        assert_eq!(
            load_response.payload.unwrap()["layout"]["panes"][1],
            "implementer"
        );

        writer
            .write(&ClientRequest::new(
                "req_layout_unknown",
                IpcCommand::LayoutGet,
                json!({ "name": "missing" }),
            ))
            .await
            .unwrap();
        let unknown_response: DaemonResponse = reader.read().await.unwrap().unwrap();
        assert!(!unknown_response.ok);
        assert_eq!(unknown_response.error.unwrap().code, "LAYOUT_NOT_FOUND");

        writer
            .write(&ClientRequest::new(
                "req_layout_invalid",
                IpcCommand::LayoutSet,
                json!({ "name": " " }),
            ))
            .await
            .unwrap();
        let invalid_response: DaemonResponse = reader.read().await.unwrap().unwrap();
        assert!(!invalid_response.ok);
        assert_eq!(invalid_response.error.unwrap().code, "INVALID_LAYOUT_SET");

        server.abort();
    }

    #[tokio::test]
    async fn ipc_rejects_protocol_version_mismatch() {
        let runtime = DaemonRuntime::new(4);
        let (client_stream, server_stream) = UnixStream::pair().unwrap();
        let server_runtime = runtime.clone();
        let server =
            tokio::spawn(async move { handle_client(server_stream, server_runtime).await });

        let (reader, writer) = client_stream.into_split();
        let mut reader = JsonlReader::new(BufReader::new(reader));
        let mut writer = JsonlWriter::new(writer);
        writer
            .write(&ClientHello {
                kind: "hello".to_string(),
                payload: agentmux_ipc::protocol::ClientHelloPayload {
                    client_version: "0.1.0".to_string(),
                    protocol: agentmux_ipc::PROTOCOL_VERSION + 1,
                },
            })
            .await
            .unwrap();

        let response: DaemonResponse = reader.read().await.unwrap().unwrap();
        assert!(!response.ok);
        assert_eq!(response.error.unwrap().code, "PROTOCOL_VERSION_MISMATCH");

        server.abort();
    }
