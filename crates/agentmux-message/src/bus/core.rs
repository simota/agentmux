use std::collections::BTreeMap;

use agentmux_core::{
    AgentProvider, AgentRole, AgentSessionId, AgentStatus, AgentmuxError, ContextItemId,
    DateTimeUtc, DeliveryMode, DeliveryStatus, MessageId, ThreadId, error::Result,
};

use super::render::{render_prompt, target_label};
use super::status::{
    agent_accepts_idle_injection, backfill_eligible_status, exclude_sender_from_fan_out,
    unknown_agent, unknown_message, unknown_thread, wait_reason_for_status,
};
use super::types::{
    AgentDescriptor, DeliveryWait, DeliveryWaitReason, IdleDelivery, Inbox, PreparedInjection,
    PromptContext,
};
use crate::message::{AgentMessage, MessageSource, MessageTarget, NewAgentMessage};
use crate::thread::{MessageThread, NewMessageThread, ThreadStatus};

#[derive(Debug, Default)]
pub struct MessageBus {
    messages: BTreeMap<MessageId, AgentMessage>,
    agents: BTreeMap<AgentSessionId, AgentDescriptor>,
    inboxes: BTreeMap<AgentSessionId, Inbox>,
    threads: BTreeMap<ThreadId, MessageThread>,
}

impl MessageBus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_agent(&mut self, agent: AgentDescriptor) {
        let agent_id = agent.id.clone();
        self.inboxes
            .entry(agent_id.clone())
            .or_insert_with(|| Inbox {
                agent_id: agent_id.clone(),
                message_ids: Vec::new(),
            });
        // Insert the descriptor first so that `resolve_target` (used by the
        // backfill below) can see the freshly registered agent when matching
        // Role/Task/Team/AgentName/Agent targets.
        self.agents.insert(agent_id.clone(), agent);
        self.backfill_inbox_for_registered_agent(&agent_id);
    }

    /// Update the role of an already-registered agent so that subsequent
    /// `resolve_target(Role(..))` lookups route to (or away from) this session.
    ///
    /// Returns `true` when the agent was registered (and its role updated),
    /// `false` when no descriptor exists for `agent_id`. Backfill is re-run
    /// because a freshly assigned role can make this session a recipient of
    /// previously stored, still-pending role-targeted messages.
    pub fn set_agent_role(&mut self, agent_id: &AgentSessionId, role: AgentRole) -> bool {
        let Some(descriptor) = self.agents.get_mut(agent_id) else {
            return false;
        };
        descriptor.role = role;
        self.backfill_inbox_for_registered_agent(agent_id);
        true
    }

    /// Remove an agent from the bus when its session stops.
    ///
    /// Drops both the routing descriptor (so `resolve_target` no longer yields
    /// the stopped session for Role/Team/Broadcast/Agent targets) and the
    /// agent's inbox (so the per-session message-id list does not leak for the
    /// lifetime of the daemon). Stored messages themselves are retained for the
    /// audit/history view; only this session's delivery routing is dropped.
    pub fn deregister_agent(&mut self, agent_id: &AgentSessionId) {
        self.agents.remove(agent_id);
        self.inboxes.remove(agent_id);
    }

    /// When an agent registers, claim any previously stored messages whose
    /// target now resolves to this agent but which were saved with no inbox
    /// entry (e.g. via `create_message_allow_no_recipients` while the target
    /// session had not yet spawned).
    ///
    /// Only messages that are still pending delivery are backfilled; messages
    /// that already reached a terminal/handled state (`Delivered`, `Failed`,
    /// `Cancelled`) are left untouched. Target matching reuses the same
    /// `resolve_target` rules (Agent/AgentName/Role/Task/Team/Broadcast) so no
    /// new matching logic is introduced.
    fn backfill_inbox_for_registered_agent(&mut self, agent_id: &AgentSessionId) {
        let claimable: Vec<MessageId> = self
            .messages
            .values()
            .filter(|message| backfill_eligible_status(&message.delivery_status))
            // A sender never claims its own fan-out message back (echo guard).
            .filter(|message| message.from != MessageSource::Agent(agent_id.clone()))
            .filter(|message| {
                self.resolve_target(&message.to)
                    .map(|ids| ids.iter().any(|id| id == agent_id))
                    .unwrap_or(false)
            })
            .map(|message| message.id.clone())
            .collect();

        if claimable.is_empty() {
            return;
        }

        let inbox = self
            .inboxes
            .entry(agent_id.clone())
            .or_insert_with(|| Inbox {
                agent_id: agent_id.clone(),
                message_ids: Vec::new(),
            });
        for message_id in claimable {
            if !inbox.message_ids.contains(&message_id) {
                inbox.message_ids.push(message_id);
            }
        }
    }

    pub fn create_message(&mut self, input: NewAgentMessage) -> Result<AgentMessage> {
        if input.body.trim().is_empty() {
            return Err(AgentmuxError::UserError(
                "message body must not be empty".to_string(),
            ));
        }

        let input = self.normalize_thread_message(input)?;
        let recipients = self.delivery_recipients(&input)?;
        if recipients.is_empty() {
            return Err(AgentmuxError::UserError(format!(
                "message target '{}' resolved to no agents other than the sender",
                target_label(&input.to)
            )));
        }
        let message = AgentMessage::new(input);
        for agent_id in recipients {
            self.inboxes
                .entry(agent_id.clone())
                .or_insert_with(|| Inbox {
                    agent_id,
                    message_ids: Vec::new(),
                })
                .message_ids
                .push(message.id.clone());
        }
        self.messages.insert(message.id.clone(), message.clone());
        Ok(message)
    }

    /// Like `create_message` but stores the message even when no agents are
    /// currently registered for the target. The message is persisted with no
    /// inbox entry; when a matching agent later registers,
    /// `register_agent` backfills the message into that agent's inbox so it
    /// can then be delivered (see `backfill_inbox_for_registered_agent`).
    ///
    /// Use this variant from automated/orchestrator paths where a target agent
    /// may not yet exist at the time the message is produced (e.g. when
    /// `persist_live_agent_result` runs before the target session is spawned).
    pub fn create_message_allow_no_recipients(
        &mut self,
        input: NewAgentMessage,
    ) -> Result<AgentMessage> {
        if input.body.trim().is_empty() {
            return Err(AgentmuxError::UserError(
                "message body must not be empty".to_string(),
            ));
        }

        // Resolve recipients; empty is acceptable — the message is stored
        // without any inbox entry and is backfilled when a matching agent
        // registers later (see `backfill_inbox_for_registered_agent`).
        let input = self.normalize_thread_message(input)?;
        let recipients = match self.delivery_recipients(&input) {
            Ok(ids) => ids,
            Err(AgentmuxError::UserError(ref msg)) if msg.contains("resolved to no agents") => {
                vec![]
            }
            Err(other) => return Err(other),
        };
        let message = AgentMessage::new(input);
        for agent_id in recipients {
            self.inboxes
                .entry(agent_id.clone())
                .or_insert_with(|| Inbox {
                    agent_id,
                    message_ids: Vec::new(),
                })
                .message_ids
                .push(message.id.clone());
        }
        self.messages.insert(message.id.clone(), message.clone());
        Ok(message)
    }

    pub fn get_message(&self, id: &MessageId) -> Option<&AgentMessage> {
        self.messages.get(id)
    }

    pub fn list_messages(&self) -> Vec<&AgentMessage> {
        self.messages.values().collect()
    }

    pub fn attach_context_ref(
        &mut self,
        id: &MessageId,
        context_id: ContextItemId,
    ) -> Result<AgentMessage> {
        let message = self.message_mut(id)?;
        if !message.context_refs.contains(&context_id) {
            message.context_refs.push(context_id);
        }
        Ok(message.clone())
    }

    pub fn update_delivery_status(
        &mut self,
        id: &MessageId,
        status: DeliveryStatus,
        now: DateTimeUtc,
    ) -> Result<()> {
        let message = self.message_mut(id)?;
        message.set_delivery_status(status, now);
        Ok(())
    }

    pub fn mark_read(&mut self, id: &MessageId, now: DateTimeUtc) -> Result<()> {
        let message = self.message_mut(id)?;
        message.mark_read(now);
        Ok(())
    }

    pub fn delete_message(&mut self, id: &MessageId) -> Result<AgentMessage> {
        let message = self
            .messages
            .remove(id)
            .ok_or_else(|| unknown_message(id))?;
        for inbox in self.inboxes.values_mut() {
            inbox.message_ids.retain(|message_id| message_id != id);
        }
        Ok(message)
    }

    pub fn inbox(&self, agent_id: &AgentSessionId) -> Result<Vec<&AgentMessage>> {
        let inbox = self
            .inboxes
            .get(agent_id)
            .ok_or_else(|| unknown_agent(agent_id))?;
        Ok(inbox
            .message_ids
            .iter()
            .filter_map(|message_id| self.messages.get(message_id))
            .collect())
    }

    pub fn prepare_next_inject_when_idle(
        &mut self,
        agent_id: &AgentSessionId,
        status: &AgentStatus,
        provider: AgentProvider,
        context: &PromptContext,
        now: DateTimeUtc,
    ) -> Result<IdleDelivery> {
        let message_id = match self.next_inject_when_idle_message_id(agent_id)? {
            Some(message_id) => message_id,
            None => {
                return Ok(IdleDelivery::Waiting(DeliveryWait {
                    agent_id: agent_id.clone(),
                    reason: DeliveryWaitReason::NoInjectWhenIdleMessage,
                }));
            }
        };

        if !agent_accepts_idle_injection(status) {
            self.update_delivery_status(&message_id, DeliveryStatus::WaitingForAgent, now)?;
            return Ok(IdleDelivery::Waiting(DeliveryWait {
                agent_id: agent_id.clone(),
                reason: wait_reason_for_status(status),
            }));
        }

        let message = self
            .messages
            .get(&message_id)
            .ok_or_else(|| unknown_message(&message_id))?;
        let thread = message
            .thread_id
            .as_ref()
            .and_then(|thread_id| self.threads.get(thread_id));
        let prompt = render_prompt(message, provider, context, thread);
        self.update_delivery_status(&message_id, DeliveryStatus::Injecting, now)?;

        Ok(IdleDelivery::Ready(PreparedInjection {
            message_id,
            agent_id: agent_id.clone(),
            prompt,
        }))
    }

    pub fn next_inject_when_idle_message(
        &self,
        agent_id: &AgentSessionId,
    ) -> Result<Option<&AgentMessage>> {
        let Some(message_id) = self.next_inject_when_idle_message_id(agent_id)? else {
            return Ok(None);
        };
        Ok(self.messages.get(&message_id))
    }

    /// Record a completed injection into `agent_id`. The aggregate status
    /// becomes `Delivered` (= at least one recipient), while `delivered_to`
    /// tracks per-recipient progress so fan-out targets (role/team/thread/
    /// broadcast) still inject into the remaining recipients.
    pub fn mark_message_injected(
        &mut self,
        id: &MessageId,
        agent_id: &AgentSessionId,
        now: DateTimeUtc,
    ) -> Result<()> {
        let message = self.message_mut(id)?;
        message.delivered_to.insert(agent_id.clone());
        message.set_delivery_status(DeliveryStatus::Delivered, now);
        Ok(())
    }

    pub fn mark_message_injection_failed(
        &mut self,
        id: &MessageId,
        now: DateTimeUtc,
    ) -> Result<()> {
        self.update_delivery_status(id, DeliveryStatus::Failed, now)
    }

    pub fn resolve_target(&self, target: &MessageTarget) -> Result<Vec<AgentSessionId>> {
        let recipients: Vec<AgentSessionId> = match target {
            MessageTarget::Agent(agent_id) => {
                if self.agents.contains_key(agent_id) {
                    vec![agent_id.clone()]
                } else {
                    Vec::new()
                }
            }
            MessageTarget::AgentName(name) => self
                .agents
                .values()
                .filter(|agent| agent.name.as_deref() == Some(name.as_str()))
                .map(|agent| agent.id.clone())
                .collect(),
            MessageTarget::Role(role) => self
                .agents
                .values()
                .filter(|agent| &agent.role == role)
                .map(|agent| agent.id.clone())
                .collect(),
            MessageTarget::Task(task_id) => self
                .agents
                .values()
                .filter(|agent| agent.task_id.as_ref() == Some(task_id))
                .map(|agent| agent.id.clone())
                .collect(),
            MessageTarget::Team(team) => self
                .agents
                .values()
                .filter(|agent| agent.teams.contains(team))
                .map(|agent| agent.id.clone())
                .collect(),
            MessageTarget::Thread(thread_id) => self
                .threads
                .get(thread_id)
                .map(|thread| {
                    thread
                        .participants
                        .iter()
                        .filter(|id| self.agents.contains_key(*id))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
            MessageTarget::Broadcast => self.agents.keys().cloned().collect(),
        };

        if recipients.is_empty() {
            return Err(AgentmuxError::UserError(format!(
                "message target '{}' resolved to no agents",
                target_label(target)
            )));
        }
        Ok(recipients)
    }

    /// Open a multi-party conversation thread (meeting).
    pub fn open_thread(&mut self, input: NewMessageThread) -> Result<MessageThread> {
        if input.topic.trim().is_empty() {
            return Err(AgentmuxError::UserError(
                "thread topic must not be empty".to_string(),
            ));
        }
        if input.participants.len() < 2 {
            return Err(AgentmuxError::UserError(
                "a thread needs at least 2 participants".to_string(),
            ));
        }
        for participant in &input.participants {
            if !self.agents.contains_key(participant) {
                return Err(unknown_agent(participant));
            }
        }
        let thread = MessageThread::new(input);
        self.threads.insert(thread.id.clone(), thread.clone());
        Ok(thread)
    }

    pub fn close_thread(&mut self, id: &ThreadId, now: DateTimeUtc) -> Result<MessageThread> {
        let thread = self.threads.get_mut(id).ok_or_else(|| unknown_thread(id))?;
        thread.close(now);
        Ok(thread.clone())
    }

    pub fn get_thread(&self, id: &ThreadId) -> Option<&MessageThread> {
        self.threads.get(id)
    }

    pub fn list_threads(&self) -> Vec<&MessageThread> {
        self.threads.values().collect()
    }

    /// Number of stored messages that belong to `thread_id`.
    pub fn thread_message_count(&self, thread_id: &ThreadId) -> usize {
        self.messages
            .values()
            .filter(|message| message.thread_id.as_ref() == Some(thread_id))
            .count()
    }

    /// Validate thread membership/limits and tag the message with its thread.
    ///
    /// - `to: Thread(id)` implies `thread_id = id`.
    /// - Messages into a thread require the thread to exist and be open.
    /// - An agent sender must be a participant and under the per-participant
    ///   message limit (loop guard); the limit error tells the agent to
    ///   summarize and ask the human instead of continuing.
    fn normalize_thread_message(&self, mut input: NewAgentMessage) -> Result<NewAgentMessage> {
        if let MessageTarget::Thread(thread_id) = &input.to {
            input.thread_id = Some(thread_id.clone());
        }
        let Some(thread_id) = input.thread_id.clone() else {
            return Ok(input);
        };
        let thread = self
            .threads
            .get(&thread_id)
            .ok_or_else(|| unknown_thread(&thread_id))?;
        if thread.status == ThreadStatus::Closed {
            return Err(AgentmuxError::UserError(format!(
                "thread '{thread_id}' is closed"
            )));
        }
        if let MessageSource::Agent(sender) = &input.from {
            if !thread.is_participant(sender) {
                return Err(AgentmuxError::UserError(format!(
                    "agent '{sender}' is not a participant of thread '{thread_id}'"
                )));
            }
            let sent = self
                .messages
                .values()
                .filter(|message| message.thread_id.as_ref() == Some(&thread_id))
                .filter(|message| message.from == input.from)
                .count();
            if sent >= thread.max_messages_per_participant as usize {
                return Err(AgentmuxError::UserError(format!(
                    "thread '{thread_id}' message limit reached ({} per participant): \
                     summarize your conclusion and ask the human for a decision \
                     (kind: Question, outside the thread) instead of continuing",
                    thread.max_messages_per_participant
                )));
            }
        }
        Ok(input)
    }

    /// Resolve the target and drop the sender from fan-out recipients so an
    /// agent never receives its own role/team/thread/broadcast message back.
    fn delivery_recipients(&self, input: &NewAgentMessage) -> Result<Vec<AgentSessionId>> {
        let recipients = self.resolve_target(&input.to)?;
        Ok(exclude_sender_from_fan_out(
            recipients,
            &input.from,
            &input.to,
        ))
    }

    fn message_mut(&mut self, id: &MessageId) -> Result<&mut AgentMessage> {
        self.messages.get_mut(id).ok_or_else(|| unknown_message(id))
    }

    fn next_inject_when_idle_message_id(
        &self,
        agent_id: &AgentSessionId,
    ) -> Result<Option<MessageId>> {
        let inbox = self
            .inboxes
            .get(agent_id)
            .ok_or_else(|| unknown_agent(agent_id))?;

        Ok(inbox.message_ids.iter().find_map(|message_id| {
            let message = self.messages.get(message_id)?;
            // `Delivered` stays eligible for recipients the message has not
            // reached yet (fan-out targets inject once per recipient);
            // `delivered_to` prevents re-injecting into the same session.
            if message.delivery_mode == DeliveryMode::InjectWhenIdle
                && !message.delivered_to.contains(agent_id)
                && matches!(
                    message.delivery_status,
                    DeliveryStatus::Queued
                        | DeliveryStatus::Rendered
                        | DeliveryStatus::WaitingForAgent
                        | DeliveryStatus::Delivered
                )
            {
                Some(message_id.clone())
            } else {
                None
            }
        }))
    }
}
