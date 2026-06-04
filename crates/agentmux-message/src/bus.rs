//! In-memory typed message bus.
//!
//! The daemon owns process and persistence boundaries; this module keeps the
//! v0.1 message behavior pure and unit-testable.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use agentmux_core::{
    AgentProvider, AgentRole, AgentSessionId, AgentStatus, AgentmuxError, ContextItemId,
    DateTimeUtc, DeliveryMode, DeliveryStatus, MessageId, Priority, TaskId, error::Result,
};

use crate::message::{AgentMessage, MessageKind, MessageSource, MessageTarget, NewAgentMessage};

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
}

impl MessageBus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_agent(&mut self, agent: AgentDescriptor) {
        self.inboxes
            .entry(agent.id.clone())
            .or_insert_with(|| Inbox {
                agent_id: agent.id.clone(),
                message_ids: Vec::new(),
            });
        self.agents.insert(agent.id.clone(), agent);
    }

    pub fn create_message(&mut self, input: NewAgentMessage) -> Result<AgentMessage> {
        if input.body.trim().is_empty() {
            return Err(AgentmuxError::UserError(
                "message body must not be empty".to_string(),
            ));
        }

        let recipients = self.resolve_target(&input.to)?;
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
        let prompt = render_prompt(message, provider, context);
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

    pub fn mark_message_injected(&mut self, id: &MessageId, now: DateTimeUtc) -> Result<()> {
        self.update_delivery_status(id, DeliveryStatus::Delivered, now)
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
            if message.delivery_mode == DeliveryMode::InjectWhenIdle
                && matches!(
                    message.delivery_status,
                    DeliveryStatus::Queued
                        | DeliveryStatus::Rendered
                        | DeliveryStatus::WaitingForAgent
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
) -> String {
    let mut rendered = format!(
        "[agentmux handoff]\nfrom: {}\nkind: {}\npriority: {}\nmessage_id: {}\n\nmessage:\n{}\n\nattached context:\n",
        source_label(&message.from),
        kind_label(&message.kind),
        priority_label(&message.priority),
        message.id,
        message.body
    );

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
    rendered.push_str("- 内容を確認してください\n");
    rendered.push_str("- 必要なら作業してください\n");
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

fn unknown_agent(agent_id: &AgentSessionId) -> AgentmuxError {
    AgentmuxError::UserError(format!("unknown agent session '{agent_id}'"))
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
                MessageTarget::Agent(agent_id),
                DeliveryMode::InjectWhenIdle,
            ))
            .expect("message is created");
        let now = DateTimeUtc::UNIX_EPOCH;

        bus.mark_message_injected(&delivered.id, now)
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

        let prompt = render_prompt(&message, AgentProvider::Codex, &context);

        assert!(prompt.contains("[agentmux handoff]"));
        assert!(prompt.contains("kind: Handoff"));
        assert!(prompt.contains("priority: High"));
        assert!(prompt.contains("message:\nPlease review this patch."));
        assert!(prompt.contains("- Decision: Keep the public API stable."));
        assert!(prompt.contains("- .agentmux/inbox/impl-codex/msg-00042.md"));
        assert!(prompt.contains("AGENTMUX_RESULT JSON"));
        assert!(prompt.contains("workspace 内の path"));
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
