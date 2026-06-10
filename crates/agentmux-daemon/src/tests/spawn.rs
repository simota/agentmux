use super::*;

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
    assert_eq!(
        chunks[1], b"\r",
        "chunk 1 must be exactly the carriage-return enter byte"
    );
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
    let spec = pty_spawn_spec_from_payload(&json!({ "provider": "mystery" })).expect("spec builds");
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
async fn stop_agent_deregisters_session_from_message_bus() {
    // #8: stopping an agent must drop it from the message bus too, otherwise
    // `resolve_target` keeps routing to the dead session and its inbox leaks.
    let runtime = DaemonRuntime::new(8);
    let agent = runtime
        .register_agent_with_role("retiring-tester".to_string(), AgentRole::Tester)
        .await;

    // While live, both an explicit-agent target and the role target resolve
    // to the session.
    {
        let state = runtime.state.read().await;
        assert_eq!(
            state
                .messages
                .resolve_target(&MessageTarget::Agent(agent.id.clone()))
                .unwrap(),
            vec![agent.id.clone()]
        );
        assert_eq!(
            state
                .messages
                .resolve_target(&MessageTarget::Role(AgentRole::Tester))
                .unwrap(),
            vec![agent.id.clone()]
        );
        // The inbox exists.
        assert!(state.messages.inbox(&agent.id).is_ok());
    }

    runtime.stop_agent(&agent.id).await.expect("agent stops");

    // After stop: the role target resolves to nobody, the explicit-agent
    // target errors as unknown, and the inbox is gone.
    let state = runtime.state.read().await;
    assert!(
        state
            .messages
            .resolve_target(&MessageTarget::Role(AgentRole::Tester))
            .is_err(),
        "role target no longer resolves to the stopped session"
    );
    assert!(
        state
            .messages
            .resolve_target(&MessageTarget::Agent(agent.id.clone()))
            .is_err(),
        "explicit-agent target no longer resolves to the stopped session"
    );
    assert!(
        state.messages.inbox(&agent.id).is_err(),
        "stopped session's inbox is dropped"
    );
}

#[tokio::test]
async fn register_agent_without_explicit_role_starts_at_default_role() {
    // Sessions no longer infer a role from their name; an operator assigns a
    // meaningful role at runtime via `agent.set_role`.
    let runtime = DaemonRuntime::new(8);
    let tester = runtime.register_agent("tester".to_string()).await;
    let planner = runtime.register_agent("planner".to_string()).await;

    let status = runtime.status_payload().await;
    let roles: BTreeSet<&str> = status["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|agent| agent["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, BTreeSet::from(["default"]));
    assert_eq!(tester.role, AgentRole::Custom("default".to_string()));
    assert_eq!(planner.role, AgentRole::Custom("default".to_string()));
}

#[tokio::test]
async fn set_agent_role_updates_metadata_bus_routing_and_publishes_event() {
    let runtime = DaemonRuntime::new(8);
    let agent = runtime.register_agent("agy-worker".to_string()).await;
    let mut events = runtime.subscribe();

    // A known label maps to its enum variant and reroutes the role target.
    let updated = runtime
        .set_agent_role(&agent.id, AgentRole::Reviewer)
        .await
        .expect("role is updated");
    assert_eq!(updated.role, AgentRole::Reviewer);

    let event = events.recv().await.expect("role change event is published");
    assert_eq!(event.kind, IpcEventKind::AgentRoleChanged);
    assert_eq!(event.payload["agent_id"], agent.id.to_string());
    assert_eq!(event.payload["role"], "reviewer");

    {
        let state = runtime.state.read().await;
        assert_eq!(
            state
                .messages
                .resolve_target(&MessageTarget::Role(AgentRole::Reviewer))
                .unwrap(),
            vec![agent.id.clone()]
        );
        assert!(
            state
                .messages
                .resolve_target(&MessageTarget::Role(AgentRole::Custom(
                    "default".to_string()
                )))
                .is_err(),
            "the previous default role no longer resolves to the session"
        );
        assert_eq!(state.agents[&agent.id].metadata.role, AgentRole::Reviewer);
    }
}

#[tokio::test]
async fn set_agent_role_accepts_custom_label_and_rejects_unknown_agent() {
    let runtime = DaemonRuntime::new(8);
    let agent = runtime.register_agent("agy-worker".to_string()).await;

    // Any non-standard label becomes a custom role verbatim.
    let updated = runtime
        .set_agent_role(&agent.id, AgentRole::Custom("qa-lead".to_string()))
        .await
        .expect("custom role is accepted");
    assert_eq!(updated.role, AgentRole::Custom("qa-lead".to_string()));

    // An unknown agent id is a user error.
    let missing = AgentSessionId::new();
    assert!(
        runtime
            .set_agent_role(&missing, AgentRole::Reviewer)
            .await
            .is_err()
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
        persisted_messages
            .iter()
            .all(|message| message.task_id.as_ref().map(ToString::to_string)
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
async fn for_project_without_agentmux_dir_uses_plain_runtime() {
    let root = std::env::temp_dir().join(format!("agentmux-noinit-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&root).expect("temporary root is created");

    let runtime = DaemonRuntime::for_project(16, &root)
        .await
        .expect("plain runtime builds for an uninitialized project");

    assert!(
        runtime.event_log.is_none(),
        "no event log before project init"
    );
    let state = runtime.state.read().await;
    assert_eq!(
        state.policy.automation_level,
        AutomationLevel::AutoPrompt,
        "spec-default policy is kept"
    );

    drop(state);
    std::fs::remove_dir_all(root).expect("temporary root is removed");
}

/// Regression for the unwired production main: with an initialized project,
/// the runtime must wire the audit event log, the configured policy engine,
/// and the `[automation]`/`[context]` injection timing — none of which were
/// connected by `DaemonRuntime::new(1024)` alone.
#[tokio::test]
async fn for_project_wires_event_log_policy_and_injection_config() {
    let root = std::env::temp_dir().join(format!("agentmux-init-{}", ulid::Ulid::new()));
    let agentmux_dir = root.join(".agentmux");
    std::fs::create_dir_all(&agentmux_dir).expect("temporary .agentmux dir is created");
    let example_config = include_str!("../../../../docs/config/agentmux.config.example.toml");
    std::fs::write(agentmux_dir.join("config.toml"), example_config)
        .expect("project config is written");

    let runtime = DaemonRuntime::for_project(16, &root)
        .await
        .expect("configured runtime builds");

    assert!(
        runtime.event_log.is_some(),
        "audit event log is wired to .agentmux/events.jsonl"
    );
    assert_eq!(
        runtime.event_log.as_ref().unwrap().path(),
        agentmux_dir.join("events.jsonl")
    );

    let config = AgentmuxConfig::parse_str(example_config).expect("example config parses");
    let state = runtime.state.read().await;
    assert_eq!(
        state.injection_timing,
        InjectionTiming::from_config(&config.automation, &config.context),
        "injection timing (incl. human_input_quiet_ms) comes from config"
    );
    assert_eq!(
        state.policy.policy.allow_network,
        PolicyDecision::Deny,
        "policy engine reflects the configured [policy] section"
    );
    assert_eq!(
        state.policy.policy.allow_git_push,
        PolicyDecision::Deny,
        "git push stays deny per spec defaults in the example config"
    );

    drop(state);
    std::fs::remove_dir_all(root).expect("temporary root is removed");
}

#[tokio::test]
async fn for_project_rejects_invalid_config_instead_of_silently_falling_back() {
    let root = std::env::temp_dir().join(format!("agentmux-badcfg-{}", ulid::Ulid::new()));
    let agentmux_dir = root.join(".agentmux");
    std::fs::create_dir_all(&agentmux_dir).expect("temporary .agentmux dir is created");
    std::fs::write(agentmux_dir.join("config.toml"), "not a valid config")
        .expect("broken config is written");

    let result = DaemonRuntime::for_project(16, &root).await;
    let Err(error) = result else {
        panic!("invalid config must abort startup");
    };
    assert!(
        error.to_string().contains("config"),
        "error points at the config, got: {error}"
    );

    std::fs::remove_dir_all(root).expect("temporary root is removed");
}

/// Poll `ps` until the process is fully reaped (a zombie still has a `ps`
/// entry, so this proves the child was actually `wait()`ed, not just killed).
fn assert_process_reaped(pid: u32) {
    for _ in 0..200 {
        let alive = Command::new("ps")
            .arg("-p")
            .arg(pid.to_string())
            .output()
            .expect("ps runs")
            .status
            .success();
        if !alive {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("process {pid} still exists (zombie or live) after the reap window");
}

/// Regression for the zombie/ghost-session gap: when an agent process exits on
/// its own (PTY EOF), the daemon must reap the child, mark the session
/// `Exited`, emit `agent.exited`, and drop it from message-bus routing so it
/// stops being an injection target.
#[tokio::test]
async fn agent_eof_reaps_child_marks_exited_and_stops_routing() {
    let root = std::env::temp_dir().join(format!("agentmux-eof-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&root).expect("temporary root is created");
    let runtime = DaemonRuntime::new(16);
    let mut events = runtime.subscribe();

    let agent = runtime
        .spawn_agent_with_role(
            "short-lived".to_string(),
            AgentRole::Tester,
            PtySpawnSpec {
                command: "/bin/sh".to_string(),
                args: vec!["-c".to_string(), "exit 0".to_string()],
                cwd: root.clone(),
                env: BTreeMap::from([("TERM".to_string(), "xterm-256color".to_string())]),
                size: Default::default(),
            },
        )
        .await
        .expect("agent is spawned");
    let pid = agent.process_id.expect("spawned agent has a pid");

    // The process exits immediately; the forwarder must observe EOF and emit
    // agent.exited with the eof reason.
    let exited = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = events.recv().await.expect("event stream stays open");
            if event.kind == IpcEventKind::AgentExited {
                break event;
            }
        }
    })
    .await
    .expect("agent.exited is emitted after PTY EOF");
    assert_eq!(exited.payload["agent_id"], agent.id.to_string());
    assert_eq!(exited.payload["reason"], "eof");

    // The child is fully reaped — no zombie left on the daemon process.
    assert_process_reaped(pid);

    {
        let state = runtime.state.read().await;
        let live = state.agents.get(&agent.id).expect("session metadata stays");
        assert_eq!(
            live.metadata.status,
            Some(AgentStatus::Exited),
            "session is marked Exited"
        );
        assert!(live.pty.is_none(), "PTY handle is released");
        assert!(
            state
                .messages
                .resolve_target(&MessageTarget::Agent(agent.id.clone()))
                .is_err(),
            "exited session is no longer an injection target"
        );
    }
    let status = runtime.status_payload().await;
    assert_eq!(status["agents"][0]["status"], "exited");
    assert_eq!(status["agents"][0]["input_ready"], false);

    std::fs::remove_dir_all(root).expect("temporary root is removed");
}

/// Regression for the stop ordering gap: `agent.stop` must confirm the child
/// is terminated and reaped *before* dropping the session from state, so a
/// failed terminate can never orphan a live process with no handle left.
#[tokio::test]
async fn stop_agent_reaps_child_before_removing_session() {
    let root = std::env::temp_dir().join(format!("agentmux-stop-reap-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&root).expect("temporary root is created");
    let runtime = DaemonRuntime::new(16);
    let agent = runtime
        .spawn_agent(
            "long-runner".to_string(),
            PtySpawnSpec {
                command: "/bin/sleep".to_string(),
                args: vec!["30".to_string()],
                cwd: root.clone(),
                env: BTreeMap::from([("TERM".to_string(), "xterm-256color".to_string())]),
                size: Default::default(),
            },
        )
        .await
        .expect("agent is spawned");
    let pid = agent.process_id.expect("spawned agent has a pid");

    let stopped = tokio::time::timeout(Duration::from_secs(5), runtime.stop_agent(&agent.id))
        .await
        .expect("stop does not hang")
        .expect("stop succeeds");
    assert_eq!(stopped.id, agent.id);

    // By the time stop_agent returns, the child has been killed AND reaped.
    assert_process_reaped(pid);
    let state = runtime.state.read().await;
    assert!(
        !state.agents.contains_key(&agent.id),
        "session is removed only after the child is reaped"
    );

    drop(state);
    std::fs::remove_dir_all(root).expect("temporary root is removed");
}

#[tokio::test]
async fn serve_until_shutdown_clean_lifecycle_starts_and_stops() {
    // Short directory name: a Unix socket path must fit in sun_path (~104
    // bytes on macOS), and temp_dir is already long there.
    let root = std::env::temp_dir().join(format!("amx-sc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("temporary root is created");
    let socket_path = root.join("agentmux.sock");
    let event_log_path = root.join(".agentmux").join("events.jsonl");
    let runtime = DaemonRuntime::new(16).with_event_log(EventLog::new(&event_log_path));
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let serve_task = tokio::spawn(serve_until_shutdown(
        DaemonConfig::new(&socket_path),
        runtime,
        async move {
            let _ = shutdown_rx.await;
        },
    ));
    for _ in 0..100 {
        if socket_path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(socket_path.exists(), "daemon socket is bound");
    shutdown_tx.send(()).expect("shutdown is signalled");

    tokio::time::timeout(Duration::from_secs(2), serve_task)
        .await
        .expect("serve loop stops on shutdown signal")
        .expect("serve task does not panic")
        .expect("serve loop exits cleanly");
    assert!(!socket_path.exists(), "socket is removed on shutdown");

    let content = std::fs::read_to_string(&event_log_path).expect("event log is written");
    let kinds: Vec<String> = content
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(kinds, vec!["daemon.started", "daemon.stopped"]);

    std::fs::remove_dir_all(root).expect("temporary daemon directory is removed");
}

/// Regression: when the serve loop exits with an error (here: a corrupt
/// `daemon.stopped` recovery entry), the shutdown tail must still run so the
/// socket file is removed and a fresh `daemon.stopped` event is recorded —
/// otherwise the next start hits a stale socket and recovery loses state.
#[tokio::test]
async fn serve_until_shutdown_error_path_still_runs_shutdown_tail() {
    // Short directory name: see serve_until_shutdown_clean_lifecycle_starts_and_stops.
    let root = std::env::temp_dir().join(format!("amx-se-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("temporary root is created");
    let socket_path = root.join("agentmux.sock");
    let event_log_path = root.join(".agentmux").join("events.jsonl");
    let event_log = EventLog::new(&event_log_path);
    // A daemon.stopped entry without payload.state.agents makes
    // recover_state_from_event_log fail inside the serve loop.
    event_log
        .append(
            &EventLogEntry::new("daemon.stopped", DateTimeUtc::now_utc(), json!({}))
                .expect("entry builds"),
        )
        .expect("corrupt recovery entry is written");
    let runtime = DaemonRuntime::new(16).with_event_log(event_log);

    let result = tokio::time::timeout(
        Duration::from_secs(2),
        serve_until_shutdown(
            DaemonConfig::new(&socket_path),
            runtime,
            std::future::pending(),
        ),
    )
    .await
    .expect("failed serve loop returns instead of hanging");

    assert!(result.is_err(), "recovery failure surfaces as an error");
    assert!(
        !socket_path.exists(),
        "socket is removed even on the error path"
    );
    let content = std::fs::read_to_string(&event_log_path).expect("event log is written");
    let last = content.lines().last().expect("log has entries");
    let entry: serde_json::Value = serde_json::from_str(last).expect("valid JSONL event");
    assert_eq!(
        entry["type"], "daemon.stopped",
        "a daemon.stopped event is recorded even on the error path"
    );

    std::fs::remove_dir_all(root).expect("temporary daemon directory is removed");
}

#[test]
fn accept_error_backoff_doubles_and_caps() {
    let mut backoff = ACCEPT_ERROR_BACKOFF_MIN;
    assert_eq!(backoff, Duration::from_millis(100));
    backoff = next_accept_backoff(backoff);
    assert_eq!(backoff, Duration::from_millis(200));
    for _ in 0..10 {
        backoff = next_accept_backoff(backoff);
    }
    assert_eq!(
        backoff, ACCEPT_ERROR_BACKOFF_MAX,
        "backoff is capped so retries keep happening at a bounded rate"
    );
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

    let recovered_runtime = DaemonRuntime::new(16).with_event_log(EventLog::new(&event_log_path));
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
