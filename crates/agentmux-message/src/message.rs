//! `AgentMessage` and related types.
//!
//! See `docs/spec/03_domain_model.md §8`.

use agentmux_core::{
    AgentRole, AgentSessionId, ArtifactId, ClientId, ContextItemId, DateTimeUtc, DeliveryMode,
    DeliveryStatus, MessageId, Priority, TaskId,
};
use serde::{Deserialize, Serialize};

/// A typed message exchanged between agents, the user, or the orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub id: MessageId,
    pub task_id: Option<TaskId>,
    pub from: MessageSource,
    pub to: MessageTarget,
    pub kind: MessageKind,
    pub priority: Priority,
    /// Plain-text (or Markdown) body.
    pub body: String,
    pub context_refs: Vec<ContextItemId>,
    pub artifact_refs: Vec<ArtifactId>,
    pub delivery_mode: DeliveryMode,
    pub delivery_status: DeliveryStatus,
    pub requires_response: bool,
    pub created_at: DateTimeUtc,
    pub delivered_at: Option<DateTimeUtc>,
    pub read_at: Option<DateTimeUtc>,
}

impl AgentMessage {
    /// Create a queued message with a generated id and current timestamp.
    pub fn new(input: NewAgentMessage) -> Self {
        let delivery_status = match input.delivery_mode {
            DeliveryMode::RequireHumanApproval => DeliveryStatus::WaitingForApproval,
            DeliveryMode::InboxOnly
            | DeliveryMode::InjectWhenIdle
            | DeliveryMode::InjectImmediately => DeliveryStatus::Queued,
        };

        Self {
            id: MessageId::new(),
            task_id: input.task_id,
            from: input.from,
            to: input.to,
            kind: input.kind,
            priority: input.priority,
            body: input.body,
            context_refs: input.context_refs,
            artifact_refs: input.artifact_refs,
            delivery_mode: input.delivery_mode,
            delivery_status,
            requires_response: input.requires_response,
            created_at: DateTimeUtc::now_utc(),
            delivered_at: None,
            read_at: None,
        }
    }

    pub fn set_delivery_status(&mut self, status: DeliveryStatus, now: DateTimeUtc) {
        if status == DeliveryStatus::Delivered {
            self.delivered_at = Some(now);
        }
        self.delivery_status = status;
    }

    pub fn mark_read(&mut self, now: DateTimeUtc) {
        self.read_at = Some(now);
    }
}

/// Input required to create an [`AgentMessage`].
#[derive(Debug, Clone)]
pub struct NewAgentMessage {
    pub task_id: Option<TaskId>,
    pub from: MessageSource,
    pub to: MessageTarget,
    pub kind: MessageKind,
    pub priority: Priority,
    pub body: String,
    pub context_refs: Vec<ContextItemId>,
    pub artifact_refs: Vec<ArtifactId>,
    pub delivery_mode: DeliveryMode,
    pub requires_response: bool,
}

/// Who sent the message.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum MessageSource {
    User(ClientId),
    Agent(AgentSessionId),
    TeamAgent(String),
    Role(AgentRole),
    System,
    Orchestrator,
}

/// Who should receive the message.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum MessageTarget {
    Agent(AgentSessionId),
    Role(AgentRole),
    Task(TaskId),
    Team(String),
    Broadcast,
}

/// Semantic category of the message content.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    TaskAssignment,
    Question,
    Finding,
    PatchProposal,
    ReviewComment,
    TestResult,
    FailureReport,
    Decision,
    Handoff,
    ApprovalRequest,
    ContextUpdate,
    StatusProbe,
}
