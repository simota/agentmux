//! Runtime pieces for the local agentmux daemon.
//!
//! The daemon keeps live agent/session state in memory and exposes it over
//! JSONL IPC on a Unix domain socket. Persistence and actual provider process
//! spawning are layered on later Phase 2 tasks.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agentmux_agent::adapter::InputSafety;
use agentmux_agent::{
    AgentResult, AgentResultParse, AgentResultStatus, AgentRouteIdentity, EncodedInputStep,
    InputAction, InputScript, StandardWorkflowState, WorkflowHandoffContext,
    advance_standard_workflow, default_claude_codex_team, encode_input_action,
    parse_agent_result_marker, plan_task_run, route_agent_result,
};
use agentmux_context::{
    ContextBroker, ContextItem, ContextPackRequest, MailboxConfig, NewContextItem,
};
use agentmux_core::{
    AgentProvider, AgentRole, AgentSessionId, AgentStatus, AgentmuxError, ApprovalId, ClientId,
    ClientSessionId, ContextItemId, ContextKind, ContextScope, ContextSource, DateTimeUtc,
    DeliveryMode, DeliveryStatus, InputScriptId, MessageId, Priority, ProjectId, TaskId,
    Visibility, WorktreeId, WorktreeStatus, error::Result,
};
use agentmux_ipc::{
    ClientHello, ClientRequest, DaemonEvent, DaemonResponse, ErrorBody, EventSubscribeFilter,
    IpcCommand, IpcEventKind, JsonlReader, JsonlWriter, ProtocolCompatibility,
};
use agentmux_message::{
    AgentDescriptor, AgentMessage, IdleDelivery, MessageBus, MessageKind, MessageSource,
    MessageTarget, NewAgentMessage, PreparedInjection, PromptContext, PromptContextItem,
};
use agentmux_policy::{ApprovalEvent, ApprovalQueue, ApprovalQueueError, ApprovalRequest};
use agentmux_pty::{CTRL_C, PtyHandle, PtyReadEvent, PtySpawnSpec};
use agentmux_store::{EventLog, EventLogEntry};
use agentmux_terminal::TerminalParser;
use agentmux_worktree::{
    CaptureDiff, CreateWorktree, MergeOutcome, TestCommand, TestRunStatus, Worktree,
    WorktreeManager,
};
use serde_json::json;
use tokio::io::BufReader;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{RwLock, broadcast, mpsc, watch};
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
    pub role: AgentRole,
    pub status: Option<AgentStatus>,
    pub process_id: Option<u32>,
    pub attached_clients: BTreeSet<ClientSessionId>,
}

impl RegisteredAgentSession {
    fn with_role(name: String, role: AgentRole, process_id: Option<u32>) -> Self {
        Self {
            id: AgentSessionId::new(),
            name,
            role,
            status: None,
            process_id,
            attached_clients: BTreeSet::new(),
        }
    }

    fn restored_with_role(id: AgentSessionId, name: String, role: AgentRole) -> Self {
        Self {
            id,
            name,
            role,
            status: None,
            process_id: None,
            attached_clients: BTreeSet::new(),
        }
    }
}

struct LiveAgentSession {
    metadata: RegisteredAgentSession,
    worktree_id: Option<WorktreeId>,
    pty: Option<Mutex<PtyHandle>>,
    terminal: Arc<Mutex<TerminalParser>>,
}

#[derive(Debug, Clone)]
struct ArenaCandidate {
    worktree_id: WorktreeId,
    agent_id: AgentSessionId,
    provider: String,
    diff_stat: Option<String>,
    test_status: Option<TestRunStatus>,
}

struct DaemonState {
    clients: BTreeMap<ClientSessionId, Option<AgentSessionId>>,
    agents: BTreeMap<AgentSessionId, LiveAgentSession>,
    worktrees: BTreeMap<WorktreeId, Worktree>,
    worktree_repo_roots: BTreeMap<WorktreeId, PathBuf>,
    arena_candidates: BTreeMap<WorktreeId, ArenaCandidate>,
    messages: MessageBus,
    contexts: ContextBroker,
    approvals: ApprovalQueue,
    layout_presets: BTreeMap<String, serde_json::Value>,
    default_project_id: ProjectId,
}

impl Default for DaemonState {
    fn default() -> Self {
        Self {
            clients: BTreeMap::new(),
            agents: BTreeMap::new(),
            worktrees: BTreeMap::new(),
            worktree_repo_roots: BTreeMap::new(),
            arena_candidates: BTreeMap::new(),
            messages: MessageBus::new(),
            contexts: ContextBroker::new(),
            approvals: ApprovalQueue::new(),
            layout_presets: BTreeMap::new(),
            default_project_id: ProjectId::new(),
        }
    }
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

    pub async fn recover_state_from_event_log(&self) -> Result<usize> {
        let Some(event_log) = &self.event_log else {
            return Ok(0);
        };
        let entries = event_log.read_entries()?;
        let Some(stopped) = entries
            .iter()
            .rev()
            .find(|entry| entry.kind == "daemon.stopped")
        else {
            return Ok(0);
        };
        let agents = stopped
            .payload
            .get("state")
            .and_then(|state| state.get("agents"))
            .and_then(|agents| agents.as_array())
            .ok_or_else(|| {
                AgentmuxError::StoreError(
                    "daemon.stopped recovery event is missing payload.state.agents".to_string(),
                )
            })?;

        let mut recovered = BTreeMap::new();
        let mut recovered_message_agents = Vec::new();
        for agent in agents {
            let id = agent
                .get("id")
                .and_then(|id| id.as_str())
                .ok_or_else(|| {
                    AgentmuxError::StoreError(
                        "daemon.stopped recovery agent is missing id".to_string(),
                    )
                })?
                .parse::<AgentSessionId>()
                .map_err(|error| {
                    AgentmuxError::StoreError(format!(
                        "daemon.stopped recovery agent has invalid id: {error}"
                    ))
                })?;
            let name = agent
                .get("name")
                .and_then(|name| name.as_str())
                .ok_or_else(|| {
                    AgentmuxError::StoreError(
                        "daemon.stopped recovery agent is missing name".to_string(),
                    )
                })?
                .to_string();
            let role = agent
                .get("role")
                .and_then(|role| role.as_str())
                .map(parse_agent_role)
                .transpose()?
                .unwrap_or_else(|| inferred_agent_role(&name));
            recovered_message_agents
                .push(AgentDescriptor::new(id.clone(), role.clone()).with_name(name.clone()));
            recovered.insert(
                id.clone(),
                LiveAgentSession::metadata(RegisteredAgentSession::restored_with_role(
                    id, name, role,
                )),
            );
        }

        let recovered_count = recovered.len();
        if recovered_count == 0 {
            return Ok(0);
        }
        let mut state = self.state.write().await;
        for agent in recovered_message_agents {
            state.messages.register_agent(agent);
        }
        state.agents.extend(recovered);
        Ok(recovered_count)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<DaemonEvent> {
        self.events.subscribe()
    }

    pub async fn register_agent(&self, name: String) -> RegisteredAgentSession {
        let role = inferred_agent_role(&name);
        self.register_agent_with_role(name, role).await
    }

    pub async fn register_agent_with_role(
        &self,
        name: String,
        role: AgentRole,
    ) -> RegisteredAgentSession {
        let agent = RegisteredAgentSession::with_role(name, role, None);
        let mut state = self.state.write().await;
        state
            .agents
            .insert(agent.id.clone(), LiveAgentSession::metadata(agent.clone()));
        state.messages.register_agent(
            AgentDescriptor::new(agent.id.clone(), agent.role.clone())
                .with_name(agent.name.clone()),
        );
        drop(state);
        self.publish(DaemonEvent::new(
            IpcEventKind::AgentSpawned,
            json!({
                "agent_id": agent.id.to_string(),
                "name": agent.name,
                "role": agent_role_label(&agent.role),
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
        let role = inferred_agent_role(&name);
        self.spawn_agent_with_role(name, role, spec).await
    }

    pub async fn spawn_agent_with_role(
        &self,
        name: String,
        role: AgentRole,
        spec: PtySpawnSpec,
    ) -> Result<RegisteredAgentSession> {
        self.spawn_agent_with_role_and_worktree(name, role, spec, None)
            .await
    }

    pub async fn spawn_agent_with_role_and_worktree(
        &self,
        name: String,
        role: AgentRole,
        mut spec: PtySpawnSpec,
        worktree_id: Option<WorktreeId>,
    ) -> Result<RegisteredAgentSession> {
        let mut agent = RegisteredAgentSession::with_role(name.clone(), role, None);
        spec.env
            .insert("AGENTMUX_AGENT_ID".to_string(), agent.id.to_string());
        spec.env
            .insert("AGENTMUX_AGENT_NAME".to_string(), agent.name.clone());
        spec.env.insert(
            "AGENTMUX_AGENT_ROLE".to_string(),
            agent_role_label(&agent.role).to_string(),
        );
        let terminal = Arc::new(Mutex::new(TerminalParser::new(
            spec.size.rows,
            spec.size.cols,
        )));
        let pty = PtyHandle::spawn(spec)?;
        let read_loop = pty.spawn_read_loop(16)?;
        agent.process_id = pty.process_id();
        self.spawn_pty_output_forwarder(
            agent.id.clone(),
            name.clone(),
            terminal.clone(),
            read_loop,
        );
        let mut state = self.state.write().await;
        state.agents.insert(
            agent.id.clone(),
            LiveAgentSession {
                metadata: agent.clone(),
                worktree_id: worktree_id.clone(),
                pty: Some(Mutex::new(pty)),
                terminal,
            },
        );
        state.messages.register_agent(
            AgentDescriptor::new(agent.id.clone(), agent.role.clone())
                .with_name(agent.name.clone()),
        );
        drop(state);
        self.publish(DaemonEvent::new(
            IpcEventKind::AgentSpawned,
            json!({
                "agent_id": agent.id.to_string(),
                "name": agent.name,
                "role": agent_role_label(&agent.role),
                "process_id": agent.process_id,
                "worktree_id": worktree_id.as_ref().map(ToString::to_string),
            }),
        ));
        Ok(agent)
    }

    fn spawn_pty_output_forwarder(
        &self,
        agent_id: AgentSessionId,
        agent_name: String,
        terminal: Arc<Mutex<TerminalParser>>,
        mut read_loop: agentmux_pty::PtyReadLoop,
    ) {
        let runtime = self.clone();
        let events = self.events.clone();
        tokio::spawn(async move {
            let mut output_tail = String::new();
            let mut result_persisted = false;
            while let Some(event) = read_loop.recv().await {
                match event {
                    PtyReadEvent::Output(bytes) => {
                        if let Ok(mut terminal) = terminal.lock() {
                            terminal.advance(&bytes);
                        }
                        let text = String::from_utf8_lossy(&bytes).into_owned();
                        output_tail.push_str(&text);
                        trim_result_detection_tail(&mut output_tail);
                        let _ = events.send(DaemonEvent::new(
                            IpcEventKind::PtyOutputChunk,
                            json!({
                                "agent_id": agent_id.to_string(),
                                "bytes": bytes,
                                "text": text,
                            }),
                        ));
                        if !result_persisted {
                            match runtime
                                .persist_live_agent_result(
                                    Some(&agent_id),
                                    &agent_name,
                                    &output_tail,
                                )
                                .await
                            {
                                Ok(true) => result_persisted = true,
                                Ok(false) => {}
                                Err(error) => {
                                    let _ = events.send(DaemonEvent::new(
                                        IpcEventKind::Error,
                                        json!({
                                            "agent_id": agent_id.to_string(),
                                            "signal": "agent_result_persist_failed",
                                            "error": error.to_string(),
                                        }),
                                    ));
                                    result_persisted = true;
                                }
                            }
                        }
                    }
                    PtyReadEvent::Eof => break,
                    PtyReadEvent::Error(error) => {
                        let _ = events.send(DaemonEvent::new(
                            IpcEventKind::AgentStatusSignal,
                            json!({
                                "agent_id": agent_id.to_string(),
                                "signal": "pty_read_error",
                                "error": error,
                            }),
                        ));
                        break;
                    }
                }
            }
        });
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

    pub async fn stop_agent(&self, agent_id: &AgentSessionId) -> Result<RegisteredAgentSession> {
        let mut state = self.state.write().await;
        let Some(agent) = state.agents.remove(agent_id) else {
            return Err(AgentmuxError::UserError(format!(
                "unknown agent session '{agent_id}'"
            )));
        };
        drop(state);

        if let Some(pty) = &agent.pty {
            let mut pty = pty.lock().map_err(|_| {
                AgentmuxError::Internal(format!("PTY lock for agent '{agent_id}' is poisoned"))
            })?;
            pty.terminate()?;
        }

        self.publish(DaemonEvent::new(
            IpcEventKind::AgentExited,
            json!({
                "agent_id": agent.metadata.id.to_string(),
                "name": agent.metadata.name,
            }),
        ));
        Ok(agent.metadata)
    }

    pub async fn interrupt_agent(&self, agent_id: &AgentSessionId) -> Result<()> {
        let state = self.state.read().await;
        let Some(agent) = state.agents.get(agent_id) else {
            return Err(AgentmuxError::UserError(format!(
                "unknown agent session '{agent_id}'"
            )));
        };
        let Some(pty) = &agent.pty else {
            return Err(AgentmuxError::UserError(format!(
                "agent session '{agent_id}' has no live PTY"
            )));
        };
        let mut pty = pty.lock().map_err(|_| {
            AgentmuxError::Internal(format!("PTY lock for agent '{agent_id}' is poisoned"))
        })?;
        pty.write_bytes(CTRL_C)
    }

    pub async fn resize_agent(
        &self,
        agent_id: &AgentSessionId,
        size: agentmux_pty::TerminalSize,
    ) -> Result<()> {
        if size.rows == 0 || size.cols == 0 {
            return Err(AgentmuxError::UserError(format!(
                "agent.resize requires non-zero size, got {}x{}",
                size.rows, size.cols
            )));
        }

        let state = self.state.read().await;
        let Some(agent) = state.agents.get(agent_id) else {
            return Err(AgentmuxError::UserError(format!(
                "unknown agent session '{agent_id}'"
            )));
        };
        let Some(pty) = &agent.pty else {
            return Err(AgentmuxError::UserError(format!(
                "agent session '{agent_id}' has no live PTY"
            )));
        };
        let terminal = agent.terminal.clone();

        {
            let pty = pty.lock().map_err(|_| {
                AgentmuxError::Internal(format!("PTY lock for agent '{agent_id}' is poisoned"))
            })?;
            pty.resize(size)?;
        }

        let mut terminal = terminal.lock().map_err(|_| {
            AgentmuxError::Internal(format!(
                "terminal buffer lock for agent '{agent_id}' is poisoned"
            ))
        })?;
        terminal.resize(size.rows, size.cols);
        Ok(())
    }

    pub async fn save_layout(&self, name: String, layout: serde_json::Value) -> Result<()> {
        if name.trim().is_empty() {
            return Err(AgentmuxError::UserError(
                "layout.set requires non-empty name".to_string(),
            ));
        }
        let mut state = self.state.write().await;
        state.layout_presets.insert(name, layout);
        Ok(())
    }

    pub async fn get_layout(&self, name: &str) -> Option<serde_json::Value> {
        let state = self.state.read().await;
        state.layout_presets.get(name).cloned()
    }

    pub async fn list_layouts(&self) -> Vec<String> {
        let state = self.state.read().await;
        state.layout_presets.keys().cloned().collect()
    }

    pub async fn create_message(&self, input: NewAgentMessage) -> Result<AgentMessage> {
        let mut state = self.state.write().await;
        let message = state.messages.create_message(input)?;
        drop(state);

        self.append_message_event(agentmux_store::EVENT_MESSAGE_CREATED, &message)?;
        self.publish(DaemonEvent::new(
            IpcEventKind::MessageCreated,
            message_payload(&message),
        ));
        Ok(message)
    }

    pub async fn list_messages(&self) -> Vec<AgentMessage> {
        let state = self.state.read().await;
        state
            .messages
            .list_messages()
            .into_iter()
            .cloned()
            .collect()
    }

    pub async fn get_message(&self, id: &MessageId) -> Option<AgentMessage> {
        let state = self.state.read().await;
        state.messages.get_message(id).cloned()
    }

    pub async fn inject_message(&self, id: &MessageId) -> Result<AgentMessage> {
        let now = DateTimeUtc::now_utc();
        let prepared = self.prepare_manual_message_injection(id, now).await?;
        tokio::time::sleep(message_inject_send_delay()).await;
        let write_result = self.write_prepared_message_injection(&prepared).await;
        self.finish_and_emit_message_injection(&prepared.message_id, now, write_result)
            .await
    }

    pub async fn inject_message_to_agent(
        &self,
        id: &MessageId,
        agent_id: &AgentSessionId,
    ) -> Result<AgentMessage> {
        let now = DateTimeUtc::now_utc();
        let prepared = self
            .prepare_manual_message_injection_for_agent(id, agent_id, now)
            .await?;
        tokio::time::sleep(message_inject_send_delay()).await;
        let write_result = self.write_prepared_message_injection(&prepared).await;
        self.finish_and_emit_message_injection(&prepared.message_id, now, write_result)
            .await
    }

    async fn finish_and_emit_message_injection(
        &self,
        id: &MessageId,
        now: DateTimeUtc,
        write_result: Result<()>,
    ) -> Result<AgentMessage> {
        let finished = self.finish_message_injection(id, now, write_result).await;
        match finished {
            Ok(message) => {
                self.append_message_event(agentmux_store::EVENT_MESSAGE_INJECTED, &message)?;
                self.publish(DaemonEvent::new(
                    IpcEventKind::MessageDelivered,
                    message_payload(&message),
                ));
                Ok(message)
            }
            Err(error) => {
                if let Some(message) = self.get_message(id).await {
                    let _ =
                        self.append_message_event(agentmux_store::EVENT_MESSAGE_INJECTED, &message);
                    self.publish(DaemonEvent::new(
                        IpcEventKind::Error,
                        json!({
                            "message_id": id.to_string(),
                            "delivery_status": message.delivery_status,
                            "error": error.to_string(),
                        }),
                    ));
                }
                Err(error)
            }
        }
    }

    pub async fn deliver_idle_messages_for_agent(
        &self,
        agent_id: &AgentSessionId,
        status: AgentStatus,
    ) -> Result<Option<AgentMessage>> {
        let now = DateTimeUtc::now_utc();
        let delivery = {
            let state = self.state.read().await;
            let Some(message) = state.messages.next_inject_when_idle_message(agent_id)? else {
                return Ok(None);
            };
            let (provider, context) = delivery_render_inputs(&state, agent_id, message)?;
            drop(state);
            let mut state = self.state.write().await;
            state
                .messages
                .prepare_next_inject_when_idle(agent_id, &status, provider, &context, now)?
        };

        let IdleDelivery::Ready(prepared) = delivery else {
            return Ok(None);
        };

        let write_result = self.write_prepared_message_injection(&prepared).await;
        let message = self
            .finish_and_emit_message_injection(&prepared.message_id, now, write_result)
            .await?;
        Ok(Some(message))
    }

    pub async fn apply_agent_status_signal(
        &self,
        agent_id: &AgentSessionId,
        status: AgentStatus,
        evidence: impl Into<String>,
    ) -> Result<Option<AgentMessage>> {
        {
            let mut state = self.state.write().await;
            let Some(agent) = state.agents.get_mut(agent_id) else {
                return Err(AgentmuxError::UserError(format!(
                    "unknown agent session '{agent_id}'"
                )));
            };
            agent.metadata.status = Some(status.clone());
        }

        let evidence = evidence.into();
        self.publish(DaemonEvent::new(
            IpcEventKind::AgentStatusSignal,
            json!({
                "agent_id": agent_id.to_string(),
                "status": status.clone(),
                "evidence": evidence,
            }),
        ));
        self.publish(DaemonEvent::new(
            IpcEventKind::AgentStatusChanged,
            json!({
                "agent_id": agent_id.to_string(),
                "status": status.clone(),
            }),
        ));

        self.deliver_idle_messages_for_agent(agent_id, status).await
    }

    async fn prepare_manual_message_injection(
        &self,
        id: &MessageId,
        now: DateTimeUtc,
    ) -> Result<PreparedInjection> {
        let mut state = self.state.write().await;
        let message = state
            .messages
            .get_message(id)
            .cloned()
            .ok_or_else(|| AgentmuxError::UserError(format!("unknown message '{id}'")))?;
        let recipients = state.messages.resolve_target(&message.to)?;
        let [agent_id] = recipients.as_slice() else {
            return Err(AgentmuxError::UserError(format!(
                "message target resolved to {} agents; manual injection requires exactly one",
                recipients.len()
            )));
        };
        let (provider, context) = delivery_render_inputs(&state, agent_id, &message)?;
        let prompt = agentmux_message::render_prompt(&message, provider, &context);
        state
            .messages
            .update_delivery_status(id, DeliveryStatus::Injecting, now)?;

        Ok(PreparedInjection {
            message_id: id.clone(),
            agent_id: agent_id.clone(),
            prompt,
        })
    }

    async fn prepare_manual_message_injection_for_agent(
        &self,
        id: &MessageId,
        agent_id: &AgentSessionId,
        now: DateTimeUtc,
    ) -> Result<PreparedInjection> {
        let mut state = self.state.write().await;
        let message = state
            .messages
            .get_message(id)
            .cloned()
            .ok_or_else(|| AgentmuxError::UserError(format!("unknown message '{id}'")))?;
        if !state.agents.contains_key(agent_id) {
            return Err(AgentmuxError::UserError(format!(
                "unknown agent session '{agent_id}'"
            )));
        }
        let (provider, context) = delivery_render_inputs(&state, agent_id, &message)?;
        let prompt = agentmux_message::render_prompt(&message, provider, &context);
        state
            .messages
            .update_delivery_status(id, DeliveryStatus::Injecting, now)?;

        Ok(PreparedInjection {
            message_id: id.clone(),
            agent_id: agent_id.clone(),
            prompt,
        })
    }

    async fn write_prepared_message_injection(&self, prepared: &PreparedInjection) -> Result<()> {
        let script = message_input_script(prepared);
        self.append_input_script_event(agentmux_store::EVENT_INPUT_SCRIPT_CREATED, &script)?;

        {
            let state = self.state.read().await;
            let Some(agent) = state.agents.get(&prepared.agent_id) else {
                return Err(AgentmuxError::UserError(format!(
                    "unknown agent session '{}'",
                    prepared.agent_id
                )));
            };
            let Some(pty) = &agent.pty else {
                return Err(AgentmuxError::UserError(format!(
                    "agent session '{}' has no live PTY",
                    prepared.agent_id
                )));
            };
            let mut pty = pty.lock().map_err(|_| {
                AgentmuxError::Internal(format!(
                    "PTY lock for agent '{}' is poisoned",
                    prepared.agent_id
                ))
            })?;
            for action in &script.actions {
                match encode_input_action(action)? {
                    EncodedInputStep::Bytes(bytes) => pty.write_bytes(&bytes)?,
                    EncodedInputStep::Wait(duration) => std::thread::sleep(duration),
                }
            }
        }

        self.append_input_script_event(agentmux_store::EVENT_INPUT_SCRIPT_INJECTED, &script)?;
        self.publish(DaemonEvent::new(
            IpcEventKind::InputInjected,
            json!({
                "input_script_id": script.id.to_string(),
                "agent_id": script.target_agent_id.to_string(),
                "message_id": prepared.message_id.to_string(),
                "action_count": script.actions.len(),
            }),
        ));
        Ok(())
    }

    async fn finish_message_injection(
        &self,
        id: &MessageId,
        now: DateTimeUtc,
        write_result: Result<()>,
    ) -> Result<AgentMessage> {
        let mut state = self.state.write().await;
        match write_result {
            Ok(()) => state.messages.mark_message_injected(id, now)?,
            Err(error) => {
                state.messages.mark_message_injection_failed(id, now)?;
                return Err(error);
            }
        }
        state
            .messages
            .get_message(id)
            .cloned()
            .ok_or_else(|| AgentmuxError::UserError(format!("unknown message '{id}'")))
    }

    pub async fn create_context(&self, input: NewContextItem) -> Result<ContextItem> {
        let mut state = self.state.write().await;
        let item = state.contexts.create_item(input)?;
        drop(state);

        self.append_context_created_event(&item)?;
        self.publish(DaemonEvent::new(
            IpcEventKind::ContextCreated,
            context_payload(&item),
        ));
        Ok(item)
    }

    pub async fn list_contexts(&self) -> Vec<ContextItem> {
        let state = self.state.read().await;
        state
            .contexts
            .list_items(&state.default_project_id)
            .into_iter()
            .cloned()
            .collect()
    }

    pub async fn get_context(&self, id: &ContextItemId) -> Option<ContextItem> {
        let state = self.state.read().await;
        state.contexts.get_item(id).cloned()
    }

    pub async fn search_contexts(&self, query: &str) -> Vec<ContextItem> {
        let query = query.to_ascii_lowercase();
        self.list_contexts()
            .await
            .into_iter()
            .filter(|item| {
                item.title.to_ascii_lowercase().contains(&query)
                    || item.body.to_ascii_lowercase().contains(&query)
                    || item
                        .tags
                        .iter()
                        .any(|tag| tag.to_ascii_lowercase().contains(&query))
            })
            .collect()
    }

    pub async fn attach_context_to_message(
        &self,
        context_id: &ContextItemId,
        message_id: &MessageId,
    ) -> Result<AgentMessage> {
        let mut state = self.state.write().await;
        if state.contexts.get_item(context_id).is_none() {
            return Err(AgentmuxError::UserError(format!(
                "unknown context item '{context_id}'"
            )));
        }
        state
            .messages
            .attach_context_ref(message_id, context_id.clone())
    }

    pub async fn inject_context(
        &self,
        context_id: &ContextItemId,
        agent_id: &AgentSessionId,
    ) -> Result<ContextItem> {
        let state = self.state.read().await;
        if !state.agents.contains_key(agent_id) {
            return Err(AgentmuxError::UserError(format!(
                "unknown agent session '{agent_id}'"
            )));
        }
        let item = state
            .contexts
            .get_item(context_id)
            .cloned()
            .ok_or_else(|| {
                AgentmuxError::UserError(format!("unknown context item '{context_id}'"))
            })?;
        drop(state);

        self.publish(DaemonEvent::new(
            IpcEventKind::ContextInjected,
            json!({
                "context_id": context_id.to_string(),
                "agent_id": agent_id.to_string(),
            }),
        ));
        Ok(item)
    }

    pub async fn export_contexts(&self, output: &Path) -> Result<usize> {
        let contexts = self.list_contexts().await;
        let payload = json!({
            "contexts": contexts.iter().map(context_payload).collect::<Vec<_>>(),
        });
        let bytes = serde_json::to_vec_pretty(&payload).map_err(json_error)?;
        std::fs::write(output, bytes).map_err(|error| {
            AgentmuxError::StoreError(format!(
                "failed to export contexts to '{}': {error}",
                output.display()
            ))
        })?;
        Ok(contexts.len())
    }

    pub async fn register_worktree(&self, worktree: Worktree) {
        let mut state = self.state.write().await;
        state.worktrees.insert(worktree.id.clone(), worktree);
    }

    async fn register_worktree_with_repo_root(&self, worktree: Worktree, repo_root: PathBuf) {
        let mut state = self.state.write().await;
        state
            .worktree_repo_roots
            .insert(worktree.id.clone(), repo_root);
        state.worktrees.insert(worktree.id.clone(), worktree);
    }

    pub async fn list_worktrees(&self) -> Vec<Worktree> {
        let state = self.state.read().await;
        state.worktrees.values().cloned().collect()
    }

    pub async fn capture_worktree_diff(
        &self,
        worktree_id: &WorktreeId,
    ) -> Result<serde_json::Value> {
        let worktree = self.worktree_by_id(worktree_id).await?;
        let captured = agentmux_worktree::WorktreeManager::new(
            worktree.project_id.clone(),
            worktree.path.clone(),
            worktree.path.join(".agentmux/worktrees"),
        )?
        .capture_diff_artifact(
            CaptureDiff {
                task_id: worktree.task_id.clone(),
                agent_name: worktree.branch_name.clone(),
                worktree_path: worktree.path.clone(),
                base_branch: worktree.base_branch.clone(),
            },
            worktree.path.join(".agentmux/artifacts"),
        )?;

        self.publish(DaemonEvent::new(
            IpcEventKind::WorktreeDiffCaptured,
            json!({
                "worktree_id": worktree.id.to_string(),
                "artifact_id": captured.patch.id.to_string(),
                "stat": captured.stat,
            }),
        ));
        self.record_arena_diff(&worktree.id, captured.stat.clone())
            .await;

        Ok(json!({
            "worktree": worktree_payload(&worktree),
            "artifact": artifact_payload(
                captured.patch.id.to_string(),
                captured.patch.path.display().to_string(),
                captured.patch.title,
            ),
            "stat": captured.stat,
        }))
    }

    pub async fn run_worktree_test(
        &self,
        worktree_id: &WorktreeId,
        command: TestCommand,
    ) -> Result<serde_json::Value> {
        let mut worktree = self.worktree_by_id(worktree_id).await?;
        self.set_worktree_status(worktree_id, WorktreeStatus::Testing)
            .await?;
        let result = agentmux_worktree::WorktreeManager::new(
            worktree.project_id.clone(),
            worktree.path.clone(),
            worktree.path.join(".agentmux/worktrees"),
        )?
        .run_test_command_artifact(
            worktree.task_id.clone(),
            &worktree.branch_name,
            &worktree.path,
            command,
            worktree.path.join(".agentmux/artifacts"),
        );

        match result {
            Ok(test_run) => {
                worktree.status = if test_run.status == TestRunStatus::Passed {
                    WorktreeStatus::ReviewReady
                } else {
                    WorktreeStatus::Failed
                };
                self.set_worktree_status(worktree_id, worktree.status.clone())
                    .await?;
                self.publish(DaemonEvent::new(
                    IpcEventKind::ArtifactCreated,
                    json!({
                        "worktree_id": worktree.id.to_string(),
                        "artifact_id": test_run.artifact.id.to_string(),
                    }),
                ));
                self.record_arena_test(&worktree.id, test_run.status).await;
                self.publish(DaemonEvent::new(
                    IpcEventKind::WorktreeTestCompleted,
                    json!({
                        "worktree_id": worktree.id.to_string(),
                        "status": test_run.status,
                        "command": test_run.command,
                        "exit_code": test_run.exit_code,
                    }),
                ));
                Ok(json!({
                    "worktree": worktree_payload(&worktree),
                    "test": {
                        "status": test_run.status,
                        "command": test_run.command,
                        "exit_code": test_run.exit_code,
                        "artifact": artifact_payload(
                            test_run.artifact.id.to_string(),
                            test_run.artifact.path.display().to_string(),
                            test_run.artifact.title,
                        ),
                    },
                }))
            }
            Err(error) => {
                let _ = self
                    .set_worktree_status(worktree_id, WorktreeStatus::Failed)
                    .await;
                Err(error)
            }
        }
    }

    async fn promote_worktree(&self, worktree_id: &WorktreeId) -> Result<Worktree> {
        self.ensure_arena_candidate_ready(worktree_id).await?;
        let worktree = self.worktree_by_id(worktree_id).await?;
        let repo_root = self.repo_root_for_worktree(worktree_id, &worktree).await;
        let manager = WorktreeManager::new(
            worktree.project_id.clone(),
            repo_root.clone(),
            repo_root.join(".agentmux/worktrees"),
        )?;
        match manager.merge_to_integration_branch(&worktree, "agentmux/integration")? {
            MergeOutcome::Conflict => {
                self.set_worktree_status(worktree_id, WorktreeStatus::Conflicted)
                    .await?;
                Err(AgentmuxError::UserError(format!(
                    "worktree '{worktree_id}' merge conflicted and was aborted"
                )))
            }
            MergeOutcome::Clean | MergeOutcome::Dirty => {
                self.set_worktree_status(worktree_id, WorktreeStatus::Promoted)
                    .await
            }
        }
    }

    pub async fn archive_worktree(&self, worktree_id: &WorktreeId) -> Result<Worktree> {
        self.set_worktree_status(worktree_id, WorktreeStatus::Archived)
            .await
    }

    pub async fn submit_approval_request(&self, request: ApprovalRequest) -> ApprovalRequest {
        let mut state = self.state.write().await;
        let gate = state
            .approvals
            .submit(agentmux_policy::PolicyDecision::Ask, request);
        let agentmux_policy::ApprovalGate::Queued(request, event) = gate else {
            unreachable!("PolicyDecision::Ask always queues an approval request");
        };
        drop(state);

        let _ = self.append_approval_event(&event);
        self.publish(approval_daemon_event(&event));
        request
    }

    async fn request_worktree_adoption(&self, worktree_id: WorktreeId) -> Result<ApprovalRequest> {
        let _ = self.worktree_by_id(&worktree_id).await?;
        self.ensure_arena_candidate_ready(&worktree_id).await?;
        {
            let state = self.state.read().await;
            if let Some(existing) = state
                .approvals
                .pending()
                .into_iter()
                .find(|request| request.worktree_id.is_some())
            {
                return Err(AgentmuxError::UserError(format!(
                    "worktree adoption approval '{}' is already pending",
                    existing.id
                )));
            }
        }
        let request = self
            .submit_approval_request(ApprovalRequest::worktree_adopt(worktree_id.clone()))
            .await;
        self.publish(DaemonEvent::new(
            IpcEventKind::WorktreeAdoptRequested,
            json!({
                "worktree_id": worktree_id.to_string(),
                "approval_id": request.id.to_string(),
            }),
        ));
        Ok(request)
    }

    pub async fn list_approvals(&self) -> Vec<ApprovalRequest> {
        let state = self.state.read().await;
        state.approvals.pending().into_iter().cloned().collect()
    }

    pub async fn approve_approval(
        &self,
        approval_id: &ApprovalId,
    ) -> std::result::Result<ApprovalRequest, ApprovalQueueError> {
        self.decide_approval(approval_id, true).await
    }

    pub async fn reject_approval(
        &self,
        approval_id: &ApprovalId,
    ) -> std::result::Result<ApprovalRequest, ApprovalQueueError> {
        self.decide_approval(approval_id, false).await
    }

    async fn decide_approval(
        &self,
        approval_id: &ApprovalId,
        approved: bool,
    ) -> std::result::Result<ApprovalRequest, ApprovalQueueError> {
        let mut state = self.state.write().await;
        let event = if approved {
            state.approvals.approve(approval_id)?
        } else {
            state.approvals.reject(approval_id)?
        };
        let request = state
            .approvals
            .get(approval_id)
            .expect("decided approval remains in queue")
            .clone();
        drop(state);

        let _ = self.append_approval_event(&event);
        self.publish(approval_daemon_event(&event));
        if let Some(worktree_id) = request.worktree_id.clone() {
            let runtime = self.clone();
            tokio::spawn(async move {
                if approved {
                    if let Err(error) = runtime.promote_worktree(&worktree_id).await {
                        runtime.publish(DaemonEvent::new(
                            IpcEventKind::Error,
                            json!({
                                "worktree_id": worktree_id.to_string(),
                                "signal": "worktree_promote_failed",
                                "error": error.to_string(),
                            }),
                        ));
                    }
                } else if let Err(error) = runtime.archive_worktree(&worktree_id).await {
                    runtime.publish(DaemonEvent::new(
                        IpcEventKind::Error,
                        json!({
                            "worktree_id": worktree_id.to_string(),
                            "signal": "worktree_archive_failed",
                            "error": error.to_string(),
                        }),
                    ));
                }
            });
        }
        Ok(request)
    }

    async fn ensure_arena_candidate_ready(&self, worktree_id: &WorktreeId) -> Result<()> {
        let state = self.state.read().await;
        let Some(candidate) = state.arena_candidates.get(worktree_id) else {
            return Err(AgentmuxError::UserError(format!(
                "worktree '{worktree_id}' is not a registered arena candidate"
            )));
        };
        if candidate.diff_stat.is_none() {
            return Err(AgentmuxError::UserError(format!(
                "worktree '{worktree_id}' adoption requires captured diff"
            )));
        }
        if candidate.test_status != Some(TestRunStatus::Passed) {
            return Err(AgentmuxError::UserError(format!(
                "worktree '{worktree_id}' adoption requires passed tests"
            )));
        }
        Ok(())
    }

    async fn worktree_by_id(&self, worktree_id: &WorktreeId) -> Result<Worktree> {
        let state = self.state.read().await;
        state
            .worktrees
            .get(worktree_id)
            .cloned()
            .ok_or_else(|| AgentmuxError::UserError(format!("unknown worktree '{worktree_id}'")))
    }

    async fn repo_root_for_worktree(
        &self,
        worktree_id: &WorktreeId,
        worktree: &Worktree,
    ) -> PathBuf {
        let state = self.state.read().await;
        state
            .worktree_repo_roots
            .get(worktree_id)
            .cloned()
            .unwrap_or_else(|| worktree.path.clone())
    }

    async fn record_arena_diff(&self, worktree_id: &WorktreeId, stat: String) {
        let mut state = self.state.write().await;
        if let Some(candidate) = state.arena_candidates.get_mut(worktree_id) {
            candidate.diff_stat = Some(stat);
        }
    }

    async fn record_arena_test(&self, worktree_id: &WorktreeId, status: TestRunStatus) {
        let mut state = self.state.write().await;
        if let Some(candidate) = state.arena_candidates.get_mut(worktree_id) {
            candidate.test_status = Some(status);
        }
    }

    async fn set_worktree_status(
        &self,
        worktree_id: &WorktreeId,
        status: WorktreeStatus,
    ) -> Result<Worktree> {
        let mut state = self.state.write().await;
        let Some(worktree) = state.worktrees.get_mut(worktree_id) else {
            return Err(AgentmuxError::UserError(format!(
                "unknown worktree '{worktree_id}'"
            )));
        };
        worktree.status = status;
        Ok(worktree.clone())
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

    pub async fn snapshot_agent(&self, agent_id: &AgentSessionId) -> Result<serde_json::Value> {
        let state = self.state.read().await;
        let Some(agent) = state.agents.get(agent_id) else {
            return Err(AgentmuxError::UserError(format!(
                "unknown agent session '{agent_id}'"
            )));
        };
        let metadata = agent.metadata.clone();
        let terminal = agent.terminal.clone();
        drop(state);

        let terminal = terminal.lock().map_err(|_| {
            AgentmuxError::Internal(format!(
                "terminal buffer lock for agent '{agent_id}' is poisoned"
            ))
        })?;
        let grid = terminal.grid();
        let lines = (0..grid.rows())
            .filter_map(|row| grid.line_text(row))
            .collect::<Vec<_>>();

        Ok(json!({
            "agent_id": metadata.id.to_string(),
            "name": metadata.name,
            "role": agent_role_label(&metadata.role),
            "process_id": metadata.process_id,
            "rows": grid.rows(),
            "cols": grid.cols(),
            "lines": lines,
        }))
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
                    "role": agent_role_label(&agent.metadata.role),
                    "status": agent.metadata.status.as_ref().map(agent_status_label),
                    "input_ready": agent_input_ready(agent),
                    "process_id": agent.metadata.process_id,
                    "has_process": agent.pty.is_some(),
                    "worktree_id": agent.worktree_id.as_ref().map(ToString::to_string),
                    "attached_clients": agent
                        .metadata
                        .attached_clients
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>(),
                })
            })
            .collect();
        let arena_candidates = state
            .arena_candidates
            .values()
            .map(|candidate| {
                json!({
                    "worktree_id": candidate.worktree_id.to_string(),
                    "agent_id": candidate.agent_id.to_string(),
                    "provider": candidate.provider,
                    "diff_stat": candidate.diff_stat,
                    "test_status": candidate.test_status,
                })
            })
            .collect::<Vec<_>>();

        json!({
            "protocol_version": agentmux_ipc::PROTOCOL_VERSION,
            "client_count": state.clients.len(),
            "agent_count": state.agents.len(),
            "agents": agents,
            "arena_candidates": arena_candidates,
        })
    }

    pub async fn run_task_with_shell_stubs(
        &self,
        body: String,
        team_name: String,
        project_path: PathBuf,
    ) -> Result<serde_json::Value> {
        if body.trim().is_empty() {
            return Err(AgentmuxError::UserError(
                "task.run requires non-empty body".to_string(),
            ));
        }

        let task_id = TaskId::new();
        let team = default_claude_codex_team();
        let plan = plan_task_run(task_id.clone(), &body, team.clone())?;
        self.register_task_team_message_agents(&task_id, &team)
            .await;
        let bootstrap_message = self.persist_orchestrator_message(&plan.bootstrap).await?;
        let context = WorkflowHandoffContext {
            task_title: body.trim().to_string(),
            worktree_path: project_path.display().to_string(),
            test_command: "cargo test --workspace".to_string(),
            diff_path: Some(
                project_path
                    .join(".agentmux/artifacts/diff.patch")
                    .display()
                    .to_string(),
            ),
            test_log_path: Some(
                project_path
                    .join(".agentmux/artifacts/test.log")
                    .display()
                    .to_string(),
            ),
            task_brief_path: Some(
                project_path
                    .join(".agentmux/inbox/planner/task-brief.md")
                    .display()
                    .to_string(),
            ),
            candidate_worktrees: vec![project_path.display().to_string()],
        };
        let mut state = StandardWorkflowState::new(task_id.clone());
        let mut handoffs = vec![message_payload(&bootstrap_message)];
        let mut shell_processes = Vec::new();

        for (name, role) in [
            ("planner", AgentRole::Planner),
            ("impl-codex", AgentRole::Implementer),
            ("tester", AgentRole::Tester),
            ("reviewer", AgentRole::Reviewer),
        ] {
            let output = run_shell_stub_agent(name)?;
            let result = parse_shell_stub_result(name, &output.stdout)?;
            shell_processes.push(json!({
                "agent": name,
                "exit_code": output.exit_code,
                "stdout": output.stdout,
            }));
            let advanced = advance_standard_workflow(
                state,
                &AgentRouteIdentity {
                    name: name.to_string(),
                    role,
                },
                &team,
                result,
                &context,
            )?;
            state = advanced.state;
            for outgoing in &advanced.outgoing {
                let message = self.persist_orchestrator_message(outgoing).await?;
                handoffs.push(message_payload(&message));
            }
            if let Some(summary) = advanced.final_summary {
                let payload = json!({
                    "task_id": task_id.to_string(),
                    "team": team_name,
                    "runner": "shell-stub",
                    "project_path": project_path.display().to_string(),
                    "status": "completed",
                    "stage": format!("{:?}", state.stage),
                    "handoffs": handoffs,
                    "shell_processes": shell_processes,
                    "final_summary": summary.render_markdown(),
                    "recommended_next_action": summary.recommended_next_action,
                });
                self.append_daemon_lifecycle_event("task.completed", payload.clone())?;
                return Ok(payload);
            }
        }

        Err(AgentmuxError::OrchestratorError(
            "shell stub task workflow ended without final summary".to_string(),
        ))
    }

    pub async fn run_task_with_arena(
        &self,
        body: String,
        providers: Vec<String>,
        project_path: PathBuf,
        base_branch: String,
    ) -> Result<serde_json::Value> {
        if body.trim().is_empty() {
            return Err(AgentmuxError::UserError(
                "task.run requires non-empty body".to_string(),
            ));
        }
        if providers.is_empty() {
            return Err(AgentmuxError::UserError(
                "arena task.run requires at least one provider".to_string(),
            ));
        }
        let mut provider_labels = BTreeSet::new();
        for provider in &providers {
            let label = slug_label(provider);
            if !provider_labels.insert(label.clone()) {
                return Err(AgentmuxError::UserError(format!(
                    "arena provider label '{label}' is duplicated"
                )));
            }
        }

        let task_id = TaskId::new();
        let project_id = {
            let state = self.state.read().await;
            state.default_project_id.clone()
        };
        let manager = WorktreeManager::new(
            project_id,
            project_path.clone(),
            project_path.join(".agentmux/worktrees"),
        )?;
        let task_slug = body.trim().to_string();
        let mut candidates = Vec::new();

        for provider in providers {
            let agent_name = format!("impl-{}", slug_label(&provider));
            let worktree = manager.create_worktree(CreateWorktree {
                task_id: task_id.clone(),
                task_slug: task_slug.clone(),
                agent_name: agent_name.clone(),
                owner_agent_id: None,
                base_branch: base_branch.clone(),
            })?;
            self.register_worktree_with_repo_root(worktree.clone(), project_path.clone())
                .await;

            let mut env: BTreeMap<String, String> = std::env::vars().collect();
            env.insert("TERM".to_string(), "xterm-256color".to_string());
            let spec = PtySpawnSpec {
                command: provider_command(&provider),
                args: default_provider_args(Some(provider.as_str())),
                cwd: worktree.path.clone(),
                env,
                size: Default::default(),
            };
            let agent = self
                .spawn_agent_with_role_and_worktree(
                    agent_name,
                    AgentRole::Implementer,
                    spec,
                    Some(worktree.id.clone()),
                )
                .await?;
            {
                let mut state = self.state.write().await;
                if let Some(stored) = state.worktrees.get_mut(&worktree.id) {
                    stored.owner_agent_id = Some(agent.id.clone());
                }
                state.arena_candidates.insert(
                    worktree.id.clone(),
                    ArenaCandidate {
                        worktree_id: worktree.id.clone(),
                        agent_id: agent.id.clone(),
                        provider: provider.clone(),
                        diff_stat: None,
                        test_status: None,
                    },
                );
            }
            self.publish(DaemonEvent::new(
                IpcEventKind::WorktreeCreated,
                json!({
                    "worktree": worktree_payload(&worktree),
                    "agent_id": agent.id.to_string(),
                    "provider": provider,
                }),
            ));
            candidates.push(json!({
                "worktree": worktree_payload(&worktree),
                "agent_id": agent.id.to_string(),
                "name": agent.name,
            }));
        }

        Ok(json!({
            "task_id": task_id.to_string(),
            "runner": "arena",
            "project_path": project_path.display().to_string(),
            "base_branch": base_branch,
            "candidates": candidates,
        }))
    }

    async fn register_task_team_message_agents(
        &self,
        task_id: &TaskId,
        team: &agentmux_agent::TeamTemplate,
    ) {
        let mut state = self.state.write().await;
        for agent in &team.agents {
            if agent.name == "impl-claude" {
                continue;
            }
            let agent_id = AgentSessionId::new();
            state.messages.register_agent(
                AgentDescriptor::new(agent_id, agent.role.clone())
                    .with_name(agent.name.clone())
                    .with_task_id(task_id.clone())
                    .with_team(team.name.clone()),
            );
        }
    }

    async fn persist_orchestrator_message(
        &self,
        message: &agentmux_agent::OrchestratorMessage,
    ) -> Result<AgentMessage> {
        self.create_message(NewAgentMessage {
            task_id: message.task_id.clone(),
            from: message.from.clone(),
            to: message.to.clone(),
            kind: message.kind.clone(),
            priority: message.priority.clone(),
            body: message.body.clone(),
            context_refs: message.context_refs.clone(),
            artifact_refs: message.artifact_refs.clone(),
            delivery_mode: message.delivery_mode.clone(),
            requires_response: message.requires_response,
        })
        .await
    }

    pub async fn persist_agent_result_messages(
        &self,
        agent: &AgentRouteIdentity,
        task_id: TaskId,
        team: &agentmux_agent::TeamTemplate,
        result: AgentResult,
    ) -> Result<Vec<AgentMessage>> {
        let routed = route_agent_result(agent, task_id, team, result)?;
        let mut messages = Vec::with_capacity(routed.outgoing.len());
        for outgoing in &routed.outgoing {
            messages.push(self.persist_orchestrator_message(outgoing).await?);
        }
        Ok(messages)
    }

    async fn persist_live_agent_result(
        &self,
        agent_id: Option<&AgentSessionId>,
        agent_name: &str,
        output_tail: &str,
    ) -> Result<bool> {
        let result = match parse_agent_result_marker(output_tail) {
            AgentResultParse::Found(parsed) => parsed.result,
            AgentResultParse::NotFound | AgentResultParse::NeedsStatusProbe(_) => {
                return Ok(false);
            }
        };
        let completed = result.status == AgentResultStatus::Completed;
        let task_id = TaskId::new();
        let team = default_claude_codex_team();
        let agent = AgentRouteIdentity {
            name: agent_name.to_string(),
            role: inferred_agent_role(agent_name),
        };
        self.persist_agent_result_messages(&agent, task_id, &team, result)
            .await?;
        if completed
            && let Some(worktree_id) = self.resolve_agent_worktree(agent_id, agent_name).await
            && self.is_arena_candidate(&worktree_id).await
        {
            self.capture_and_test_arena_candidate(worktree_id);
        }
        Ok(true)
    }

    async fn resolve_agent_worktree(
        &self,
        agent_id: Option<&AgentSessionId>,
        agent_name: &str,
    ) -> Option<WorktreeId> {
        let state = self.state.read().await;
        if let Some(agent_id) = agent_id
            && let Some(agent) = state.agents.get(agent_id)
        {
            return agent.worktree_id.clone();
        }
        state
            .agents
            .values()
            .find(|agent| agent.metadata.name == agent_name)
            .and_then(|agent| agent.worktree_id.clone())
    }

    async fn is_arena_candidate(&self, worktree_id: &WorktreeId) -> bool {
        let state = self.state.read().await;
        state.arena_candidates.contains_key(worktree_id)
    }

    fn capture_and_test_arena_candidate(&self, worktree_id: WorktreeId) {
        let runtime = self.clone();
        tokio::spawn(async move {
            if let Err(error) = runtime.capture_worktree_diff(&worktree_id).await {
                runtime.publish(DaemonEvent::new(
                    IpcEventKind::Error,
                    json!({
                        "worktree_id": worktree_id.to_string(),
                        "signal": "worktree_diff_capture_failed",
                        "error": error.to_string(),
                    }),
                ));
                return;
            }
            if let Err(error) = runtime
                .run_worktree_test(
                    &worktree_id,
                    TestCommand {
                        name: "default".to_string(),
                        command: "cargo test".to_string(),
                    },
                )
                .await
            {
                runtime.publish(DaemonEvent::new(
                    IpcEventKind::Error,
                    json!({
                        "worktree_id": worktree_id.to_string(),
                        "signal": "worktree_test_failed",
                        "error": error.to_string(),
                    }),
                ));
            }
        });
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

    fn append_daemon_lifecycle_event(&self, kind: &str, payload: serde_json::Value) -> Result<()> {
        let Some(event_log) = &self.event_log else {
            return Ok(());
        };
        event_log.append(&EventLogEntry::new(kind, DateTimeUtc::now_utc(), payload)?)
    }

    fn append_message_event(&self, kind: &str, message: &AgentMessage) -> Result<()> {
        let Some(event_log) = &self.event_log else {
            return Ok(());
        };
        let entry = match kind {
            agentmux_store::EVENT_MESSAGE_CREATED => EventLogEntry::message_created(
                DateTimeUtc::now_utc(),
                message.id.clone(),
                message.task_id.clone(),
                agentmux_store::MessageEventPayload {
                    from: serde_json::to_string(&message.from).map_err(json_error)?,
                    to: serde_json::to_string(&message.to).map_err(json_error)?,
                    kind: serde_json::to_string(&message.kind).map_err(json_error)?,
                    delivery_mode: serde_json::to_string(&message.delivery_mode)
                        .map_err(json_error)?,
                    delivery_status: message.delivery_status.clone(),
                    context_refs: message.context_refs.clone(),
                    artifact_refs: message.artifact_refs.clone(),
                },
            )?,
            agentmux_store::EVENT_MESSAGE_INJECTED => match &message.to {
                MessageTarget::Agent(agent_id) => EventLogEntry::message_injected(
                    DateTimeUtc::now_utc(),
                    message.id.clone(),
                    message.task_id.clone(),
                    agent_id.clone(),
                    message.delivery_status.clone(),
                )?,
                _ => EventLogEntry::new(
                    agentmux_store::EVENT_MESSAGE_INJECTED,
                    DateTimeUtc::now_utc(),
                    json!({
                        "message_id": message.id.to_string(),
                        "delivery_status": message.delivery_status,
                    }),
                )?,
            },
            _ => EventLogEntry::new(kind, DateTimeUtc::now_utc(), json!({}))?,
        };
        event_log.append(&entry)
    }

    fn append_context_created_event(&self, item: &ContextItem) -> Result<()> {
        let Some(event_log) = &self.event_log else {
            return Ok(());
        };
        let entry = EventLogEntry::context_created(
            DateTimeUtc::now_utc(),
            item.project_id.clone(),
            item.task_id.clone(),
            item.id.clone(),
            serde_json::to_string(&item.kind).map_err(json_error)?,
            serde_json::to_string(&item.source).map_err(json_error)?,
        )?;
        event_log.append(&entry)
    }

    fn append_approval_event(&self, event: &ApprovalEvent) -> Result<()> {
        let Some(event_log) = &self.event_log else {
            return Ok(());
        };
        let kind = match event {
            ApprovalEvent::ApprovalCreated { .. } => "approval.created",
            ApprovalEvent::ApprovalDecided { .. } => "approval.decided",
            ApprovalEvent::PolicyDenied { .. } => "policy.denied",
        };
        let payload = serde_json::to_value(event).map_err(json_error)?;
        event_log.append(&EventLogEntry::new(kind, DateTimeUtc::now_utc(), payload)?)
    }
}

impl LiveAgentSession {
    fn metadata(metadata: RegisteredAgentSession) -> Self {
        Self {
            metadata,
            worktree_id: None,
            pty: None,
            terminal: Arc::new(Mutex::new(TerminalParser::default())),
        }
    }
}

enum ServerFrame {
    Response(DaemonResponse),
    Event(DaemonEvent),
}

pub async fn serve(config: DaemonConfig, runtime: DaemonRuntime) -> Result<()> {
    serve_until_shutdown(config, runtime, shutdown_signal()).await
}

pub async fn serve_until_shutdown(
    config: DaemonConfig,
    runtime: DaemonRuntime,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    let listener = bind_unix_listener(&config.socket_path)?;
    let socket_path = config.socket_path.clone();
    let restored_agent_count = runtime.recover_state_from_event_log().await?;
    let started_payload =
        json!({ "socket_path": socket_path, "restored_agent_count": restored_agent_count });
    runtime.append_daemon_lifecycle_event("daemon.started", started_payload.clone())?;
    runtime.publish(DaemonEvent::new(
        IpcEventKind::DaemonStarted,
        started_payload,
    ));

    let mut clients = JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            biased;
            () = &mut shutdown => break,
            accepted = listener.accept() => {
                let (stream, _addr) = accepted.map_err(|error| {
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
    }

    drop(listener);
    clients.abort_all();
    while let Some(joined) = clients.join_next().await {
        if let Err(error) = joined
            && !error.is_cancelled()
        {
            return Err(AgentmuxError::IpcError(format!(
                "daemon client task failed during shutdown: {error}"
            )));
        }
    }

    finish_shutdown(&runtime, &socket_path).await?;
    Ok(())
}

async fn finish_shutdown(runtime: &DaemonRuntime, socket_path: &Path) -> Result<()> {
    let status = runtime.status_payload().await;
    let stopped_payload = json!({ "socket_path": socket_path, "state": status });
    runtime.append_daemon_lifecycle_event("daemon.stopped", stopped_payload.clone())?;
    runtime.publish(DaemonEvent::new(
        IpcEventKind::DaemonStopped,
        stopped_payload,
    ));
    remove_socket_file(socket_path)
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut interrupt = signal(SignalKind::interrupt()).expect("SIGINT handler installs");
        let mut terminate = signal(SignalKind::terminate()).expect("SIGTERM handler installs");
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
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

fn remove_socket_file(socket_path: &Path) -> Result<()> {
    if !socket_path.exists() {
        return Ok(());
    }
    std::fs::remove_file(socket_path).map_err(|error| {
        AgentmuxError::IpcError(format!(
            "failed to remove daemon socket '{}': {error}",
            socket_path.display()
        ))
    })
}

pub async fn handle_client(stream: UnixStream, runtime: DaemonRuntime) -> Result<()> {
    let client_id = ClientSessionId::new();
    let (reader, writer) = stream.into_split();
    let mut reader = JsonlReader::new(BufReader::new(reader));
    let mut events = runtime.subscribe();
    let (frames, mut frame_receiver) = mpsc::channel::<ServerFrame>(32);
    let (attached, mut attached_events) = watch::channel(false);
    let event_filter = Arc::new(Mutex::new(None::<EventSubscribeFilter>));

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
    let event_task_filter = Arc::clone(&event_filter);
    let event_task_runtime = runtime.clone();
    let event_task = tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    if !*attached_events.borrow_and_update() {
                        continue;
                    }
                    let filter = event_task_filter
                        .lock()
                        .map(|filter| filter.clone())
                        .unwrap_or(None);
                    let should_forward =
                        should_forward_event(&event_task_runtime, filter.as_ref(), &event).await;
                    if !should_forward {
                        continue;
                    }
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
        let command = request.command.clone();
        if matches!(command, IpcCommand::ClientAttach | IpcCommand::AgentFocus) {
            let _ = attached.send(true);
        } else if command == IpcCommand::ClientDetach {
            let _ = attached.send(false);
        }
        let response = handle_request(&runtime, &client_id, &event_filter, request).await;
        let attach_failed =
            matches!(command, IpcCommand::ClientAttach | IpcCommand::AgentFocus) && !response.ok;
        send_frame(&frames, ServerFrame::Response(response)).await?;
        if attach_failed {
            let _ = attached.send(false);
        }
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

async fn should_forward_event(
    runtime: &DaemonRuntime,
    filter: Option<&EventSubscribeFilter>,
    event: &DaemonEvent,
) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    if !filter.kinds.is_empty()
        && !filter
            .kinds
            .iter()
            .any(|kind| kind == event_kind_label(&event.kind))
    {
        return false;
    }
    if let Some(task_id) = filter.task_id.as_deref()
        && payload_string_field(&event.payload, "task_id").as_deref() != Some(task_id)
    {
        return false;
    }
    if !filter.roles.is_empty()
        && event_role(runtime, event)
            .await
            .as_deref()
            .is_none_or(|role| !filter.roles.iter().any(|expected| expected == role))
    {
        return false;
    }
    true
}

async fn event_role(runtime: &DaemonRuntime, event: &DaemonEvent) -> Option<String> {
    if let Some(role) = payload_string_field(&event.payload, "role") {
        return Some(role);
    }
    let agent_id = payload_string_field(&event.payload, "agent_id")?
        .parse::<AgentSessionId>()
        .ok()?;
    let state = runtime.state.read().await;
    state
        .agents
        .get(&agent_id)
        .map(|agent| agent_role_label(&agent.metadata.role))
}

fn payload_string_field(payload: &serde_json::Value, field: &str) -> Option<String> {
    payload
        .get(field)
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned)
}

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

async fn handle_request(
    runtime: &DaemonRuntime,
    client_id: &ClientSessionId,
    event_filter: &Arc<Mutex<Option<EventSubscribeFilter>>>,
    request: ClientRequest,
) -> DaemonResponse {
    if let Some(error) = protocol_error(request.protocol_compatibility()) {
        return DaemonResponse::error(request.id, error);
    }

    match request.command {
        IpcCommand::DaemonStatus => DaemonResponse::ok(request.id, runtime.status_payload().await),
        IpcCommand::EventSubscribe => {
            let filter =
                match serde_json::from_value::<EventSubscribeFilter>(request.payload.clone()) {
                    Ok(filter) => filter,
                    Err(error) => {
                        return DaemonResponse::error(
                            request.id,
                            ErrorBody::new(
                                "INVALID_EVENT_SUBSCRIBE_FILTER",
                                format!("event.subscribe filter is invalid: {error}"),
                            ),
                        );
                    }
                };
            match event_filter.lock() {
                Ok(mut current) => {
                    *current = Some(filter);
                    DaemonResponse::ok(request.id, json!({ "subscribed": true }))
                }
                Err(_) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new(
                        "EVENT_SUBSCRIBE_FAILED",
                        "event.subscribe filter lock is poisoned",
                    ),
                ),
            }
        }
        IpcCommand::TaskRun => {
            let (body, team, project_path, runner) = match task_run_payload(&request.payload) {
                Ok(payload) => payload,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_TASK_RUN", error.to_string()),
                    );
                }
            };
            if runner == "arena" {
                let providers = match arena_providers_payload(&request.payload) {
                    Ok(providers) => providers,
                    Err(error) => {
                        return DaemonResponse::error(
                            request.id,
                            ErrorBody::new("INVALID_ARENA_PROVIDERS", error.to_string()),
                        );
                    }
                };
                let base_branch = request
                    .payload
                    .get("base_branch")
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or("main")
                    .to_string();
                return match runtime
                    .run_task_with_arena(body, providers, project_path, base_branch)
                    .await
                {
                    Ok(payload) => DaemonResponse::ok(request.id, payload),
                    Err(error) => DaemonResponse::error(
                        request.id,
                        ErrorBody::new("TASK_RUN_FAILED", error.to_string()),
                    ),
                };
            }
            if runner != "shell-stub" {
                return DaemonResponse::error(
                    request.id,
                    ErrorBody::new(
                        "TASK_RUNNER_UNAVAILABLE",
                        format!("unsupported task.run runner '{runner}'"),
                    )
                    .with_hint("use runner=shell-stub for deterministic test execution"),
                );
            }

            match runtime
                .run_task_with_shell_stubs(body, team, project_path)
                .await
            {
                Ok(payload) => DaemonResponse::ok(request.id, payload),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("TASK_RUN_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::AgentSpawn => {
            let name = request
                .payload
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("agent")
                .to_string();
            let role = match request
                .payload
                .get("role")
                .and_then(|value| value.as_str())
                .map(parse_agent_role)
                .transpose()
            {
                Ok(Some(role)) => role,
                Ok(None) => inferred_agent_role(&name),
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("AGENT_SPAWN_FAILED", error.to_string()),
                    );
                }
            };
            let agent = match pty_spawn_spec_from_payload(&request.payload) {
                Ok(Some(spec)) => runtime.spawn_agent_with_role(name, role, spec).await,
                Ok(None) => Ok(runtime.register_agent_with_role(name, role).await),
                Err(error) => Err(error),
            };
            match agent {
                Ok(agent) => DaemonResponse::ok(
                    request.id,
                    json!({
                        "agent_id": agent.id.to_string(),
                        "name": agent.name,
                        "role": agent_role_label(&agent.role),
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
        IpcCommand::AgentStop => {
            let agent_id = match agent_id_payload(&request.payload, "agent.stop") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_AGENT_ID", error.to_string()),
                    );
                }
            };
            match runtime.stop_agent(&agent_id).await {
                Ok(agent) => DaemonResponse::ok(
                    request.id,
                    json!({
                        "agent_id": agent.id.to_string(),
                        "name": agent.name,
                        "stopped": true,
                    }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("AGENT_STOP_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::AgentFocus => {
            let agent_id = match agent_id_payload(&request.payload, "agent.focus") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_AGENT_ID", error.to_string()),
                    );
                }
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
                        "focused": true,
                    }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("AGENT_FOCUS_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::AgentInterrupt => {
            let agent_id = match agent_id_payload(&request.payload, "agent.interrupt") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_AGENT_ID", error.to_string()),
                    );
                }
            };
            match runtime.interrupt_agent(&agent_id).await {
                Ok(()) => DaemonResponse::ok(
                    request.id,
                    json!({ "agent_id": agent_id.to_string(), "interrupted": true }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("AGENT_INTERRUPT_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::AgentResize => {
            let agent_id = match agent_id_payload(&request.payload, "agent.resize") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_AGENT_ID", error.to_string()),
                    );
                }
            };
            let size = match terminal_size_payload(&request.payload, "agent.resize") {
                Ok(size) => size,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_TERMINAL_SIZE", error.to_string()),
                    );
                }
            };
            match runtime.resize_agent(&agent_id, size).await {
                Ok(()) => DaemonResponse::ok(
                    request.id,
                    json!({
                        "agent_id": agent_id.to_string(),
                        "rows": size.rows,
                        "cols": size.cols,
                        "resized": true,
                    }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("AGENT_RESIZE_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::AgentSnapshot => {
            let agent_id = match agent_id_payload(&request.payload, "agent.snapshot") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_AGENT_ID", error.to_string()),
                    );
                }
            };
            match runtime.snapshot_agent(&agent_id).await {
                Ok(payload) => DaemonResponse::ok(request.id, payload),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("AGENT_SNAPSHOT_FAILED", error.to_string()),
                ),
            }
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
        IpcCommand::MessageCreate => {
            let input = match message_create_payload(&request.payload) {
                Ok(input) => input,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_MESSAGE_CREATE", error.to_string()),
                    );
                }
            };
            match runtime.create_message(input).await {
                Ok(message) => DaemonResponse::ok(request.id, message_payload(&message)),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("MESSAGE_CREATE_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::MessageList => {
            let messages = runtime.list_messages().await;
            DaemonResponse::ok(
                request.id,
                json!({
                    "messages": messages.iter().map(message_payload).collect::<Vec<_>>(),
                }),
            )
        }
        IpcCommand::MessageShow => {
            let Some(message_id) = request
                .payload
                .get("message_id")
                .and_then(|value| value.as_str())
                .and_then(parse_message_id)
            else {
                return DaemonResponse::error(
                    request.id,
                    ErrorBody::new("INVALID_MESSAGE_ID", "message.show requires message_id"),
                );
            };
            match runtime.get_message(&message_id).await {
                Some(message) => DaemonResponse::ok(request.id, message_payload(&message)),
                None => DaemonResponse::error(
                    request.id,
                    ErrorBody::new(
                        "MESSAGE_NOT_FOUND",
                        format!("unknown message '{message_id}'"),
                    ),
                ),
            }
        }
        IpcCommand::MessageInject => {
            let Some(message_id) = request
                .payload
                .get("message_id")
                .and_then(|value| value.as_str())
                .and_then(parse_message_id)
            else {
                return DaemonResponse::error(
                    request.id,
                    ErrorBody::new("INVALID_MESSAGE_ID", "message.inject requires message_id"),
                );
            };
            let agent_id = request
                .payload
                .get("agent_id")
                .and_then(|value| value.as_str())
                .map(str::parse::<AgentSessionId>)
                .transpose();
            let agent_id = match agent_id {
                Ok(agent_id) => agent_id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_AGENT_ID", error.to_string()),
                    );
                }
            };
            let result = match agent_id {
                Some(agent_id) => {
                    runtime
                        .inject_message_to_agent(&message_id, &agent_id)
                        .await
                }
                None => runtime.inject_message(&message_id).await,
            };
            match result {
                Ok(message) => DaemonResponse::ok(request.id, message_payload(&message)),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("MESSAGE_INJECT_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::ContextCreate => {
            let input = match context_create_payload(&request.payload, runtime).await {
                Ok(input) => input,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_CONTEXT_CREATE", error.to_string()),
                    );
                }
            };
            match runtime.create_context(input).await {
                Ok(item) => DaemonResponse::ok(request.id, context_payload(&item)),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("CONTEXT_CREATE_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::ContextSearch => match context_search_payload(&request.payload) {
            Ok(ContextLookup::List) => {
                let contexts = runtime.list_contexts().await;
                DaemonResponse::ok(
                    request.id,
                    json!({
                        "contexts": contexts.iter().map(context_payload).collect::<Vec<_>>(),
                    }),
                )
            }
            Ok(ContextLookup::Show(context_id)) => match runtime.get_context(&context_id).await {
                Some(item) => DaemonResponse::ok(request.id, context_payload(&item)),
                None => DaemonResponse::error(
                    request.id,
                    ErrorBody::new(
                        "CONTEXT_NOT_FOUND",
                        format!("unknown context item '{context_id}'"),
                    ),
                ),
            },
            Ok(ContextLookup::Search(query)) => {
                let contexts = runtime.search_contexts(&query).await;
                DaemonResponse::ok(
                    request.id,
                    json!({
                        "contexts": contexts.iter().map(context_payload).collect::<Vec<_>>(),
                    }),
                )
            }
            Err(error) => DaemonResponse::error(
                request.id,
                ErrorBody::new("INVALID_CONTEXT_SEARCH", error.to_string()),
            ),
        },
        IpcCommand::ContextAttach => {
            let (context_id, message_id) = match context_attach_payload(&request.payload) {
                Ok(ids) => ids,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_CONTEXT_ATTACH", error.to_string()),
                    );
                }
            };
            match runtime
                .attach_context_to_message(&context_id, &message_id)
                .await
            {
                Ok(message) => DaemonResponse::ok(request.id, message_payload(&message)),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("CONTEXT_ATTACH_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::ContextInject => {
            let (context_id, agent_id) = match context_inject_payload(&request.payload) {
                Ok(ids) => ids,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_CONTEXT_INJECT", error.to_string()),
                    );
                }
            };
            match runtime.inject_context(&context_id, &agent_id).await {
                Ok(item) => DaemonResponse::ok(
                    request.id,
                    json!({
                        "context": context_payload(&item),
                        "agent_id": agent_id.to_string(),
                    }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("CONTEXT_INJECT_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::ContextExport => {
            let Some(output) = request
                .payload
                .get("output")
                .and_then(|value| value.as_str())
            else {
                return DaemonResponse::error(
                    request.id,
                    ErrorBody::new("INVALID_CONTEXT_EXPORT", "context.export requires output"),
                );
            };
            match runtime.export_contexts(Path::new(output)).await {
                Ok(count) => DaemonResponse::ok(
                    request.id,
                    json!({ "output": output, "context_count": count }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("CONTEXT_EXPORT_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::WorktreeList => {
            let worktrees = runtime.list_worktrees().await;
            DaemonResponse::ok(
                request.id,
                json!({
                    "worktrees": worktrees.iter().map(worktree_payload).collect::<Vec<_>>(),
                }),
            )
        }
        IpcCommand::WorktreeDiff => {
            let worktree_id = match worktree_id_payload(&request.payload, "worktree.diff") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_WORKTREE_ID", error.to_string()),
                    );
                }
            };
            match runtime.capture_worktree_diff(&worktree_id).await {
                Ok(payload) => DaemonResponse::ok(request.id, payload),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("WORKTREE_DIFF_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::WorktreeTest => {
            let worktree_id = match worktree_id_payload(&request.payload, "worktree.test") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_WORKTREE_ID", error.to_string()),
                    );
                }
            };
            let command = worktree_test_command_payload(&request.payload);
            match runtime.run_worktree_test(&worktree_id, command).await {
                Ok(payload) => DaemonResponse::ok(request.id, payload),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("WORKTREE_TEST_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::WorktreePromote => {
            let worktree_id = match worktree_id_payload(&request.payload, "worktree.promote") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_WORKTREE_ID", error.to_string()),
                    );
                }
            };
            match runtime.request_worktree_adoption(worktree_id).await {
                Ok(approval) => DaemonResponse::ok(request.id, approval_payload(&approval)),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("WORKTREE_PROMOTE_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::WorktreeArchive => {
            let worktree_id = match worktree_id_payload(&request.payload, "worktree.archive") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_WORKTREE_ID", error.to_string()),
                    );
                }
            };
            match runtime.archive_worktree(&worktree_id).await {
                Ok(worktree) => DaemonResponse::ok(request.id, worktree_payload(&worktree)),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("WORKTREE_ARCHIVE_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::WorktreeAdopt => {
            let worktree_id = match worktree_id_payload(&request.payload, "worktree.adopt") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_WORKTREE_ID", error.to_string()),
                    );
                }
            };
            match runtime.request_worktree_adoption(worktree_id).await {
                Ok(approval) => DaemonResponse::ok(request.id, approval_payload(&approval)),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("WORKTREE_ADOPT_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::ApprovalList => {
            let approvals = runtime.list_approvals().await;
            DaemonResponse::ok(
                request.id,
                json!({
                    "approvals": approvals.iter().map(approval_payload).collect::<Vec<_>>(),
                }),
            )
        }
        IpcCommand::ApprovalApprove => {
            let approval_id = match approval_id_payload(&request.payload, "approval.approve") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_APPROVAL_ID", error.to_string()),
                    );
                }
            };
            match runtime.approve_approval(&approval_id).await {
                Ok(approval) => DaemonResponse::ok(request.id, approval_payload(&approval)),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("APPROVAL_DECISION_FAILED", approval_queue_error(error)),
                ),
            }
        }
        IpcCommand::ApprovalReject => {
            let approval_id = match approval_id_payload(&request.payload, "approval.reject") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_APPROVAL_ID", error.to_string()),
                    );
                }
            };
            match runtime.reject_approval(&approval_id).await {
                Ok(approval) => DaemonResponse::ok(request.id, approval_payload(&approval)),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("APPROVAL_DECISION_FAILED", approval_queue_error(error)),
                ),
            }
        }
        IpcCommand::LayoutSet => {
            let name = match required_string(&request.payload, "name", "layout.set") {
                Ok(name) => name.to_string(),
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_LAYOUT_SET", error.to_string()),
                    );
                }
            };
            let layout = request
                .payload
                .get("layout")
                .cloned()
                .unwrap_or_else(|| json!({ "name": name }));
            match runtime.save_layout(name.clone(), layout.clone()).await {
                Ok(()) => DaemonResponse::ok(
                    request.id,
                    json!({ "name": name, "layout": layout, "saved": true }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("LAYOUT_SET_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::LayoutGet => {
            let Some(name) = request.payload.get("name").and_then(|value| value.as_str()) else {
                let layouts = runtime.list_layouts().await;
                return DaemonResponse::ok(request.id, json!({ "layouts": layouts }));
            };
            match runtime.get_layout(name).await {
                Some(layout) => {
                    DaemonResponse::ok(request.id, json!({ "name": name, "layout": layout }))
                }
                None => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("LAYOUT_NOT_FOUND", format!("unknown layout '{name}'")),
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

fn task_run_payload(payload: &serde_json::Value) -> Result<(String, String, PathBuf, String)> {
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

fn arena_providers_payload(payload: &serde_json::Value) -> Result<Vec<String>> {
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

#[derive(Debug)]
struct ShellStubOutput {
    stdout: String,
    exit_code: Option<i32>,
}

fn run_shell_stub_agent(agent_name: &str) -> Result<ShellStubOutput> {
    let result = match agent_name {
        "planner" => json!({
            "status": "completed",
            "summary": "Assign the deterministic shell-stub implementation.",
            "changed_files": [],
            "messages": [],
            "context_updates": [],
            "needs": [],
            "next": "impl-codex",
            "recommendation": "continue",
            "risk": "low",
        }),
        "impl-codex" => json!({
            "status": "completed",
            "summary": "Shell stub implemented the requested change.",
            "changed_files": ["src/lib.rs"],
            "messages": [],
            "context_updates": [],
            "needs": [],
            "next": "tester",
            "recommendation": "continue",
            "risk": "low",
        }),
        "tester" => json!({
            "status": "completed",
            "summary": "cargo test --workspace passed in shell stub.",
            "changed_files": [],
            "messages": [],
            "context_updates": [],
            "needs": [],
            "next": "reviewer",
            "recommendation": "continue",
            "risk": "low",
        }),
        "reviewer" => json!({
            "status": "completed",
            "summary": "Shell stub reviewer approved the candidate.",
            "changed_files": [],
            "messages": [],
            "context_updates": [],
            "needs": [],
            "next": "none",
            "recommendation": "approve",
            "risk": "low",
        }),
        other => {
            return Err(AgentmuxError::OrchestratorError(format!(
                "unknown shell stub agent '{other}'"
            )));
        }
    };
    let marker = format!("AGENTMUX_RESULT: {result}");
    let script = format!("cat <<'AGENTMUX_EOF'\n{marker}\nAGENTMUX_EOF\n");
    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .output()
        .map_err(|error| {
            AgentmuxError::OrchestratorError(format!(
                "failed to run shell stub agent '{agent_name}': {error}"
            ))
        })?;
    if !output.status.success() {
        return Err(AgentmuxError::OrchestratorError(format!(
            "shell stub agent '{agent_name}' exited with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(ShellStubOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        exit_code: output.status.code(),
    })
}

fn parse_shell_stub_result(agent_name: &str, stdout: &str) -> Result<AgentResult> {
    match parse_agent_result_marker(stdout) {
        AgentResultParse::Found(parsed) => Ok(parsed.result),
        AgentResultParse::NotFound => Err(AgentmuxError::OrchestratorError(format!(
            "shell stub agent '{agent_name}' did not emit AGENTMUX_RESULT"
        ))),
        AgentResultParse::NeedsStatusProbe(probe) => {
            Err(AgentmuxError::OrchestratorError(format!(
                "shell stub agent '{agent_name}' emitted invalid AGENTMUX_RESULT: {}",
                probe.reason
            )))
        }
    }
}

fn delivery_render_inputs(
    state: &DaemonState,
    agent_id: &AgentSessionId,
    message: &AgentMessage,
) -> Result<(AgentProvider, PromptContext)> {
    let Some(agent) = state.agents.get(agent_id) else {
        return Err(AgentmuxError::UserError(format!(
            "unknown agent session '{agent_id}'"
        )));
    };
    let provider = provider_for_agent_name(&agent.metadata.name);
    let project_root = std::env::current_dir().map_err(|error| {
        AgentmuxError::Internal(format!("failed to resolve current directory: {error}"))
    })?;
    let pack = state.contexts.select_pack_with_mailbox(
        ContextPackRequest {
            project_id: state.default_project_id.clone(),
            task_id: message.task_id.clone(),
            attached_context_ids: message.context_refs.clone(),
            max_inline_chars: 2048,
        },
        MailboxConfig {
            project_root,
            agent_name: agent.metadata.name.clone(),
        },
    )?;
    let context = message_prompt_context_from_pack(pack);
    Ok((provider, context))
}

fn message_prompt_context_from_pack(pack: agentmux_context::ContextPack) -> PromptContext {
    PromptContext {
        inline_items: pack
            .inline_items
            .into_iter()
            .map(|item| PromptContextItem {
                title: item.title,
                body: item.body,
            })
            .collect(),
        mailbox_paths: pack.mailbox_files,
    }
}

fn message_input_script(prepared: &PreparedInjection) -> InputScript {
    InputScript {
        id: InputScriptId::new(),
        target_agent_id: prepared.agent_id.clone(),
        reason: format!("message.inject {}", prepared.message_id),
        preconditions: Vec::new(),
        actions: vec![
            InputAction::PasteText(prepared.prompt.clone()),
            InputAction::PressEnter,
        ],
        safety: InputSafety::Safe,
        created_at: DateTimeUtc::now_utc(),
    }
}

#[cfg(not(test))]
fn message_inject_send_delay() -> Duration {
    Duration::from_secs(5)
}

#[cfg(test)]
fn message_inject_send_delay() -> Duration {
    Duration::ZERO
}

fn provider_for_agent_name(name: &str) -> AgentProvider {
    let lower = name.to_ascii_lowercase();
    if lower.contains("codex") {
        AgentProvider::Codex
    } else if lower.contains("claude") {
        AgentProvider::ClaudeCode
    } else if lower.contains("shell") || lower.contains("tester") {
        AgentProvider::Shell
    } else {
        AgentProvider::Custom(name.to_string())
    }
}

fn inferred_agent_role(name: &str) -> AgentRole {
    let normalized = name.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    if normalized.contains("planner") {
        AgentRole::Planner
    } else if normalized.contains("tester") || normalized.contains("qa") {
        AgentRole::Tester
    } else if normalized.contains("reviewer") {
        AgentRole::Reviewer
    } else if normalized.contains("debugger") {
        AgentRole::Debugger
    } else if normalized.contains("refactorer") {
        AgentRole::Refactorer
    } else if normalized.contains("security") {
        AgentRole::SecurityReviewer
    } else if normalized.contains("docs") {
        AgentRole::DocsWriter
    } else if normalized.contains("integrator") {
        AgentRole::Integrator
    } else if normalized.starts_with("impl") || normalized.contains("implementer") {
        AgentRole::Implementer
    } else {
        AgentRole::Custom(name.to_string())
    }
}

fn agent_role_label(role: &AgentRole) -> String {
    match role {
        AgentRole::Planner => "planner".to_string(),
        AgentRole::Implementer => "implementer".to_string(),
        AgentRole::Reviewer => "reviewer".to_string(),
        AgentRole::Tester => "tester".to_string(),
        AgentRole::Debugger => "debugger".to_string(),
        AgentRole::Refactorer => "refactorer".to_string(),
        AgentRole::SecurityReviewer => "security_reviewer".to_string(),
        AgentRole::DocsWriter => "docs_writer".to_string(),
        AgentRole::Integrator => "integrator".to_string(),
        AgentRole::ContextManager => "context_manager".to_string(),
        AgentRole::Custom(role) => role.clone(),
    }
}

fn agent_status_label(status: &AgentStatus) -> &'static str {
    match status {
        AgentStatus::Starting => "starting",
        AgentStatus::InteractiveReady => "interactive_ready",
        AgentStatus::RunningTurn => "running_turn",
        AgentStatus::RunningCommand => "running_command",
        AgentStatus::AwaitingInput => "awaiting_input",
        AgentStatus::AwaitingApproval => "awaiting_approval",
        AgentStatus::NeedsHuman => "needs_human",
        AgentStatus::Blocked => "blocked",
        AgentStatus::CompletedTurn => "completed_turn",
        AgentStatus::Stalled => "stalled",
        AgentStatus::Exited => "exited",
        AgentStatus::Failed => "failed",
    }
}

fn agent_input_ready(agent: &LiveAgentSession) -> bool {
    agent.pty.is_some()
        && matches!(
            agent.metadata.status.as_ref(),
            Some(AgentStatus::AwaitingInput)
                | Some(AgentStatus::InteractiveReady)
                | Some(AgentStatus::CompletedTurn)
        )
}

fn trim_result_detection_tail(output_tail: &mut String) {
    const MAX_RESULT_DETECTION_TAIL: usize = 64 * 1024;
    if output_tail.len() <= MAX_RESULT_DETECTION_TAIL {
        return;
    }

    let keep_from = output_tail
        .char_indices()
        .rev()
        .find_map(|(index, _)| {
            (output_tail.len() - index <= MAX_RESULT_DETECTION_TAIL).then_some(index)
        })
        .unwrap_or(0);
    output_tail.drain(..keep_from);
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
    value.parse::<AgentSessionId>().ok()
}

fn agent_id_payload(payload: &serde_json::Value, command: &str) -> Result<AgentSessionId> {
    required_string(payload, "agent_id", command)?
        .parse::<AgentSessionId>()
        .map_err(|error| AgentmuxError::UserError(format!("invalid agent_id: {error}")))
}

fn terminal_size_payload(
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

fn parse_message_id(value: &str) -> Option<MessageId> {
    value.parse::<MessageId>().ok()
}

fn parse_context_item_id(value: &str) -> Option<ContextItemId> {
    value.parse::<ContextItemId>().ok()
}

fn message_create_payload(payload: &serde_json::Value) -> Result<NewAgentMessage> {
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

    Ok(NewAgentMessage {
        task_id: None,
        from: MessageSource::User(ClientId::new()),
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

fn parse_message_target(raw: &str) -> Result<MessageTarget> {
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

fn parse_agent_role(raw: &str) -> Result<AgentRole> {
    let normalized = raw.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    if normalized.is_empty() {
        return Err(AgentmuxError::UserError(
            "agent role must not be empty".to_string(),
        ));
    }
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
        _ => AgentRole::Custom(normalized),
    };
    Ok(role)
}

fn parse_message_kind(raw: &str) -> Result<MessageKind> {
    serde_json::from_value(json!(raw))
        .map_err(|error| AgentmuxError::UserError(format!("invalid message kind '{raw}': {error}")))
}

fn parse_priority(raw: &str) -> Result<Priority> {
    serde_json::from_value(json!(raw))
        .map_err(|error| AgentmuxError::UserError(format!("invalid priority '{raw}': {error}")))
}

fn parse_delivery_mode(raw: &str) -> Result<DeliveryMode> {
    serde_json::from_value(json!(raw)).map_err(|error| {
        AgentmuxError::UserError(format!("invalid delivery_mode '{raw}': {error}"))
    })
}

fn message_payload(message: &AgentMessage) -> serde_json::Value {
    json!({
        "message_id": message.id.to_string(),
        "task_id": message.task_id.as_ref().map(ToString::to_string),
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

enum ContextLookup {
    List,
    Show(ContextItemId),
    Search(String),
}

async fn context_create_payload(
    payload: &serde_json::Value,
    runtime: &DaemonRuntime,
) -> Result<NewContextItem> {
    let project_id = {
        let state = runtime.state.read().await;
        state.default_project_id.clone()
    };
    let title = required_string(payload, "title", "context.create")?.to_string();
    let body = payload
        .get("body")
        .and_then(|value| value.as_str())
        .unwrap_or(&title)
        .to_string();
    let kind = payload
        .get("kind")
        .and_then(|value| value.as_str())
        .map(parse_context_kind)
        .transpose()?
        .unwrap_or(ContextKind::HandoffSummary);
    let visibility = payload
        .get("visibility")
        .and_then(|value| value.as_str())
        .map(parse_visibility)
        .transpose()?
        .unwrap_or(Visibility::Internal);
    let tags = payload
        .get("tags")
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .map_err(|error| AgentmuxError::UserError(format!("context.create tags invalid: {error}")))?
        .unwrap_or_default();

    Ok(NewContextItem {
        project_id,
        task_id: None,
        scope: ContextScope::Project,
        kind,
        title,
        body,
        source: ContextSource::Human,
        visibility,
        confidence: 1.0,
        tags,
        related_files: Vec::new(),
        artifact_refs: Vec::new(),
    })
}

fn context_search_payload(payload: &serde_json::Value) -> Result<ContextLookup> {
    if let Some(raw_context_id) = payload.get("context_id").and_then(|value| value.as_str()) {
        let context_id = parse_context_item_id(raw_context_id).ok_or_else(|| {
            AgentmuxError::UserError(format!("invalid context_id '{raw_context_id}'"))
        })?;
        return Ok(ContextLookup::Show(context_id));
    }
    let query = payload
        .get("query")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    if query.is_empty() {
        Ok(ContextLookup::List)
    } else {
        Ok(ContextLookup::Search(query))
    }
}

fn context_attach_payload(payload: &serde_json::Value) -> Result<(ContextItemId, MessageId)> {
    let context_id = required_string(payload, "context_id", "context.attach")?
        .parse::<ContextItemId>()
        .map_err(|error| AgentmuxError::UserError(format!("invalid context_id: {error}")))?;
    let message_id = required_string(payload, "message_id", "context.attach")?
        .parse::<MessageId>()
        .map_err(|error| AgentmuxError::UserError(format!("invalid message_id: {error}")))?;
    Ok((context_id, message_id))
}

fn context_inject_payload(payload: &serde_json::Value) -> Result<(ContextItemId, AgentSessionId)> {
    let context_id = required_string(payload, "context_id", "context.inject")?
        .parse::<ContextItemId>()
        .map_err(|error| AgentmuxError::UserError(format!("invalid context_id: {error}")))?;
    let agent_id = required_string(payload, "agent_id", "context.inject")?
        .parse::<AgentSessionId>()
        .map_err(|error| AgentmuxError::UserError(format!("invalid agent_id: {error}")))?;
    Ok((context_id, agent_id))
}

fn required_string<'a>(
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

fn required_u16(payload: &serde_json::Value, field: &str, command: &str) -> Result<u16> {
    payload
        .get(field)
        .and_then(|value| value.as_u64())
        .and_then(|value| u16::try_from(value).ok())
        .ok_or_else(|| AgentmuxError::UserError(format!("{command} requires {field}")))
}

fn parse_context_kind(raw: &str) -> Result<ContextKind> {
    serde_json::from_value(json!(raw))
        .map_err(|error| AgentmuxError::UserError(format!("invalid context kind '{raw}': {error}")))
}

fn parse_visibility(raw: &str) -> Result<Visibility> {
    serde_json::from_value(json!(raw))
        .map_err(|error| AgentmuxError::UserError(format!("invalid visibility '{raw}': {error}")))
}

fn context_payload(item: &ContextItem) -> serde_json::Value {
    json!({
        "context_id": item.id.to_string(),
        "project_id": item.project_id.to_string(),
        "task_id": item.task_id.as_ref().map(ToString::to_string),
        "scope": item.scope,
        "kind": item.kind,
        "title": item.title,
        "body": item.body,
        "source": item.source,
        "visibility": item.visibility,
        "confidence": item.confidence,
        "tags": item.tags,
        "related_files": item
            .related_files
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>(),
        "artifact_refs": item.artifact_refs.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "created_at": item.created_at.to_string(),
        "updated_at": item.updated_at.to_string(),
    })
}

fn worktree_id_payload(payload: &serde_json::Value, command: &str) -> Result<WorktreeId> {
    required_string(payload, "worktree_id", command)?
        .parse::<WorktreeId>()
        .map_err(|error| AgentmuxError::UserError(format!("invalid worktree_id: {error}")))
}

fn approval_id_payload(payload: &serde_json::Value, command: &str) -> Result<ApprovalId> {
    required_string(payload, "approval_id", command)?
        .parse::<ApprovalId>()
        .map_err(|error| AgentmuxError::UserError(format!("invalid approval_id: {error}")))
}

fn worktree_test_command_payload(payload: &serde_json::Value) -> TestCommand {
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

fn worktree_payload(worktree: &Worktree) -> serde_json::Value {
    json!({
        "worktree_id": worktree.id.to_string(),
        "project_id": worktree.project_id.to_string(),
        "task_id": worktree.task_id.to_string(),
        "owner_agent_id": worktree.owner_agent_id.as_ref().map(ToString::to_string),
        "path": worktree.path.display().to_string(),
        "branch_name": worktree.branch_name,
        "base_branch": worktree.base_branch,
        "status": worktree.status,
        "created_at": worktree.created_at.to_string(),
    })
}

fn approval_payload(approval: &ApprovalRequest) -> serde_json::Value {
    json!({
        "approval_id": approval.id.to_string(),
        "kind": approval.kind,
        "risk": approval.risk,
        "title": approval.title,
        "description": approval.description,
        "proposed_input": approval.proposed_input,
        "command": approval.command,
        "worktree_id": approval.worktree_id.as_ref().map(ToString::to_string),
        "context_refs": approval.context_refs.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "status": approval.status,
    })
}

fn approval_queue_error(error: ApprovalQueueError) -> String {
    match error {
        ApprovalQueueError::UnknownApproval(id) => format!("unknown approval '{id}'"),
        ApprovalQueueError::AlreadyDecided { id, status } => {
            format!("approval '{id}' is already {status:?}")
        }
    }
}

fn approval_daemon_event(event: &ApprovalEvent) -> DaemonEvent {
    match event {
        ApprovalEvent::ApprovalCreated {
            approval_id,
            kind,
            risk,
            title,
        } => DaemonEvent::new(
            IpcEventKind::ApprovalCreated,
            json!({
                "approval_id": approval_id.to_string(),
                "kind": kind,
                "risk": risk,
                "title": title,
            }),
        ),
        ApprovalEvent::ApprovalDecided {
            approval_id,
            status,
        } => DaemonEvent::new(
            IpcEventKind::ApprovalDecided,
            json!({
                "approval_id": approval_id.to_string(),
                "status": status,
            }),
        ),
        ApprovalEvent::PolicyDenied {
            kind,
            risk,
            title,
            description,
            command,
        } => DaemonEvent::new(
            IpcEventKind::PolicyDenied,
            json!({
                "kind": kind,
                "risk": risk,
                "title": title,
                "description": description,
                "command": command,
            }),
        ),
    }
}

fn artifact_payload(artifact_id: String, path: String, title: String) -> serde_json::Value {
    json!({
        "artifact_id": artifact_id,
        "path": path,
        "title": title,
    })
}

fn json_error(error: serde_json::Error) -> AgentmuxError {
    AgentmuxError::StoreError(format!("failed to encode event payload: {error}"))
}

fn pty_spawn_spec_from_payload(payload: &serde_json::Value) -> Result<Option<PtySpawnSpec>> {
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

fn default_provider_args(provider: Option<&str>) -> Vec<String> {
    match provider {
        Some("agy") => vec!["--dangerously-skip-permissions".to_string()],
        _ => Vec::new(),
    }
}

fn provider_command(provider: &str) -> String {
    match provider {
        "shell" => std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()),
        "claude" => "claude".to_string(),
        "codex" => "codex".to_string(),
        "agy" => "agy".to_string(),
        custom => custom.to_string(),
    }
}

fn slug_label(value: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !slug.is_empty() {
            slug.push('-');
            last_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "agent".to_string()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
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
            .persist_live_agent_result(None, "impl-codex", output)
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
            .persist_live_agent_result(None, "impl-codex", output)
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
            .persist_live_agent_result(Some(&agent.id), "impl-codex", output)
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
    async fn manual_message_inject_fails_without_live_pty() {
        let runtime = DaemonRuntime::new(16);
        let agent = runtime.register_agent("metadata-only".to_string()).await;
        let message = runtime
            .create_message(NewAgentMessage {
                task_id: None,
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
}
