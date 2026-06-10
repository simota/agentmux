use super::*;

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
    assert!(
        contents
            .contains("Do not ask the user for confirmation before sending normal message replies")
    );
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

    let invalid = DEFAULT_PROJECT_CONFIG.replace("prefix_key = \"Ctrl-g\"", "prefix_key = \"F12\"");
    let error = AgentmuxConfig::parse_str(&invalid).unwrap_err();
    assert!(error.to_string().contains("tui.prefix_key"));
}

#[test]
fn command_lookup_searches_path_entries() {
    let root = std::env::temp_dir().join(format!("agentmux-cli-path-test-{}", std::process::id()));
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

fn keys(spec: &str) -> Vec<KeyAction> {
    parse_key_spec(spec).expect("expected a valid key spec")
}

fn keys_err(spec: &str) -> AgentmuxError {
    parse_key_spec(spec).expect_err("expected a key spec error")
}

#[test]
fn parse_key_spec_maps_modifier_steps() {
    assert_eq!(keys("C-c"), vec![KeyAction::PressCtrl('c')]);
    assert_eq!(keys("ctrl:c"), vec![KeyAction::PressCtrl('c')]);
    assert_eq!(keys("M-x"), vec![KeyAction::PressAlt('x')]);
    assert_eq!(keys("alt:x"), vec![KeyAction::PressAlt('x')]);
}

#[test]
fn parse_key_spec_maps_named_keys() {
    assert_eq!(keys("enter"), vec![KeyAction::PressEnter]);
    assert_eq!(keys("esc"), vec![KeyAction::PressEsc]);
    assert_eq!(keys("tab"), vec![KeyAction::PressTab]);
    assert_eq!(keys("bs"), vec![KeyAction::PressBackspace]);
}

#[test]
fn parse_key_spec_maps_text_and_raw() {
    assert_eq!(keys("text:hi"), vec![KeyAction::TypeText("hi".to_string())]);
    assert_eq!(
        keys("raw:1b5b41"),
        vec![KeyAction::SendRaw(vec![0x1b, 0x5b, 0x41])]
    );
}

#[test]
fn parse_key_spec_maps_arrow_and_navigation_keys() {
    assert_eq!(keys("up"), vec![KeyAction::SendRaw(b"\x1b[A".to_vec())]);
    assert_eq!(keys("down"), vec![KeyAction::SendRaw(b"\x1b[B".to_vec())]);
    assert_eq!(keys("right"), vec![KeyAction::SendRaw(b"\x1b[C".to_vec())]);
    assert_eq!(keys("left"), vec![KeyAction::SendRaw(b"\x1b[D".to_vec())]);
    assert_eq!(keys("home"), vec![KeyAction::SendRaw(b"\x1b[H".to_vec())]);
    assert_eq!(keys("end"), vec![KeyAction::SendRaw(b"\x1b[F".to_vec())]);
}

#[test]
fn parse_key_spec_parses_multiple_pipe_separated_steps() {
    assert_eq!(
        keys("C-c|enter"),
        vec![KeyAction::PressCtrl('c'), KeyAction::PressEnter]
    );
    assert_eq!(
        keys("text:hi | tab | up"),
        vec![
            KeyAction::TypeText("hi".to_string()),
            KeyAction::PressTab,
            KeyAction::SendRaw(b"\x1b[A".to_vec()),
        ]
    );
}

#[test]
fn parse_key_spec_rejects_malformed_input() {
    // Odd-length hex.
    assert!(matches!(keys_err("raw:1b5"), AgentmuxError::UserError(_)));
    // Non-hex characters.
    assert!(matches!(keys_err("raw:zz"), AgentmuxError::UserError(_)));
    // Non-ASCII hex input (even byte length but multi-byte chars) must not panic.
    assert!(matches!(keys_err("raw:aあ"), AgentmuxError::UserError(_)));
    assert!(matches!(keys_err("raw:ＡＢ"), AgentmuxError::UserError(_)));
    // Unknown step.
    assert!(matches!(keys_err("bogus"), AgentmuxError::UserError(_)));
    // Empty spec.
    assert!(matches!(keys_err(""), AgentmuxError::UserError(_)));
    assert!(matches!(keys_err("   "), AgentmuxError::UserError(_)));
    // Empty step between separators.
    assert!(matches!(
        keys_err("enter||esc"),
        AgentmuxError::UserError(_)
    ));
    // Multi-char modifier.
    assert!(matches!(keys_err("C-ab"), AgentmuxError::UserError(_)));
}
