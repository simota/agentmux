//! `AgentSession` domain struct — stub.
//!
//! See `docs/spec/03_domain_model.md §5`.
//!
//! #TODO(agent): implement full AgentSession with in-memory state management
//! #TODO(agent): implement InputLock (mutex + quiet-period timer)

use std::collections::BTreeMap;
use std::path::PathBuf;

use agentmux_core::{
    AgentMode, AgentProvider, AgentRole, AgentSessionId, AgentStatus,
    ContextScopeId, DateTimeUtc, InboxId, PaneId, ProjectId, PtyId,
    TaskId, TerminalBufferId, WorktreeId,
};
use serde::{Deserialize, Serialize};

use crate::capabilities::AgentCapabilities;

/// Represents a live (or recently-exited) agent process managed by agentmux.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSession {
    pub id: AgentSessionId,
    pub project_id: ProjectId,
    pub task_id: Option<TaskId>,
    pub name: String,
    pub provider: AgentProvider,
    pub role: AgentRole,
    pub mode: AgentMode,
    pub pty_id: PtyId,
    pub process_id: Option<u32>,
    pub pane_id: Option<PaneId>,
    pub terminal_buffer_id: TerminalBufferId,
    pub worktree_id: Option<WorktreeId>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub status: AgentStatus,
    pub capabilities: AgentCapabilities,
    pub inbox_id: InboxId,
    pub context_scope_id: ContextScopeId,
    pub created_at: DateTimeUtc,
    pub last_activity_at: DateTimeUtc,
    pub exited_at: Option<DateTimeUtc>,
}
