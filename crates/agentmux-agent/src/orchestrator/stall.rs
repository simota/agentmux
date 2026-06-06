use std::time::Duration;

use agentmux_core::{AgentStatus, DateTimeUtc, DeliveryMode, Priority, TaskId};
use agentmux_message::{MessageKind, MessageSource, MessageTarget};

use super::message::OrchestratorMessage;
use super::routing::AgentRouteIdentity;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StallDecision {
    NoAction,
    SendStatusProbe(OrchestratorMessage),
    NeedsHuman {
        agent: AgentRouteIdentity,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StalledDetector {
    pub quiet_threshold: Duration,
    pub max_status_probe: u8,
}

impl StalledDetector {
    pub fn detect(
        &self,
        agent: AgentRouteIdentity,
        task_id: TaskId,
        status: AgentStatus,
        last_activity_at: DateTimeUtc,
        now: DateTimeUtc,
        status_probe_count: u8,
    ) -> StallDecision {
        if !is_stall_candidate(&status) || !quiet_for(last_activity_at, now, self.quiet_threshold) {
            return StallDecision::NoAction;
        }

        if status_probe_count < self.max_status_probe {
            return StallDecision::SendStatusProbe(status_probe_message(
                task_id,
                MessageTarget::Role(agent.role.clone()),
                "一定時間出力がないため、現在の状態を AGENTMUX_RESULT JSON で報告してください。"
                    .to_string(),
            ));
        }

        StallDecision::NeedsHuman {
            agent,
            reason: "status probe retry limit exceeded".to_string(),
        }
    }
}

fn is_stall_candidate(status: &AgentStatus) -> bool {
    matches!(
        status,
        AgentStatus::RunningTurn
            | AgentStatus::RunningCommand
            | AgentStatus::Stalled
            | AgentStatus::AwaitingInput
    )
}

fn quiet_for(earlier: DateTimeUtc, later: DateTimeUtc, threshold: Duration) -> bool {
    let nanos = later.unix_timestamp_nanos() - earlier.unix_timestamp_nanos();
    if nanos < 0 {
        return false;
    }
    u64::try_from(nanos)
        .map(Duration::from_nanos)
        .is_ok_and(|elapsed| elapsed >= threshold)
}

pub(crate) fn status_probe_message(
    task_id: TaskId,
    to: MessageTarget,
    body: String,
) -> OrchestratorMessage {
    OrchestratorMessage {
        task_id: Some(task_id),
        from: MessageSource::Orchestrator,
        to,
        kind: MessageKind::StatusProbe,
        priority: Priority::Urgent,
        body,
        delivery_mode: DeliveryMode::InjectWhenIdle,
        requires_response: true,
        context_refs: Vec::new(),
        artifact_refs: Vec::new(),
    }
}
