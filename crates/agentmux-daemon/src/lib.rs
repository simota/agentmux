//! Runtime pieces for the local agentmux daemon.
//!
//! The daemon keeps live agent/session state in memory and exposes it over
//! JSONL IPC on a Unix domain socket. Persistence and actual provider process
//! spawning are layered on later Phase 2 tasks.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use agentmux_agent::{EncodedInputStep, InputScript, encode_input_action};
use agentmux_core::{AgentSessionId, AgentmuxError, ClientSessionId, DateTimeUtc, error::Result};
use agentmux_ipc::{
    ClientHello, ClientRequest, DaemonEvent, DaemonResponse, ErrorBody, IpcCommand, IpcEventKind,
    JsonlReader, JsonlWriter, ProtocolCompatibility,
};
use agentmux_pty::{PtyHandle, PtySpawnSpec};
use agentmux_store::{EventLog, EventLogEntry};
use serde_json::json;
use tokio::io::BufReader;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{RwLock, broadcast, mpsc};
use tokio::task::JoinSet;

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub socket_path: PathBuf,
}

impl DaemonConfig {
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredAgentSession {
    pub id: AgentSessionId,
    pub name: String,
    pub process_id: Option<u32>,
    pub attached_clients: BTreeSet<ClientSessionId>,
}

impl RegisteredAgentSession {
    fn new(name: String, process_id: Option<u32>) -> Self {
        Self {
            id: AgentSessionId::new(),
            name,
            process_id,
            attached_clients: BTreeSet::new(),
        }
    }
}

struct LiveAgentSession {
    metadata: RegisteredAgentSession,
    pty: Option<Mutex<PtyHandle>>,
}

#[derive(Default)]
struct DaemonState {
    clients: BTreeMap<ClientSessionId, Option<AgentSessionId>>,
    agents: BTreeMap<AgentSessionId, LiveAgentSession>,
}

#[derive(Clone)]
pub struct DaemonRuntime {
    state: Arc<RwLock<DaemonState>>,
    events: broadcast::Sender<DaemonEvent>,
    event_log: Option<EventLog>,
}

impl DaemonRuntime {
    pub fn new(event_capacity: usize) -> Self {
        let (events, _receiver) = broadcast::channel(event_capacity.max(1));
        Self {
            state: Arc::new(RwLock::new(DaemonState::default())),
            events,
            event_log: None,
        }
    }

    pub fn with_event_log(mut self, event_log: EventLog) -> Self {
        self.event_log = Some(event_log);
        self
    }

    pub fn subscribe(&self) -> broadcast::Receiver<DaemonEvent> {
        self.events.subscribe()
    }

    pub async fn register_agent(&self, name: String) -> RegisteredAgentSession {
        let agent = RegisteredAgentSession::new(name, None);
        self.state
            .write()
            .await
            .agents
            .insert(agent.id.clone(), LiveAgentSession::metadata(agent.clone()));
        self.publish(DaemonEvent::new(
            IpcEventKind::AgentSpawned,
            json!({
                "agent_id": agent.id.to_string(),
                "name": agent.name,
                "process_id": agent.process_id,
            }),
        ));
        agent
    }

    pub async fn spawn_agent(
        &self,
        name: String,
        spec: PtySpawnSpec,
    ) -> Result<RegisteredAgentSession> {
        let pty = PtyHandle::spawn(spec)?;
        let agent = RegisteredAgentSession::new(name, pty.process_id());
        self.state.write().await.agents.insert(
            agent.id.clone(),
            LiveAgentSession {
                metadata: agent.clone(),
                pty: Some(Mutex::new(pty)),
            },
        );
        self.publish(DaemonEvent::new(
            IpcEventKind::AgentSpawned,
            json!({
                "agent_id": agent.id.to_string(),
                "name": agent.name,
                "process_id": agent.process_id,
            }),
        ));
        Ok(agent)
    }

    pub async fn attach_client(
        &self,
        client_id: ClientSessionId,
        agent_id: AgentSessionId,
    ) -> Result<()> {
        let mut state = self.state.write().await;
        let Some(agent) = state.agents.get_mut(&agent_id) else {
            return Err(AgentmuxError::UserError(format!(
                "unknown agent session '{agent_id}'"
            )));
        };
        agent.metadata.attached_clients.insert(client_id.clone());
        state
            .clients
            .insert(client_id.clone(), Some(agent_id.clone()));
        drop(state);

        self.publish(DaemonEvent::new(
            IpcEventKind::ClientAttached,
            json!({ "client_id": client_id.to_string(), "agent_id": agent_id.to_string() }),
        ));
        Ok(())
    }

    pub async fn detach_client(&self, client_id: &ClientSessionId) -> Option<AgentSessionId> {
        let mut state = self.state.write().await;
        let detached_agent_id = state.clients.insert(client_id.clone(), None).flatten();
        if let Some(agent_id) = &detached_agent_id
            && let Some(agent) = state.agents.get_mut(agent_id)
        {
            agent.metadata.attached_clients.remove(client_id);
        }
        drop(state);

        self.publish(DaemonEvent::new(
            IpcEventKind::ClientDetached,
            json!({
                "client_id": client_id.to_string(),
                "agent_id": detached_agent_id.as_ref().map(ToString::to_string),
            }),
        ));
        detached_agent_id
    }

    pub async fn status_payload(&self) -> serde_json::Value {
        let state = self.state.read().await;
        let agents: Vec<_> = state
            .agents
            .values()
            .map(|agent| {
                json!({
                    "id": agent.metadata.id.to_string(),
                    "name": agent.metadata.name,
                    "process_id": agent.metadata.process_id,
                    "has_process": agent.pty.is_some(),
                    "attached_clients": agent
                        .metadata
                        .attached_clients
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>(),
                })
            })
            .collect();

        json!({
            "protocol_version": agentmux_ipc::PROTOCOL_VERSION,
            "client_count": state.clients.len(),
            "agent_count": state.agents.len(),
            "agents": agents,
        })
    }

    pub async fn send_input_script(&self, script: &InputScript) -> Result<()> {
        self.append_input_script_event("input_script.created", script)?;

        {
            let state = self.state.read().await;
            let Some(agent) = state.agents.get(&script.target_agent_id) else {
                return Err(AgentmuxError::UserError(format!(
                    "unknown agent session '{}'",
                    script.target_agent_id
                )));
            };
            let Some(pty) = &agent.pty else {
                return Err(AgentmuxError::UserError(format!(
                    "agent session '{}' has no live PTY",
                    script.target_agent_id
                )));
            };
            let mut pty = pty.lock().map_err(|_| {
                AgentmuxError::Internal(format!(
                    "PTY lock for agent '{}' is poisoned",
                    script.target_agent_id
                ))
            })?;
            for action in &script.actions {
                match encode_input_action(action)? {
                    EncodedInputStep::Bytes(bytes) => pty.write_bytes(&bytes)?,
                    EncodedInputStep::Wait(duration) => std::thread::sleep(duration),
                }
            }
        }

        self.append_input_script_event("input_script.injected", script)?;
        self.publish(DaemonEvent::new(
            IpcEventKind::InputInjected,
            json!({
                "input_script_id": script.id.to_string(),
                "agent_id": script.target_agent_id.to_string(),
                "action_count": script.actions.len(),
            }),
        ));
        Ok(())
    }

    fn publish(&self, event: DaemonEvent) {
        let _ = self.events.send(event);
    }

    fn append_input_script_event(&self, kind: &str, script: &InputScript) -> Result<()> {
        let Some(event_log) = &self.event_log else {
            return Ok(());
        };
        let entry = match kind {
            agentmux_store::EVENT_INPUT_SCRIPT_CREATED => EventLogEntry::input_script_created(
                DateTimeUtc::now_utc(),
                script.id.clone(),
                script.target_agent_id.clone(),
                &script.reason,
                &script.safety,
                script.actions.len(),
                &script.actions,
            )?,
            agentmux_store::EVENT_INPUT_SCRIPT_INJECTED => EventLogEntry::input_script_injected(
                DateTimeUtc::now_utc(),
                script.id.clone(),
                script.target_agent_id.clone(),
                &script.reason,
                &script.safety,
                script.actions.len(),
                &script.actions,
            )?,
            _ => EventLogEntry::new(kind, DateTimeUtc::now_utc(), json!({}))?,
        };
        event_log.append(&entry)
    }
}

impl LiveAgentSession {
    fn metadata(metadata: RegisteredAgentSession) -> Self {
        Self {
            metadata,
            pty: None,
        }
    }
}

enum ServerFrame {
    Response(DaemonResponse),
    Event(DaemonEvent),
}

pub async fn serve(config: DaemonConfig, runtime: DaemonRuntime) -> Result<()> {
    let listener = bind_unix_listener(&config.socket_path)?;
    runtime.publish(DaemonEvent::new(
        IpcEventKind::DaemonStarted,
        json!({ "socket_path": config.socket_path }),
    ));

    let mut clients = JoinSet::new();
    loop {
        let (stream, _addr) = listener.accept().await.map_err(|error| {
            AgentmuxError::IpcError(format!("failed to accept daemon client: {error}"))
        })?;
        let runtime = runtime.clone();
        clients.spawn(async move {
            if let Err(error) = handle_client(stream, runtime).await {
                eprintln!("agentmux-daemon client error: {error}");
            }
        });
    }
}

fn bind_unix_listener(socket_path: &Path) -> Result<UnixListener> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            AgentmuxError::IpcError(format!(
                "failed to create socket directory '{}': {error}",
                parent.display()
            ))
        })?;
    }
    if socket_path.exists() {
        std::fs::remove_file(socket_path).map_err(|error| {
            AgentmuxError::IpcError(format!(
                "failed to remove stale socket '{}': {error}",
                socket_path.display()
            ))
        })?;
    }

    let listener = UnixListener::bind(socket_path).map_err(|error| {
        AgentmuxError::IpcError(format!(
            "failed to bind daemon socket '{}': {error}",
            socket_path.display()
        ))
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600)).map_err(
            |error| {
                AgentmuxError::IpcError(format!(
                    "failed to set socket permissions '{}': {error}",
                    socket_path.display()
                ))
            },
        )?;
    }

    Ok(listener)
}

async fn handle_client(stream: UnixStream, runtime: DaemonRuntime) -> Result<()> {
    let client_id = ClientSessionId::new();
    let (reader, writer) = stream.into_split();
    let mut reader = JsonlReader::new(BufReader::new(reader));
    let mut events = runtime.subscribe();
    let (frames, mut frame_receiver) = mpsc::channel::<ServerFrame>(32);

    let writer_task = tokio::spawn(async move {
        let mut writer = JsonlWriter::new(writer);
        while let Some(frame) = frame_receiver.recv().await {
            match frame {
                ServerFrame::Response(response) => writer.write(&response).await?,
                ServerFrame::Event(event) => writer.write(&event).await?,
            }
        }
        Result::<()>::Ok(())
    });

    let event_frames = frames.clone();
    let event_task = tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    if event_frames.send(ServerFrame::Event(event)).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    let hello = match reader.read::<ClientHello>().await? {
        Some(hello) => hello,
        None => {
            event_task.abort();
            drop(frames);
            return writer_task
                .await
                .map_err(|error| AgentmuxError::IpcError(error.to_string()))?;
        }
    };
    if let Some(error) = protocol_error(hello.protocol_compatibility()) {
        send_frame(
            &frames,
            ServerFrame::Response(DaemonResponse::error("hello", error)),
        )
        .await?;
        event_task.abort();
        drop(frames);
        return writer_task
            .await
            .map_err(|error| AgentmuxError::IpcError(error.to_string()))?;
    }

    runtime
        .state
        .write()
        .await
        .clients
        .insert(client_id.clone(), None);

    while let Some(request) = reader.read::<ClientRequest>().await? {
        let response = handle_request(&runtime, &client_id, request).await;
        send_frame(&frames, ServerFrame::Response(response)).await?;
    }

    runtime.detach_client(&client_id).await;
    event_task.abort();
    drop(frames);
    writer_task
        .await
        .map_err(|error| AgentmuxError::IpcError(error.to_string()))?
}

async fn send_frame(frames: &mpsc::Sender<ServerFrame>, frame: ServerFrame) -> Result<()> {
    frames
        .send(frame)
        .await
        .map_err(|_| AgentmuxError::IpcError("client writer task stopped".to_string()))
}

async fn handle_request(
    runtime: &DaemonRuntime,
    client_id: &ClientSessionId,
    request: ClientRequest,
) -> DaemonResponse {
    if let Some(error) = protocol_error(request.protocol_compatibility()) {
        return DaemonResponse::error(request.id, error);
    }

    match request.command {
        IpcCommand::DaemonStatus => DaemonResponse::ok(request.id, runtime.status_payload().await),
        IpcCommand::AgentSpawn => {
            let name = request
                .payload
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("agent")
                .to_string();
            let agent = match pty_spawn_spec_from_payload(&request.payload) {
                Ok(Some(spec)) => runtime.spawn_agent(name, spec).await,
                Ok(None) => Ok(runtime.register_agent(name).await),
                Err(error) => Err(error),
            };
            match agent {
                Ok(agent) => DaemonResponse::ok(
                    request.id,
                    json!({
                        "agent_id": agent.id.to_string(),
                        "name": agent.name,
                        "process_id": agent.process_id,
                    }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("AGENT_SPAWN_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::ClientAttach => {
            let Some(agent_id) = request
                .payload
                .get("agent_id")
                .and_then(|value| value.as_str())
                .and_then(parse_agent_session_id)
            else {
                return DaemonResponse::error(
                    request.id,
                    ErrorBody::new("INVALID_ATTACH_TARGET", "client.attach requires agent_id"),
                );
            };

            match runtime
                .attach_client(client_id.clone(), agent_id.clone())
                .await
            {
                Ok(()) => DaemonResponse::ok(
                    request.id,
                    json!({
                        "client_id": client_id.to_string(),
                        "agent_id": agent_id.to_string(),
                    }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("AGENT_NOT_FOUND", error.to_string())
                        .with_hint("call agent.spawn or choose an agent from daemon.status"),
                ),
            }
        }
        IpcCommand::ClientDetach => {
            let agent_id = runtime.detach_client(client_id).await;
            DaemonResponse::ok(
                request.id,
                json!({
                    "client_id": client_id.to_string(),
                    "agent_id": agent_id.map(|id| id.to_string()),
                }),
            )
        }
        IpcCommand::AgentSendInputScript => {
            let script = match serde_json::from_value::<InputScript>(request.payload) {
                Ok(script) => script,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new(
                            "INVALID_INPUT_SCRIPT",
                            format!("agent.send_input_script payload is invalid: {error}"),
                        ),
                    );
                }
            };
            match runtime.send_input_script(&script).await {
                Ok(()) => DaemonResponse::ok(
                    request.id,
                    json!({
                        "input_script_id": script.id.to_string(),
                        "agent_id": script.target_agent_id.to_string(),
                    }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("INPUT_SCRIPT_FAILED", error.to_string()),
                ),
            }
        }
        _ => DaemonResponse::error(
            request.id,
            ErrorBody::new(
                "COMMAND_NOT_IMPLEMENTED",
                "command is not implemented by the Phase 2 daemon listener",
            ),
        ),
    }
}

fn protocol_error(compatibility: ProtocolCompatibility) -> Option<ErrorBody> {
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

fn parse_agent_session_id(value: &str) -> Option<AgentSessionId> {
    let raw = value.strip_prefix(AgentSessionId::prefix())?;
    raw.parse::<ulid::Ulid>().ok().map(AgentSessionId)
}

fn pty_spawn_spec_from_payload(payload: &serde_json::Value) -> Result<Option<PtySpawnSpec>> {
    let Some(command) = payload.get("command").and_then(|value| value.as_str()) else {
        return Ok(None);
    };

    let args = payload
        .get("args")
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .map_err(|error| {
            AgentmuxError::UserError(format!("agent.spawn args must be strings: {error}"))
        })?
        .unwrap_or_default();
    let cwd = payload
        .get("cwd")
        .and_then(|value| value.as_str())
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir().map_err(|error| {
            AgentmuxError::Internal(format!("failed to resolve current directory: {error}"))
        })?);
    let env = payload
        .get("env")
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .map_err(|error| {
            AgentmuxError::UserError(format!("agent.spawn env must be a string map: {error}"))
        })?
        .unwrap_or_default();
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
        command: command.to_string(),
        args,
        cwd,
        env,
        size,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::time::Duration;

    use agentmux_agent::InputAction;
    use agentmux_agent::adapter::{InputPrecondition, InputSafety};
    use agentmux_core::InputScriptId;
    use agentmux_ipc::{IpcCommand, JsonlReader, JsonlWriter};
    use agentmux_store::EventLog;

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

        let (spawn_response, spawned_event) = read_response_and_event(&mut reader).await;
        assert!(spawn_response.ok);
        let agent_id = spawn_response.payload.unwrap()["agent_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(spawned_event.kind, IpcEventKind::AgentSpawned);

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
                "req_detach",
                IpcCommand::ClientDetach,
                json!({}),
            ))
            .await
            .unwrap();
        let (detach_response, detach_event) = read_response_and_event(&mut reader).await;
        assert!(detach_response.ok);
        assert_eq!(detach_event.kind, IpcEventKind::ClientDetached);

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

        let (spawn_response, _) = read_response_and_event(&mut reader).await;
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
}
