//! `AgentMessage` and related types.
//!
//! See `docs/spec/03_domain_model.md §8`.

use agentmux_core::{
    AgentRole, AgentSessionId, ArtifactId, ClientId, ContextItemId,
    DateTimeUtc, DeliveryMode, DeliveryStatus, MessageId, Priority, TaskId,
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

/// Who sent the message.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum MessageSource {
    User(ClientId),
    Agent(AgentSessionId),
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
