use crate::*;

impl DaemonRuntime {
    pub async fn register_agent(&self, name: String) -> RegisteredAgentSession {
        self.register_agent_with_role(name, default_agent_role())
            .await
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
        self.spawn_agent_with_role(name, default_agent_role(), spec)
            .await
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
                pty: Some(Arc::new(Mutex::new(pty))),
                terminal,
                input_activity: InputActivity::new(),
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
                    PtyReadEvent::Eof => {
                        runtime.handle_agent_pty_closed(&agent_id, "eof").await;
                        break;
                    }
                    PtyReadEvent::Error(error) => {
                        let _ = events.send(DaemonEvent::new(
                            IpcEventKind::AgentStatusSignal,
                            json!({
                                "agent_id": agent_id.to_string(),
                                "signal": "pty_read_error",
                                "error": error,
                            }),
                        ));
                        runtime
                            .handle_agent_pty_closed(&agent_id, "pty_read_error")
                            .await;
                        break;
                    }
                }
            }
        });
    }

    /// Handle the natural end of an agent's PTY stream (EOF or read error):
    /// reap the child process (no zombie), mark the session `Exited`, drop it
    /// from message-bus routing so it stops being an injection target, and
    /// emit `agent.exited`.
    ///
    /// The session metadata stays in `state.agents` so `daemon.status` keeps
    /// showing the pane with its final `exited` status until an explicit
    /// `agent.stop` removes it. If the PTY handle was already taken (an
    /// `agent.stop` is in flight and owns process shutdown), this is a no-op.
    pub(crate) async fn handle_agent_pty_closed(&self, agent_id: &AgentSessionId, reason: &str) {
        let (pty, name) = {
            let mut state = self.state.write().await;
            let Some(agent) = state.agents.get_mut(agent_id) else {
                return;
            };
            let Some(pty) = agent.pty.take() else {
                // `stop_agent` already took the handle and owns the shutdown.
                return;
            };
            agent.metadata.status = Some(AgentStatus::Exited);
            let name = agent.metadata.name.clone();
            state.messages.deregister_agent(agent_id);
            (pty, name)
        };

        // Reap off-worker: `try_wait`/`wait` are blocking child-process calls.
        let exit = tokio::task::spawn_blocking(move || terminate_and_reap_pty(&pty))
            .await
            .unwrap_or_else(|error| {
                Err(AgentmuxError::Internal(format!(
                    "PTY reap task failed: {error}"
                )))
            });
        let exit_payload = match &exit {
            Ok(Some(status)) => json!(status.exit_code),
            Ok(None) | Err(_) => serde_json::Value::Null,
        };

        self.publish(DaemonEvent::new(
            IpcEventKind::AgentStatusChanged,
            json!({
                "agent_id": agent_id.to_string(),
                "status": AgentStatus::Exited,
            }),
        ));
        let payload = json!({
            "agent_id": agent_id.to_string(),
            "name": name,
            "reason": reason,
            "exit_code": exit_payload,
        });
        let _ = self.append_daemon_lifecycle_event("agent.exited", payload.clone());
        self.publish(DaemonEvent::new(IpcEventKind::AgentExited, payload));
        if let Err(error) = exit {
            self.publish(DaemonEvent::new(
                IpcEventKind::Error,
                json!({
                    "agent_id": agent_id.to_string(),
                    "signal": "agent_reap_failed",
                    "error": error.to_string(),
                }),
            ));
        }
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

    /// Reassign the role of a live session at runtime.
    ///
    /// Updates both the session metadata (so the role surfaced in
    /// `daemon.status` / snapshots changes) and the message-bus descriptor (so
    /// `resolve_target(Role(..))` routes to — or away from — this session).
    /// Publishes an `agent.role_changed` event so attached clients can refresh
    /// the role rendered for the pane. The returned session reflects the new
    /// role.
    pub async fn set_agent_role(
        &self,
        agent_id: &AgentSessionId,
        role: AgentRole,
    ) -> Result<RegisteredAgentSession> {
        let mut state = self.state.write().await;
        let Some(agent) = state.agents.get_mut(agent_id) else {
            return Err(AgentmuxError::UserError(format!(
                "unknown agent session '{agent_id}'"
            )));
        };
        agent.metadata.role = role.clone();
        let updated = agent.metadata.clone();
        // Keep the bus routing descriptor in sync; a missing descriptor here
        // would mean state.agents and the bus diverged, which the bus tolerates
        // by reporting `false` (no panic).
        state.messages.set_agent_role(agent_id, role.clone());
        drop(state);

        self.publish(DaemonEvent::new(
            IpcEventKind::AgentRoleChanged,
            json!({
                "agent_id": updated.id.to_string(),
                "role": agent_role_label(&updated.role),
            }),
        ));
        Ok(updated)
    }

    pub async fn stop_agent(&self, agent_id: &AgentSessionId) -> Result<RegisteredAgentSession> {
        // Phase 1: take the PTY handle while keeping the session registered.
        // Removing the session before the child is confirmed dead would orphan
        // the process when terminate fails (no handle left to retry with).
        let pty = {
            let mut state = self.state.write().await;
            let Some(agent) = state.agents.get_mut(agent_id) else {
                return Err(AgentmuxError::UserError(format!(
                    "unknown agent session '{agent_id}'"
                )));
            };
            agent.pty.take()
        };

        if let Some(pty) = pty {
            let reap_handle = pty.clone();
            let reaped = tokio::task::spawn_blocking(move || terminate_and_reap_pty(&reap_handle))
                .await
                .unwrap_or_else(|error| {
                    Err(AgentmuxError::Internal(format!(
                        "PTY reap task failed: {error}"
                    )))
                });
            if let Err(error) = reaped {
                // Put the handle back so the session is still stoppable; the
                // process was not confirmed dead, so the session must not be
                // silently dropped from state.
                let mut state = self.state.write().await;
                if let Some(agent) = state.agents.get_mut(agent_id) {
                    agent.pty = Some(pty);
                }
                return Err(error);
            }
        }

        // Phase 2: the child is reaped (or there was no process) — now drop
        // the session from state and from the message bus, otherwise
        // `resolve_target` keeps routing to the stopped agent and its inbox
        // leaks (#8).
        let mut state = self.state.write().await;
        let Some(agent) = state.agents.remove(agent_id) else {
            return Err(AgentmuxError::UserError(format!(
                "unknown agent session '{agent_id}'"
            )));
        };
        state.messages.deregister_agent(agent_id);
        drop(state);

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
        // Reuse the lock-free write path: the interrupt byte goes through the
        // same bounded blocking write as message injection, so a pane that
        // stopped reading input cannot wedge the daemon behind a state guard.
        self.write_bytes_to_agent_pty(agent_id, CTRL_C).await
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

/// Window within which a signalled/EOF'd PTY child must be reaped before the
/// reap is reported as failed (poll loop of 10ms steps).
const PTY_REAP_POLL_STEPS: u32 = 200;
const PTY_REAP_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Ensure a PTY child is dead and reaped, returning its exit status when one
/// is available.
///
/// Blocking — call from `spawn_blocking`. An already-exited child (the EOF
/// path) is reaped via `try_wait` without signalling. A still-running child is
/// terminated (`PtyHandle::terminate` = SIGHUP to the process group, close of
/// the master handles, SIGKILL to the child) and then reaped within a bounded
/// window, so neither a zombie nor a live orphan process is left behind.
pub(crate) fn terminate_and_reap_pty(pty: &Arc<Mutex<PtyHandle>>) -> Result<Option<PtyExitStatus>> {
    let mut pty = pty.lock().map_err(|_| {
        AgentmuxError::Internal("PTY lock is poisoned during terminate/reap".to_string())
    })?;
    if let Ok(Some(status)) = pty.try_wait() {
        return Ok(Some(status));
    }

    let terminate_result = pty.terminate();
    for _ in 0..PTY_REAP_POLL_STEPS {
        match pty.try_wait() {
            Ok(Some(status)) => return Ok(Some(status)),
            Ok(None) => std::thread::sleep(PTY_REAP_POLL_INTERVAL),
            Err(error) => return Err(error),
        }
    }
    // The child survived SIGKILL's reap window: surface the terminate error if
    // there was one, otherwise report the stuck reap itself.
    terminate_result?;
    Err(AgentmuxError::PtyError(
        "PTY child did not exit within the reap window after kill".to_string(),
    ))
}
