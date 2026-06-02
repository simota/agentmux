//! `InteractiveAgentAdapter` trait and associated supporting types.
//!
//! See `docs/spec/05_agent_adapter_design.md §3`.
//!
//! All provider adapters (ClaudeCode, Codex, Shell, Custom) implement this
//! trait. The orchestrator only calls methods on this trait — never on the
//! concrete adapter type.

use std::collections::BTreeMap;
use std::path::PathBuf;

use agentmux_core::{
    AgentProvider, AgentRole, ProjectId, TaskId,
    error::Result,
};
use agentmux_pty::TerminalSize;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::capabilities::AgentCapabilities;
use crate::signal::StateSignal;

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

/// Opaque handle returned by `InteractiveAgentAdapter::spawn`.
///
/// The orchestrator passes this back for all subsequent operations on a
/// live agent session.
///
/// #TODO(agent): hold PTY master handle, process ID, and async channels
pub struct AgentHandle {
    /// Human-readable identifier used in log messages.
    pub name: String,
    /// OS PID of the spawned agent process, once known.
    pub process_id: Option<u32>,
    /// Capabilities declared by the adapter at spawn time.
    pub capabilities: AgentCapabilities,
}

/// All parameters needed to spawn a new agent session.
///
/// See `docs/spec/05_agent_adapter_design.md §4`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSpawnSpec {
    pub project_id: ProjectId,
    pub task_id: Option<TaskId>,
    pub name: String,
    pub provider: AgentProvider,
    pub role: AgentRole,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub initial_size: TerminalSize,
    pub permission_profile: PermissionProfile,
    pub startup_prompt: Option<String>,
}

/// Permission boundary applied when spawning an agent.
///
/// #TODO(agent): expand variants to match Codex sandbox / Claude permission docs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionProfile {
    /// Read-only access to the worktree; no shell commands.
    Readonly,
    /// Writes confined to the dedicated worktree.
    WorkspaceWrite,
    /// Full access — requires manual approval gate before spawn.
    FullAccess,
}

/// A frozen screenshot of the agent's current terminal screen.
///
/// #TODO(agent): replace Vec<u8> with a proper ScreenGrid snapshot
pub struct ScreenSnapshot {
    pub rows: u16,
    pub cols: u16,
    /// Raw cell data — placeholder until ScreenGrid is implemented.
    pub raw_bytes: Vec<u8>,
}

/// An ordered sequence of input actions to send to a running agent.
///
/// See `docs/spec/03_domain_model.md §12`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputScript {
    pub id: agentmux_core::InputScriptId,
    pub target_agent_id: agentmux_core::AgentSessionId,
    pub reason: String,
    pub actions: Vec<InputAction>,
}

/// Atomic input action the adapter can send to the PTY.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputAction {
    TypeText(String),
    PasteText(String),
    PressEnter,
    PressEsc,
    PressTab,
    PressBackspace,
    PressCtrl(char),
    PressAlt(char),
    SendRaw(Vec<u8>),
    Wait(std::time::Duration),
}

/// A context pack — set of `ContextItem` bodies selected for handoff.
///
/// #TODO(agent): import from agentmux-context once types are stable
pub struct ContextPack {
    pub items: Vec<String>,
}

// ---------------------------------------------------------------------------
// Core trait
// ---------------------------------------------------------------------------

/// The single interface through which the orchestrator controls any agent.
///
/// Each provider (ClaudeCode, Codex, Shell, Custom) implements this trait.
/// Provider-specific details MUST NOT leak outside the implementing type.
///
/// All methods return `agentmux_core::error::Result<T>` so callers can
/// handle errors through the unified `AgentmuxError` enum.
#[async_trait]
pub trait InteractiveAgentAdapter: Send + Sync {
    /// Spawn a new agent process according to `spec` and return a live handle.
    async fn spawn(&self, spec: AgentSpawnSpec) -> Result<AgentHandle>;

    /// Send an `InputScript` (sequence of key actions) to the agent's PTY.
    ///
    /// Callers must hold the input lock before calling this method.
    async fn send_input_script(
        &self,
        handle: &AgentHandle,
        script: InputScript,
    ) -> Result<()>;

    /// Send a SIGINT (Ctrl-C) to interrupt a running command within the agent.
    async fn interrupt(&self, handle: &AgentHandle) -> Result<()>;

    /// Notify the agent's PTY of a terminal resize.
    async fn resize(
        &self,
        handle: &AgentHandle,
        size: TerminalSize,
    ) -> Result<()>;

    /// Capture a point-in-time screenshot of the agent's terminal screen.
    async fn snapshot_screen(
        &self,
        handle: &AgentHandle,
    ) -> Result<ScreenSnapshot>;

    /// Analyse a screen snapshot and produce `StateSignal`s.
    ///
    /// The caller merges signals from all sources and resolves the final
    /// `AgentStatus` by priority order.
    async fn detect_state(
        &self,
        handle: &AgentHandle,
        snapshot: &ScreenSnapshot,
    ) -> Result<Vec<StateSignal>>;

    /// Render a handoff prompt string to inject into the agent's input.
    ///
    /// Inline if `context_pack` is small; path-based if large (ADR-0005).
    async fn render_handoff_prompt(
        &self,
        message: &agentmux_core::AgentSessionId, // placeholder — will be AgentMessage
        context_pack: &ContextPack,
    ) -> Result<String>;
}
