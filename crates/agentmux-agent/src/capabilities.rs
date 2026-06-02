//! `AgentCapabilities` — per-provider feature flags.
//!
//! See `docs/spec/05_agent_adapter_design.md §11`.

use serde::{Deserialize, Serialize};

/// Declares which optional agentmux features a provider supports.
///
/// Adapters fill this struct at construction time; the orchestrator
/// gates optional automation paths on the relevant flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCapabilities {
    /// Supports bracketed-paste escape sequences for multi-line input injection.
    pub supports_bracketed_paste: bool,
    /// Supports the Claude Code hooks API for side-channel state signals.
    pub supports_hooks: bool,
    /// Supports provider slash commands (e.g. Codex `/status`, `/review`).
    pub supports_slash_commands: bool,
    /// Supports per-session permission profiles (e.g. Codex sandbox).
    pub supports_permission_profiles: bool,
    /// Will emit `AGENTMUX_RESULT` JSON on turn completion.
    pub supports_result_marker: bool,
    /// Allowed to create or modify files in the worktree.
    pub can_edit_files: bool,
    /// Allowed to run shell commands.
    pub can_run_commands: bool,
}

impl AgentCapabilities {
    /// Minimal safe defaults — no optional features enabled.
    pub fn minimal() -> Self {
        Self {
            supports_bracketed_paste: false,
            supports_hooks: false,
            supports_slash_commands: false,
            supports_permission_profiles: false,
            supports_result_marker: false,
            can_edit_files: false,
            can_run_commands: false,
        }
    }

    /// Full capabilities for Claude Code TUI adapter.
    pub fn claude_code() -> Self {
        Self {
            supports_bracketed_paste: true,
            supports_hooks: true,
            supports_slash_commands: false,
            supports_permission_profiles: false,
            supports_result_marker: true,
            can_edit_files: true,
            can_run_commands: true,
        }
    }

    /// Capabilities for the Codex TUI adapter.
    pub fn codex() -> Self {
        Self {
            supports_bracketed_paste: true,
            supports_hooks: false,
            supports_slash_commands: true,
            supports_permission_profiles: true,
            supports_result_marker: true,
            can_edit_files: true,
            can_run_commands: true,
        }
    }
}
