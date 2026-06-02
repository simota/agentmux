//! `agentmux-message` — Typed agent message bus.
//!
//! Responsibilities (see `docs/spec/02_system_architecture.md §4.7`):
//! - `AgentMessage` domain struct and associated enums
//! - inbox and delivery queue management
//! - prompt renderer (inline vs mailbox-file delivery per ADR-0005)
//!
//! #TODO(agent): implement delivery queue with policy check integration
//! #TODO(agent): implement prompt renderer (inline / mailbox-file split)

pub mod message;

pub use message::{AgentMessage, MessageKind, MessageSource, MessageTarget};
