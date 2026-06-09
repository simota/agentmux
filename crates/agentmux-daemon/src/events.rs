use crate::*;

impl DaemonRuntime {
    pub(crate) fn append_input_script_event(&self, kind: &str, script: &InputScript) -> Result<()> {
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

    pub(crate) fn append_daemon_lifecycle_event(
        &self,
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<()> {
        let Some(event_log) = &self.event_log else {
            return Ok(());
        };
        event_log.append(&EventLogEntry::new(kind, DateTimeUtc::now_utc(), payload)?)
    }

    pub(crate) fn append_message_event(&self, kind: &str, message: &AgentMessage) -> Result<()> {
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

    pub(crate) fn append_context_created_event(&self, item: &ContextItem) -> Result<()> {
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

    pub(crate) fn append_approval_event(&self, event: &ApprovalEvent) -> Result<()> {
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

pub(crate) fn event_kind_label(kind: &IpcEventKind) -> &'static str {
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
        IpcEventKind::AgentRoleChanged => "agent.role_changed",
        IpcEventKind::AgentExited => "agent.exited",
        IpcEventKind::PtyOutputChunk => "pty.output_chunk",
        IpcEventKind::ScreenDiff => "screen.diff",
        IpcEventKind::TerminalSnapshotSaved => "terminal.snapshot_saved",
        IpcEventKind::InputScriptCreated => "input_script.created",
        IpcEventKind::InputScriptInjected => "input_script.injected",
        IpcEventKind::InputInjected => "input.injected",
        IpcEventKind::MessageCreated => "message.created",
        IpcEventKind::MessageDelivered => "message.delivered",
        IpcEventKind::ThreadOpened => "thread.opened",
        IpcEventKind::ThreadClosed => "thread.closed",
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

pub(crate) fn approval_daemon_event(event: &ApprovalEvent) -> DaemonEvent {
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

pub(crate) fn artifact_payload(
    artifact_id: String,
    path: String,
    title: String,
) -> serde_json::Value {
    json!({
        "artifact_id": artifact_id,
        "path": path,
        "title": title,
    })
}
