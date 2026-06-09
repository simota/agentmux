use crate::*;

impl DaemonRuntime {
    /// Open a multi-party meeting thread and inject the agenda to every
    /// participant. Returns the thread plus the kickoff message.
    pub async fn open_meeting(
        &self,
        input: OpenMeetingInput,
    ) -> Result<(MessageThread, AgentMessage)> {
        let mut state = self.state.write().await;
        let participants = input
            .participants
            .iter()
            .map(|raw| resolve_participant(&state.messages, raw))
            .collect::<Result<Vec<_>>>()?;
        let thread = state.messages.open_thread(NewMessageThread {
            topic: input.topic.clone(),
            participants,
            opened_by: input.opened_by.clone(),
            max_messages_per_participant: input.max_messages_per_participant,
        })?;
        drop(state);

        self.append_thread_event(agentmux_store::EVENT_THREAD_OPENED, &thread)?;
        self.publish(DaemonEvent::new(
            IpcEventKind::ThreadOpened,
            thread_payload(&thread, 0),
        ));

        let body = input.body.unwrap_or_else(|| {
            format!(
                "会議を開始します。\n議題: {}\n\nまず各自の見解をこのスレッドに共有してください。",
                input.topic
            )
        });
        let kickoff = self
            .create_message(NewAgentMessage {
                task_id: None,
                thread_id: Some(thread.id.clone()),
                from: input.opened_by,
                to: MessageTarget::Thread(thread.id.clone()),
                kind: input.kind,
                priority: input.priority,
                body,
                context_refs: Vec::new(),
                artifact_refs: Vec::new(),
                delivery_mode: DeliveryMode::InjectWhenIdle,
                requires_response: true,
            })
            .await?;
        self.trigger_idle_delivery_for_messages(std::slice::from_ref(&kickoff))
            .await;
        Ok((thread, kickoff))
    }

    pub async fn close_meeting(&self, thread_id: &ThreadId) -> Result<MessageThread> {
        let mut state = self.state.write().await;
        let thread = state
            .messages
            .close_thread(thread_id, DateTimeUtc::now_utc())?;
        let message_count = state.messages.thread_message_count(thread_id);
        drop(state);

        self.append_thread_event(agentmux_store::EVENT_THREAD_CLOSED, &thread)?;
        self.publish(DaemonEvent::new(
            IpcEventKind::ThreadClosed,
            thread_payload(&thread, message_count),
        ));
        Ok(thread)
    }

    pub async fn list_meetings(&self) -> Vec<(MessageThread, usize)> {
        let state = self.state.read().await;
        state
            .messages
            .list_threads()
            .into_iter()
            .map(|thread| {
                let count = state.messages.thread_message_count(&thread.id);
                (thread.clone(), count)
            })
            .collect()
    }

    pub(crate) fn append_thread_event(&self, kind: &str, thread: &MessageThread) -> Result<()> {
        let Some(event_log) = &self.event_log else {
            return Ok(());
        };
        let entry = EventLogEntry::new(kind, DateTimeUtc::now_utc(), thread_payload(thread, 0))?;
        event_log.append(&entry)
    }
}

/// Resolve one `meeting.open` participant entry (session id or unique name).
pub(crate) fn resolve_participant(bus: &MessageBus, raw: &str) -> Result<AgentSessionId> {
    let raw = raw.trim();
    if let Ok(agent_id) = raw.parse::<AgentSessionId>() {
        return Ok(agent_id);
    }
    let resolved = bus.resolve_target(&MessageTarget::AgentName(raw.to_string()))?;
    match resolved.as_slice() {
        [agent_id] => Ok(agent_id.clone()),
        _ => Err(AgentmuxError::UserError(format!(
            "meeting participant '{raw}' resolved to {} sessions; use a unique session name or id",
            resolved.len()
        ))),
    }
}

pub(crate) fn thread_payload(thread: &MessageThread, message_count: usize) -> serde_json::Value {
    json!({
        "thread_id": thread.id.to_string(),
        "topic": thread.topic,
        "participants": thread
            .participants
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        "opened_by": thread.opened_by,
        "status": thread.status,
        "max_messages_per_participant": thread.max_messages_per_participant,
        "message_count": message_count,
        "created_at": thread.created_at.to_string(),
        "closed_at": thread.closed_at.map(|ts| ts.to_string()),
    })
}
