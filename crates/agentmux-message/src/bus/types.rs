use std::collections::BTreeSet;
use std::path::PathBuf;

use agentmux_core::{AgentRole, AgentSessionId, MessageId, TaskId};

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
