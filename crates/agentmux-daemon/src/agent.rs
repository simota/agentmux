use crate::*;

impl DaemonRuntime {
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

    pub(crate) fn spawn_pty_output_forwarder(
        &self,
        agent_id: AgentSessionId,
        agent_name: String,
        terminal: Arc<Mutex<TerminalParser>>,
        mut read_loop: agentmux_pty::PtyReadLoop,
    ) {
        let runtime = self.clone();
        let events = self.events.clone();
        tokio::spawn(async move {
            let result_detection_tail_bytes = runtime
                .state
                .read()
                .await
                .injection_timing
                .result_detection_tail_bytes;
            let mut output_tail = String::new();
            // Content-hash dedup across the session lifetime (drip/repaint emits
            // the same result repeatedly; distinct results must still persist).
            let mut seen_hashes = SeenResultHashes::new(8);
            // Suppress repeated probe/error events for the same unchanged cause
            // during a drip render.
            let mut last_probe_reason: Option<String> = None;
            while let Some(event) = read_loop.recv().await {
                match event {
                    PtyReadEvent::Output(bytes) => {
                        if let Ok(mut terminal) = terminal.lock() {
                            terminal.advance(&bytes);
                        }
                        let text = String::from_utf8_lossy(&bytes).into_owned();
                        output_tail.push_str(&text);
                        trim_result_detection_tail(&mut output_tail, result_detection_tail_bytes);
                        let _ = events.send(DaemonEvent::new(
                            IpcEventKind::PtyOutputChunk,
                            json!({
                                "agent_id": agent_id.to_string(),
                                "bytes": bytes,
                                "text": text,
                            }),
                        ));
                        match runtime
                            .persist_live_agent_result(
                                Some(&agent_id),
                                &agent_name,
                                &output_tail,
                                &mut seen_hashes,
                            )
                            .await
                        {
                            Ok(LiveResultOutcome::Persisted) => {
                                last_probe_reason = None;
                            }
                            Ok(LiveResultOutcome::NotFound) | Ok(LiveResultOutcome::Duplicate) => {}
                            Ok(LiveResultOutcome::NeedsProbe { reason }) => {
                                // Only emit when the cause changed since the last
                                // emission, to avoid one event per drip frame.
                                if last_probe_reason.as_deref() != Some(reason.as_str()) {
                                    let _ = events.send(DaemonEvent::new(
                                        IpcEventKind::Error,
                                        json!({
                                            "agent_id": agent_id.to_string(),
                                            "agent_name": agent_name,
                                            "signal": "agent_result_needs_status_probe",
                                            "reason": reason,
                                        }),
                                    ));
                                    last_probe_reason = Some(reason);
                                }
                            }
                            Err(error) => {
                                let _ = events.send(DaemonEvent::new(
                                    IpcEventKind::Error,
                                    json!({
                                        "agent_id": agent_id.to_string(),
                                        "signal": "agent_result_persist_failed",
                                        "error": error.to_string(),
                                    }),
                                ));
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
        // Drop the session from the message bus too, otherwise `resolve_target`
        // keeps routing to the stopped agent and its inbox leaks (#8).
        state.messages.deregister_agent(agent_id);
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

}

