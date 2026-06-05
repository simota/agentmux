//! `agentmux-ipc` — IPC protocol over JSONL on a Unix domain socket.
//!
//! Responsibilities (see `docs/spec/02_system_architecture.md §4.2`):
//! - request / response / event message types
//! - JSONL framing (newline-delimited JSON)
//! - protocol versioning
//! - client session management helpers
//!
//! v0.1: Unix domain socket + JSON Lines. Binary protocol upgrade deferred.

pub mod framing;
pub mod protocol;

/// Current protocol version. Bump on protocol shape changes.
pub const PROTOCOL_VERSION: u32 = 3;

/// First daemon protocol version that understands `event.subscribe`.
pub const EVENT_SUBSCRIBE_PROTOCOL_VERSION: u32 = 2;

/// First daemon protocol version that understands arena worktree adoption.
pub const ARENA_PROTOCOL_VERSION: u32 = 3;

pub use framing::{JsonlReader, JsonlWriter, MAX_JSONL_FRAME_BYTES, read_jsonl, write_jsonl};
pub use protocol::{
    ClientHello, ClientRequest, DaemonEvent, DaemonResponse, DaemonStreamFrame, ErrorBody,
    EventSubscribeFilter, IpcCommand, IpcEventKind, ProtocolCompatibility,
};
