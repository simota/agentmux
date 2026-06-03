//! `agentmux-store` — SQLite persistence layer.
//!
//! Responsibilities (see `docs/spec/02_system_architecture.md §7.2`):
//! - persist Projects, Tasks, AgentSessions, Messages, ContextItems,
//!   Artifacts, Approvals, and event-log metadata to SQLite
//! - use `rusqlite` with the `bundled` feature (no system libsqlite3 needed)
//! - provide retry / degraded-mode fallback on `StoreError`
//! - append-only JSONL event log (separate from the database file)
//!
//! Database file: `.agentmux/state.db` (protected path, must not be
//! writable by agent processes).

pub mod event_log;
pub mod store;

pub use event_log::{
    EVENT_AGENT_RESULT, EVENT_CONTEXT_CREATED, EVENT_INPUT_SCRIPT_CREATED,
    EVENT_INPUT_SCRIPT_INJECTED, EVENT_MAILBOX_WRITTEN, EVENT_MESSAGE_CREATED,
    EVENT_MESSAGE_DELIVERED, EVENT_MESSAGE_INJECTED, EventLog, EventLogEntry, MessageEventPayload,
};
pub use store::{AgentSessionRecord, ProjectRecord, Store, TaskRecord};
