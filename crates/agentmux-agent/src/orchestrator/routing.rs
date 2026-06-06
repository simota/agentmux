use agentmux_core::{AgentRole, DeliveryMode, Priority, TaskId, error::Result};
use agentmux_message::{MessageKind, MessageSource, MessageTarget};

use super::message::{
    OrchestratorMessage, map_message_kind, map_priority, parse_artifact_refs, parse_context_refs,
    resolve_result_target,
};
use super::stall::status_probe_message;
use super::team::TeamTemplate;
use crate::result::{
    AgentResult, AgentResultParse, AgentResultStatus, OutgoingMessage, ParsedAgentResult,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRouteIdentity {
    pub name: String,
    pub role: AgentRole,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OrchestratorResult {
    Routed(ResultRouting),
    NeedsStatusProbe(OrchestratorMessage),
    WaitingForResult,
}

pub fn route_agent_result_parse(
    agent: &AgentRouteIdentity,
    task_id: TaskId,
    team: &TeamTemplate,
    parsed: AgentResultParse,
) -> Result<OrchestratorResult> {
    match parsed {
        AgentResultParse::Found(ParsedAgentResult { result, .. }) => {
            route_agent_result(agent, task_id, team, result).map(OrchestratorResult::Routed)
        }
        AgentResultParse::NotFound => Ok(OrchestratorResult::WaitingForResult),
        AgentResultParse::NeedsStatusProbe(probe) => {
            Ok(OrchestratorResult::NeedsStatusProbe(status_probe_message(
                task_id,
                MessageTarget::Role(agent.role.clone()),
                format!(
                    "AGENTMUX_RESULT を正しい JSON で再出力してください。reason: {}",
                    probe.reason
                ),
            )))
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResultRouting {
    pub status: AgentResultStatus,
    pub summary: String,
    pub outgoing: Vec<OrchestratorMessage>,
    pub needs_human: bool,
}

pub fn route_agent_result(
    agent: &AgentRouteIdentity,
    task_id: TaskId,
    team: &TeamTemplate,
    result: AgentResult,
) -> Result<ResultRouting> {
    let needs_human = matches!(
        result.status,
        AgentResultStatus::Blocked | AgentResultStatus::NeedsInput | AgentResultStatus::Failed
    );
    let mut outgoing = Vec::new();

    for message in &result.messages {
        outgoing.push(convert_outgoing_message(
            agent,
            task_id.clone(),
            team,
            message,
        )?);
    }

    if outgoing.is_empty() {
        if let Some(next) = result.next.as_deref() {
            if !next.eq_ignore_ascii_case("none") {
                outgoing.push(summary_handoff(
                    task_id.clone(),
                    MessageSource::TeamAgent(agent.name.clone()),
                    resolve_result_target(team, next)?,
                    &agent.name,
                    &result,
                ));
            }
        }
    }

    Ok(ResultRouting {
        status: result.status,
        summary: result.summary,
        outgoing,
        needs_human,
    })
}

pub(crate) fn convert_outgoing_message(
    agent: &AgentRouteIdentity,
    task_id: TaskId,
    team: &TeamTemplate,
    message: &OutgoingMessage,
) -> Result<OrchestratorMessage> {
    Ok(OrchestratorMessage {
        task_id: Some(task_id),
        from: MessageSource::TeamAgent(agent.name.clone()),
        to: resolve_result_target(team, &message.to)?,
        kind: map_message_kind(message.kind),
        priority: map_priority(message.priority),
        body: message.body.clone(),
        delivery_mode: DeliveryMode::InjectWhenIdle,
        requires_response: true,
        context_refs: parse_context_refs(&message.context_refs)?,
        artifact_refs: parse_artifact_refs(&message.artifact_refs)?,
    })
}

fn summary_handoff(
    task_id: TaskId,
    from: MessageSource,
    to: MessageTarget,
    from_agent_name: &str,
    result: &AgentResult,
) -> OrchestratorMessage {
    OrchestratorMessage {
        task_id: Some(task_id),
        from,
        to,
        kind: MessageKind::Handoff,
        priority: Priority::Normal,
        body: format!(
            "[agentmux handoff]\nfrom: {from_agent_name}\nkind: Handoff\n\n{}\n\nAGENTMUX_RESULT JSON で結果を返してください。\n",
            result.summary
        ),
        delivery_mode: DeliveryMode::InjectWhenIdle,
        requires_response: true,
        context_refs: Vec::new(),
        artifact_refs: Vec::new(),
    }
}
