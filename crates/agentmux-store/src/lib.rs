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
//!
//! #TODO(agent): implement Store struct wrapping a Connection pool
//! #TODO(agent): implement DDL migration runner (embedded SQL)
//! #TODO(agent): implement CRUD helpers for each domain entity
//! #TODO(agent): implement JSONL event log appender

pub mod store;

pub use store::Store;
