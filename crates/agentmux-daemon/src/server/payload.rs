use crate::*;

pub(crate) fn task_run_payload(
    payload: &serde_json::Value,
) -> Result<(String, String, PathBuf, String)> {
    let body = required_string(payload, "body", "task.run")?.to_string();
    let team = payload
        .get("team")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("claude-codex")
        .to_string();
    let project_path = payload
        .get("project_path")
        .and_then(|value| value.as_str())
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir().map_err(|error| {
            AgentmuxError::Internal(format!("failed to resolve current directory: {error}"))
        })?);
    let runner = payload
        .get("runner")
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
        .or_else(|| std::env::var("AGENTMUX_TASK_RUNNER").ok())
        .unwrap_or_else(|| "shell-stub".to_string());
    Ok((body, team, project_path, runner))
}

pub(crate) fn arena_providers_payload(payload: &serde_json::Value) -> Result<Vec<String>> {
    let providers =
        if let Some(values) = payload.get("providers").and_then(|value| value.as_array()) {
            values
                .iter()
                .map(|value| {
                    value.as_str().map(str::to_string).ok_or_else(|| {
                        AgentmuxError::UserError("providers must be strings".to_string())
                    })
                })
                .collect::<Result<Vec<_>>>()?
        } else if let Some(value) = payload.get("arena").and_then(|value| value.as_str()) {
            value.split(',').map(str::to_string).collect()
        } else {
            Vec::new()
        };
    let providers = providers
        .into_iter()
        .map(|provider| provider.trim().to_string())
        .filter(|provider| !provider.is_empty())
        .collect::<Vec<_>>();
    if providers.is_empty() {
        return Err(AgentmuxError::UserError(
            "arena task.run requires providers or arena".to_string(),
        ));
    }
    Ok(providers)
}

pub(crate) fn protocol_error(compatibility: ProtocolCompatibility) -> Option<ErrorBody> {
    match compatibility {
        ProtocolCompatibility::Compatible => None,
        ProtocolCompatibility::VersionMismatch { expected, actual } => Some(ErrorBody::new(
            "PROTOCOL_VERSION_MISMATCH",
            format!("expected protocol {expected}, got {actual}"),
        )),
        ProtocolCompatibility::InvalidHandshake => {
            Some(ErrorBody::new("INVALID_HANDSHAKE", "expected hello frame"))
        }
    }
}

pub(crate) fn parse_agent_session_id(value: &str) -> Option<AgentSessionId> {
    value.parse::<AgentSessionId>().ok()
}

pub(crate) fn agent_id_payload(
    payload: &serde_json::Value,
    command: &str,
) -> Result<AgentSessionId> {
    required_string(payload, "agent_id", command)?
        .parse::<AgentSessionId>()
        .map_err(|error| AgentmuxError::UserError(format!("invalid agent_id: {error}")))
}

pub(crate) fn terminal_size_payload(
    payload: &serde_json::Value,
    command: &str,
) -> Result<agentmux_pty::TerminalSize> {
    let rows = required_u16(payload, "rows", command)?;
    let cols = required_u16(payload, "cols", command)?;
    if rows == 0 || cols == 0 {
        return Err(AgentmuxError::UserError(format!(
            "{command} requires non-zero rows and cols, got {rows}x{cols}"
        )));
    }
    Ok(agentmux_pty::TerminalSize { rows, cols })
}

pub(crate) fn parse_message_id(value: &str) -> Option<MessageId> {
    value.parse::<MessageId>().ok()
}

pub(crate) fn message_create_payload(payload: &serde_json::Value) -> Result<NewAgentMessage> {
    let to = payload
        .get("to")
        .and_then(|value| value.as_str())
        .ok_or_else(|| AgentmuxError::UserError("message.create requires to".to_string()))
        .and_then(parse_message_target)?;
    let body = payload
        .get("body")
        .and_then(|value| value.as_str())
        .ok_or_else(|| AgentmuxError::UserError("message.create requires body".to_string()))?
        .to_string();
    let kind = payload
        .get("kind")
        .and_then(|value| value.as_str())
        .map(parse_message_kind)
        .transpose()?
        .unwrap_or(MessageKind::Handoff);
    let priority = payload
        .get("priority")
        .and_then(|value| value.as_str())
        .map(parse_priority)
        .transpose()?
        .unwrap_or(Priority::Normal);
    let delivery_mode = payload
        .get("delivery_mode")
        .and_then(|value| value.as_str())
        .map(parse_delivery_mode)
        .transpose()?
        .unwrap_or(DeliveryMode::InjectWhenIdle);

    // Attribute the message to the sending agent session when the client passes
    // a valid `from_agent_id` (sourced from the `AGENTMUX_AGENT_ID` env inside a
    // live session). Fall back to an anonymous User source otherwise; a
    // malformed id is treated as absent rather than an error.
    let from = payload
        .get("from_agent_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| raw.trim().parse::<AgentSessionId>().ok())
        .map(MessageSource::Agent)
        .unwrap_or_else(|| MessageSource::User(ClientId::new()));

    let thread_id = payload
        .get("thread_id")
        .and_then(|value| value.as_str())
        .map(|raw| {
            raw.trim()
                .parse::<ThreadId>()
                .map_err(|error| AgentmuxError::UserError(format!("invalid thread_id: {error}")))
        })
        .transpose()?;

    Ok(NewAgentMessage {
        task_id: None,
        thread_id,
        from,
        to,
        kind,
        priority,
        body,
        context_refs: Vec::new(),
        artifact_refs: Vec::new(),
        delivery_mode,
        requires_response: false,
    })
}

pub(crate) fn parse_message_target(raw: &str) -> Result<MessageTarget> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(AgentmuxError::UserError(
            "message target must not be empty".to_string(),
        ));
    }
    if raw == "broadcast" {
        return Ok(MessageTarget::Broadcast);
    }
    if let Some(role) = raw.strip_prefix("role:") {
        return Ok(MessageTarget::Role(parse_agent_role(role)?));
    }
    if let Some(team) = raw.strip_prefix("team:") {
        let team = team.trim();
        if team.is_empty() {
            return Err(AgentmuxError::UserError(
                "team message target must not be empty".to_string(),
            ));
        }
        return Ok(MessageTarget::Team(team.to_string()));
    }
    if let Some(thread) = raw.strip_prefix("thread:") {
        let thread_id = thread.trim().parse::<ThreadId>().map_err(|error| {
            AgentmuxError::UserError(format!("invalid thread message target: {error}"))
        })?;
        return Ok(MessageTarget::Thread(thread_id));
    }
    if raw.starts_with(ThreadId::prefix()) {
        let thread_id = raw.parse::<ThreadId>().map_err(|error| {
            AgentmuxError::UserError(format!("invalid thread message target: {error}"))
        })?;
        return Ok(MessageTarget::Thread(thread_id));
    }
    if let Some(agent) = raw.strip_prefix("agent:") {
        let agent = agent.trim();
        if agent.is_empty() {
            return Err(AgentmuxError::UserError(
                "agent message target must not be empty".to_string(),
            ));
        }
        if let Ok(agent_id) = agent.parse::<AgentSessionId>() {
            return Ok(MessageTarget::Agent(agent_id));
        }
        return Ok(MessageTarget::AgentName(agent.to_string()));
    }
    if let Ok(agent_id) = raw.parse::<AgentSessionId>() {
        return Ok(MessageTarget::Agent(agent_id));
    }
    Ok(MessageTarget::AgentName(raw.to_string()))
}

/// Parse a free-form role label into an `AgentRole`.
///
/// Known labels (matched case-insensitively, with `-`/space treated like `_`)
/// map to their enum variant — this is the inverse of `agent_role_label`. Any
/// other non-empty input becomes `AgentRole::Custom` holding the *trimmed raw*
/// string (not the normalized form) so that a custom role round-trips through
/// `agent_role_label` unchanged and `role:<label>` bus targets resolve to the
/// same session that was assigned the label. An empty label is a user error.
pub(crate) fn parse_agent_role(raw: &str) -> Result<AgentRole> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AgentmuxError::UserError(
            "agent role must not be empty".to_string(),
        ));
    }
    let normalized = trimmed.to_ascii_lowercase().replace(['-', ' '], "_");
    let role = match normalized.as_str() {
        "planner" => AgentRole::Planner,
        "implementer" | "impl" => AgentRole::Implementer,
        "reviewer" | "review" => AgentRole::Reviewer,
        "tester" | "qa" => AgentRole::Tester,
        "debugger" | "debug" => AgentRole::Debugger,
        "refactorer" | "refactor" => AgentRole::Refactorer,
        "security_reviewer" | "security" => AgentRole::SecurityReviewer,
        "docs_writer" | "docs" | "docswriter" => AgentRole::DocsWriter,
        "integrator" => AgentRole::Integrator,
        "context_manager" | "context" => AgentRole::ContextManager,
        _ => AgentRole::Custom(trimmed.to_string()),
    };
    Ok(role)
}

pub(crate) fn parse_message_kind(raw: &str) -> Result<MessageKind> {
    serde_json::from_value(json!(raw))
        .map_err(|error| AgentmuxError::UserError(format!("invalid message kind '{raw}': {error}")))
}

pub(crate) fn parse_priority(raw: &str) -> Result<Priority> {
    serde_json::from_value(json!(raw))
        .map_err(|error| AgentmuxError::UserError(format!("invalid priority '{raw}': {error}")))
}

pub(crate) fn parse_delivery_mode(raw: &str) -> Result<DeliveryMode> {
    serde_json::from_value(json!(raw)).map_err(|error| {
        AgentmuxError::UserError(format!("invalid delivery_mode '{raw}': {error}"))
    })
}

pub(crate) fn message_payload(message: &AgentMessage) -> serde_json::Value {
    json!({
        "message_id": message.id.to_string(),
        "task_id": message.task_id.as_ref().map(ToString::to_string),
        "thread_id": message.thread_id.as_ref().map(ToString::to_string),
        "from": message.from,
        "to": message.to,
        "kind": message.kind,
        "priority": message.priority,
        "body": message.body,
        "context_refs": message.context_refs.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "artifact_refs": message.artifact_refs.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "delivery_mode": message.delivery_mode,
        "delivery_status": message.delivery_status,
        "requires_response": message.requires_response,
        "created_at": message.created_at.to_string(),
        "delivered_at": message.delivered_at.map(|ts| ts.to_string()),
        "read_at": message.read_at.map(|ts| ts.to_string()),
    })
}

pub(crate) fn required_string<'a>(
    payload: &'a serde_json::Value,
    field: &str,
    command: &str,
) -> Result<&'a str> {
    payload
        .get(field)
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AgentmuxError::UserError(format!("{command} requires {field}")))
}

pub(crate) fn required_u16(payload: &serde_json::Value, field: &str, command: &str) -> Result<u16> {
    payload
        .get(field)
        .and_then(|value| value.as_u64())
        .and_then(|value| u16::try_from(value).ok())
        .ok_or_else(|| AgentmuxError::UserError(format!("{command} requires {field}")))
}

pub(crate) fn parse_visibility(raw: &str) -> Result<Visibility> {
    serde_json::from_value(json!(raw))
        .map_err(|error| AgentmuxError::UserError(format!("invalid visibility '{raw}': {error}")))
}

pub(crate) fn worktree_id_payload(
    payload: &serde_json::Value,
    command: &str,
) -> Result<WorktreeId> {
    required_string(payload, "worktree_id", command)?
        .parse::<WorktreeId>()
        .map_err(|error| AgentmuxError::UserError(format!("invalid worktree_id: {error}")))
}

pub(crate) fn approval_id_payload(
    payload: &serde_json::Value,
    command: &str,
) -> Result<ApprovalId> {
    required_string(payload, "approval_id", command)?
        .parse::<ApprovalId>()
        .map_err(|error| AgentmuxError::UserError(format!("invalid approval_id: {error}")))
}

pub(crate) fn worktree_test_command_payload(payload: &serde_json::Value) -> TestCommand {
    TestCommand {
        name: payload
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or("default")
            .to_string(),
        command: payload
            .get("command")
            .and_then(|value| value.as_str())
            .unwrap_or("cargo test")
            .to_string(),
    }
}

pub(crate) fn json_error(error: serde_json::Error) -> AgentmuxError {
    AgentmuxError::StoreError(format!("failed to encode event payload: {error}"))
}

pub(crate) fn pty_spawn_spec_from_payload(
    payload: &serde_json::Value,
) -> Result<Option<PtySpawnSpec>> {
    let provider = payload.get("provider").and_then(|value| value.as_str());
    let command = match payload.get("command").and_then(|value| value.as_str()) {
        Some(command) => command.to_string(),
        // No explicit command: derive the launch command from the provider so a
        // bare `shell` pane (or claude/codex) actually gets a live PTY instead of
        // a metadata-only session that nothing can be typed into (spec §05 adapters).
        None => match provider {
            Some("shell") => std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()),
            Some("claude") => "claude".to_string(),
            Some("codex") => "codex".to_string(),
            Some("agy") => "agy".to_string(),
            _ => return Ok(None),
        },
    };

    let args = if let Some(value) = payload.get("args") {
        serde_json::from_value(value.clone()).map_err(|error| {
            AgentmuxError::UserError(format!("agent.spawn args must be strings: {error}"))
        })?
    } else {
        default_provider_args(provider)
    };
    let cwd = payload
        .get("cwd")
        .and_then(|value| value.as_str())
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir().map_err(|error| {
            AgentmuxError::Internal(format!("failed to resolve current directory: {error}"))
        })?);
    let mut env: BTreeMap<String, String> = payload
        .get("env")
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .map_err(|error| {
            AgentmuxError::UserError(format!("agent.spawn env must be a string map: {error}"))
        })?
        .unwrap_or_default();
    // A spawned shell/agent needs a usable environment (PATH, HOME, ...). When the
    // caller does not pass env, inherit the daemon's; always ensure TERM is set.
    if env.is_empty() {
        env = std::env::vars().collect();
    }
    env.entry("TERM".to_string())
        .or_insert_with(|| "xterm-256color".to_string());
    let size = payload
        .get("size")
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .map_err(|error| {
            AgentmuxError::UserError(format!(
                "agent.spawn size must include rows and cols: {error}"
            ))
        })?
        .unwrap_or_default();

    Ok(Some(PtySpawnSpec {
        command,
        args,
        cwd,
        env,
        size,
    }))
}

pub(crate) fn default_provider_args(provider: Option<&str>) -> Vec<String> {
    match provider {
        Some("agy") => vec!["--dangerously-skip-permissions".to_string()],
        _ => Vec::new(),
    }
}

pub(crate) fn provider_command(provider: &str) -> String {
    match provider {
        "shell" => std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()),
        "claude" => "claude".to_string(),
        "codex" => "codex".to_string(),
        "agy" => "agy".to_string(),
        custom => custom.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_agent_role_maps_known_labels_to_variants() {
        assert_eq!(parse_agent_role("reviewer").unwrap(), AgentRole::Reviewer);
        assert_eq!(parse_agent_role("Tester").unwrap(), AgentRole::Tester);
        assert_eq!(
            parse_agent_role("security-reviewer").unwrap(),
            AgentRole::SecurityReviewer
        );
        // The inverse of agent_role_label round-trips for a known variant.
        assert_eq!(
            parse_agent_role(&agent_role_label(&AgentRole::DocsWriter)).unwrap(),
            AgentRole::DocsWriter
        );
    }

    #[test]
    fn parse_agent_role_keeps_custom_label_verbatim_and_round_trips() {
        let role = parse_agent_role("qa-lead").unwrap();
        assert_eq!(role, AgentRole::Custom("qa-lead".to_string()));
        // A custom role survives a label/parse round-trip unchanged so that a
        // `role:qa-lead` bus target resolves to the assigned session.
        assert_eq!(parse_agent_role(&agent_role_label(&role)).unwrap(), role);
    }

    #[test]
    fn parse_agent_role_rejects_empty_label() {
        assert!(parse_agent_role("   ").is_err());
    }

    #[test]
    fn default_agent_role_label_is_default() {
        assert_eq!(agent_role_label(&default_agent_role()), "default");
    }
}
