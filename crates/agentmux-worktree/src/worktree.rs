//! `Worktree` domain struct — stub.
//!
//! See `docs/spec/03_domain_model.md §7`.

use std::path::PathBuf;

use agentmux_core::{
    AgentSessionId, DateTimeUtc, ProjectId, TaskId, WorktreeId, WorktreeStatus,
};
use serde::{Deserialize, Serialize};

/// A git worktree dedicated to a specific agent session within a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Worktree {
    pub id: WorktreeId,
    pub project_id: ProjectId,
    pub task_id: TaskId,
    pub owner_agent_id: Option<AgentSessionId>,
    /// Absolute path to the worktree checkout on disk.
    pub path: PathBuf,
    pub branch_name: String,
    pub base_branch: String,
    pub status: WorktreeStatus,
    pub created_at: DateTimeUtc,
}
