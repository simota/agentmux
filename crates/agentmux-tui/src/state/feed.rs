//! Activity-feed entries, sitrep rows, and shared event-payload helpers.

use serde_json::Value;

#[cfg(feature = "activity-feed")]
use agentmux_ipc::protocol::{DaemonEvent, IpcEventKind};

#[cfg(feature = "activity-feed")]
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct EventFeedFilter {
    pub task_id: Option<String>,
    pub roles: Vec<String>,
    pub kinds: Vec<String>,
}

#[cfg(feature = "activity-feed")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeedEntry {
    pub ts: String,
    pub actor: String,
    pub action: String,
    pub target: String,
    pub kind: String,
    pub focus_agent_id: Option<String>,
}

#[cfg(feature = "activity-feed")]
impl FeedEntry {
    pub fn from_event(event: &DaemonEvent) -> Option<Self> {
        match event.kind {
            IpcEventKind::PtyOutputChunk | IpcEventKind::ScreenDiff => None,
            IpcEventKind::AgentStatusChanged => {
                let agent_id = string_field(&event.payload, "agent_id")?;
                let status = string_field(&event.payload, "status")
                    .or_else(|| string_field(&event.payload, "new_status"))?;
                Some(Self::new(
                    event,
                    agent_id.clone(),
                    format!("status {status}"),
                    agent_id.clone(),
                    Some(agent_id),
                ))
            }
            IpcEventKind::MessageCreated | IpcEventKind::MessageDelivered => {
                let message_id = string_field(&event.payload, "message_id")?;
                let status = string_field(&event.payload, "delivery_status")
                    .unwrap_or_else(|| "created".to_string());
                let to = endpoint_label(event.payload.get("to"));
                Some(Self::new(
                    event,
                    endpoint_label(event.payload.get("from")),
                    format!("message {status}"),
                    to,
                    target_agent_id(event.payload.get("to")).or(Some(message_id)),
                ))
            }
            IpcEventKind::ApprovalCreated => {
                let approval_id = string_field(&event.payload, "approval_id")?;
                Some(Self::new(
                    event,
                    "policy".to_string(),
                    "approval requested".to_string(),
                    approval_id,
                    None,
                ))
            }
            _ => Some(Self::new(
                event,
                event_actor(&event.payload),
                event_action(&event.kind),
                event_target(&event.payload),
                string_field(&event.payload, "agent_id"),
            )),
        }
    }

    fn new(
        event: &DaemonEvent,
        actor: String,
        action: String,
        target: String,
        focus_agent_id: Option<String>,
    ) -> Self {
        Self {
            ts: string_field(&event.payload, "created_at")
                .or_else(|| string_field(&event.payload, "ts"))
                .unwrap_or_else(|| "-".to_string()),
            actor,
            action,
            target,
            kind: event_kind_label(&event.kind).to_string(),
            focus_agent_id,
        }
    }
}

#[cfg(feature = "activity-feed")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SitrepEntry {
    pub agent_id: String,
    pub name: String,
    pub status: String,
    pub needs_attention: bool,
}

pub(crate) fn string_field(payload: &serde_json::Value, field: &str) -> Option<String> {
    payload
        .get(field)
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned)
}

#[cfg(feature = "activity-feed")]
pub(crate) fn needs_attention_status(status: &str) -> bool {
    matches!(
        status,
        "awaiting_input" | "needs_human" | "awaiting_approval" | "blocked" | "stalled"
    )
}

#[cfg(feature = "activity-feed")]
fn target_agent_id(value: Option<&Value>) -> Option<String> {
    let value = value?;
    let kind = value.get("kind").and_then(Value::as_str)?;
    if kind != "agent" {
        return None;
    }
    string_field(value, "id")
}

#[cfg(feature = "activity-feed")]
fn event_actor(payload: &Value) -> String {
    string_field(payload, "agent_id")
        .or_else(|| string_field(payload, "client_id"))
        .or_else(|| string_field(payload, "task_id"))
        .unwrap_or_else(|| "daemon".to_string())
}

#[cfg(feature = "activity-feed")]
fn event_target(payload: &Value) -> String {
    string_field(payload, "agent_id")
        .or_else(|| string_field(payload, "task_id"))
        .or_else(|| string_field(payload, "message_id"))
        .or_else(|| string_field(payload, "approval_id"))
        .unwrap_or_else(|| "-".to_string())
}

#[cfg(feature = "activity-feed")]
fn event_action(kind: &IpcEventKind) -> String {
    event_kind_label(kind)
        .rsplit('.')
        .next()
        .unwrap_or("event")
        .replace('_', " ")
}

#[cfg(feature = "activity-feed")]
fn event_kind_label(kind: &IpcEventKind) -> &'static str {
    match kind {
        IpcEventKind::DaemonStarted => "daemon.started",
        IpcEventKind::DaemonStopped => "daemon.stopped",
        IpcEventKind::ClientAttached => "client.attached",
        IpcEventKind::ClientDetached => "client.detached",
        IpcEventKind::TaskCreated => "task.created",
        IpcEventKind::TaskStatusChanged => "task.status_changed",
        IpcEventKind::AgentSpawned => "agent.spawned",
        IpcEventKind::AgentStatusSignal => "agent.status_signal",
        IpcEventKind::AgentStatusChanged => "agent.status_changed",
        IpcEventKind::AgentExited => "agent.exited",
        IpcEventKind::PtyOutputChunk => "pty.output_chunk",
        IpcEventKind::ScreenDiff => "screen.diff",
        IpcEventKind::TerminalSnapshotSaved => "terminal.snapshot_saved",
        IpcEventKind::InputScriptCreated => "input_script.created",
        IpcEventKind::InputScriptInjected => "input_script.injected",
        IpcEventKind::InputInjected => "input.injected",
        IpcEventKind::MessageCreated => "message.created",
        IpcEventKind::MessageDelivered => "message.delivered",
        IpcEventKind::ContextCreated => "context.created",
        IpcEventKind::ContextInjected => "context.injected",
        IpcEventKind::MailboxWritten => "mailbox.written",
        IpcEventKind::ArtifactCreated => "artifact.created",
        IpcEventKind::ApprovalCreated => "approval.created",
        IpcEventKind::ApprovalDecided => "approval.decided",
        IpcEventKind::WorktreeCreated => "worktree.created",
        IpcEventKind::WorktreeDiffCaptured => "worktree.diff_captured",
        IpcEventKind::WorktreeAdoptRequested => "worktree.adopt_requested",
        IpcEventKind::WorktreeTestCompleted => "worktree.test_completed",
        IpcEventKind::PolicyDenied => "policy.denied",
        IpcEventKind::Error => "error",
    }
}

pub(crate) fn endpoint_label(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return "-".to_string();
    };
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let id = value.get("id").and_then(Value::as_str).unwrap_or("-");
    if id == "-" {
        kind.to_string()
    } else {
        format!("{kind}:{id}")
    }
}

pub(crate) fn output_bytes(payload: &serde_json::Value) -> Option<Vec<u8>> {
    if let Some(bytes) = payload
        .get("bytes")
        .and_then(|value| value.as_array())
        .map(|bytes| {
            bytes
                .iter()
                .filter_map(|value| value.as_u64())
                .filter_map(|value| u8::try_from(value).ok())
                .collect::<Vec<_>>()
        })
        .filter(|bytes| !bytes.is_empty())
    {
        return Some(bytes);
    }

    if let Some(text) = payload
        .get("text")
        .or_else(|| payload.get("data"))
        .or_else(|| payload.get("chunk"))
        .and_then(|value| value.as_str())
    {
        return Some(text.as_bytes().to_vec());
    }

    None
}
