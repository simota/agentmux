//! IPC protocol envelope types.

use crate::PROTOCOL_VERSION;
use serde::{Deserialize, Serialize};

/// Handshake sent when a client opens a daemon connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHello {
    #[serde(rename = "type")]
    pub kind: String,
    pub payload: ClientHelloPayload,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHelloPayload {
    pub client_version: String,
    pub protocol: u32,
}

impl ClientHello {
    pub fn new(client_version: impl Into<String>) -> Self {
        Self {
            kind: "hello".to_string(),
            payload: ClientHelloPayload {
                client_version: client_version.into(),
                protocol: PROTOCOL_VERSION,
            },
        }
    }

    pub fn protocol_compatibility(&self) -> ProtocolCompatibility {
        if self.kind != "hello" {
            return ProtocolCompatibility::InvalidHandshake;
        }

        if self.payload.protocol == PROTOCOL_VERSION {
            ProtocolCompatibility::Compatible
        } else {
            ProtocolCompatibility::VersionMismatch {
                expected: PROTOCOL_VERSION,
                actual: self.payload.protocol,
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolCompatibility {
    Compatible,
    VersionMismatch { expected: u32, actual: u32 },
    InvalidHandshake,
}

/// Request envelope sent from client to daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientRequest {
    /// Request correlation ID, e.g. `req_001`.
    pub id: String,
    /// Protocol version used to encode this request.
    pub version: u32,
    #[serde(rename = "type")]
    pub command: IpcCommand,
    #[serde(default)]
    pub payload: serde_json::Value,
}

impl ClientRequest {
    pub fn new(
        id: impl Into<String>,
        command: IpcCommand,
        payload: impl Into<serde_json::Value>,
    ) -> Self {
        Self {
            id: id.into(),
            version: PROTOCOL_VERSION,
            command,
            payload: payload.into(),
        }
    }

    pub fn protocol_compatibility(&self) -> ProtocolCompatibility {
        if self.version == PROTOCOL_VERSION {
            ProtocolCompatibility::Compatible
        } else {
            ProtocolCompatibility::VersionMismatch {
                expected: PROTOCOL_VERSION,
                actual: self.version,
            }
        }
    }
}

/// Response envelope sent from daemon to client for a request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonResponse {
    /// Mirrors the request ID.
    pub id: String,
    /// Protocol version used to encode this response.
    pub version: u32,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

impl DaemonResponse {
    pub fn ok(id: impl Into<String>, payload: impl Into<serde_json::Value>) -> Self {
        Self {
            id: id.into(),
            version: PROTOCOL_VERSION,
            ok: true,
            payload: Some(payload.into()),
            error: None,
        }
    }

    pub fn error(id: impl Into<String>, error: ErrorBody) -> Self {
        Self {
            id: id.into(),
            version: PROTOCOL_VERSION,
            ok: false,
            payload: None,
            error: Some(error),
        }
    }
}

/// Unsolicited daemon event delivered over the same JSONL stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonEvent {
    pub version: u32,
    #[serde(rename = "type")]
    pub kind: IpcEventKind,
    #[serde(default)]
    pub payload: serde_json::Value,
}

impl DaemonEvent {
    pub fn new(kind: IpcEventKind, payload: impl Into<serde_json::Value>) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            kind,
            payload: payload.into(),
        }
    }
}

/// Frame sent from daemon to client on the JSONL stream.
///
/// A client receives request responses and unsolicited daemon events on the
/// same socket. After `client.attach`, the daemon keeps this stream open and
/// may continue to push events until `client.detach` or disconnect.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DaemonStreamFrame {
    Response(DaemonResponse),
    Event(DaemonEvent),
}

impl From<DaemonResponse> for DaemonStreamFrame {
    fn from(response: DaemonResponse) -> Self {
        Self::Response(response)
    }
}

impl From<DaemonEvent> for DaemonStreamFrame {
    fn from(event: DaemonEvent) -> Self {
        Self::Event(event)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl ErrorBody {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            hint: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EventSubscribeFilter {
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub kinds: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IpcCommand {
    #[serde(rename = "daemon.status")]
    DaemonStatus,
    #[serde(rename = "client.attach")]
    ClientAttach,
    #[serde(rename = "client.detach")]
    ClientDetach,
    #[serde(rename = "event.subscribe")]
    EventSubscribe,
    #[serde(rename = "layout.get")]
    LayoutGet,
    #[serde(rename = "layout.set")]
    LayoutSet,
    #[serde(rename = "task.run")]
    TaskRun,
    #[serde(rename = "task.pause")]
    TaskPause,
    #[serde(rename = "task.resume")]
    TaskResume,
    #[serde(rename = "task.cancel")]
    TaskCancel,
    #[serde(rename = "task.status")]
    TaskStatus,
    #[serde(rename = "agent.spawn")]
    AgentSpawn,
    #[serde(rename = "agent.stop")]
    AgentStop,
    #[serde(rename = "agent.interrupt")]
    AgentInterrupt,
    #[serde(rename = "agent.resize")]
    AgentResize,
    #[serde(rename = "agent.focus")]
    AgentFocus,
    #[serde(rename = "agent.send_input_script")]
    AgentSendInputScript,
    #[serde(rename = "agent.broadcast_input")]
    AgentBroadcastInput,
    #[serde(rename = "agent.set_role")]
    AgentSetRole,
    #[serde(rename = "agent.snapshot")]
    AgentSnapshot,
    #[serde(rename = "meeting.open")]
    MeetingOpen,
    #[serde(rename = "meeting.close")]
    MeetingClose,
    #[serde(rename = "meeting.list")]
    MeetingList,
    #[serde(rename = "message.create")]
    MessageCreate,
    #[serde(rename = "message.inject")]
    MessageInject,
    #[serde(rename = "message.list")]
    MessageList,
    #[serde(rename = "message.show")]
    MessageShow,
    #[serde(rename = "context.create")]
    ContextCreate,
    #[serde(rename = "context.search")]
    ContextSearch,
    #[serde(rename = "context.attach")]
    ContextAttach,
    #[serde(rename = "context.inject")]
    ContextInject,
    #[serde(rename = "context.export")]
    ContextExport,
    #[serde(rename = "worktree.list")]
    WorktreeList,
    #[serde(rename = "worktree.diff")]
    WorktreeDiff,
    #[serde(rename = "worktree.test")]
    WorktreeTest,
    #[serde(rename = "worktree.promote")]
    WorktreePromote,
    #[serde(rename = "worktree.archive")]
    WorktreeArchive,
    #[serde(rename = "worktree.adopt")]
    WorktreeAdopt,
    #[serde(rename = "approval.list")]
    ApprovalList,
    #[serde(rename = "approval.approve")]
    ApprovalApprove,
    #[serde(rename = "approval.reject")]
    ApprovalReject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IpcEventKind {
    #[serde(rename = "daemon.started")]
    DaemonStarted,
    #[serde(rename = "daemon.stopped")]
    DaemonStopped,
    #[serde(rename = "client.attached")]
    ClientAttached,
    #[serde(rename = "client.detached")]
    ClientDetached,
    #[serde(rename = "task.created")]
    TaskCreated,
    #[serde(rename = "task.status_changed")]
    TaskStatusChanged,
    #[serde(rename = "agent.spawned")]
    AgentSpawned,
    #[serde(rename = "agent.status_signal")]
    AgentStatusSignal,
    #[serde(rename = "agent.status_changed")]
    AgentStatusChanged,
    #[serde(rename = "agent.role_changed")]
    AgentRoleChanged,
    #[serde(rename = "agent.exited")]
    AgentExited,
    #[serde(rename = "pty.output_chunk")]
    PtyOutputChunk,
    #[serde(rename = "screen.diff")]
    ScreenDiff,
    #[serde(rename = "terminal.snapshot_saved")]
    TerminalSnapshotSaved,
    #[serde(rename = "input_script.created")]
    InputScriptCreated,
    #[serde(rename = "input_script.injected")]
    InputScriptInjected,
    #[serde(rename = "input.injected")]
    InputInjected,
    #[serde(rename = "message.created")]
    MessageCreated,
    #[serde(rename = "message.delivered")]
    MessageDelivered,
    #[serde(rename = "thread.opened")]
    ThreadOpened,
    #[serde(rename = "thread.closed")]
    ThreadClosed,
    #[serde(rename = "context.created")]
    ContextCreated,
    #[serde(rename = "context.injected")]
    ContextInjected,
    #[serde(rename = "mailbox.written")]
    MailboxWritten,
    #[serde(rename = "artifact.created")]
    ArtifactCreated,
    #[serde(rename = "approval.created")]
    ApprovalCreated,
    #[serde(rename = "approval.decided")]
    ApprovalDecided,
    #[serde(rename = "worktree.created")]
    WorktreeCreated,
    #[serde(rename = "worktree.diff_captured")]
    WorktreeDiffCaptured,
    #[serde(rename = "worktree.adopt_requested")]
    WorktreeAdoptRequested,
    #[serde(rename = "worktree.test_completed")]
    WorktreeTestCompleted,
    #[serde(rename = "policy.denied")]
    PolicyDenied,
    #[serde(rename = "error")]
    Error,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_serializes_spec_shape_with_protocol_version() {
        let request = ClientRequest::new(
            "req_001",
            IpcCommand::TaskRun,
            json!({
                "project_path": ".",
                "body": "refresh token bug",
                "team": "claude-codex"
            }),
        );

        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(encoded["id"], "req_001");
        assert_eq!(encoded["version"], PROTOCOL_VERSION);
        assert_eq!(encoded["type"], "task.run");
        assert_eq!(encoded["payload"]["team"], "claude-codex");

        let decoded: ClientRequest = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.command, IpcCommand::TaskRun);
        assert_eq!(
            decoded.protocol_compatibility(),
            ProtocolCompatibility::Compatible
        );
    }

    #[test]
    fn response_serializes_success_and_error_shapes() {
        let success = DaemonResponse::ok("req_001", json!({ "task_id": "task_123" }));
        let encoded = serde_json::to_value(success).unwrap();
        assert_eq!(encoded["ok"], true);
        assert_eq!(encoded["payload"]["task_id"], "task_123");
        assert!(encoded.get("error").is_none());

        let error = DaemonResponse::error(
            "req_123",
            ErrorBody::new("AGENT_NOT_FOUND", "agent 'impl-codex' not found")
                .with_hint("agentmux agent ls"),
        );
        let encoded = serde_json::to_value(error).unwrap();
        assert_eq!(encoded["ok"], false);
        assert_eq!(encoded["error"]["code"], "AGENT_NOT_FOUND");
        assert_eq!(encoded["error"]["hint"], "agentmux agent ls");
        assert!(encoded.get("payload").is_none());
    }

    #[test]
    fn event_serializes_spec_shape() {
        let event = DaemonEvent::new(
            IpcEventKind::ScreenDiff,
            json!({ "pane_id": "pane_001", "regions": [] }),
        );

        let encoded = serde_json::to_value(&event).unwrap();
        assert_eq!(encoded["version"], PROTOCOL_VERSION);
        assert_eq!(encoded["type"], "screen.diff");
        assert_eq!(encoded["payload"]["pane_id"], "pane_001");
    }

    #[test]
    fn daemon_stream_frame_decodes_mixed_response_and_event_frames() {
        let response = DaemonResponse::ok("req_attach", json!({ "client_id": "client_001" }));
        let event = DaemonEvent::new(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_001" }),
        );

        let response_frame: DaemonStreamFrame =
            serde_json::from_value(serde_json::to_value(response).unwrap()).unwrap();
        let event_frame: DaemonStreamFrame =
            serde_json::from_value(serde_json::to_value(event).unwrap()).unwrap();

        assert!(matches!(response_frame, DaemonStreamFrame::Response(_)));
        assert!(matches!(event_frame, DaemonStreamFrame::Event(_)));
    }

    #[test]
    fn attach_command_uses_streaming_protocol_shape() {
        let request = ClientRequest::new(
            "req_attach",
            IpcCommand::ClientAttach,
            json!({ "agent_id": "agent_001" }),
        );

        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(encoded["type"], "client.attach");
        assert_eq!(encoded["payload"]["agent_id"], "agent_001");
    }

    #[test]
    fn event_subscribe_command_round_trips_filter_payload() {
        let request = ClientRequest::new(
            "req_event_subscribe",
            IpcCommand::EventSubscribe,
            serde_json::to_value(EventSubscribeFilter {
                task_id: Some("task_001".to_string()),
                roles: vec!["implementer".to_string()],
                kinds: vec!["agent.status_changed".to_string()],
            })
            .unwrap(),
        );

        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(encoded["type"], "event.subscribe");
        assert_eq!(encoded["payload"]["task_id"], "task_001");
        assert_eq!(encoded["payload"]["roles"][0], "implementer");
        assert_eq!(encoded["payload"]["kinds"][0], "agent.status_changed");

        let decoded: ClientRequest = serde_json::from_value(encoded).unwrap();
        let filter: EventSubscribeFilter = serde_json::from_value(decoded.payload).unwrap();
        assert_eq!(decoded.command, IpcCommand::EventSubscribe);
        assert_eq!(filter.task_id.as_deref(), Some("task_001"));
        assert_eq!(filter.roles, ["implementer"]);
        assert_eq!(filter.kinds, ["agent.status_changed"]);
    }

    #[test]
    fn event_kinds_include_required_continuous_stream_events() {
        let cases = [
            (IpcEventKind::AgentSpawned, "agent.spawned"),
            (IpcEventKind::AgentExited, "agent.exited"),
            (IpcEventKind::PtyOutputChunk, "pty.output_chunk"),
            (IpcEventKind::AgentStatusChanged, "agent.status_changed"),
            (IpcEventKind::MessageDelivered, "message.delivered"),
            (IpcEventKind::ApprovalCreated, "approval.created"),
            (IpcEventKind::InputScriptInjected, "input_script.injected"),
        ];

        for (kind, expected) in cases {
            let event = DaemonEvent::new(kind, json!({}));
            let encoded = serde_json::to_value(event).unwrap();
            assert_eq!(encoded["type"], expected);
        }
    }

    #[test]
    fn agent_broadcast_input_command_round_trips_payload() {
        // The broadcast-input wire shape: a target string (broadcast / role:<r>
        // / agent:<id> ...) plus a list of InputAction values. The daemon writes
        // these actions verbatim into every resolved-and-quiet agent PTY.
        let request = ClientRequest::new(
            "req_agent_broadcast",
            IpcCommand::AgentBroadcastInput,
            json!({
                "target": "broadcast",
                "actions": [
                    { "paste_text": "ls -la" },
                    "press_enter"
                ]
            }),
        );

        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(encoded["type"], "agent.broadcast_input");
        assert_eq!(encoded["payload"]["target"], "broadcast");
        assert_eq!(encoded["payload"]["actions"][0]["paste_text"], "ls -la");

        let decoded: ClientRequest = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.command, IpcCommand::AgentBroadcastInput);
        assert_eq!(
            decoded.protocol_compatibility(),
            ProtocolCompatibility::Compatible
        );
    }

    #[test]
    fn agent_set_role_command_round_trips_payload() {
        // The set-role wire shape: an agent_id plus a free-form role label.
        // The daemon parses the label (known label -> variant, otherwise custom)
        // and replies with the resolved label.
        let request = ClientRequest::new(
            "req_agent_set_role",
            IpcCommand::AgentSetRole,
            json!({ "agent_id": "agent_001", "role": "reviewer" }),
        );

        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(encoded["type"], "agent.set_role");
        assert_eq!(encoded["payload"]["agent_id"], "agent_001");
        assert_eq!(encoded["payload"]["role"], "reviewer");

        let decoded: ClientRequest = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.command, IpcCommand::AgentSetRole);
        assert_eq!(
            decoded.protocol_compatibility(),
            ProtocolCompatibility::Compatible
        );
    }

    #[test]
    fn agent_role_changed_event_serializes_spec_shape() {
        let event = DaemonEvent::new(
            IpcEventKind::AgentRoleChanged,
            json!({ "agent_id": "agent_001", "role": "qa-lead" }),
        );

        let encoded = serde_json::to_value(&event).unwrap();
        assert_eq!(encoded["type"], "agent.role_changed");
        assert_eq!(encoded["payload"]["agent_id"], "agent_001");
        assert_eq!(encoded["payload"]["role"], "qa-lead");
    }

    #[test]
    fn hello_checks_protocol_version() {
        let hello = ClientHello::new("0.1.0");
        assert_eq!(hello.kind, "hello");
        assert_eq!(
            hello.protocol_compatibility(),
            ProtocolCompatibility::Compatible
        );

        let mismatched = ClientHello {
            kind: "hello".to_string(),
            payload: ClientHelloPayload {
                client_version: "0.1.0".to_string(),
                protocol: PROTOCOL_VERSION + 1,
            },
        };
        assert_eq!(
            mismatched.protocol_compatibility(),
            ProtocolCompatibility::VersionMismatch {
                expected: PROTOCOL_VERSION,
                actual: PROTOCOL_VERSION + 1,
            }
        );
    }
}
