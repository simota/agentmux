    use super::*;
    use std::collections::BTreeMap;
    use std::time::Duration;

    use agentmux_agent::adapter::{InputPrecondition, InputSafety};
    use agentmux_agent::{
        AgentResultStatus, InputAction, OutgoingMessage, OutgoingMessageKind, OutgoingPriority,
        ResultRecommendation, ResultRisk,
    };
    use agentmux_core::InputScriptId;
    use agentmux_ipc::{IpcCommand, JsonlReader, JsonlWriter};
    use agentmux_store::EventLog;

    impl DaemonRuntime {
        /// Test helper: persist a live result with a fresh dedup ring and map the
        /// outcome to `bool` (true == a new result was persisted). Mirrors the
        /// pre-dedup `Ok(bool)` contract for the single-call test cases.
        async fn persist_live_agent_result_once(
            &self,
            agent_id: Option<&AgentSessionId>,
            agent_name: &str,
            output_tail: &str,
        ) -> Result<bool> {
            let mut seen = SeenResultHashes::new(8);
            let outcome = self
                .persist_live_agent_result(agent_id, agent_name, output_tail, &mut seen)
                .await?;
            Ok(matches!(outcome, LiveResultOutcome::Persisted))
        }
    }

    #[test]
    fn message_input_script_has_wait_between_paste_and_enter() {
        // Regression: without a Wait, PasteText and PressEnter bytes arrive in
        // the same PTY read chunk. Codex's crossterm event parser coalesces the
        // trailing `\r` into the bracketed-paste buffer instead of treating it
        // as a submit keypress, causing intermittent injection failures.
        let prepared = PreparedInjection {
            message_id: MessageId::new(),
            agent_id: AgentSessionId::new(),
            prompt: "hello world".to_string(),
        };
        let script = message_input_script(&prepared, Duration::from_millis(120));
        assert_eq!(
            script.actions.len(),
            3,
            "script must have exactly three actions: PasteText, Wait, PressEnter"
        );
        assert!(
            matches!(script.actions[0], InputAction::PasteText(_)),
            "first action must be PasteText"
        );
        assert!(
            matches!(script.actions[1], InputAction::Wait(_)),
            "second action must be a Wait to split PTY read chunks"
        );
        assert!(
            matches!(script.actions[2], InputAction::PressEnter),
            "third action must be PressEnter"
        );
    }

    /// Regression: the split-write path in `write_input_actions_to_agent_pty`
    /// flushes pending bytes, releases the PTY lock, awaits the sleep, then
    /// re-acquires the lock for the next write.  Byte ordering must be preserved:
    /// bracketed-paste bytes first, then (after the Wait) the `\r` enter byte —
    /// even when the Wait duration is zero (cfg(test)).
    #[test]
    fn split_write_path_preserves_paste_then_enter_byte_order() {
        let prepared = PreparedInjection {
            message_id: MessageId::new(),
            agent_id: AgentSessionId::new(),
            prompt: "order check".to_string(),
        };
        // Duration::ZERO mimics cfg(test) timing (no real sleep in production tests).
        let script = message_input_script(&prepared, Duration::ZERO);

        // Collect the encoded bytes in the order `write_input_actions_to_agent_pty`
        // would flush them: each Wait boundary produces a separate flush, so we
        // split on Wait actions and verify the byte sequences on each side.
        let mut chunks: Vec<Vec<u8>> = Vec::new();
        let mut current: Vec<u8> = Vec::new();
        for action in &script.actions {
            match encode_input_action(action).expect("encoding must succeed") {
                EncodedInputStep::Bytes(bytes) => current.extend_from_slice(&bytes),
                EncodedInputStep::Wait(_) => {
                    chunks.push(std::mem::take(&mut current));
                }
            }
        }
        if !current.is_empty() {
            chunks.push(current);
        }

        // Chunk 0: bracketed-paste wrapping the prompt text.
        assert_eq!(chunks.len(), 2, "one chunk before Wait, one after");
        assert!(
            chunks[0].starts_with(b"\x1b[200~"),
            "chunk 0 must start with bracketed-paste open"
        );
        assert!(
            chunks[0].ends_with(b"\x1b[201~"),
            "chunk 0 must end with bracketed-paste close"
        );
        assert!(
            chunks[0].windows(11).any(|w| w == b"order check"),
            "chunk 0 must contain the prompt body"
        );

        // Chunk 1: the submit Enter byte, written after the Wait.
        assert_eq!(chunks[1], b"\r", "chunk 1 must be exactly the carriage-return enter byte");
    }

    #[test]
    fn shell_provider_without_command_maps_to_a_live_pty_spec() {
        // Regression: a bare `shell` spawn carries no `command`, which used to
        // fall through to a PTY-less metadata session ("nothing can be typed in").
        let spec = pty_spawn_spec_from_payload(&json!({
            "provider": "shell",
            "role": "shell",
            "name": "shell",
        }))
        .expect("spec builds")
        .expect("shell provider yields a live PTY spec");

        assert!(!spec.command.is_empty(), "shell command resolved");
        assert!(spec.env.contains_key("TERM"), "TERM is set for the shell");
        assert!(
            spec.env.contains_key("PATH"),
            "daemon environment is inherited so the shell is usable"
        );
    }

    #[test]
    fn terminal_size_payload_requires_positive_rows_and_cols() {
        let size = terminal_size_payload(&json!({ "rows": 22, "cols": 78 }), "agent.resize")
            .expect("valid size");

        assert_eq!(size.rows, 22);
        assert_eq!(size.cols, 78);
        assert!(terminal_size_payload(&json!({ "rows": 0, "cols": 78 }), "agent.resize").is_err());
        assert!(terminal_size_payload(&json!({ "rows": 22 }), "agent.resize").is_err());
    }

    #[test]
    fn coding_agent_providers_map_to_live_pty_commands() {
        for (provider, command) in [("claude", "claude"), ("codex", "codex"), ("agy", "agy")] {
            let spec = pty_spawn_spec_from_payload(&json!({
                "provider": provider,
                "role": "implementer",
                "name": provider,
            }))
            .expect("provider payload parses")
            .expect("provider yields a live PTY spec");

            assert_eq!(spec.command, command);
        }
    }

    #[test]
    fn agy_provider_defaults_to_strong_permission_mode() {
        let spec = pty_spawn_spec_from_payload(&json!({
            "provider": "agy",
            "role": "implementer",
            "name": "agy",
        }))
        .expect("provider payload parses")
        .expect("provider yields a live PTY spec");

        assert_eq!(spec.command, "agy");
        assert_eq!(spec.args, vec!["--dangerously-skip-permissions"]);
    }

    #[test]
    fn explicit_provider_args_override_agy_permission_default() {
        let spec = pty_spawn_spec_from_payload(&json!({
            "provider": "agy",
            "role": "implementer",
            "name": "agy",
            "args": ["--sandbox"],
        }))
        .expect("provider payload parses")
        .expect("provider yields a live PTY spec");

        assert_eq!(spec.command, "agy");
        assert_eq!(spec.args, vec!["--sandbox"]);
    }

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
    async fn spawned_agent_receives_identity_environment() {
        let runtime = DaemonRuntime::new(8);
        let root = std::env::temp_dir().join(format!("agentmux-env-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&root).expect("temporary root is created");
        let output_path = root.join("env.txt");
        let script = "printf '%s\n%s\n%s\n' \"$AGENTMUX_AGENT_NAME\" \"$AGENTMUX_AGENT_ROLE\" \"$AGENTMUX_AGENT_ID\" > \"$1\"";

        let agent = runtime
            .spawn_agent_with_role(
                "codex-a1b2c3".to_string(),
                AgentRole::Implementer,
                PtySpawnSpec {
                    command: "/bin/sh".to_string(),
                    args: vec![
                        "-c".to_string(),
                        script.to_string(),
                        "agentmux-env-test".to_string(),
                        output_path.to_string_lossy().into_owned(),
                    ],
                    cwd: root.clone(),
                    env: BTreeMap::from([("TERM".to_string(), "xterm-256color".to_string())]),
                    size: agentmux_pty::TerminalSize::default(),
                },
            )
            .await
            .expect("agent spawns");

        for _ in 0..20 {
            if output_path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        let contents = std::fs::read_to_string(&output_path).expect("env output is written");
        let lines = contents.lines().collect::<Vec<_>>();
        assert_eq!(lines[0], "codex-a1b2c3");
        assert_eq!(lines[1], "implementer");
        assert_eq!(lines[2], agent.id.to_string());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unknown_provider_without_command_stays_metadata_only() {
        let spec =
            pty_spawn_spec_from_payload(&json!({ "provider": "mystery" })).expect("spec builds");
        assert!(
            spec.is_none(),
            "unknown provider with no command is metadata-only"
        );
    }

    #[tokio::test]
    async fn runtime_registers_attaches_and_detaches_client() {
        let runtime = DaemonRuntime::new(8);
        let client_id = ClientSessionId::new();
        let agent = runtime.register_agent("impl-codex".to_string()).await;

        runtime
            .attach_client(client_id.clone(), agent.id.clone())
            .await
            .unwrap();
        let status = runtime.status_payload().await;
        assert_eq!(status["agent_count"], 1);
        assert_eq!(
            status["agents"][0]["attached_clients"][0],
            client_id.to_string()
        );

        let detached = runtime.detach_client(&client_id).await;
        assert_eq!(detached, Some(agent.id));
        let status = runtime.status_payload().await;
        assert_eq!(
            status["agents"][0]["attached_clients"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }

    #[tokio::test]
    async fn task_run_shell_stub_completes_standard_workflow() {
        let root = std::env::temp_dir().join(format!("agentmux-daemon-task-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&root).expect("temporary root is created");
        let runtime = DaemonRuntime::new(8);

        let payload = runtime
            .run_task_with_shell_stubs(
                "small deterministic task".to_string(),
                "shell-stub".to_string(),
                root.clone(),
            )
            .await
            .expect("shell-stub task run completes");

        assert_eq!(payload["status"], "completed");
        assert_eq!(payload["stage"], "Completed");
        assert_eq!(payload["shell_processes"].as_array().unwrap().len(), 4);
        let handoffs = payload["handoffs"].as_array().unwrap();
        assert_eq!(handoffs.len(), 4);
        assert!(handoffs[1]["body"].as_str().unwrap().contains("impl"));
        let persisted_messages = runtime.list_messages().await;
        assert_eq!(persisted_messages.len(), 4);
        assert!(
            persisted_messages.iter().all(|message| message
                .task_id
                .as_ref()
                .map(ToString::to_string)
                == Some(payload["task_id"].as_str().unwrap().to_string()))
        );
        assert!(
            payload["final_summary"]
                .as_str()
                .unwrap()
                .contains("promote approved candidate worktree")
        );

        std::fs::remove_dir_all(root).expect("temporary root is removed");
    }

    #[tokio::test]
    async fn agent_result_messages_are_persisted_to_message_bus() {
        let runtime = DaemonRuntime::new(8);
        let task_id = TaskId::new();
        let team = default_claude_codex_team();
        runtime
            .register_task_team_message_agents(&task_id, &team)
            .await;
        let result = AgentResult {
            status: AgentResultStatus::Completed,
            summary: "Planner found test work.".to_string(),
            changed_files: Vec::new(),
            messages: vec![OutgoingMessage {
                to: "role:tester".to_string(),
                kind: OutgoingMessageKind::TestResult,
                body: "Run focused daemon message tests.".to_string(),
                priority: OutgoingPriority::High,
                context_refs: Vec::new(),
                artifact_refs: Vec::new(),
            }],
            context_updates: Vec::new(),
            needs: Vec::new(),
            next: Some("impl-codex".to_string()),
            recommendation: Some(ResultRecommendation::Continue),
            risk: Some(ResultRisk::Low),
        };

        let messages = runtime
            .persist_agent_result_messages(
                &AgentRouteIdentity {
                    name: "planner".to_string(),
                    role: AgentRole::Planner,
                },
                task_id.clone(),
                &team,
                result,
            )
            .await
            .expect("result messages persist");

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].task_id, Some(task_id));
        assert_eq!(
            messages[0].from,
            MessageSource::TeamAgent("planner".to_string())
        );
        assert_eq!(messages[0].kind, MessageKind::TestResult);
        assert_eq!(messages[0].to, MessageTarget::Role(AgentRole::Tester));
        assert_eq!(messages[0].delivery_mode, DeliveryMode::InjectWhenIdle);
        assert_eq!(runtime.list_messages().await.len(), 1);
    }

    #[tokio::test]
    async fn registered_agent_role_is_used_for_message_routing() {
        let runtime = DaemonRuntime::new(8);
        runtime
            .register_agent_with_role("custom-name".to_string(), AgentRole::Tester)
            .await;

        let message = runtime
            .create_message(NewAgentMessage {
                task_id: None,
                thread_id: None,
                from: MessageSource::Orchestrator,
                to: MessageTarget::Role(AgentRole::Tester),
                kind: MessageKind::TestResult,
                priority: Priority::Normal,
                body: "verify role routing".to_string(),
                context_refs: Vec::new(),
                artifact_refs: Vec::new(),
                delivery_mode: DeliveryMode::InjectWhenIdle,
                requires_response: false,
            })
            .await
            .expect("role target resolves");

        let messages = runtime.list_messages().await;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, message.id);
        assert_eq!(messages[0].to, MessageTarget::Role(AgentRole::Tester));

        let status = runtime.status_payload().await;
        assert_eq!(status["agents"][0]["name"], "custom-name");
        assert_eq!(status["agents"][0]["role"], "tester");
    }

    #[tokio::test]
    async fn live_agent_result_output_is_persisted_to_message_bus() {
        let runtime = DaemonRuntime::new(8);
        runtime.register_agent("tester".to_string()).await;
        let output = r#"
some terminal output
AGENTMUX_RESULT:
{
  "status": "completed",
  "summary": "Implementation is ready for test.",
  "changed_files": [],
  "messages": [
    {
      "to": "role:tester",
      "kind": "TestResult",
      "body": "Run the focused daemon message tests.",
      "priority": "normal"
    }
  ],
  "context_updates": [],
  "needs": [],
  "next": null
}
"#;

        let persisted = runtime
            .persist_live_agent_result_once(None, "impl-codex", output)
            .await
            .expect("live result persists");

        assert!(persisted);
        let messages = runtime.list_messages().await;
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].from,
            MessageSource::TeamAgent("impl-codex".to_string())
        );
        assert_eq!(messages[0].to, MessageTarget::Role(AgentRole::Tester));
        assert_eq!(messages[0].body, "Run the focused daemon message tests.");
    }

    #[tokio::test]
    async fn live_agent_result_can_target_unique_agent_name() {
        let runtime = DaemonRuntime::new(8);
        runtime
            .register_agent_with_role("tester-a1b2c3".to_string(), AgentRole::Tester)
            .await;
        let output = r#"
AGENTMUX_RESULT:
{
  "status": "completed",
  "summary": "Implementation is ready for named test.",
  "changed_files": [],
  "messages": [
    {
      "to": "agent:tester-a1b2c3",
      "kind": "TestResult",
      "body": "Run only the named tester session.",
      "priority": "normal"
    }
  ],
  "context_updates": [],
  "needs": [],
  "next": null
}
"#;

        let persisted = runtime
            .persist_live_agent_result_once(None, "impl-codex", output)
            .await
            .expect("live result persists");

        assert!(persisted);
        let messages = runtime.list_messages().await;
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].to,
            MessageTarget::AgentName("tester-a1b2c3".to_string())
        );
        assert_eq!(messages[0].body, "Run only the named tester session.");
    }

    /// Multi-turn exchange: the same session emits two *different* results in
    /// sequence (sharing a dedup ring). Both must be persisted — content-hash
    /// dedup must not collapse distinct results into one (the old one-shot latch
    /// dropped the second turn entirely).
    #[tokio::test]
    async fn distinct_results_from_same_session_are_both_persisted() {
        let runtime = DaemonRuntime::new(8);
        runtime
            .register_agent_with_role("tester".to_string(), AgentRole::Tester)
            .await;
        let mut seen = SeenResultHashes::new(8);

        let first = "AGENTMUX_RESULT:\n{\n  \"status\": \"completed\",\n  \"summary\": \"first turn\",\n  \"messages\": [ { \"to\": \"role:tester\", \"kind\": \"TestResult\", \"body\": \"run turn one\", \"priority\": \"normal\" } ],\n  \"next\": null\n}\n";
        let outcome = runtime
            .persist_live_agent_result(None, "impl-codex", first, &mut seen)
            .await
            .expect("first persists");
        assert_eq!(outcome, LiveResultOutcome::Persisted);

        // A genuinely different result (different summary/body), as if a second
        // turn occurred. The accumulated tail would contain both markers; rfind
        // picks the latest, which is new content.
        let second = format!(
            "{first}\nmore output\nAGENTMUX_RESULT:\n{{\n  \"status\": \"completed\",\n  \"summary\": \"second turn\",\n  \"messages\": [ {{ \"to\": \"role:tester\", \"kind\": \"TestResult\", \"body\": \"run turn two\", \"priority\": \"normal\" }} ],\n  \"next\": null\n}}\n"
        );
        let outcome = runtime
            .persist_live_agent_result(None, "impl-codex", &second, &mut seen)
            .await
            .expect("second persists");
        assert_eq!(outcome, LiveResultOutcome::Persisted);

        let messages = runtime.list_messages().await;
        assert_eq!(messages.len(), 2, "both distinct turns must be persisted");
        let bodies: Vec<&str> = messages.iter().map(|m| m.body.as_str()).collect();
        assert!(bodies.contains(&"run turn one"));
        assert!(bodies.contains(&"run turn two"));
    }

    /// Drip/repaint: the same result block is re-emitted (e.g. the tail still
    /// ends with the identical marker on the next chunk). Content-hash dedup
    /// must persist it exactly once.
    #[tokio::test]
    async fn repainted_identical_result_is_persisted_only_once() {
        let runtime = DaemonRuntime::new(8);
        runtime
            .register_agent_with_role("tester".to_string(), AgentRole::Tester)
            .await;
        let mut seen = SeenResultHashes::new(8);

        let output = "AGENTMUX_RESULT:\n{\n  \"status\": \"completed\",\n  \"summary\": \"repaint me\",\n  \"messages\": [ { \"to\": \"role:tester\", \"kind\": \"TestResult\", \"body\": \"only once\", \"priority\": \"normal\" } ],\n  \"next\": null\n}\n";

        let first = runtime
            .persist_live_agent_result(None, "impl-codex", output, &mut seen)
            .await
            .expect("first persists");
        assert_eq!(first, LiveResultOutcome::Persisted);

        // Same content re-rendered: must be detected as a duplicate.
        let again = runtime
            .persist_live_agent_result(None, "impl-codex", output, &mut seen)
            .await
            .expect("repaint does not fail");
        assert_eq!(again, LiveResultOutcome::Duplicate);

        let third = runtime
            .persist_live_agent_result(None, "impl-codex", output, &mut seen)
            .await
            .expect("repaint does not fail");
        assert_eq!(third, LiveResultOutcome::Duplicate);

        assert_eq!(
            runtime.list_messages().await.len(),
            1,
            "identical repaint must persist exactly one message"
        );
    }

    /// When agent A emits AGENTMUX_RESULT with a messages[] entry targeting
    /// agent B by role, and agent B is idle (AwaitingInput), the handoff
    /// prompt must be injected into agent B's PTY automatically — without any
    /// explicit `apply_agent_status_signal` or `deliver_idle_messages_for_agent`
    /// call from outside.
    #[tokio::test]
    async fn live_agent_result_auto_delivers_to_idle_pty_agent() {
        let root = std::env::temp_dir().join(format!(
            "agentmux-result-auto-deliver-{}",
            ulid::Ulid::new()
        ));
        std::fs::create_dir_all(&root).expect("temporary root is created");
        let output_path = root.join("tester-output.txt");
        let runtime = DaemonRuntime::new(16);

        // Agent B: a live PTY tester that writes received input to a file.
        let tester = runtime
            .spawn_agent_with_role(
                "auto-tester".to_string(),
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

        // Pre-set tester status to AwaitingInput so it is eligible for idle
        // delivery.  Write directly to state to avoid triggering delivery
        // before the message exists.
        {
            let mut state = runtime.state.write().await;
            if let Some(live) = state.agents.get_mut(&tester.id) {
                live.metadata.status = Some(AgentStatus::AwaitingInput);
            }
        }

        // Agent A: a logical implementer with no PTY (as if it just finished
        // its turn and scrolled AGENTMUX_RESULT through its terminal output).
        let output = r#"
AGENTMUX_RESULT:
{
  "status": "completed",
  "summary": "Implementation complete, handing off to tester.",
  "changed_files": [],
  "messages": [
    {
      "to": "role:tester",
      "kind": "TestResult",
      "body": "auto-deliver-handoff-body",
      "priority": "normal"
    }
  ],
  "context_updates": [],
  "needs": [],
  "next": null
}
"#;

        let persisted = runtime
            .persist_live_agent_result_once(None, "auto-impl", output)
            .await
            .expect("live result persists");
        assert!(persisted, "AGENTMUX_RESULT must be parsed and persisted");

        // The message should have been created …
        let messages = runtime.list_messages().await;
        assert_eq!(messages.len(), 1, "one message is created");

        // … and delivered to the idle tester PTY automatically.
        let output = wait_for_file_contains(&output_path, "auto-deliver-handoff-body")
            .await
            .expect("handoff prompt reached tester PTY");
        assert!(
            output.contains("auto-deliver-handoff-body"),
            "tester PTY must contain the handoff body"
        );

        let messages_after = runtime.list_messages().await;
        assert_eq!(
            messages_after[0].delivery_status,
            DeliveryStatus::Delivered,
            "message delivery_status must be Delivered"
        );

        terminate_agent_process(&runtime, &tester.id.to_string()).await;
        std::fs::remove_dir_all(root).expect("temporary root is removed");
    }

    /// `message send` (the `MessageCreate` IPC command) must inject the message
    /// into an idle target PTY without a separate manual `message inject` step.
    /// Spawns a tester PTY, marks it idle (AwaitingInput), then creates an
    /// `inject_when_idle` message through the IPC dispatch arm and asserts the
    /// handoff prompt reaches the PTY and the message becomes Delivered.
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
    #[tokio::test]
    async fn live_agent_result_needs_status_probe_is_surfaced_not_persisted() {
        let runtime = DaemonRuntime::new(8);

        // A valid AGENTMUX_RESULT: marker followed by malformed JSON triggers
        // NeedsStatusProbe.
        let output = "AGENTMUX_RESULT:\n{ invalid json }\n";

        let mut seen = SeenResultHashes::new(8);
        let outcome = runtime
            .persist_live_agent_result(None, "probe-agent", output, &mut seen)
            .await
            .expect("persist call itself must not fail");

        match outcome {
            LiveResultOutcome::NeedsProbe { reason } => {
                assert!(
                    reason.contains("JSON is invalid")
                        || reason.contains("without a complete JSON object"),
                    "unexpected probe reason: {reason}"
                );
            }
            other => panic!("expected NeedsProbe, got {other:?}"),
        }

        // No message must have been persisted.
        assert!(runtime.list_messages().await.is_empty());
    }

    /// The forwarder must not spam Error events: while a drip render repeatedly
    /// yields the same NeedsProbe reason, only the first transition emits an
    /// event. A *changed* reason emits again. This mirrors the forwarder's
    /// `last_probe_reason` suppression logic at the unit level.
    #[test]
    fn needs_probe_error_events_are_suppressed_until_reason_changes() {
        // Mirrors the forwarder's `last_probe_reason` gate: emit only when the
        // reason differs from the previous emission.
        fn observe(reason: &str, last: &mut Option<String>) -> bool {
            if last.as_deref() != Some(reason) {
                *last = Some(reason.to_string());
                true
            } else {
                false
            }
        }

        let mut last_probe_reason: Option<String> = None;
        let mut emitted = 0usize;

        // Same reason repeated across drip frames -> a single emission.
        for _ in 0..3 {
            if observe("JSON is invalid: a", &mut last_probe_reason) {
                emitted += 1;
            }
        }
        assert_eq!(emitted, 1);

        // A changed reason emits once more.
        if observe("JSON is invalid: b", &mut last_probe_reason) {
            emitted += 1;
        }
        assert_eq!(emitted, 2);
    }

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
    async fn live_agent_result_target_running_turn_stays_queued_no_panic() {
        let runtime = DaemonRuntime::new(8);

        // Register a tester without a PTY so no actual PTY write can happen.
        let tester = runtime
            .register_agent_with_role("busy-tester".to_string(), AgentRole::Tester)
            .await;

        // Mark the tester as RunningTurn — not eligible for idle injection.
        {
            let mut state = runtime.state.write().await;
            if let Some(live) = state.agents.get_mut(&tester.id) {
                live.metadata.status = Some(AgentStatus::RunningTurn);
            }
        }

        let output = r#"
AGENTMUX_RESULT:
{
  "status": "completed",
  "summary": "Impl done.",
  "changed_files": [],
  "messages": [
    {
      "to": "role:tester",
      "kind": "TestResult",
      "body": "busy-tester-body",
      "priority": "normal"
    }
  ],
  "context_updates": [],
  "needs": [],
  "next": null
}
"#;
        // Must not panic or return Err.
        let persisted = runtime
            .persist_live_agent_result_once(None, "impl-agent", output)
            .await
            .expect("persist must not fail even with a busy target");
        assert!(persisted, "AGENTMUX_RESULT must be parsed and persisted");

        // The message must exist and remain Queued — not Delivered.
        let messages = runtime.list_messages().await;
        assert_eq!(messages.len(), 1, "one message is created");
        assert_ne!(
            messages[0].delivery_status,
            DeliveryStatus::Delivered,
            "message to a RunningTurn target must not be delivered yet"
        );
        // Acceptable queued states: Queued or WaitingForAgent.
        assert!(
            matches!(
                messages[0].delivery_status,
                DeliveryStatus::Queued | DeliveryStatus::WaitingForAgent
            ),
            "message delivery_status must be a waiting state, got {:?}",
            messages[0].delivery_status,
        );
    }

    /// Regression: when no session is registered for the target role
    /// (e.g. "role:tester" resolves to 0 sessions), persist_live_agent_result
    /// must still succeed and the message must remain Queued — not lost or
    /// errored.
    #[tokio::test]
    async fn live_agent_result_unregistered_target_stays_queued() {
        let runtime = DaemonRuntime::new(8);
        // Intentionally do NOT register any tester session.

        let output = r#"
AGENTMUX_RESULT:
{
  "status": "completed",
  "summary": "Impl done, no tester available yet.",
  "changed_files": [],
  "messages": [
    {
      "to": "role:tester",
      "kind": "TestResult",
      "body": "orphan-message-body",
      "priority": "normal"
    }
  ],
  "context_updates": [],
  "needs": [],
  "next": null
}
"#;
        let persisted = runtime
            .persist_live_agent_result_once(None, "impl-agent", output)
            .await
            .expect("persist must succeed even without a registered target");
        assert!(persisted, "AGENTMUX_RESULT must be parsed and persisted");

        let messages = runtime.list_messages().await;
        assert_eq!(messages.len(), 1, "message must be stored");
        assert_ne!(
            messages[0].delivery_status,
            DeliveryStatus::Delivered,
            "unroutable message must not be marked Delivered"
        );
    }

    /// Regression: when AGENTMUX_RESULT contains multiple messages targeting
    /// different agents, each eligible agent must receive its own message
    /// and its delivery_status must become Delivered.
    #[tokio::test]
    async fn live_agent_result_multiple_messages_each_delivered_to_eligible_target() {
        let root =
            std::env::temp_dir().join(format!("agentmux-multi-deliver-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&root).expect("temporary root is created");
        let tester_out = root.join("tester-out.txt");
        let reviewer_out = root.join("reviewer-out.txt");
        let runtime = DaemonRuntime::new(16);

        // Spawn tester PTY.
        let tester = runtime
            .spawn_agent_with_role(
                "multi-tester".to_string(),
                AgentRole::Tester,
                PtySpawnSpec {
                    command: "/usr/bin/perl".to_string(),
                    args: vec![
                        "-e".to_string(),
                        pty_capture_script().to_string(),
                        tester_out.display().to_string(),
                    ],
                    cwd: root.clone(),
                    env: BTreeMap::from([("TERM".to_string(), "xterm-256color".to_string())]),
                    size: Default::default(),
                },
            )
            .await
            .expect("tester agent is spawned");

        // Spawn reviewer PTY.
        let reviewer = runtime
            .spawn_agent_with_role(
                "multi-reviewer".to_string(),
                AgentRole::Reviewer,
                PtySpawnSpec {
                    command: "/usr/bin/perl".to_string(),
                    args: vec![
                        "-e".to_string(),
                        pty_capture_script().to_string(),
                        reviewer_out.display().to_string(),
                    ],
                    cwd: root.clone(),
                    env: BTreeMap::from([("TERM".to_string(), "xterm-256color".to_string())]),
                    size: Default::default(),
                },
            )
            .await
            .expect("reviewer agent is spawned");

        // Set both to AwaitingInput.
        {
            let mut state = runtime.state.write().await;
            for id in [&tester.id, &reviewer.id] {
                if let Some(live) = state.agents.get_mut(id) {
                    live.metadata.status = Some(AgentStatus::AwaitingInput);
                }
            }
        }

        let output = r#"
AGENTMUX_RESULT:
{
  "status": "completed",
  "summary": "Implementation done, notify tester and reviewer.",
  "changed_files": [],
  "messages": [
    {
      "to": "role:tester",
      "kind": "TestResult",
      "body": "multi-tester-body",
      "priority": "normal"
    },
    {
      "to": "role:reviewer",
      "kind": "ReviewComment",
      "body": "multi-reviewer-body",
      "priority": "normal"
    }
  ],
  "context_updates": [],
  "needs": [],
  "next": null
}
"#;
        let persisted = runtime
            .persist_live_agent_result_once(None, "multi-impl", output)
            .await
            .expect("persist must succeed");
        assert!(persisted, "AGENTMUX_RESULT must be parsed and persisted");

        let messages = runtime.list_messages().await;
        assert_eq!(messages.len(), 2, "two messages must be created");

        // Tester receives its message.
        let tester_content = wait_for_file_contains(&tester_out, "multi-tester-body")
            .await
            .expect("tester PTY must receive its message");
        assert!(tester_content.contains("multi-tester-body"));

        // Reviewer receives its message.
        let reviewer_content = wait_for_file_contains(&reviewer_out, "multi-reviewer-body")
            .await
            .expect("reviewer PTY must receive its message");
        assert!(reviewer_content.contains("multi-reviewer-body"));

        // Both messages must be Delivered.
        let messages_after = runtime.list_messages().await;
        for msg in &messages_after {
            assert_eq!(
                msg.delivery_status,
                DeliveryStatus::Delivered,
                "message {:?} must be Delivered, body={}",
                msg.id,
                msg.body,
            );
        }

        terminate_agent_process(&runtime, &tester.id.to_string()).await;
        terminate_agent_process(&runtime, &reviewer.id.to_string()).await;
        std::fs::remove_dir_all(root).expect("temporary root is removed");
    }

    /// Regression: an agent whose status is None (not yet reported) must be
    /// treated as InteractiveReady (the fallback in
    /// trigger_idle_delivery_for_result_messages) and must therefore receive
    /// idle-injected messages from a just-persisted AGENTMUX_RESULT.
    #[tokio::test]
    async fn live_agent_result_status_none_fallback_delivers_as_interactive_ready() {
        let root =
            std::env::temp_dir().join(format!("agentmux-none-fallback-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&root).expect("temporary root is created");
        let output_path = root.join("none-tester-out.txt");
        let runtime = DaemonRuntime::new(16);

        // Spawn a tester PTY but deliberately leave its status as None
        // (no explicit status signal received yet).
        let tester = runtime
            .spawn_agent_with_role(
                "none-tester".to_string(),
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

        // Confirm status is None (spawn does not set an explicit status).
        {
            let state = runtime.state.read().await;
            let live = state.agents.get(&tester.id).expect("agent is registered");
            assert!(
                live.metadata.status.is_none(),
                "status must be None before any status signal"
            );
        }

        let output = r#"
AGENTMUX_RESULT:
{
  "status": "completed",
  "summary": "Impl done, fallback tester.",
  "changed_files": [],
  "messages": [
    {
      "to": "role:tester",
      "kind": "TestResult",
      "body": "none-fallback-body",
      "priority": "normal"
    }
  ],
  "context_updates": [],
  "needs": [],
  "next": null
}
"#;
        let persisted = runtime
            .persist_live_agent_result_once(None, "fallback-impl", output)
            .await
            .expect("persist must succeed");
        assert!(persisted, "AGENTMUX_RESULT must be parsed and persisted");

        // The message must be delivered even though the tester's status was None.
        let content = wait_for_file_contains(&output_path, "none-fallback-body")
            .await
            .expect("tester PTY must receive the message via InteractiveReady fallback");
        assert!(content.contains("none-fallback-body"));

        let messages = runtime.list_messages().await;
        assert_eq!(
            messages[0].delivery_status,
            DeliveryStatus::Delivered,
            "message must be Delivered (status-None fallback to InteractiveReady)"
        );

        terminate_agent_process(&runtime, &tester.id.to_string()).await;
        std::fs::remove_dir_all(root).expect("temporary root is removed");
    }

    /// End-to-end spawn-order race: agent A emits an AGENTMUX_RESULT targeting
    /// role:tester *before* any tester session exists.  The message must be
    /// stored (not lost).  When a tester is spawned later, the message is
    /// backfilled into its inbox, and the first idle status signal injects the
    /// handoff prompt into the tester PTY.  This is the regression guard for
    /// the spawn-order race that backfill closes.
    #[tokio::test]
    async fn live_agent_result_backfills_to_tester_spawned_after_result() {
        let root =
            std::env::temp_dir().join(format!("agentmux-backfill-e2e-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&root).expect("temporary root is created");
        let output_path = root.join("late-tester-out.txt");
        let runtime = DaemonRuntime::new(16);

        // Agent A finishes its turn and emits a handoff to role:tester while
        // NO tester session is registered yet.
        let output = r#"
AGENTMUX_RESULT:
{
  "status": "completed",
  "summary": "Impl done, tester not spawned yet.",
  "changed_files": [],
  "messages": [
    {
      "to": "role:tester",
      "kind": "TestResult",
      "body": "backfill-late-tester-body",
      "priority": "normal"
    }
  ],
  "context_updates": [],
  "needs": [],
  "next": null
}
"#;
        let persisted = runtime
            .persist_live_agent_result_once(None, "impl-agent", output)
            .await
            .expect("persist must succeed even with no tester");
        assert!(persisted, "AGENTMUX_RESULT must be parsed and persisted");

        // The message exists but is not yet delivered.
        let messages = runtime.list_messages().await;
        assert_eq!(
            messages.len(),
            1,
            "message must be stored before tester exists"
        );
        assert_ne!(
            messages[0].delivery_status,
            DeliveryStatus::Delivered,
            "message must not be delivered while tester is absent"
        );

        // The tester is spawned later — register_agent backfills the message
        // into its inbox.
        let tester = runtime
            .spawn_agent_with_role(
                "late-tester".to_string(),
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

        // First idle status signal triggers delivery of the backfilled message.
        runtime
            .apply_agent_status_signal(&tester.id, AgentStatus::AwaitingInput, "test")
            .await
            .expect("status signal applies and delivers idle messages");

        // The handoff prompt reaches the tester PTY.
        let content = wait_for_file_contains(&output_path, "backfill-late-tester-body")
            .await
            .expect("backfilled message must reach the late-spawned tester PTY");
        assert!(content.contains("backfill-late-tester-body"));

        let messages_after = runtime.list_messages().await;
        assert_eq!(
            messages_after[0].delivery_status,
            DeliveryStatus::Delivered,
            "backfilled message must be Delivered after the tester goes idle"
        );

        terminate_agent_process(&runtime, &tester.id.to_string()).await;
        std::fs::remove_dir_all(root).expect("temporary root is removed");
    }

    #[tokio::test]
    async fn live_agent_result_for_non_arena_worktree_does_not_capture_or_test() {
        let runtime = DaemonRuntime::new(8);
        let worktree = Worktree {
            id: WorktreeId::new(),
            project_id: ProjectId::new(),
            task_id: TaskId::new(),
            owner_agent_id: None,
            path: std::env::temp_dir(),
            branch_name: "agentmux/task-non-arena".to_string(),
            base_branch: "main".to_string(),
            status: WorktreeStatus::Ready,
            created_at: DateTimeUtc::UNIX_EPOCH,
        };
        let worktree_id = worktree.id.clone();
        runtime.register_worktree(worktree).await;
        let agent = RegisteredAgentSession::with_role(
            "impl-codex".to_string(),
            AgentRole::Implementer,
            None,
        );
        {
            let mut state = runtime.state.write().await;
            state.agents.insert(
                agent.id.clone(),
                LiveAgentSession {
                    metadata: agent.clone(),
                    worktree_id: Some(worktree_id.clone()),
                    pty: None,
                    terminal: Arc::new(Mutex::new(TerminalParser::new(24, 80))),
                },
            );
        }
        let output = r#"
AGENTMUX_RESULT:
{
  "status": "completed",
  "summary": "Non arena implementation complete.",
  "changed_files": [],
  "messages": [],
  "context_updates": [],
  "needs": [],
  "next": null
}
"#;

        let persisted = runtime
            .persist_live_agent_result_once(Some(&agent.id), "impl-codex", output)
            .await
            .expect("live result persists");

        assert!(persisted);
        tokio::time::sleep(Duration::from_millis(75)).await;
        assert_eq!(
            runtime.worktree_by_id(&worktree_id).await.unwrap().status,
            WorktreeStatus::Ready
        );
        assert!(
            runtime.status_payload().await["arena_candidates"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn agent_result_next_is_persisted_as_summary_handoff() {
        let runtime = DaemonRuntime::new(8);
        let task_id = TaskId::new();
        let team = default_claude_codex_team();
        runtime
            .register_task_team_message_agents(&task_id, &team)
            .await;
        let result = AgentResult {
            status: AgentResultStatus::Completed,
            summary: "Implement the selected fix.".to_string(),
            changed_files: Vec::new(),
            messages: Vec::new(),
            context_updates: Vec::new(),
            needs: Vec::new(),
            next: Some("impl-codex".to_string()),
            recommendation: Some(ResultRecommendation::Continue),
            risk: Some(ResultRisk::Low),
        };

        let messages = runtime
            .persist_agent_result_messages(
                &AgentRouteIdentity {
                    name: "planner".to_string(),
                    role: AgentRole::Planner,
                },
                task_id.clone(),
                &team,
                result,
            )
            .await
            .expect("next handoff persists");

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].task_id, Some(task_id));
        assert_eq!(
            messages[0].from,
            MessageSource::TeamAgent("planner".to_string())
        );
        assert_eq!(messages[0].kind, MessageKind::Handoff);
        assert_eq!(messages[0].to, MessageTarget::Role(AgentRole::Implementer));
        assert!(messages[0].body.contains("Implement the selected fix."));
        assert_eq!(runtime.list_messages().await.len(), 1);
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
        assert_eq!(spawn_payload["role"], "implementer");
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
    async fn ipc_worktree_commands_list_test_promote_and_archive() {
        let root =
            std::env::temp_dir().join(format!("agentmux-worktree-ipc-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&root).expect("temporary worktree root is created");
        test_git(&root, ["init", "-b", "main"]);
        std::fs::write(root.join("README.md"), "hello\n").unwrap();
        test_git(&root, ["add", "README.md"]);
        test_git(
            &root,
            [
                "-c",
                "user.name=Agentmux Test",
                "-c",
                "user.email=agentmux@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        test_git(&root, ["branch", "agentmux/task-impl"]);
        let runtime = DaemonRuntime::new(16);
        let attached_agent = runtime
            .register_agent("worktree-observer".to_string())
            .await;
        let worktree = Worktree {
            id: WorktreeId::new(),
            project_id: ProjectId::new(),
            task_id: agentmux_core::TaskId::new(),
            owner_agent_id: None,
            path: root.clone(),
            branch_name: "agentmux/task-impl".to_string(),
            base_branch: "main".to_string(),
            status: WorktreeStatus::Ready,
            created_at: DateTimeUtc::UNIX_EPOCH,
        };
        let worktree_id = worktree.id.to_string();
        let parsed_worktree_id = worktree.id.clone();
        runtime.register_worktree(worktree).await;

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
                "req_worktree_list",
                IpcCommand::WorktreeList,
                json!({}),
            ))
            .await
            .unwrap();
        let list_response: DaemonResponse = reader.read().await.unwrap().unwrap();
        assert!(list_response.ok);
        let list_payload = list_response.payload.unwrap();
        assert_eq!(list_payload["worktrees"].as_array().unwrap().len(), 1);
        assert_eq!(list_payload["worktrees"][0]["worktree_id"], worktree_id);

        writer
            .write(&ClientRequest::new(
                "req_worktree_test",
                IpcCommand::WorktreeTest,
                json!({
                    "worktree_id": worktree_id,
                    "name": "smoke",
                    "command": "printf test-ok",
                }),
            ))
            .await
            .unwrap();
        let (test_response, artifact_event) = read_response_and_event(&mut reader).await;
        assert!(test_response.ok);
        assert_eq!(artifact_event.kind, IpcEventKind::ArtifactCreated);
        let test_event: DaemonEvent = reader.read().await.unwrap().unwrap();
        assert_eq!(test_event.kind, IpcEventKind::WorktreeTestCompleted);
        let test_payload = test_response.payload.unwrap();
        assert_eq!(test_payload["worktree"]["status"], "review_ready");
        assert_eq!(test_payload["test"]["status"], "passed");
        assert!(
            std::fs::read_to_string(test_payload["test"]["artifact"]["path"].as_str().unwrap())
                .unwrap()
                .contains("test-ok")
        );
        mark_arena_candidate(
            &runtime,
            parsed_worktree_id.clone(),
            Some("README.md | 0".to_string()),
            Some(TestRunStatus::Passed),
        )
        .await;

        writer
            .write(&ClientRequest::new(
                "req_worktree_promote",
                IpcCommand::WorktreePromote,
                json!({ "worktree_id": test_payload["worktree"]["worktree_id"] }),
            ))
            .await
            .unwrap();
        let promote_response: DaemonResponse = reader.read().await.unwrap().unwrap();
        assert!(promote_response.ok);
        let promote_payload = promote_response.payload.unwrap();
        assert_eq!(promote_payload["status"], "pending");
        assert_eq!(promote_payload["worktree_id"], worktree_id);
        assert_eq!(
            runtime
                .worktree_by_id(&parsed_worktree_id)
                .await
                .unwrap()
                .status,
            WorktreeStatus::ReviewReady
        );
        let approval_event: DaemonEvent = reader.read().await.unwrap().unwrap();
        assert_eq!(approval_event.kind, IpcEventKind::ApprovalCreated);
        let adopt_event: DaemonEvent = reader.read().await.unwrap().unwrap();
        assert_eq!(adopt_event.kind, IpcEventKind::WorktreeAdoptRequested);

        writer
            .write(&ClientRequest::new(
                "req_worktree_archive",
                IpcCommand::WorktreeArchive,
                json!({ "worktree_id": test_payload["worktree"]["worktree_id"] }),
            ))
            .await
            .unwrap();
        let archive_response: DaemonResponse = reader.read().await.unwrap().unwrap();
        assert!(archive_response.ok);
        assert_eq!(archive_response.payload.unwrap()["status"], "archived");

        server.abort();
        std::fs::remove_dir_all(root).expect("temporary worktree root is removed");
    }

    #[tokio::test]
    async fn ipc_worktree_diff_reports_unknown_worktree() {
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
                "req_worktree_diff",
                IpcCommand::WorktreeDiff,
                json!({ "worktree_id": WorktreeId::new().to_string() }),
            ))
            .await
            .unwrap();

        let response: DaemonResponse = reader.read().await.unwrap().unwrap();
        assert!(!response.ok);
        assert_eq!(response.error.unwrap().code, "WORKTREE_DIFF_FAILED");

        server.abort();
    }

    #[tokio::test]
    async fn worktree_adopt_requires_approval_and_reject_archives() {
        let runtime = DaemonRuntime::new(16);
        let worktree = Worktree {
            id: WorktreeId::new(),
            project_id: ProjectId::new(),
            task_id: TaskId::new(),
            owner_agent_id: None,
            path: std::env::temp_dir(),
            branch_name: "agentmux/task-impl".to_string(),
            base_branch: "main".to_string(),
            status: WorktreeStatus::ReviewReady,
            created_at: DateTimeUtc::UNIX_EPOCH,
        };
        let worktree_id = worktree.id.clone();
        runtime.register_worktree(worktree).await;
        mark_arena_candidate(
            &runtime,
            worktree_id.clone(),
            Some("README.md | 0".to_string()),
            Some(TestRunStatus::Passed),
        )
        .await;

        let approval = runtime
            .request_worktree_adoption(worktree_id.clone())
            .await
            .expect("adoption approval is queued");

        assert_eq!(approval.worktree_id, Some(worktree_id.clone()));
        assert_eq!(
            runtime.worktree_by_id(&worktree_id).await.unwrap().status,
            WorktreeStatus::ReviewReady
        );

        runtime
            .reject_approval(&approval.id)
            .await
            .expect("approval is rejected");
        wait_for_worktree_status(&runtime, &worktree_id, WorktreeStatus::Archived).await;
    }

    #[tokio::test]
    async fn ipc_worktree_promote_without_approval_queues_request_and_does_not_merge() {
        let root =
            std::env::temp_dir().join(format!("agentmux-unapproved-promote-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&root).expect("temporary worktree root is created");
        test_git(&root, ["init", "-b", "main"]);
        std::fs::write(root.join("README.md"), "hello\n").unwrap();
        test_git(&root, ["add", "README.md"]);
        test_git(
            &root,
            [
                "-c",
                "user.name=Agentmux Test",
                "-c",
                "user.email=agentmux@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        test_git(&root, ["checkout", "-b", "agentmux/task-impl"]);
        std::fs::write(root.join("feature.txt"), "candidate\n").unwrap();
        test_git(&root, ["add", "feature.txt"]);
        test_git(
            &root,
            [
                "-c",
                "user.name=Agentmux Test",
                "-c",
                "user.email=agentmux@example.invalid",
                "commit",
                "-m",
                "candidate",
            ],
        );
        test_git(&root, ["checkout", "main"]);
        let runtime = DaemonRuntime::new(16);
        let worktree = Worktree {
            id: WorktreeId::new(),
            project_id: ProjectId::new(),
            task_id: TaskId::new(),
            owner_agent_id: None,
            path: root.clone(),
            branch_name: "agentmux/task-impl".to_string(),
            base_branch: "main".to_string(),
            status: WorktreeStatus::ReviewReady,
            created_at: DateTimeUtc::UNIX_EPOCH,
        };
        let worktree_id = worktree.id.clone();
        runtime
            .register_worktree_with_repo_root(worktree, root.clone())
            .await;
        mark_arena_candidate(
            &runtime,
            worktree_id.clone(),
            Some("feature.txt | 1".to_string()),
            Some(TestRunStatus::Passed),
        )
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
                "req_worktree_promote",
                IpcCommand::WorktreePromote,
                json!({ "worktree_id": worktree_id.to_string() }),
            ))
            .await
            .unwrap();
        let promote_response: DaemonResponse = reader.read().await.unwrap().unwrap();

        assert!(promote_response.ok);
        let promote_payload = promote_response.payload.unwrap();
        assert_eq!(promote_payload["status"], "pending");
        assert_eq!(
            runtime.worktree_by_id(&worktree_id).await.unwrap().status,
            WorktreeStatus::ReviewReady
        );
        assert!(runtime.list_approvals().await.len() == 1);
        assert_eq!(git_stdout(&root, ["branch", "--show-current"]), "main\n");
        assert!(!root.join("feature.txt").exists());
        assert!(git_stdout(&root, ["branch", "--list", "agentmux/integration"]).is_empty());

        server.abort();
        std::fs::remove_dir_all(root).expect("temporary worktree root is removed");
    }

    #[tokio::test]
    async fn worktree_adopt_unknown_worktree_does_not_queue_approval() {
        let runtime = DaemonRuntime::new(16);
        let unknown_id = WorktreeId::new();

        let error = runtime
            .request_worktree_adoption(unknown_id.clone())
            .await
            .expect_err("unknown worktree adoption is rejected");

        assert!(error.to_string().contains(&unknown_id.to_string()));
        assert!(runtime.list_approvals().await.is_empty());
    }

    #[tokio::test]
    async fn worktree_adopt_before_diff_capture_is_rejected() {
        let runtime = DaemonRuntime::new(16);
        let worktree = Worktree {
            id: WorktreeId::new(),
            project_id: ProjectId::new(),
            task_id: TaskId::new(),
            owner_agent_id: None,
            path: std::env::temp_dir(),
            branch_name: "agentmux/task-impl".to_string(),
            base_branch: "main".to_string(),
            status: WorktreeStatus::ReviewReady,
            created_at: DateTimeUtc::UNIX_EPOCH,
        };
        let worktree_id = worktree.id.clone();
        runtime.register_worktree(worktree).await;
        mark_arena_candidate(
            &runtime,
            worktree_id.clone(),
            None,
            Some(TestRunStatus::Passed),
        )
        .await;

        let error = runtime
            .request_worktree_adoption(worktree_id)
            .await
            .expect_err("adoption before diff capture is rejected");

        assert!(error.to_string().contains("captured diff"));
        assert!(runtime.list_approvals().await.is_empty());
    }

    #[tokio::test]
    async fn worktree_adopt_after_test_failure_is_rejected() {
        let runtime = DaemonRuntime::new(16);
        let worktree = Worktree {
            id: WorktreeId::new(),
            project_id: ProjectId::new(),
            task_id: TaskId::new(),
            owner_agent_id: None,
            path: std::env::temp_dir(),
            branch_name: "agentmux/task-impl".to_string(),
            base_branch: "main".to_string(),
            status: WorktreeStatus::Failed,
            created_at: DateTimeUtc::UNIX_EPOCH,
        };
        let worktree_id = worktree.id.clone();
        runtime.register_worktree(worktree).await;
        mark_arena_candidate(
            &runtime,
            worktree_id.clone(),
            Some("README.md | 1".to_string()),
            Some(TestRunStatus::Failed),
        )
        .await;

        let error = runtime
            .request_worktree_adoption(worktree_id)
            .await
            .expect_err("adoption after failed tests is rejected");

        assert!(error.to_string().contains("passed tests"));
        assert!(runtime.list_approvals().await.is_empty());
    }

    #[tokio::test]
    async fn worktree_adopt_rejects_second_pending_approval() {
        let runtime = DaemonRuntime::new(16);
        let first = Worktree {
            id: WorktreeId::new(),
            project_id: ProjectId::new(),
            task_id: TaskId::new(),
            owner_agent_id: None,
            path: std::env::temp_dir(),
            branch_name: "agentmux/task-first".to_string(),
            base_branch: "main".to_string(),
            status: WorktreeStatus::ReviewReady,
            created_at: DateTimeUtc::UNIX_EPOCH,
        };
        let second = Worktree {
            id: WorktreeId::new(),
            branch_name: "agentmux/task-second".to_string(),
            ..first.clone()
        };
        let first_id = first.id.clone();
        let second_id = second.id.clone();
        runtime.register_worktree(first).await;
        runtime.register_worktree(second).await;
        mark_arena_candidate(
            &runtime,
            first_id.clone(),
            Some("first".to_string()),
            Some(TestRunStatus::Passed),
        )
        .await;
        mark_arena_candidate(
            &runtime,
            second_id.clone(),
            Some("second".to_string()),
            Some(TestRunStatus::Passed),
        )
        .await;

        let first_approval = runtime
            .request_worktree_adoption(first_id.clone())
            .await
            .expect("first adoption approval is queued");
        let second_error = runtime
            .request_worktree_adoption(second_id.clone())
            .await
            .expect_err("second pending adoption is rejected");

        assert!(second_error.to_string().contains("already pending"));
        assert_eq!(runtime.list_approvals().await.len(), 1);

        runtime
            .reject_approval(&first_approval.id)
            .await
            .expect("first approval is rejected");
        wait_for_worktree_status(&runtime, &first_id, WorktreeStatus::Archived).await;

        let second_approval = runtime
            .request_worktree_adoption(second_id.clone())
            .await
            .expect("adoption is allowed after pending approval is decided");
        assert_eq!(second_approval.worktree_id, Some(second_id));
    }

    #[tokio::test]
    async fn rejecting_one_worktree_adoption_keeps_other_candidate_ready() {
        let runtime = DaemonRuntime::new(16);
        let first = Worktree {
            id: WorktreeId::new(),
            project_id: ProjectId::new(),
            task_id: TaskId::new(),
            owner_agent_id: None,
            path: std::env::temp_dir(),
            branch_name: "agentmux/task-first".to_string(),
            base_branch: "main".to_string(),
            status: WorktreeStatus::ReviewReady,
            created_at: DateTimeUtc::UNIX_EPOCH,
        };
        let second = Worktree {
            id: WorktreeId::new(),
            branch_name: "agentmux/task-second".to_string(),
            ..first.clone()
        };
        let first_id = first.id.clone();
        let second_id = second.id.clone();
        runtime.register_worktree(first).await;
        runtime.register_worktree(second).await;
        mark_arena_candidate(
            &runtime,
            first_id.clone(),
            Some("first".to_string()),
            Some(TestRunStatus::Passed),
        )
        .await;
        mark_arena_candidate(
            &runtime,
            second_id.clone(),
            Some("second".to_string()),
            Some(TestRunStatus::Passed),
        )
        .await;

        let approval = runtime
            .request_worktree_adoption(first_id.clone())
            .await
            .expect("adoption approval is queued");
        runtime
            .reject_approval(&approval.id)
            .await
            .expect("approval is rejected");

        wait_for_worktree_status(&runtime, &first_id, WorktreeStatus::Archived).await;
        assert_eq!(
            runtime.worktree_by_id(&second_id).await.unwrap().status,
            WorktreeStatus::ReviewReady
        );
    }

    #[tokio::test]
    async fn approving_adoption_for_missing_repo_reports_error_without_status_change() {
        let runtime = DaemonRuntime::new(16);
        let mut events = runtime.subscribe();
        let root =
            std::env::temp_dir().join(format!("agentmux-missing-promote-{}", ulid::Ulid::new()));
        let worktree = Worktree {
            id: WorktreeId::new(),
            project_id: ProjectId::new(),
            task_id: TaskId::new(),
            owner_agent_id: None,
            path: root.join("worktree"),
            branch_name: "agentmux/task-missing".to_string(),
            base_branch: "main".to_string(),
            status: WorktreeStatus::ReviewReady,
            created_at: DateTimeUtc::UNIX_EPOCH,
        };
        let worktree_id = worktree.id.clone();
        runtime
            .register_worktree_with_repo_root(worktree, root.join("repo"))
            .await;
        mark_arena_candidate(
            &runtime,
            worktree_id.clone(),
            Some("missing".to_string()),
            Some(TestRunStatus::Passed),
        )
        .await;

        let approval = runtime
            .request_worktree_adoption(worktree_id.clone())
            .await
            .expect("adoption approval is queued");
        runtime
            .approve_approval(&approval.id)
            .await
            .expect("approval decision is accepted");

        for _ in 0..20 {
            let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
                .await
                .expect("daemon emits promote failure")
                .expect("event is available");
            if event.kind == IpcEventKind::Error
                && event.payload["signal"] == "worktree_promote_failed"
            {
                assert_eq!(event.payload["worktree_id"], worktree_id.to_string());
                assert_eq!(
                    runtime.worktree_by_id(&worktree_id).await.unwrap().status,
                    WorktreeStatus::ReviewReady
                );
                return;
            }
        }
        panic!("promote failure event was not emitted");
    }

    #[tokio::test]
    async fn approving_worktree_adopt_promotes_via_merge() {
        let root =
            std::env::temp_dir().join(format!("agentmux-worktree-adopt-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&root).expect("temporary worktree root is created");
        test_git(&root, ["init", "-b", "main"]);
        std::fs::write(root.join("README.md"), "hello\n").unwrap();
        test_git(&root, ["add", "README.md"]);
        test_git(
            &root,
            [
                "-c",
                "user.name=Agentmux Test",
                "-c",
                "user.email=agentmux@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        test_git(&root, ["branch", "agentmux/task-impl"]);
        let runtime = DaemonRuntime::new(16);
        let worktree = Worktree {
            id: WorktreeId::new(),
            project_id: ProjectId::new(),
            task_id: TaskId::new(),
            owner_agent_id: None,
            path: root.clone(),
            branch_name: "agentmux/task-impl".to_string(),
            base_branch: "main".to_string(),
            status: WorktreeStatus::ReviewReady,
            created_at: DateTimeUtc::UNIX_EPOCH,
        };
        let worktree_id = worktree.id.clone();
        runtime
            .register_worktree_with_repo_root(worktree, root.clone())
            .await;
        mark_arena_candidate(
            &runtime,
            worktree_id.clone(),
            Some("README.md | 0".to_string()),
            Some(TestRunStatus::Passed),
        )
        .await;

        let approval = runtime
            .request_worktree_adoption(worktree_id.clone())
            .await
            .expect("adoption approval is queued");
        runtime
            .approve_approval(&approval.id)
            .await
            .expect("approval is accepted");

        wait_for_worktree_status(&runtime, &worktree_id, WorktreeStatus::Promoted).await;
        std::fs::remove_dir_all(root).expect("temporary worktree root is removed");
    }

    #[tokio::test]
    async fn arena_run_rejects_duplicate_provider_labels_before_side_effects() {
        let root = std::env::temp_dir().join(format!(
            "agentmux-arena-duplicate-provider-{}",
            ulid::Ulid::new()
        ));
        std::fs::create_dir_all(&root).expect("temporary repo root is created");
        test_git(&root, ["init", "-b", "main"]);
        std::fs::write(root.join("README.md"), "hello\n").unwrap();
        test_git(&root, ["add", "README.md"]);
        test_git(
            &root,
            [
                "-c",
                "user.name=Agentmux Test",
                "-c",
                "user.email=agentmux@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        let runtime = DaemonRuntime::new(16);

        let error = runtime
            .run_task_with_arena(
                "compare duplicate providers".to_string(),
                vec!["claude".to_string(), "claude".to_string()],
                root.clone(),
                "main".to_string(),
            )
            .await
            .expect_err("duplicate providers are rejected");

        assert!(error.to_string().contains("duplicated"));
        assert!(runtime.list_worktrees().await.is_empty());
        assert_eq!(runtime.status_payload().await["agent_count"], 0);
        assert_eq!(
            git_stdout(&root, ["worktree", "list", "--porcelain"])
                .matches("worktree ")
                .count(),
            1
        );

        std::fs::remove_dir_all(root).expect("temporary repo root is removed");
    }

    #[tokio::test]
    async fn ipc_worktree_commands_reject_invalid_worktree_id() {
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
                "req_worktree_test",
                IpcCommand::WorktreeTest,
                json!({ "worktree_id": "not-a-worktree-id" }),
            ))
            .await
            .unwrap();

        let response: DaemonResponse = reader.read().await.unwrap().unwrap();
        assert!(!response.ok);
        assert_eq!(response.error.unwrap().code, "INVALID_WORKTREE_ID");

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
    async fn send_input_script_appends_audit_events_to_event_log() {
        let root = std::env::temp_dir().join(format!("agentmux-daemon-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&root).expect("temporary root is created");
        let event_log_path = root.join(".agentmux").join("events.jsonl");
        let output_path = root.join("input.txt");
        let runtime = DaemonRuntime::new(16).with_event_log(EventLog::new(&event_log_path));
        let agent = runtime
            .spawn_agent(
                "audit-shell".to_string(),
                PtySpawnSpec {
                    command: "/bin/sh".to_string(),
                    args: vec!["-c".to_string(), format!("cat > {}", output_path.display())],
                    cwd: root.clone(),
                    env: BTreeMap::from([("TERM".to_string(), "xterm-256color".to_string())]),
                    size: Default::default(),
                },
            )
            .await
            .expect("agent is spawned");
        let script = InputScript {
            id: InputScriptId::new(),
            target_agent_id: agent.id.clone(),
            reason: "unit test audit".to_string(),
            preconditions: vec![InputPrecondition::InputLockAvailable],
            actions: vec![InputAction::TypeText("audit works\n".to_string())],
            safety: InputSafety::Safe,
            created_at: DateTimeUtc::UNIX_EPOCH,
        };

        runtime
            .send_input_script(&script)
            .await
            .expect("input script is sent");
        std::thread::sleep(Duration::from_millis(50));
        terminate_agent_process(&runtime, &agent.id.to_string()).await;

        let output = std::fs::read_to_string(&output_path).expect("input reached PTY process");
        assert!(output.contains("audit works"), "output was {output:?}");

        let content = std::fs::read_to_string(&event_log_path).expect("event log is written");
        let events: Vec<serde_json::Value> = content
            .lines()
            .map(|line| serde_json::from_str(line).expect("valid JSONL event"))
            .collect();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["type"], "input_script.created");
        assert_eq!(events[1]["type"], "input_script.injected");
        for event in &events {
            assert!(event["id"].as_str().unwrap().starts_with("evt_"));
            assert_eq!(event["agent_id"], agent.id.to_string());
            assert_eq!(event["payload"]["input_script_id"], script.id.to_string());
            assert_eq!(event["payload"]["reason"], "unit test audit");
            assert_eq!(event["payload"]["action_count"], 1);
            assert_eq!(event["payload"]["target_agent_id"], agent.id.to_string());
            assert_eq!(event["payload"]["actions"][0]["type_text"], "audit works\n");
        }

        std::fs::remove_dir_all(root).expect("temporary daemon directory is removed");
    }

    #[tokio::test]
    async fn finish_shutdown_removes_socket_and_flushes_state_event() {
        let root = std::env::temp_dir().join(format!("agentmux-daemon-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&root).expect("temporary root is created");
        let socket_path = root.join("agentmux.sock");
        std::fs::write(&socket_path, b"stale socket marker").expect("socket marker is written");
        let event_log_path = root.join(".agentmux").join("events.jsonl");
        let runtime = DaemonRuntime::new(16).with_event_log(EventLog::new(&event_log_path));
        let agent = runtime.register_agent("planner".to_string()).await;
        let mut events = runtime.subscribe();

        finish_shutdown(&runtime, &socket_path).await.unwrap();

        assert!(!socket_path.exists(), "daemon socket should be removed");
        let event = events.recv().await.expect("shutdown event is published");
        assert_eq!(event.kind, IpcEventKind::DaemonStopped);
        let content = std::fs::read_to_string(&event_log_path).expect("event log is written");
        let events: Vec<serde_json::Value> = content
            .lines()
            .map(|line| serde_json::from_str(line).expect("valid JSONL event"))
            .collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "daemon.stopped");
        assert_eq!(events[0]["payload"]["state"]["agent_count"], 1);
        assert_eq!(
            events[0]["payload"]["state"]["agents"][0]["id"],
            agent.id.to_string()
        );

        std::fs::remove_dir_all(root).expect("temporary daemon directory is removed");
    }

    #[tokio::test]
    async fn runtime_recovers_agent_metadata_from_latest_shutdown_event() {
        let root = std::env::temp_dir().join(format!("agentmux-daemon-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&root).expect("temporary root is created");
        let socket_path = root.join("agentmux.sock");
        let event_log_path = root.join(".agentmux").join("events.jsonl");
        let first_runtime = DaemonRuntime::new(16).with_event_log(EventLog::new(&event_log_path));
        let planner = first_runtime.register_agent("planner".to_string()).await;
        let implementer = first_runtime
            .register_agent("implementer".to_string())
            .await;

        finish_shutdown(&first_runtime, &socket_path).await.unwrap();

        let recovered_runtime =
            DaemonRuntime::new(16).with_event_log(EventLog::new(&event_log_path));
        let recovered_count = recovered_runtime
            .recover_state_from_event_log()
            .await
            .expect("state is recovered");
        let status = recovered_runtime.status_payload().await;

        assert_eq!(recovered_count, 2);
        assert_eq!(status["agent_count"], 2);
        let agents = status["agents"].as_array().expect("agents are listed");
        let planner_id = planner.id.to_string();
        let implementer_id = implementer.id.to_string();
        let recovered_ids = agents
            .iter()
            .map(|agent| agent["id"].as_str().unwrap())
            .collect::<BTreeSet<_>>();
        assert!(recovered_ids.contains(planner_id.as_str()));
        assert!(recovered_ids.contains(implementer_id.as_str()));
        for agent in agents {
            assert_eq!(agent["has_process"], false);
            assert_eq!(agent["process_id"], serde_json::Value::Null);
            assert!(agent["attached_clients"].as_array().unwrap().is_empty());
        }

        std::fs::remove_dir_all(root).expect("temporary daemon directory is removed");
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

    async fn read_response_and_event<R>(
        reader: &mut JsonlReader<R>,
    ) -> (DaemonResponse, DaemonEvent)
    where
        R: tokio::io::AsyncBufRead + Unpin,
    {
        let mut response = None;
        let mut event = None;
        for _ in 0..2 {
            let frame: serde_json::Value = reader.read().await.unwrap().unwrap();
            if frame.get("ok").is_some() {
                response = Some(serde_json::from_value(frame).unwrap());
            } else {
                event = Some(serde_json::from_value(frame).unwrap());
            }
        }
        (response.unwrap(), event.unwrap())
    }

    async fn read_response<R>(reader: &mut JsonlReader<R>) -> DaemonResponse
    where
        R: tokio::io::AsyncBufRead + Unpin,
    {
        let frame: serde_json::Value = tokio::time::timeout(Duration::from_secs(2), reader.read())
            .await
            .expect("response frame is not timed out")
            .expect("response frame is readable")
            .expect("response frame exists");
        assert!(
            frame.get("ok").is_some(),
            "expected response frame, got {frame:?}"
        );
        serde_json::from_value(frame).expect("response frame is valid")
    }

    fn test_git<const N: usize>(repo: &Path, args: [&str; N]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git command failed: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_stdout<const N: usize>(repo: &Path, args: [&str; N]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git command failed: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    async fn mark_arena_candidate(
        runtime: &DaemonRuntime,
        worktree_id: WorktreeId,
        diff_stat: Option<String>,
        test_status: Option<TestRunStatus>,
    ) {
        let mut state = runtime.state.write().await;
        state.arena_candidates.insert(
            worktree_id.clone(),
            ArenaCandidate {
                worktree_id,
                agent_id: AgentSessionId::new(),
                provider: "test".to_string(),
                diff_stat,
                test_status,
            },
        );
    }

    async fn wait_for_worktree_status(
        runtime: &DaemonRuntime,
        worktree_id: &WorktreeId,
        expected: WorktreeStatus,
    ) {
        for _ in 0..20 {
            let worktree = runtime.worktree_by_id(worktree_id).await.unwrap();
            if worktree.status == expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let worktree = runtime.worktree_by_id(worktree_id).await.unwrap();
        assert_eq!(worktree.status, expected);
    }

    async fn assert_no_frame<R>(reader: &mut JsonlReader<R>)
    where
        R: tokio::io::AsyncBufRead + Unpin,
    {
        let frame = tokio::time::timeout(
            Duration::from_millis(50),
            reader.read::<serde_json::Value>(),
        )
        .await;
        assert!(frame.is_err(), "unexpected daemon frame: {frame:?}");
    }

    fn pty_capture_script() -> &'static str {
        r#"my $out = shift; open my $fh, ">", $out or die $!; select((select($fh), $| = 1)[0]); while (defined(my $line = <STDIN>)) { print {$fh} $line; last if $line =~ /AGENTMUX_RESULT JSON/; }"#
    }

    async fn wait_for_file_contains(path: &Path, needle: &str) -> Option<String> {
        for _ in 0..100 {
            if let Ok(output) = std::fs::read_to_string(path)
                && output.contains(needle)
            {
                return Some(output);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        None
    }

    async fn terminate_agent_process(runtime: &DaemonRuntime, agent_id: &str) {
        let agent_id = parse_agent_session_id(agent_id).unwrap();
        let state = runtime.state.read().await;
        let live_agent = state.agents.get(&agent_id).unwrap();
        if let Some(pty) = &live_agent.pty {
            let mut pty = pty.lock().unwrap();
            let _ = pty.terminate();
            // Bounded reap (<=2s): never block the current-thread test runtime
            // forever if the child does not exit promptly (e.g. a shell that
            // keeps the PTY open via a child process). The assertions that
            // matter run before termination, so the exit status is irrelevant.
            for _ in 0..200 {
                match pty.try_wait() {
                    Ok(Some(_)) | Err(_) => break,
                    Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        }
    }
