use std::str::FromStr;

use agentmux_core::{
    AgentRole, AgentmuxError, ArtifactId, ContextItemId, DeliveryMode, Priority, TaskId,
    error::Result,
};
use agentmux_message::{
    MessageKind, MessageSource, MessageTarget, NewAgentMessage, message::AgentMessage,
};

use crate::result::{OutgoingMessageKind, OutgoingPriority};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrchestratorMessage {
    pub task_id: Option<TaskId>,
    pub from: MessageSource,
    pub to: MessageTarget,
    pub kind: MessageKind,
    pub priority: Priority,
    pub body: String,
    pub delivery_mode: DeliveryMode,
    pub requires_response: bool,
    pub context_refs: Vec<ContextItemId>,
    pub artifact_refs: Vec<ArtifactId>,
}

impl OrchestratorMessage {
    pub fn into_new_agent_message(self) -> NewAgentMessage {
        NewAgentMessage {
            task_id: self.task_id,
            thread_id: None,
            from: self.from,
            to: self.to,
            kind: self.kind,
            priority: self.priority,
            body: self.body,
            context_refs: self.context_refs,
            artifact_refs: self.artifact_refs,
            delivery_mode: self.delivery_mode,
            requires_response: self.requires_response,
        }
    }

    pub fn into_agent_message(self) -> AgentMessage {
        AgentMessage::new(self.into_new_agent_message())
    }
}

pub(crate) fn resolve_result_target(team: &super::TeamTemplate, raw: &str) -> Result<MessageTarget> {
    let target = raw.trim();
    if target.is_empty() {
        return Err(AgentmuxError::OrchestratorError(
            "empty result target".to_string(),
        ));
    }

    if let Some(role) = target.strip_prefix("role:") {
        return parse_role_target(role);
    }
    if let Some(agent) = target.strip_prefix("agent:") {
        let agent = agent.trim();
        if agent.is_empty() {
            return Err(AgentmuxError::OrchestratorError(
                "empty agent result target".to_string(),
            ));
        }
        if let Ok(agent_id) = agent.parse::<agentmux_core::AgentSessionId>() {
            return Ok(MessageTarget::Agent(agent_id));
        }
        return Ok(MessageTarget::AgentName(agent.to_string()));
    }
    if let Some(team_name) = target.strip_prefix("team:") {
        return Ok(MessageTarget::Team(team_name.to_string()));
    }
    if target.eq_ignore_ascii_case("all") || target.eq_ignore_ascii_case("broadcast") {
        return Ok(MessageTarget::Broadcast);
    }

    if let Some(agent) = team.agent_named(target) {
        return Ok(MessageTarget::Role(agent.role.clone()));
    }

    parse_role_target(target)
}

pub(crate) fn parse_role_target(raw: &str) -> Result<MessageTarget> {
    let normalized = raw.trim().to_ascii_lowercase().replace('-', "_");
    let role = match normalized.as_str() {
        "planner" => AgentRole::Planner,
        "implementer" | "impl" => AgentRole::Implementer,
        "reviewer" => AgentRole::Reviewer,
        "tester" => AgentRole::Tester,
        "debugger" => AgentRole::Debugger,
        "refactorer" => AgentRole::Refactorer,
        "security_reviewer" => AgentRole::SecurityReviewer,
        "docs_writer" => AgentRole::DocsWriter,
        "integrator" => AgentRole::Integrator,
        "context_manager" => AgentRole::ContextManager,
        _ => {
            return Err(AgentmuxError::OrchestratorError(format!(
                "unknown result target '{raw}'"
            )));
        }
    };
    Ok(MessageTarget::Role(role))
}

pub(crate) fn parse_context_refs(refs: &[String]) -> Result<Vec<ContextItemId>> {
    refs.iter()
        .map(|value| {
            ContextItemId::from_str(value).map_err(|error| {
                AgentmuxError::OrchestratorError(format!("invalid context ref '{value}': {error}"))
            })
        })
        .collect()
}

pub(crate) fn parse_artifact_refs(refs: &[String]) -> Result<Vec<ArtifactId>> {
    refs.iter()
        .map(|value| {
            ArtifactId::from_str(value).map_err(|error| {
                AgentmuxError::OrchestratorError(format!("invalid artifact ref '{value}': {error}"))
            })
        })
        .collect()
}

pub(crate) fn map_message_kind(kind: OutgoingMessageKind) -> MessageKind {
    match kind {
        OutgoingMessageKind::TaskAssignment => MessageKind::TaskAssignment,
        OutgoingMessageKind::Question => MessageKind::Question,
        OutgoingMessageKind::Finding => MessageKind::Finding,
        OutgoingMessageKind::PatchProposal => MessageKind::PatchProposal,
        OutgoingMessageKind::ReviewComment => MessageKind::ReviewComment,
        OutgoingMessageKind::TestResult => MessageKind::TestResult,
        OutgoingMessageKind::FailureReport => MessageKind::FailureReport,
        OutgoingMessageKind::Decision => MessageKind::Decision,
        OutgoingMessageKind::Handoff => MessageKind::Handoff,
        OutgoingMessageKind::ApprovalRequest => MessageKind::ApprovalRequest,
        OutgoingMessageKind::ContextUpdate => MessageKind::ContextUpdate,
        OutgoingMessageKind::StatusProbe => MessageKind::StatusProbe,
    }
}

pub(crate) fn map_priority(priority: OutgoingPriority) -> Priority {
    match priority {
        OutgoingPriority::Low => Priority::Low,
        OutgoingPriority::Normal => Priority::Normal,
        OutgoingPriority::High => Priority::High,
        OutgoingPriority::Urgent => Priority::Urgent,
    }
}
