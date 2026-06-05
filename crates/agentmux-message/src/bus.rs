//! In-memory typed message bus.
//!
//! The daemon owns process and persistence boundaries; this module keeps the
//! v0.1 message behavior pure and unit-testable.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use agentmux_core::{
    AgentProvider, AgentRole, AgentSessionId, AgentStatus, AgentmuxError, ContextItemId,
    DateTimeUtc, DeliveryMode, DeliveryStatus, MessageId, Priority, TaskId, ThreadId,
    error::Result,
};

use crate::message::{AgentMessage, MessageKind, MessageSource, MessageTarget, NewAgentMessage};
use crate::thread::{MessageThread, NewMessageThread, ThreadStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDescriptor {
    pub id: AgentSessionId,
    pub name: Option<String>,
    pub role: AgentRole,
    pub task_id: Option<TaskId>,
    pub teams: BTreeSet<String>,
}

impl AgentDescriptor {
    pub fn new(id: AgentSessionId, role: AgentRole) -> Self {
        Self {
            id,
            name: None,
            role,
            task_id: None,
            teams: BTreeSet::new(),
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn with_task_id(mut self, task_id: TaskId) -> Self {
        self.task_id = Some(task_id);
        self
    }

    pub fn with_team(mut self, team: impl Into<String>) -> Self {
        self.teams.insert(team.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inbox {
    pub agent_id: AgentSessionId,
    pub message_ids: Vec<MessageId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedInjection {
    pub message_id: MessageId,
    pub agent_id: AgentSessionId,
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryWait {
    pub agent_id: AgentSessionId,
    pub reason: DeliveryWaitReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryWaitReason {
    AgentBusy,
    AgentNeedsHuman,
    NoInjectWhenIdleMessage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdleDelivery {
    Ready(PreparedInjection),
    Waiting(DeliveryWait),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptContext {
    pub inline_items: Vec<PromptContextItem>,
    pub mailbox_paths: Vec<PathBuf>,
}

impl PromptContext {
    pub fn empty() -> Self {
        Self {
            inline_items: Vec::new(),
            mailbox_paths: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptContextItem {
    pub title: String,
    pub body: String,
}

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

pub fn render_prompt(
    message: &AgentMessage,
    provider: AgentProvider,
    context: &PromptContext,
    thread: Option<&MessageThread>,
) -> String {
    let mut rendered = format!(
        "[agentmux handoff]\nfrom: {}\nkind: {}\npriority: {}\nmessage_id: {}\n",
        source_label(&message.from),
        kind_label(&message.kind),
        priority_label(&message.priority),
        message.id,
    );
    if let Some(thread) = thread {
        rendered.push_str(&format!("thread: {}\ntopic: {}\n", thread.id, thread.topic));
    }
    rendered.push_str(&format!(
        "\nmessage:\n{}\n\nattached context:\n",
        message.body
    ));

    if context.inline_items.is_empty() && context.mailbox_paths.is_empty() {
        rendered.push_str("- none\n");
    } else {
        for item in &context.inline_items {
            rendered.push_str(&format!("- {}: {}\n", item.title, item.body));
        }
        for path in &context.mailbox_paths {
            rendered.push_str(&format!("- {}\n", path.display()));
        }
    }

    rendered.push_str("\nrequired:\n");
    if !context.mailbox_paths.is_empty() {
        rendered.push_str("- attached context の path を必要に応じて読んでください\n");
    }
    if let Some(thread) = thread {
        rendered.push_str(&format!(
            "- この会議スレッドへの返信は `agentmux message send --thread {} --kind <Kind> \"<body>\"` を使ってください(自分以外の参加者全員に届きます)\n",
            thread.id
        ));
        rendered.push_str(&format!(
            "- 発言上限は 1 参加者あたり {} 通です。上限に達したら結論を要約し、スレッド外で人間に判断を仰いでください\n",
            thread.max_messages_per_participant
        ));
    }
    rendered.push_str("- 内容を読んで必要なら作業してください\n");
    rendered.push_str("- 通常の返信や進捗共有では送信前に人間確認を求めないでください\n");
    rendered.push_str("- 完了時は必ず AGENTMUX_RESULT JSON を出力してください\n");

    match provider {
        AgentProvider::Codex => {
            rendered.push_str("\nprovider note: workspace 内の path はそのまま参照してください\n");
        }
        AgentProvider::ClaudeCode => {
            rendered.push_str("\nprovider note: mailbox file は作業ディレクトリから読めます\n");
        }
        AgentProvider::Shell | AgentProvider::Custom(_) => {}
    }

    rendered
}

/// Drop the sender from fan-out recipients. Explicitly addressed targets
/// (`Agent` / `AgentName`) are kept as-is — only one-to-many targets
/// (role/task/team/thread/broadcast) exclude the sender, so an agent never
/// gets its own meeting statement injected back (echo loop guard).
fn exclude_sender_from_fan_out(
    recipients: Vec<AgentSessionId>,
    from: &MessageSource,
    to: &MessageTarget,
) -> Vec<AgentSessionId> {
    let MessageSource::Agent(sender) = from else {
        return recipients;
    };
    if matches!(to, MessageTarget::Agent(_) | MessageTarget::AgentName(_)) {
        return recipients;
    }
    recipients.into_iter().filter(|id| id != sender).collect()
}

fn unknown_agent(agent_id: &AgentSessionId) -> AgentmuxError {
    AgentmuxError::UserError(format!("unknown agent session '{agent_id}'"))
}

fn unknown_thread(thread_id: &ThreadId) -> AgentmuxError {
    AgentmuxError::UserError(format!("unknown thread '{thread_id}'"))
}

fn unknown_message(message_id: &MessageId) -> AgentmuxError {
    AgentmuxError::UserError(format!("unknown message '{message_id}'"))
}

fn source_label(source: &MessageSource) -> String {
    match source {
        MessageSource::User(id) => format!("user:{id}"),
        MessageSource::Agent(id) => format!("agent:{id}"),
        MessageSource::TeamAgent(name) => format!("team_agent:{name}"),
        MessageSource::Role(role) => format!("role:{role:?}"),
        MessageSource::System => "system".to_string(),
        MessageSource::Orchestrator => "orchestrator".to_string(),
    }
}

fn target_label(target: &MessageTarget) -> String {
    match target {
        MessageTarget::Agent(id) => format!("agent:{id}"),
        MessageTarget::AgentName(name) => format!("agent:{name}"),
        MessageTarget::Role(role) => format!("role:{role:?}"),
        MessageTarget::Task(id) => format!("task:{id}"),
        MessageTarget::Team(team) => format!("team:{team}"),
        MessageTarget::Thread(id) => format!("thread:{id}"),
        MessageTarget::Broadcast => "broadcast".to_string(),
    }
}

fn kind_label(kind: &MessageKind) -> &'static str {
    match kind {
        MessageKind::TaskAssignment => "TaskAssignment",
        MessageKind::Question => "Question",
        MessageKind::Finding => "Finding",
        MessageKind::PatchProposal => "PatchProposal",
        MessageKind::ReviewComment => "ReviewComment",
        MessageKind::TestResult => "TestResult",
        MessageKind::FailureReport => "FailureReport",
        MessageKind::Decision => "Decision",
        MessageKind::Handoff => "Handoff",
        MessageKind::ApprovalRequest => "ApprovalRequest",
        MessageKind::ContextUpdate => "ContextUpdate",
        MessageKind::StatusProbe => "StatusProbe",
    }
}

fn priority_label(priority: &Priority) -> &'static str {
    match priority {
        Priority::Low => "Low",
        Priority::Normal => "Normal",
        Priority::High => "High",
        Priority::Urgent => "Urgent",
    }
}

/// A message is eligible for inbox backfill (when its target agent registers
/// later) only while it is still pending delivery. Terminal/handled states are
/// excluded so a delivered or cancelled message is never re-queued.
fn backfill_eligible_status(status: &DeliveryStatus) -> bool {
    match status {
        DeliveryStatus::Queued
        | DeliveryStatus::Rendered
        | DeliveryStatus::WaitingForAgent
        | DeliveryStatus::WaitingForApproval => true,
        DeliveryStatus::Injecting
        | DeliveryStatus::Delivered
        | DeliveryStatus::Failed
        | DeliveryStatus::Cancelled => false,
    }
}

fn agent_accepts_idle_injection(status: &AgentStatus) -> bool {
    matches!(
        status,
        AgentStatus::AwaitingInput | AgentStatus::InteractiveReady | AgentStatus::CompletedTurn
    )
}

fn wait_reason_for_status(status: &AgentStatus) -> DeliveryWaitReason {
    match status {
        AgentStatus::NeedsHuman | AgentStatus::Stalled | AgentStatus::AwaitingApproval => {
            DeliveryWaitReason::AgentNeedsHuman
        }
        _ => DeliveryWaitReason::AgentBusy,
    }
}

pub fn initial_delivery_status(delivery_mode: &DeliveryMode) -> DeliveryStatus {
    match delivery_mode {
        DeliveryMode::RequireHumanApproval => DeliveryStatus::WaitingForApproval,
        DeliveryMode::InboxOnly
        | DeliveryMode::InjectWhenIdle
        | DeliveryMode::InjectImmediately => DeliveryStatus::Queued,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentmux_core::{ClientId, ContextItemId, DeliveryMode};

    fn message_input(to: MessageTarget, delivery_mode: DeliveryMode) -> NewAgentMessage {
        NewAgentMessage {
            task_id: None,
            thread_id: None,
            from: MessageSource::User(ClientId::new()),
            to,
            kind: MessageKind::Handoff,
            priority: Priority::High,
            body: "Please review this patch.".to_string(),
            context_refs: vec![ContextItemId::new()],
            artifact_refs: Vec::new(),
            delivery_mode,
            requires_response: true,
        }
    }

    #[test]
    fn create_message_resolves_role_target_and_places_message_in_inbox() {
        let mut bus = MessageBus::new();
        let implementer = AgentSessionId::new();
        let reviewer = AgentSessionId::new();
        bus.register_agent(AgentDescriptor::new(
            implementer.clone(),
            AgentRole::Implementer,
        ));
        bus.register_agent(AgentDescriptor::new(reviewer.clone(), AgentRole::Reviewer));

        let message = bus
            .create_message(message_input(
                MessageTarget::Role(AgentRole::Implementer),
                DeliveryMode::InboxOnly,
            ))
            .expect("message is created");

        assert_eq!(message.delivery_status, DeliveryStatus::Queued);
        assert_eq!(bus.inbox(&implementer).unwrap()[0].id, message.id);
        assert!(bus.inbox(&reviewer).unwrap().is_empty());
        assert_eq!(bus.get_message(&message.id).unwrap().body, message.body);
    }

    #[test]
    fn registering_agent_backfills_messages_stored_before_it_existed() {
        let mut bus = MessageBus::new();

        // A message addressed to role:tester is created before any tester
        // session exists — stored with no inbox entry.
        let message = bus
            .create_message_allow_no_recipients(message_input(
                MessageTarget::Role(AgentRole::Tester),
                DeliveryMode::InjectWhenIdle,
            ))
            .expect("message is stored even with no recipient");
        assert_eq!(
            message.delivery_status,
            DeliveryStatus::Queued,
            "an unroutable message starts Queued"
        );

        // No tester inbox yet, so the message lives only in the store.
        assert_eq!(bus.list_messages().len(), 1);

        // The tester registers later …
        let tester = AgentSessionId::new();
        bus.register_agent(AgentDescriptor::new(tester.clone(), AgentRole::Tester));

        // … and the previously-stored message is backfilled into its inbox.
        let inbox = bus.inbox(&tester).expect("tester has an inbox");
        assert_eq!(inbox.len(), 1, "queued message is backfilled on register");
        assert_eq!(inbox[0].id, message.id);

        // Registering again (or a second matching agent) must not duplicate the
        // backfilled message in the same inbox.
        bus.register_agent(AgentDescriptor::new(tester.clone(), AgentRole::Tester));
        assert_eq!(
            bus.inbox(&tester).unwrap().len(),
            1,
            "re-registering must not duplicate the backfilled message"
        );

        // A non-matching role (reviewer) must not claim the tester message.
        let reviewer = AgentSessionId::new();
        bus.register_agent(AgentDescriptor::new(reviewer.clone(), AgentRole::Reviewer));
        assert!(
            bus.inbox(&reviewer).unwrap().is_empty(),
            "a non-matching agent must not receive the backfilled message"
        );
    }

    #[test]
    fn backfill_skips_already_delivered_messages() {
        let mut bus = MessageBus::new();
        let now = DateTimeUtc::now_utc();

        let message = bus
            .create_message_allow_no_recipients(message_input(
                MessageTarget::Role(AgentRole::Tester),
                DeliveryMode::InjectWhenIdle,
            ))
            .expect("message is stored");

        // Mark it Delivered before the tester ever registers (e.g. it was
        // delivered to a different matching session earlier).
        bus.update_delivery_status(&message.id, DeliveryStatus::Delivered, now)
            .expect("status updates");

        let tester = AgentSessionId::new();
        bus.register_agent(AgentDescriptor::new(tester.clone(), AgentRole::Tester));

        assert!(
            bus.inbox(&tester).unwrap().is_empty(),
            "a Delivered message must not be backfilled"
        );
    }

    #[test]
    fn target_resolution_supports_agent_task_team_and_broadcast() {
        let mut bus = MessageBus::new();
        let task_id = TaskId::new();
        let planner = AgentSessionId::new();
        let tester = AgentSessionId::new();
        bus.register_agent(
            AgentDescriptor::new(planner.clone(), AgentRole::Planner)
                .with_name("planner-a1b2c3")
                .with_task_id(task_id.clone())
                .with_team("alpha"),
        );
        bus.register_agent(AgentDescriptor::new(tester.clone(), AgentRole::Tester).with_team("qa"));

        assert_eq!(
            bus.resolve_target(&MessageTarget::Agent(planner.clone()))
                .unwrap(),
            vec![planner.clone()]
        );
        assert_eq!(
            bus.resolve_target(&MessageTarget::AgentName("planner-a1b2c3".to_string()))
                .unwrap(),
            vec![planner.clone()]
        );
        assert_eq!(
            bus.resolve_target(&MessageTarget::Task(task_id)).unwrap(),
            vec![planner.clone()]
        );
        assert_eq!(
            bus.resolve_target(&MessageTarget::Team("qa".to_string()))
                .unwrap(),
            vec![tester.clone()]
        );
        let broadcast: BTreeSet<_> = bus
            .resolve_target(&MessageTarget::Broadcast)
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(broadcast, BTreeSet::from([planner, tester]));
    }

    #[test]
    fn require_human_approval_starts_waiting_for_approval() {
        assert_eq!(
            initial_delivery_status(&DeliveryMode::RequireHumanApproval),
            DeliveryStatus::WaitingForApproval
        );

        let mut bus = MessageBus::new();
        let agent_id = AgentSessionId::new();
        bus.register_agent(AgentDescriptor::new(agent_id.clone(), AgentRole::Reviewer));
        let message = bus
            .create_message(message_input(
                MessageTarget::Agent(agent_id),
                DeliveryMode::RequireHumanApproval,
            ))
            .expect("message is created");

        assert_eq!(message.delivery_status, DeliveryStatus::WaitingForApproval);
    }

    #[test]
    fn delivery_status_and_read_time_are_mutable_crud_fields() {
        let mut bus = MessageBus::new();
        let agent_id = AgentSessionId::new();
        bus.register_agent(AgentDescriptor::new(agent_id.clone(), AgentRole::Tester));
        let message = bus
            .create_message(message_input(
                MessageTarget::Agent(agent_id),
                DeliveryMode::InjectWhenIdle,
            ))
            .expect("message is created");
        let now = DateTimeUtc::UNIX_EPOCH;

        bus.update_delivery_status(&message.id, DeliveryStatus::Delivered, now)
            .expect("status is updated");
        bus.mark_read(&message.id, now)
            .expect("message is marked read");

        let updated = bus.get_message(&message.id).unwrap();
        assert_eq!(updated.delivery_status, DeliveryStatus::Delivered);
        assert_eq!(updated.delivered_at, Some(now));
        assert_eq!(updated.read_at, Some(now));
    }

    #[test]
    fn inject_when_idle_prepares_prompt_and_marks_message_injecting() {
        let mut bus = MessageBus::new();
        let agent_id = AgentSessionId::new();
        bus.register_agent(AgentDescriptor::new(
            agent_id.clone(),
            AgentRole::Implementer,
        ));
        let message = bus
            .create_message(message_input(
                MessageTarget::Agent(agent_id.clone()),
                DeliveryMode::InjectWhenIdle,
            ))
            .expect("message is created");
        let now = DateTimeUtc::UNIX_EPOCH;

        let delivery = bus
            .prepare_next_inject_when_idle(
                &agent_id,
                &AgentStatus::AwaitingInput,
                AgentProvider::Codex,
                &PromptContext::empty(),
                now,
            )
            .expect("idle message is prepared");

        match delivery {
            IdleDelivery::Ready(prepared) => {
                assert_eq!(prepared.message_id, message.id);
                assert_eq!(prepared.agent_id, agent_id);
                assert!(prepared.prompt.contains("[agentmux handoff]"));
                assert!(
                    prepared
                        .prompt
                        .contains("message:\nPlease review this patch.")
                );
            }
            IdleDelivery::Waiting(wait) => panic!("expected ready delivery, got {wait:?}"),
        }
        assert_eq!(
            bus.get_message(&message.id).unwrap().delivery_status,
            DeliveryStatus::Injecting
        );
    }

    #[test]
    fn inject_when_idle_waits_when_agent_is_busy_or_needs_human() {
        let mut bus = MessageBus::new();
        let agent_id = AgentSessionId::new();
        bus.register_agent(AgentDescriptor::new(
            agent_id.clone(),
            AgentRole::Implementer,
        ));
        let message = bus
            .create_message(message_input(
                MessageTarget::Agent(agent_id.clone()),
                DeliveryMode::InjectWhenIdle,
            ))
            .expect("message is created");
        let now = DateTimeUtc::UNIX_EPOCH;

        let busy = bus
            .prepare_next_inject_when_idle(
                &agent_id,
                &AgentStatus::RunningTurn,
                AgentProvider::Codex,
                &PromptContext::empty(),
                now,
            )
            .expect("busy agent is a wait decision");

        assert_eq!(
            busy,
            IdleDelivery::Waiting(DeliveryWait {
                agent_id: agent_id.clone(),
                reason: DeliveryWaitReason::AgentBusy,
            })
        );
        assert_eq!(
            bus.get_message(&message.id).unwrap().delivery_status,
            DeliveryStatus::WaitingForAgent
        );

        let needs_human = bus
            .prepare_next_inject_when_idle(
                &agent_id,
                &AgentStatus::Stalled,
                AgentProvider::Codex,
                &PromptContext::empty(),
                now,
            )
            .expect("stalled agent is a wait decision");

        assert_eq!(
            needs_human,
            IdleDelivery::Waiting(DeliveryWait {
                agent_id,
                reason: DeliveryWaitReason::AgentNeedsHuman,
            })
        );
    }

    #[test]
    fn injection_result_helpers_manage_delivered_and_failed_status() {
        let mut bus = MessageBus::new();
        let agent_id = AgentSessionId::new();
        bus.register_agent(AgentDescriptor::new(agent_id.clone(), AgentRole::Tester));
        let delivered = bus
            .create_message(message_input(
                MessageTarget::Agent(agent_id.clone()),
                DeliveryMode::InjectWhenIdle,
            ))
            .expect("message is created");
        let failed = bus
            .create_message(message_input(
                MessageTarget::Agent(agent_id.clone()),
                DeliveryMode::InjectWhenIdle,
            ))
            .expect("message is created");
        let now = DateTimeUtc::UNIX_EPOCH;

        bus.mark_message_injected(&delivered.id, &agent_id, now)
            .expect("delivered status is recorded");
        bus.mark_message_injection_failed(&failed.id, now)
            .expect("failed status is recorded");

        let delivered = bus.get_message(&delivered.id).unwrap();
        let failed = bus.get_message(&failed.id).unwrap();
        assert_eq!(delivered.delivery_status, DeliveryStatus::Delivered);
        assert_eq!(delivered.delivered_at, Some(now));
        assert_eq!(failed.delivery_status, DeliveryStatus::Failed);
        assert_eq!(failed.delivered_at, None);
    }

    #[test]
    fn non_inject_when_idle_messages_are_not_prepared_for_idle_injection() {
        let mut bus = MessageBus::new();
        let agent_id = AgentSessionId::new();
        bus.register_agent(AgentDescriptor::new(agent_id.clone(), AgentRole::Tester));
        bus.create_message(message_input(
            MessageTarget::Agent(agent_id.clone()),
            DeliveryMode::InboxOnly,
        ))
        .expect("message is created");

        let delivery = bus
            .prepare_next_inject_when_idle(
                &agent_id,
                &AgentStatus::AwaitingInput,
                AgentProvider::Codex,
                &PromptContext::empty(),
                DateTimeUtc::UNIX_EPOCH,
            )
            .expect("inbox only message is ignored");

        assert_eq!(
            delivery,
            IdleDelivery::Waiting(DeliveryWait {
                agent_id,
                reason: DeliveryWaitReason::NoInjectWhenIdleMessage,
            })
        );
    }

    #[test]
    fn delete_message_removes_it_from_inboxes() {
        let mut bus = MessageBus::new();
        let agent_id = AgentSessionId::new();
        bus.register_agent(AgentDescriptor::new(agent_id.clone(), AgentRole::Tester));
        let message = bus
            .create_message(message_input(
                MessageTarget::Agent(agent_id.clone()),
                DeliveryMode::InboxOnly,
            ))
            .expect("message is created");

        let deleted = bus.delete_message(&message.id).expect("message is deleted");

        assert_eq!(deleted.id, message.id);
        assert!(bus.get_message(&message.id).is_none());
        assert!(bus.inbox(&agent_id).unwrap().is_empty());
    }

    #[test]
    fn prompt_renderer_includes_message_context_paths_and_provider_note() {
        let message = AgentMessage::new(message_input(
            MessageTarget::Role(AgentRole::Implementer),
            DeliveryMode::InjectWhenIdle,
        ));
        let context = PromptContext {
            inline_items: vec![PromptContextItem {
                title: "Decision".to_string(),
                body: "Keep the public API stable.".to_string(),
            }],
            mailbox_paths: vec![PathBuf::from(".agentmux/inbox/impl-codex/msg-00042.md")],
        };

        let prompt = render_prompt(&message, AgentProvider::Codex, &context, None);

        assert!(prompt.contains("[agentmux handoff]"));
        assert!(prompt.contains("kind: Handoff"));
        assert!(prompt.contains("priority: High"));
        assert!(prompt.contains("message:\nPlease review this patch."));
        assert!(prompt.contains("- Decision: Keep the public API stable."));
        assert!(prompt.contains("- .agentmux/inbox/impl-codex/msg-00042.md"));
        assert!(prompt.contains("AGENTMUX_RESULT JSON"));
        assert!(prompt.contains("送信前に人間確認を求めない"));
        assert!(!prompt.contains("内容を確認してください"));
        assert!(prompt.contains("workspace 内の path"));
    }

    fn three_party_bus() -> (MessageBus, AgentSessionId, AgentSessionId, AgentSessionId) {
        let mut bus = MessageBus::new();
        let claude = AgentSessionId::new();
        let codex = AgentSessionId::new();
        let agy = AgentSessionId::new();
        bus.register_agent(
            AgentDescriptor::new(claude.clone(), AgentRole::Implementer).with_name("claude-a"),
        );
        bus.register_agent(
            AgentDescriptor::new(codex.clone(), AgentRole::Reviewer).with_name("codex-b"),
        );
        bus.register_agent(AgentDescriptor::new(agy.clone(), AgentRole::Tester).with_name("agy-c"));
        (bus, claude, codex, agy)
    }

    fn thread_input(
        participants: Vec<AgentSessionId>,
        max_messages_per_participant: Option<u32>,
    ) -> NewMessageThread {
        NewMessageThread {
            topic: "X の設計方針".to_string(),
            participants,
            opened_by: MessageSource::User(ClientId::new()),
            max_messages_per_participant,
        }
    }

    #[test]
    fn thread_message_fans_out_to_all_participants_except_sender() {
        let (mut bus, claude, codex, agy) = three_party_bus();
        let thread = bus
            .open_thread(thread_input(
                vec![claude.clone(), codex.clone(), agy.clone()],
                None,
            ))
            .expect("thread opens");

        let mut input = message_input(
            MessageTarget::Thread(thread.id.clone()),
            DeliveryMode::InjectWhenIdle,
        );
        input.from = MessageSource::Agent(claude.clone());
        let message = bus.create_message(input).expect("thread message accepted");

        assert_eq!(message.thread_id, Some(thread.id.clone()));
        assert!(
            bus.inbox(&claude).unwrap().is_empty(),
            "sender must not receive its own thread message"
        );
        assert_eq!(bus.inbox(&codex).unwrap()[0].id, message.id);
        assert_eq!(bus.inbox(&agy).unwrap()[0].id, message.id);
        assert_eq!(bus.thread_message_count(&thread.id), 1);
    }

    #[test]
    fn broadcast_and_role_fan_out_exclude_the_sending_agent() {
        let (mut bus, claude, codex, agy) = three_party_bus();

        let mut input = message_input(MessageTarget::Broadcast, DeliveryMode::InjectWhenIdle);
        input.from = MessageSource::Agent(claude.clone());
        bus.create_message(input).expect("broadcast accepted");

        assert!(bus.inbox(&claude).unwrap().is_empty());
        assert_eq!(bus.inbox(&codex).unwrap().len(), 1);
        assert_eq!(bus.inbox(&agy).unwrap().len(), 1);

        // A role target that resolves only to the sender is an error instead
        // of a silent self-delivery.
        let mut input = message_input(
            MessageTarget::Role(AgentRole::Implementer),
            DeliveryMode::InjectWhenIdle,
        );
        input.from = MessageSource::Agent(claude.clone());
        let error = bus.create_message(input).expect_err("self-only fan-out");
        assert!(error.to_string().contains("other than the sender"));
    }

    #[test]
    fn thread_enforces_participants_limit_and_close() {
        let (mut bus, claude, codex, agy) = three_party_bus();
        let thread = bus
            .open_thread(thread_input(vec![claude.clone(), codex.clone()], Some(1)))
            .expect("thread opens");

        // Non-participant agents cannot post.
        let mut outsider = message_input(
            MessageTarget::Thread(thread.id.clone()),
            DeliveryMode::InjectWhenIdle,
        );
        outsider.from = MessageSource::Agent(agy.clone());
        assert!(
            bus.create_message(outsider)
                .expect_err("outsider rejected")
                .to_string()
                .contains("not a participant")
        );

        // First message is fine; the second hits the per-participant limit.
        let mut first = message_input(
            MessageTarget::Thread(thread.id.clone()),
            DeliveryMode::InjectWhenIdle,
        );
        first.from = MessageSource::Agent(claude.clone());
        bus.create_message(first).expect("first message accepted");

        let mut second = message_input(
            MessageTarget::Thread(thread.id.clone()),
            DeliveryMode::InjectWhenIdle,
        );
        second.from = MessageSource::Agent(claude.clone());
        let error = bus.create_message(second).expect_err("limit reached");
        assert!(error.to_string().contains("message limit reached"));

        // The user is not turn-limited (moderator can keep steering) …
        bus.create_message(message_input(
            MessageTarget::Thread(thread.id.clone()),
            DeliveryMode::InjectWhenIdle,
        ))
        .expect("user message accepted");

        // … and a closed thread rejects everything.
        bus.close_thread(&thread.id, DateTimeUtc::UNIX_EPOCH)
            .expect("thread closes");
        let error = bus
            .create_message(message_input(
                MessageTarget::Thread(thread.id.clone()),
                DeliveryMode::InjectWhenIdle,
            ))
            .expect_err("closed thread rejects");
        assert!(error.to_string().contains("is closed"));
    }

    #[test]
    fn open_thread_validates_topic_and_participants() {
        let (mut bus, claude, _codex, _agy) = three_party_bus();

        let mut empty_topic = thread_input(vec![claude.clone(), AgentSessionId::new()], None);
        empty_topic.topic = "  ".to_string();
        assert!(bus.open_thread(empty_topic).is_err());

        assert!(
            bus.open_thread(thread_input(vec![claude.clone()], None))
                .expect_err("single participant rejected")
                .to_string()
                .contains("at least 2 participants")
        );

        assert!(
            bus.open_thread(thread_input(vec![claude, AgentSessionId::new()], None))
                .expect_err("unknown participant rejected")
                .to_string()
                .contains("unknown agent session")
        );
    }

    #[test]
    fn fan_out_message_is_injected_once_per_recipient() {
        let (mut bus, claude, codex, agy) = three_party_bus();
        let thread = bus
            .open_thread(thread_input(
                vec![claude.clone(), codex.clone(), agy.clone()],
                None,
            ))
            .expect("thread opens");

        let mut input = message_input(
            MessageTarget::Thread(thread.id.clone()),
            DeliveryMode::InjectWhenIdle,
        );
        input.from = MessageSource::Agent(claude.clone());
        let message = bus.create_message(input).expect("thread message accepted");
        let now = DateTimeUtc::UNIX_EPOCH;

        // First recipient injects and the message becomes Delivered …
        let first = bus
            .prepare_next_inject_when_idle(
                &codex,
                &AgentStatus::AwaitingInput,
                AgentProvider::Codex,
                &PromptContext::empty(),
                now,
            )
            .expect("first delivery prepared");
        assert!(matches!(first, IdleDelivery::Ready(_)));
        bus.mark_message_injected(&message.id, &codex, now)
            .expect("first injection recorded");

        // … but the second recipient must still receive its own injection.
        let second = bus
            .prepare_next_inject_when_idle(
                &agy,
                &AgentStatus::AwaitingInput,
                AgentProvider::Codex,
                &PromptContext::empty(),
                now,
            )
            .expect("second delivery prepared");
        match second {
            IdleDelivery::Ready(prepared) => assert_eq!(prepared.message_id, message.id),
            IdleDelivery::Waiting(wait) => {
                panic!("second recipient must get the fan-out message, got {wait:?}")
            }
        }
        bus.mark_message_injected(&message.id, &agy, now)
            .expect("second injection recorded");

        // Already-served recipients are not re-injected.
        let again = bus
            .prepare_next_inject_when_idle(
                &codex,
                &AgentStatus::AwaitingInput,
                AgentProvider::Codex,
                &PromptContext::empty(),
                now,
            )
            .expect("no duplicate delivery");
        assert_eq!(
            again,
            IdleDelivery::Waiting(DeliveryWait {
                agent_id: codex.clone(),
                reason: DeliveryWaitReason::NoInjectWhenIdleMessage,
            })
        );
    }

    #[test]
    fn thread_prompt_includes_topic_and_reply_instruction() {
        let (mut bus, claude, codex, _agy) = three_party_bus();
        let thread = bus
            .open_thread(thread_input(vec![claude.clone(), codex.clone()], None))
            .expect("thread opens");

        let mut input = message_input(
            MessageTarget::Thread(thread.id.clone()),
            DeliveryMode::InjectWhenIdle,
        );
        input.from = MessageSource::Agent(claude.clone());
        bus.create_message(input).expect("thread message accepted");

        let delivery = bus
            .prepare_next_inject_when_idle(
                &codex,
                &AgentStatus::AwaitingInput,
                AgentProvider::Codex,
                &PromptContext::empty(),
                DateTimeUtc::UNIX_EPOCH,
            )
            .expect("delivery prepared");

        match delivery {
            IdleDelivery::Ready(prepared) => {
                assert!(prepared.prompt.contains(&format!("thread: {}", thread.id)));
                assert!(prepared.prompt.contains("topic: X の設計方針"));
                assert!(prepared.prompt.contains(&format!("--thread {}", thread.id)));
                assert!(prepared.prompt.contains("発言上限"));
            }
            IdleDelivery::Waiting(wait) => panic!("expected ready delivery, got {wait:?}"),
        }
    }

    #[test]
    fn empty_body_and_unresolved_target_are_rejected() {
        let mut bus = MessageBus::new();
        let mut empty = message_input(MessageTarget::Broadcast, DeliveryMode::InboxOnly);
        empty.body = "  ".to_string();
        assert!(bus.create_message(empty).is_err());

        let unresolved = message_input(
            MessageTarget::Role(AgentRole::Implementer),
            DeliveryMode::InboxOnly,
        );
        assert!(bus.create_message(unresolved).is_err());
    }
}
