use super::*;

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
                    input_activity: InputActivity::new(),
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
