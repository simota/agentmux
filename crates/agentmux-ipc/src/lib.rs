//! `agentmux-ipc` — IPC protocol over JSONL on a Unix domain socket.
//!
//! Responsibilities (see `docs/spec/02_system_architecture.md §4.2`):
//! - request / response / event message types
//! - JSONL framing (newline-delimited JSON)
//! - protocol versioning
//! - client session management helpers
//!
//! v0.1: Unix domain socket + JSON Lines. Binary protocol upgrade deferred.
//!
//! #TODO(agent): implement request/response envelope types
//! #TODO(agent): implement async JSONL framing (BufReader + lines())
//! #TODO(agent): implement client session handshake with protocol version check

pub mod protocol;

/// Current protocol version. Bump on breaking changes.
pub const PROTOCOL_VERSION: u32 = 1;
