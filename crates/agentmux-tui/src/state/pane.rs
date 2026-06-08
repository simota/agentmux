//! Per-pane terminal state and supporting value types.

#[cfg(feature = "arena")]
use serde_json::Value;

use agentmux_ipc::protocol::IpcEventKind;
use agentmux_terminal::{ScreenGrid, TerminalParser};

#[cfg(feature = "arena")]
use super::feed::string_field;

/// Stable pane state derived from daemon agent/session events.
pub struct AgentPaneState {
    pub(crate) agent_id: String,
    pub(crate) name: String,
    pub(crate) role: Option<String>,
    pub(crate) process_id: Option<u32>,
    pub(crate) status: Option<String>,
    pub(crate) terminal: TerminalParser,
    pub(crate) scroll_offset: usize,
    pub(crate) last_event: Option<IpcEventKind>,
    /// Set when this pane's terminal buffer updates while it is NOT focused;
    /// cleared when the pane gains focus. Drives the new-content attention marker.
    pub(crate) has_unseen_output: bool,
}

impl AgentPaneState {
    pub(crate) fn new(
        agent_id: String,
        name: String,
        process_id: Option<u32>,
        size: TerminalSize,
    ) -> Self {
        Self {
            agent_id,
            name,
            role: None,
            process_id,
            status: None,
            terminal: TerminalParser::new(size.rows, size.cols),
            scroll_offset: 0,
            last_event: Some(IpcEventKind::AgentSpawned),
            has_unseen_output: false,
        }
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn process_id(&self) -> Option<u32> {
        self.process_id
    }

    pub fn role(&self) -> Option<&str> {
        self.role.as_deref()
    }

    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub fn grid(&self) -> &ScreenGrid {
        self.terminal.grid()
    }

    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    pub fn last_event(&self) -> Option<&IpcEventKind> {
        self.last_event.as_ref()
    }

    /// Whether this pane received output while unfocused and the user has not
    /// yet looked at it (focused it).
    pub fn has_unseen_output(&self) -> bool {
        self.has_unseen_output
    }

    pub fn chrome_title(&self) -> String {
        match (self.role.as_deref(), self.status.as_deref()) {
            (Some(role), Some(status)) => format!("{} ({}) | {}", self.name, role, status),
            (Some(role), None) => format!("{} ({})", self.name, role),
            (None, Some(status)) => format!("{} | {}", self.name, status),
            (None, None) => self.name.clone(),
        }
    }
}

/// Terminal dimensions used for new panes before the renderer supplies a real size.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalSize {
    pub rows: u16,
    pub cols: u16,
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self { rows: 24, cols: 80 }
    }
}

#[cfg(feature = "arena")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArenaCandidateState {
    pub worktree_id: String,
    pub name: String,
    pub provider: String,
    pub diff_stat: String,
    pub test_status: String,
    pub summary: String,
}

#[cfg(feature = "arena")]
impl ArenaCandidateState {
    pub(crate) fn from_payload(payload: &Value) -> Option<Self> {
        let worktree_id = string_field(payload, "worktree_id")?;
        Some(Self {
            name: string_field(payload, "name")
                .or_else(|| string_field(payload, "branch_name"))
                .unwrap_or_else(|| worktree_id.clone()),
            worktree_id,
            provider: string_field(payload, "provider").unwrap_or_else(|| "-".to_string()),
            diff_stat: string_field(payload, "diff_stat").unwrap_or_else(|| "-".to_string()),
            test_status: string_field(payload, "test_status")
                .unwrap_or_else(|| "pending".to_string()),
            summary: string_field(payload, "summary").unwrap_or_default(),
        })
    }

    pub(crate) fn from_worktree_created(payload: &Value) -> Option<Self> {
        let worktree = payload.get("worktree").unwrap_or(payload);
        let worktree_id = string_field(worktree, "worktree_id")
            .or_else(|| string_field(payload, "worktree_id"))?;
        Some(Self {
            name: string_field(worktree, "branch_name").unwrap_or_else(|| worktree_id.clone()),
            worktree_id,
            provider: string_field(payload, "provider").unwrap_or_else(|| "-".to_string()),
            diff_stat: "-".to_string(),
            test_status: "pending".to_string(),
            summary: string_field(payload, "summary").unwrap_or_default(),
        })
    }
}
