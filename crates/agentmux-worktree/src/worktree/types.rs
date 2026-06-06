//! Worktree domain structs (DTO types).
//!
//! See `docs/spec/03_domain_model.md §7`.

use std::path::PathBuf;

use agentmux_core::{
    AgentSessionId, ArtifactId, ArtifactKind, DateTimeUtc, ProjectId, TaskId, WorktreeId,
    WorktreeStatus,
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

/// Request to create a dedicated git worktree for an agent.
#[derive(Debug, Clone)]
pub struct CreateWorktree {
    pub task_id: TaskId,
    pub task_slug: String,
    pub agent_name: String,
    pub owner_agent_id: Option<AgentSessionId>,
    pub base_branch: String,
}

/// Raw worktree entry returned by `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitWorktree {
    pub path: PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub detached: bool,
    pub bare: bool,
}

/// Persisted artifact metadata for files under `.agentmux/artifacts/<task>/`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub id: ArtifactId,
    pub project_id: ProjectId,
    pub task_id: Option<TaskId>,
    pub kind: ArtifactKind,
    pub path: PathBuf,
    pub title: String,
    pub mime_type: Option<String>,
    pub size_bytes: u64,
    pub checksum: Option<String>,
    pub created_at: DateTimeUtc,
}

/// Request to capture a worktree diff as reviewable artifacts.
#[derive(Debug, Clone)]
pub struct CaptureDiff {
    pub task_id: TaskId,
    pub agent_name: String,
    pub worktree_path: PathBuf,
    pub base_branch: String,
}

/// Diff capture result: full patch artifact plus stat output for summaries.
#[derive(Debug, Clone)]
pub struct CapturedDiff {
    pub patch: Artifact,
    pub stat: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeOutcome {
    Clean,
    Dirty,
    Conflict,
}

/// Project-configured test command to run in a target worktree.
#[derive(Debug, Clone)]
pub struct TestCommand {
    pub name: String,
    pub command: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestRunStatus {
    Passed,
    Failed,
}

/// Captured test command output for a test pane.
#[derive(Debug, Clone)]
pub struct TestRunArtifact {
    pub artifact: Artifact,
    pub command: String,
    pub status: TestRunStatus,
    pub exit_code: Option<i32>,
}
