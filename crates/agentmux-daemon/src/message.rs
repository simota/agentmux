use crate::*;

impl DaemonRuntime {
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

    /// Like `create_message` but stores the message even when no agents are
    /// currently registered for the target (used from automated
    /// `persist_live_agent_result` paths where the target may not yet be
    /// spawned).
    pub(crate) async fn create_message_allow_no_recipients(
        &self,
        input: NewAgentMessage,
    ) -> Result<AgentMessage> {
        let mut state = self.state.write().await;
        let message = state.messages.create_message_allow_no_recipients(input)?;
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
        self.settle_write_and_finish(&prepared, now).await
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
        self.settle_write_and_finish(&prepared, now).await
    }

    /// Wait the settle delay, write the prepared injection into the target PTY,
    /// then finish and emit the delivery event. Shared by the manual-inject and
    /// idle-delivery paths.
    ///
    /// The settle delay gives the target TUI composer time to finish
    /// drip-rendering the previous turn before it receives bracketed-paste
    /// input. The delay is `Duration::ZERO` in tests (see `InjectionTiming`), so
    /// the sleep is a no-op there and delivery still completes end-to-end.
    pub(crate) async fn settle_write_and_finish(
        &self,
        prepared: &PreparedInjection,
        now: DateTimeUtc,
    ) -> Result<AgentMessage> {
        let send_delay = self.state.read().await.injection_timing.send_delay;
        if !send_delay.is_zero() {
            tokio::time::sleep(send_delay).await;
        }
        let write_result = self.write_prepared_message_injection(prepared).await;
        self.finish_and_emit_message_injection(
            &prepared.message_id,
            &prepared.agent_id,
            now,
            write_result,
        )
        .await
    }

    pub(crate) async fn finish_and_emit_message_injection(
        &self,
        id: &MessageId,
        agent_id: &AgentSessionId,
        now: DateTimeUtc,
        write_result: Result<()>,
    ) -> Result<AgentMessage> {
        let finished = self
            .finish_message_injection(id, agent_id, now, write_result)
            .await;
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

        let message = self.settle_write_and_finish(&prepared, now).await?;
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

    pub(crate) async fn prepare_manual_message_injection(
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
        let agent_id = agent_id.clone();
        prepare_message_injection_for_resolved(&mut state, id, &agent_id, &message, now)
    }

    pub(crate) async fn prepare_manual_message_injection_for_agent(
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
        prepare_message_injection_for_resolved(&mut state, id, agent_id, &message, now)
    }

    /// Write an input script's actions to an agent's PTY, preserving byte order
    /// while never blocking the Tokio worker across a `Wait`.
    ///
    /// Each contiguous run of byte-steps is flushed under a short-lived
    /// `state.read()` + PTY `Mutex` lock that is dropped *before* the
    /// subsequent `Wait` is awaited with `tokio::time::sleep`. No std `Mutex`
    /// guard is ever held across an `.await`, and the observable timing (the
    /// 120ms paste→enter gap) is unchanged.
    pub(crate) async fn write_input_actions_to_agent_pty(
        &self,
        agent_id: &AgentSessionId,
        actions: &[InputAction],
    ) -> Result<()> {
        let mut pending: Vec<u8> = Vec::new();
        for action in actions {
            match encode_input_action(action)? {
                EncodedInputStep::Bytes(bytes) => pending.extend_from_slice(&bytes),
                EncodedInputStep::Wait(duration) => {
                    // Flush everything written so far, release all locks, then
                    // sleep asynchronously without holding the PTY mutex. A zero
                    // wait is skipped entirely so it introduces no scheduler yield
                    // (matching the previous in-lock `thread::sleep(ZERO)`).
                    self.write_bytes_to_agent_pty(agent_id, &pending).await?;
                    pending.clear();
                    if !duration.is_zero() {
                        tokio::time::sleep(duration).await;
                    }
                }
            }
        }
        self.write_bytes_to_agent_pty(agent_id, &pending).await
    }

    /// Write a byte buffer to an agent's PTY under a short-lived lock.
    ///
    /// The `state.read()` guard and the PTY `std::sync::Mutex` guard are both
    /// confined to a non-async scope and dropped before this function returns,
    /// so no guard can leak across an `.await` in the caller.
    pub(crate) async fn write_bytes_to_agent_pty(
        &self,
        agent_id: &AgentSessionId,
        bytes: &[u8],
    ) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
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
        let mut pty = pty.lock().map_err(|_| {
            AgentmuxError::Internal(format!("PTY lock for agent '{agent_id}' is poisoned"))
        })?;
        pty.write_bytes(bytes)
    }

    pub(crate) async fn write_prepared_message_injection(&self, prepared: &PreparedInjection) -> Result<()> {
        let paste_enter_delay = self.state.read().await.injection_timing.paste_enter_delay;
        let script = message_input_script(prepared, paste_enter_delay);
        self.append_input_script_event(agentmux_store::EVENT_INPUT_SCRIPT_CREATED, &script)?;

        self.write_input_actions_to_agent_pty(&prepared.agent_id, &script.actions)
            .await?;

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

    pub(crate) async fn finish_message_injection(
        &self,
        id: &MessageId,
        agent_id: &AgentSessionId,
        now: DateTimeUtc,
        write_result: Result<()>,
    ) -> Result<AgentMessage> {
        let mut state = self.state.write().await;
        match write_result {
            Ok(()) => state.messages.mark_message_injected(id, agent_id, now)?,
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

    /// Resolve message targets for a set of just-persisted messages and trigger
    /// idle delivery for each target agent that is currently registered.
    ///
    /// Used by every persist path that wants the queued message to reach an
    /// idle target PTY without a separate manual `inject` step (live
    /// `AGENTMUX_RESULT` routing and the `message create`/`send` command).
    ///
    /// Eligibility (delivery_mode, agent status) is delegated to the existing
    /// `deliver_idle_messages_for_agent` machinery — no extra gating is added
    /// here, so `InboxOnly` / `RequireHumanApproval` semantics are preserved.
    ///
    /// Unknown targets (agents not yet registered) are silently skipped and
    /// their messages remain queued so they can be picked up when the target
    /// agent eventually calls `deliver_idle_messages_for_agent`.
    pub(crate) async fn trigger_idle_delivery_for_messages(&self, messages: &[AgentMessage]) {
        // Collect unique target agent IDs from the message bus.
        let target_ids: BTreeSet<AgentSessionId> = {
            let state = self.state.read().await;
            messages
                .iter()
                .flat_map(|msg| state.messages.resolve_target(&msg.to).unwrap_or_default())
                .collect()
        };

        if target_ids.is_empty() {
            return;
        }

        for target_id in target_ids {
            // Fetch the current status for this agent; fall back to
            // InteractiveReady so that an agent without an explicit status is
            // still considered eligible for idle delivery.  Eligibility of the
            // status itself (idle vs running) is delegated to
            // `deliver_idle_messages_for_agent`.
            let status = {
                let state = self.state.read().await;
                state
                    .agents
                    .get(&target_id)
                    .and_then(|a| a.metadata.status.clone())
                    .unwrap_or(AgentStatus::InteractiveReady)
            };
            if let Err(error) = self
                .deliver_idle_messages_for_agent(&target_id, status)
                .await
            {
                self.publish(DaemonEvent::new(
                    IpcEventKind::Error,
                    json!({
                        "agent_id": target_id.to_string(),
                        "signal": "idle_delivery_failed_after_result",
                        "error": error.to_string(),
                    }),
                ));
            }
        }
    }

    pub async fn send_input_script(&self, script: &InputScript) -> Result<()> {
        self.append_input_script_event("input_script.created", script)?;

        self.write_input_actions_to_agent_pty(&script.target_agent_id, &script.actions)
            .await?;

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

}

/// Render inputs + prompt for an already-resolved recipient, mark the message
/// `Injecting`, and build the `PreparedInjection`. Shared by both manual-inject
/// preparation paths (auto-resolved target vs. explicitly chosen agent).
pub(crate) fn prepare_message_injection_for_resolved(
    state: &mut DaemonState,
    id: &MessageId,
    agent_id: &AgentSessionId,
    message: &AgentMessage,
    now: DateTimeUtc,
) -> Result<PreparedInjection> {
    // Guard against a double injection (#7): an idle auto-delivery marks the
    // message `Injecting` and then sleeps for the settle delay before writing
    // to the PTY. A manual `MessageInject` IPC arriving during that window must
    // not start a second injection of the same message, or the prompt is
    // written into the PTY twice. The idle path already self-guards via
    // `next_inject_when_idle_message_id`; this closes the manual path.
    if message.delivery_status == DeliveryStatus::Injecting {
        return Err(AgentmuxError::UserError(format!(
            "message '{id}' is already injecting"
        )));
    }
    let (provider, context) = delivery_render_inputs(state, agent_id, message)?;
    let thread = message
        .thread_id
        .as_ref()
        .and_then(|thread_id| state.messages.get_thread(thread_id))
        .cloned();
    let prompt = agentmux_message::render_prompt(message, provider, &context, thread.as_ref());
    state
        .messages
        .update_delivery_status(id, DeliveryStatus::Injecting, now)?;

    Ok(PreparedInjection {
        message_id: id.clone(),
        agent_id: agent_id.clone(),
        prompt,
    })
}

pub(crate) fn delivery_render_inputs(
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
            max_inline_chars: state.injection_timing.max_inline_chars,
        },
        MailboxConfig {
            project_root,
            agent_name: agent.metadata.name.clone(),
        },
    )?;
    let context = message_prompt_context_from_pack(pack);
    Ok((provider, context))
}

pub(crate) fn message_prompt_context_from_pack(pack: agentmux_context::ContextPack) -> PromptContext {
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

/// Build the three-action injection script: PasteText, a `paste_enter_delay`
/// Wait, then PressEnter.  The Wait ensures the paste body and the trailing `\r`
/// land in separate PTY read chunks; without it Codex's crossterm bracketed-paste
/// handler coalesces the `\r` into the paste buffer instead of treating it as a
/// submit keypress.  The delay is sourced from `InjectionTiming` (config-driven).
pub(crate) fn message_input_script(prepared: &PreparedInjection, paste_enter_delay: Duration) -> InputScript {
    InputScript {
        id: InputScriptId::new(),
        target_agent_id: prepared.agent_id.clone(),
        reason: format!("message.inject {}", prepared.message_id),
        preconditions: Vec::new(),
        actions: vec![
            InputAction::PasteText(prepared.prompt.clone()),
            InputAction::Wait(paste_enter_delay),
            InputAction::PressEnter,
        ],
        safety: InputSafety::Safe,
        created_at: DateTimeUtc::now_utc(),
    }
}

pub(crate) fn provider_for_agent_name(name: &str) -> AgentProvider {
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

pub(crate) fn inferred_agent_role(name: &str) -> AgentRole {
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

pub(crate) fn agent_role_label(role: &AgentRole) -> String {
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

pub(crate) fn agent_status_label(status: &AgentStatus) -> &'static str {
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

pub(crate) fn agent_input_ready(agent: &LiveAgentSession) -> bool {
    agent.pty.is_some()
        && matches!(
            agent.metadata.status.as_ref(),
            Some(AgentStatus::AwaitingInput)
                | Some(AgentStatus::InteractiveReady)
                | Some(AgentStatus::CompletedTurn)
        )
}

