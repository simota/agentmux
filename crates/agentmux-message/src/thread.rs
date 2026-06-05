//! `MessageThread` — a multi-party conversation (meeting) over the message bus.
//!
//! A thread groups messages around one topic and fans each message out to all
//! participants except the sender. Threads carry a per-participant message
//! limit so agent-to-agent discussions terminate instead of looping.

use agentmux_core::{AgentSessionId, DateTimeUtc, ThreadId};
use serde::{Deserialize, Serialize};

use crate::message::MessageSource;

/// Default per-participant message limit for a thread.
///
/// Mirrors the pairwise loop guard in the agent protocol (3 back-and-forth
/// turns before requiring human confirmation), with headroom for an opening
/// statement and a closing summary.
pub const DEFAULT_MAX_MESSAGES_PER_PARTICIPANT: u32 = 5;

/// Lifecycle of a thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadStatus {
    Open,
    Closed,
}

/// A multi-party conversation thread (opened via `agentmux meeting open`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageThread {
    pub id: ThreadId,
    /// Human-readable agenda; included in every injected thread prompt.
    pub topic: String,
    /// Sessions that receive thread messages (the sender is excluded per
    /// message at delivery time).
    pub participants: Vec<AgentSessionId>,
    pub opened_by: MessageSource,
    pub status: ThreadStatus,
    /// Per-participant message limit; further messages are rejected with a
    /// hint to summarize and ask the human (loop guard).
    pub max_messages_per_participant: u32,
    pub created_at: DateTimeUtc,
    pub closed_at: Option<DateTimeUtc>,
}

/// Input required to open a [`MessageThread`].
#[derive(Debug, Clone)]
pub struct NewMessageThread {
    pub topic: String,
    pub participants: Vec<AgentSessionId>,
    pub opened_by: MessageSource,
    pub max_messages_per_participant: Option<u32>,
}

impl MessageThread {
    pub fn new(input: NewMessageThread) -> Self {
        Self {
            id: ThreadId::new(),
            topic: input.topic,
            participants: input.participants,
            opened_by: input.opened_by,
            status: ThreadStatus::Open,
            max_messages_per_participant: input
                .max_messages_per_participant
                .unwrap_or(DEFAULT_MAX_MESSAGES_PER_PARTICIPANT),
            created_at: DateTimeUtc::now_utc(),
            closed_at: None,
        }
    }

    pub fn close(&mut self, now: DateTimeUtc) {
        self.status = ThreadStatus::Closed;
        self.closed_at = Some(now);
    }

    pub fn is_participant(&self, agent_id: &AgentSessionId) -> bool {
        self.participants.contains(agent_id)
    }
}
