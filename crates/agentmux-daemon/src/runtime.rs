use crate::*;

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

    /// Override message-injection timing/size knobs from project config
    /// (`[automation]` + `[context]`). Without this, the daemon uses the
    /// historical production defaults (5s settle, 120ms paste-enter, 64KiB tail).
    pub async fn with_injection_config(
        self,
        automation: &AutomationConfig,
        context: &ContextConfig,
    ) -> Self {
        self.state.write().await.injection_timing =
            InjectionTiming::from_config(automation, context);
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

    pub(crate) fn publish(&self, event: DaemonEvent) {
        let _ = self.events.send(event);
    }

}

#[derive(Clone)]
pub struct DaemonRuntime {
    pub(crate) state: Arc<RwLock<DaemonState>>,
    pub(crate) events: broadcast::Sender<DaemonEvent>,
    pub(crate) event_log: Option<EventLog>,
}

