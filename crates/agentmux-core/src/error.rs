//! Common error enum covering all error categories defined in
//! `docs/spec/02_system_architecture.md §9`.

use thiserror::Error;

/// Top-level error type for agentmux.
///
/// Each variant maps to an error category from the spec §9 error table.
/// Crate-specific sub-errors should be boxed or nested here.
#[derive(Debug, Error)]
pub enum AgentmuxError {
    /// User-visible errors — e.g. unknown session ID, invalid message target.
    /// Display a hint to the user in the CLI.
    #[error("user error: {0}")]
    UserError(String),

    /// Provider-level errors — e.g. `claude` or `codex` binary not found.
    /// Surfaced by `agentmux doctor`.
    #[error("provider error: {0}")]
    ProviderError(String),

    /// PTY creation or I/O failure. Recorded as session startup failure.
    #[error("pty error: {0}")]
    PtyError(String),

    /// VT/ANSI parser encountered unsupported escape sequence.
    /// Emit a warning and continue with a fallback.
    #[error("terminal error: {0}")]
    TerminalError(String),

    /// SQLite read/write failure. Retry or enter degraded mode.
    #[error("store error: {0}")]
    StoreError(String),

    /// Unsafe input was rejected by the policy engine.
    /// Route to the approval queue.
    #[error("policy error: {0}")]
    PolicyError(String),

    /// Orchestrator-level inconsistency — e.g. malformed `AGENTMUX_RESULT`.
    /// Trigger human intervention.
    #[error("orchestrator error: {0}")]
    OrchestratorError(String),

    /// IPC protocol framing or deserialization failure.
    #[error("ipc error: {0}")]
    IpcError(String),

    /// Catch-all for unexpected internal errors that don't fit a category.
    #[error("internal error: {0}")]
    Internal(String),
}

/// Convenience alias used throughout the workspace.
pub type Result<T, E = AgentmuxError> = std::result::Result<T, E>;
