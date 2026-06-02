//! `agentmux-daemon` — The agentmux background daemon.
//!
//! The daemon owns all mutable state: agent sessions, PTYs, terminal buffers,
//! message queues, context items, worktrees, approvals, and the SQLite store.
//!
//! Client processes connect via a Unix domain socket and exchange JSONL
//! messages (see `agentmux-ipc`). Clients may disconnect and reconnect
//! without losing session state.
//!
//! Tokio tasks spawned at startup (see `docs/spec/02_system_architecture.md §8`):
//! - IPC listener task
//! - per-client connection task (spawned on accept)
//! - per-PTY read task (uses `spawn_blocking` for blocking reads)
//! - terminal parse/update task
//! - orchestrator task
//! - message delivery task
//! - file watcher task
//! - store writer task
//!
//! #TODO(agent): implement IPC listener (UnixListener)
//! #TODO(agent): implement per-client handler task
//! #TODO(agent): implement orchestrator event loop
//! #TODO(agent): implement graceful shutdown on SIGTERM / SIGINT

fn main() {
    println!("agentmux-daemon v{} starting…", env!("CARGO_PKG_VERSION"));
    println!("Socket path: ~/.local/share/agentmux/daemon.sock (TODO)");
    println!("Store path:  .agentmux/state.db (TODO)");
    println!();
    // #TODO(agent): build tokio runtime and spawn all daemon tasks
    println!("Daemon is a stub. Full implementation pending Phase 2.");
}
