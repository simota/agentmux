//! `agentmux-pty` — PTY lifecycle management.
//!
//! Responsibilities (see `docs/spec/02_system_architecture.md §4.3`):
//! - PTY creation via `portable-pty`
//! - child process spawn (claude / codex / shell)
//! - write to PTY master (key input, bracketed paste)
//! - read from PTY master (async, relayed to terminal parser)
//! - terminal resize (SIGWINCH / `portable_pty::PtySize`)
//! - graceful and forced process termination
//!
//! NOTE: `portable_pty` blocking reads must run on a dedicated blocking
//! task (`tokio::task::spawn_blocking`) and feed output over an async
//! channel back to the daemon orchestrator.
//!
//! #TODO(agent): implement PtyHandle wrapper struct
//! #TODO(agent): implement spawn() that returns PtyHandle + output Receiver
//! #TODO(agent): implement resize() forwarding PtySize
//! #TODO(agent): implement write_bytes() for raw input injection

pub mod pty;

pub use pty::TerminalSize;
