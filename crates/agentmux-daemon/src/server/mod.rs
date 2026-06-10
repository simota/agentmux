mod client;
mod dispatch;
mod lifecycle;
mod payload;

// Preserve the crate's public API: the crate root re-exports
// `pub use server::{handle_client, serve, serve_until_shutdown};`.
pub use client::handle_client;
pub use lifecycle::{serve, serve_until_shutdown};

// These are exercised by the crate's test module via `crate::*`.
#[cfg(test)]
pub(crate) use lifecycle::{
    ACCEPT_ERROR_BACKOFF_MAX, ACCEPT_ERROR_BACKOFF_MIN, finish_shutdown, next_accept_backoff,
};

// Preserve internal cross-module visibility: lib.rs does `pub(crate) use
// server::*;`, so every `pub(crate)` item moved into a submodule must remain
// reachable at the `server::` path for sibling modules (message/agent/tests/...).
pub(crate) use client::*;
pub(crate) use dispatch::*;
pub(crate) use payload::*;
