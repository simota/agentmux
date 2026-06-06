use agentmux_core::{
    AgentSessionId, AgentStatus, AgentmuxError, DeliveryMode, DeliveryStatus, MessageId, ThreadId,
};

use super::types::DeliveryWaitReason;
use crate::message::{MessageSource, MessageTarget};

/// Drop the sender from fan-out recipients. Explicitly addressed targets
/// (`Agent` / `AgentName`) are kept as-is — only one-to-many targets
/// (role/task/team/thread/broadcast) exclude the sender, so an agent never
/// gets its own meeting statement injected back (echo loop guard).
pub(crate) fn exclude_sender_from_fan_out(
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

pub(crate) fn unknown_agent(agent_id: &AgentSessionId) -> AgentmuxError {
    AgentmuxError::UserError(format!("unknown agent session '{agent_id}'"))
}

pub(crate) fn unknown_thread(thread_id: &ThreadId) -> AgentmuxError {
    AgentmuxError::UserError(format!("unknown thread '{thread_id}'"))
}

pub(crate) fn unknown_message(message_id: &MessageId) -> AgentmuxError {
    AgentmuxError::UserError(format!("unknown message '{message_id}'"))
}

/// A message is eligible for inbox backfill (when its target agent registers
/// later) only while it is still pending delivery. Terminal/handled states are
/// excluded so a delivered or cancelled message is never re-queued.
pub(crate) fn backfill_eligible_status(status: &DeliveryStatus) -> bool {
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

pub(crate) fn agent_accepts_idle_injection(status: &AgentStatus) -> bool {
    matches!(
        status,
        AgentStatus::AwaitingInput | AgentStatus::InteractiveReady | AgentStatus::CompletedTurn
    )
}

pub(crate) fn wait_reason_for_status(status: &AgentStatus) -> DeliveryWaitReason {
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
