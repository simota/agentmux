//! `ClientRequest` builders for every CLI/TUI daemon command.

use agentmux_core::{AgentmuxError, error::Result};
#[cfg(feature = "activity-feed")]
use agentmux_ipc::EventSubscribeFilter;
use agentmux_ipc::{ClientRequest, IpcCommand};
use agentmux_tui::state::{AgentProviderChoice, TerminalSize as TuiTerminalSize};
use serde_json::json;

use crate::parse::{normalize_agent_target, normalize_message_kind, normalize_priority, unique_agent_name};

pub(crate) fn daemon_status_request() -> ClientRequest {
    ClientRequest::new("req_daemon_status", IpcCommand::DaemonStatus, json!({}))
}

pub(crate) fn tui_daemon_status_request() -> ClientRequest {
    ClientRequest::new("req_tui_status", IpcCommand::DaemonStatus, json!({}))
}

#[cfg(not(feature = "arena"))]
pub(crate) fn task_run_request(description: String, team: Option<String>) -> Result<ClientRequest> {
    let project_path = std::env::current_dir()
        .map_err(|error| AgentmuxError::Internal(format!("failed to resolve cwd: {error}")))?;

    Ok(ClientRequest::new(
        "req_task_run",
        IpcCommand::TaskRun,
        json!({
            "project_path": project_path,
            "body": description,
            "team": team.unwrap_or_else(|| "claude-codex".to_string()),
        }),
    ))
}

#[cfg(feature = "arena")]
pub(crate) fn task_run_request(
    description: String,
    team: Option<String>,
    arena: Option<String>,
    base_branch: Option<String>,
) -> Result<ClientRequest> {
    let project_path = std::env::current_dir()
        .map_err(|error| AgentmuxError::Internal(format!("failed to resolve cwd: {error}")))?;
    let mut payload = json!({
        "project_path": project_path,
        "body": description,
        "team": team.unwrap_or_else(|| "claude-codex".to_string()),
    });

    if let Some(arena) = arena {
        let providers = arena
            .split(',')
            .map(str::trim)
            .filter(|provider| !provider.is_empty())
            .collect::<Vec<_>>();
        if providers.is_empty() {
            return Err(AgentmuxError::UserError(
                "task run --arena requires at least one provider".to_string(),
            ));
        }
        payload["runner"] = json!("arena");
        payload["arena"] = json!(arena);
        payload["providers"] = json!(providers);
        if let Some(base_branch) = base_branch.filter(|value| !value.trim().is_empty()) {
            payload["base_branch"] = json!(base_branch);
        }
    }

    Ok(ClientRequest::new(
        "req_task_run",
        IpcCommand::TaskRun,
        payload,
    ))
}

#[cfg(feature = "arena")]
pub(crate) fn worktree_adopt_request(worktree_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_worktree_adopt",
        IpcCommand::WorktreeAdopt,
        json!({ "worktree_id": worktree_id }),
    )
}

pub(crate) fn attach_request(target: String) -> ClientRequest {
    ClientRequest::new(
        "req_attach",
        IpcCommand::ClientAttach,
        json!({ "agent_id": target }),
    )
}

pub(crate) fn snapshot_request(target: String) -> ClientRequest {
    ClientRequest::new(
        "req_snapshot",
        IpcCommand::AgentSnapshot,
        json!({ "agent_id": target }),
    )
}

pub(crate) fn detach_request() -> ClientRequest {
    ClientRequest::new("req_detach", IpcCommand::ClientDetach, json!({}))
}

#[cfg(feature = "activity-feed")]
pub(crate) fn event_subscribe_request(filter: &agentmux_tui::state::EventFeedFilter) -> ClientRequest {
    ClientRequest::new(
        "req_event_subscribe",
        IpcCommand::EventSubscribe,
        serde_json::to_value(EventSubscribeFilter {
            task_id: filter.task_id.clone(),
            roles: filter.roles.clone(),
            kinds: filter.kinds.clone(),
        })
        .unwrap_or_else(|_| json!({})),
    )
}

pub(crate) fn agent_ls_request() -> ClientRequest {
    ClientRequest::new("req_agent_ls", IpcCommand::DaemonStatus, json!({}))
}

pub(crate) fn sessions_list_request() -> ClientRequest {
    ClientRequest::new("req_sessions_list", IpcCommand::DaemonStatus, json!({}))
}

pub(crate) fn agent_spawn_for_provider_request_with_size(
    provider: AgentProviderChoice,
    size: Option<TuiTerminalSize>,
) -> ClientRequest {
    agent_spawn_for_provider_request_with_id("req_agent_spawn_provider", provider, size)
}

pub(crate) fn agent_spawn_for_provider_request_with_id(
    request_id: impl Into<String>,
    provider: AgentProviderChoice,
    size: Option<TuiTerminalSize>,
) -> ClientRequest {
    let mut payload = json!({
        "provider": provider.provider(),
        "role": "implementer",
        "name": unique_agent_name(provider.default_name()),
    });
    if let Some(size) = size {
        payload["size"] = json!({
            "rows": size.rows,
            "cols": size.cols,
        });
    }

    ClientRequest::new(request_id, IpcCommand::AgentSpawn, payload)
}

pub(crate) fn agent_spawn_request(provider: String, role: String) -> Result<ClientRequest> {
    if provider.trim().is_empty() {
        return Err(AgentmuxError::UserError(
            "agent provider must not be empty".to_string(),
        ));
    }
    if role.trim().is_empty() {
        return Err(AgentmuxError::UserError(
            "agent role must not be empty".to_string(),
        ));
    }

    Ok(ClientRequest::new(
        "req_agent_spawn",
        IpcCommand::AgentSpawn,
        json!({
            "provider": provider,
            "role": role,
            "name": unique_agent_name(&role),
        }),
    ))
}

pub(crate) fn agent_stop_request(agent_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_agent_stop",
        IpcCommand::AgentStop,
        json!({ "agent_id": agent_id }),
    )
}

pub(crate) fn agent_send_request(agent_id: String, body: String) -> Result<ClientRequest> {
    if agent_id.trim().is_empty() {
        return Err(AgentmuxError::UserError(
            "agent message target must not be empty".to_string(),
        ));
    }
    if body.trim().is_empty() {
        return Err(AgentmuxError::UserError(
            "agent message body must not be empty".to_string(),
        ));
    }
    Ok(ClientRequest::new(
        "req_agent_send",
        IpcCommand::MessageCreate,
        json!({
            "to": normalize_agent_target(&agent_id),
            "body": body,
            "kind": "handoff",
            "priority": "normal",
            "delivery_mode": "inject_when_idle",
        }),
    ))
}

pub(crate) fn agent_inject_request(message_id: String, agent_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_agent_inject",
        IpcCommand::MessageInject,
        json!({ "message_id": message_id, "agent_id": agent_id }),
    )
}

pub(crate) fn agent_focus_request(agent_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_agent_focus",
        IpcCommand::AgentFocus,
        json!({ "agent_id": agent_id }),
    )
}

pub(crate) fn agent_interrupt_request(agent_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_agent_interrupt",
        IpcCommand::AgentInterrupt,
        json!({ "agent_id": agent_id }),
    )
}

pub(crate) fn agent_resize_request(id: String, agent_id: String, size: TuiTerminalSize) -> ClientRequest {
    ClientRequest::new(
        id,
        IpcCommand::AgentResize,
        json!({
            "agent_id": agent_id,
            "rows": size.rows,
            "cols": size.cols,
        }),
    )
}

pub(crate) fn message_list_request() -> ClientRequest {
    ClientRequest::new("req_message_list", IpcCommand::MessageList, json!({}))
}

pub(crate) fn message_show_request(message_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_message_show",
        IpcCommand::MessageShow,
        json!({ "message_id": message_id }),
    )
}

pub(crate) fn message_send_request(
    to: String,
    body: String,
    kind: Option<String>,
    priority: Option<String>,
) -> Result<ClientRequest> {
    if body.trim().is_empty() {
        return Err(AgentmuxError::UserError(
            "message body must not be empty".to_string(),
        ));
    }

    let kind = match kind {
        Some(raw) => normalize_message_kind(&raw)?,
        None => "handoff".to_string(),
    };
    let priority = match priority {
        Some(raw) => normalize_priority(&raw)?,
        None => "normal".to_string(),
    };

    let mut payload = json!({
        "to": to,
        "body": body,
        "kind": kind,
        "priority": priority,
        "delivery_mode": "inject_when_idle",
    });

    // When invoked from inside a live agent session, attribute the message to
    // that agent so recipients can reply with `agent:<sender-session-name>`.
    if let Ok(agent_id) = std::env::var("AGENTMUX_AGENT_ID")
        && !agent_id.trim().is_empty()
    {
        payload["from_agent_id"] = json!(agent_id);
    }

    Ok(ClientRequest::new(
        "req_message_send",
        IpcCommand::MessageCreate,
        payload,
    ))
}

pub(crate) fn meeting_open_request(
    topic: String,
    participants: String,
    max_turns: Option<u32>,
    kind: Option<String>,
    priority: Option<String>,
    body: Option<String>,
) -> Result<ClientRequest> {
    if topic.trim().is_empty() {
        return Err(AgentmuxError::UserError(
            "meeting topic must not be empty".to_string(),
        ));
    }
    let participants: Vec<String> = participants
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    if participants.len() < 2 {
        return Err(AgentmuxError::UserError(
            "meeting requires at least 2 participants (comma-separated session names or ids)"
                .to_string(),
        ));
    }
    let kind = match kind {
        Some(raw) => normalize_message_kind(&raw)?,
        None => "question".to_string(),
    };
    let priority = match priority {
        Some(raw) => normalize_priority(&raw)?,
        None => "normal".to_string(),
    };

    let mut payload = json!({
        "topic": topic,
        "participants": participants,
        "kind": kind,
        "priority": priority,
    });
    if let Some(max_turns) = max_turns {
        payload["max_messages_per_participant"] = json!(max_turns);
    }
    if let Some(body) = body {
        payload["body"] = json!(body);
    }
    // Attribute the meeting (and its kickoff) to the opening agent session.
    if let Ok(agent_id) = std::env::var("AGENTMUX_AGENT_ID")
        && !agent_id.trim().is_empty()
    {
        payload["from_agent_id"] = json!(agent_id);
    }

    Ok(ClientRequest::new(
        "req_meeting_open",
        IpcCommand::MeetingOpen,
        payload,
    ))
}

pub(crate) fn meeting_close_request(thread_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_meeting_close",
        IpcCommand::MeetingClose,
        json!({ "thread_id": thread_id }),
    )
}

pub(crate) fn meeting_list_request() -> ClientRequest {
    ClientRequest::new("req_meeting_list", IpcCommand::MeetingList, json!({}))
}

pub(crate) fn message_inject_request(message_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_message_inject",
        IpcCommand::MessageInject,
        json!({ "message_id": message_id }),
    )
}

pub(crate) fn context_add_request(title: String) -> Result<ClientRequest> {
    if title.trim().is_empty() {
        return Err(AgentmuxError::UserError(
            "context title must not be empty".to_string(),
        ));
    }

    Ok(ClientRequest::new(
        "req_context_add",
        IpcCommand::ContextCreate,
        json!({
            "title": title,
            "kind": "handoff_summary",
            "visibility": "internal",
        }),
    ))
}

pub(crate) fn context_list_request() -> ClientRequest {
    ClientRequest::new("req_context_list", IpcCommand::ContextSearch, json!({}))
}

pub(crate) fn context_show_request(context_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_context_show",
        IpcCommand::ContextSearch,
        json!({ "context_id": context_id }),
    )
}

pub(crate) fn context_search_request(query: String) -> Result<ClientRequest> {
    if query.trim().is_empty() {
        return Err(AgentmuxError::UserError(
            "context search query must not be empty".to_string(),
        ));
    }

    Ok(ClientRequest::new(
        "req_context_search",
        IpcCommand::ContextSearch,
        json!({ "query": query }),
    ))
}

pub(crate) fn context_attach_request(context_id: String, message_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_context_attach",
        IpcCommand::ContextAttach,
        json!({ "context_id": context_id, "message_id": message_id }),
    )
}

pub(crate) fn context_inject_request(context_id: String, agent_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_context_inject",
        IpcCommand::ContextInject,
        json!({ "context_id": context_id, "agent_id": agent_id }),
    )
}

pub(crate) fn context_export_request(output: String) -> ClientRequest {
    ClientRequest::new(
        "req_context_export",
        IpcCommand::ContextExport,
        json!({ "output": output }),
    )
}

pub(crate) fn worktree_list_request() -> ClientRequest {
    ClientRequest::new("req_worktree_list", IpcCommand::WorktreeList, json!({}))
}

pub(crate) fn worktree_diff_request(worktree_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_worktree_diff",
        IpcCommand::WorktreeDiff,
        json!({ "worktree_id": worktree_id }),
    )
}

pub(crate) fn worktree_test_request(worktree_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_worktree_test",
        IpcCommand::WorktreeTest,
        json!({ "worktree_id": worktree_id }),
    )
}

pub(crate) fn worktree_promote_request(worktree_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_worktree_promote",
        IpcCommand::WorktreePromote,
        json!({ "worktree_id": worktree_id }),
    )
}

pub(crate) fn worktree_archive_request(worktree_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_worktree_archive",
        IpcCommand::WorktreeArchive,
        json!({ "worktree_id": worktree_id }),
    )
}

pub(crate) fn approval_list_request() -> ClientRequest {
    ClientRequest::new("req_approval_list", IpcCommand::ApprovalList, json!({}))
}

pub(crate) fn approval_approve_request(approval_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_approval_approve",
        IpcCommand::ApprovalApprove,
        json!({ "approval_id": approval_id }),
    )
}

pub(crate) fn approval_reject_request(approval_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_approval_reject",
        IpcCommand::ApprovalReject,
        json!({ "approval_id": approval_id }),
    )
}

pub(crate) fn layout_save_request(name: String) -> Result<ClientRequest> {
    if name.trim().is_empty() {
        return Err(AgentmuxError::UserError(
            "layout name must not be empty".to_string(),
        ));
    }

    Ok(ClientRequest::new(
        "req_layout_save",
        IpcCommand::LayoutSet,
        json!({ "name": name }),
    ))
}

pub(crate) fn layout_load_request(name: String) -> ClientRequest {
    ClientRequest::new(
        "req_layout_load",
        IpcCommand::LayoutGet,
        json!({ "name": name }),
    )
}

pub(crate) fn layout_list_request() -> ClientRequest {
    ClientRequest::new("req_layout_list", IpcCommand::LayoutGet, json!({}))
}
