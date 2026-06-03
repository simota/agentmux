//! `agentmux-context` — Shared context management.
//!
//! Responsibilities (see `docs/spec/02_system_architecture.md §4.8`):
//! - `ContextItem` CRUD (create, read, update, archive)
//! - context pack selection (which items to include in a handoff)
//! - mailbox file writer (writes long context to `.agentmux/inbox/<agent>/`)
//! - redaction (strip secrets before sharing across agent boundaries)
//!
//! Inline vs mailbox split is governed by `max_inline_chars` from config
//! (ADR-0005). Short items go inline in the prompt; long items go to a
//! file whose path is embedded in the handoff prompt.

pub mod broker;
pub mod item;

pub use broker::{
    ContextBroker, ContextPack, ContextPackRequest, ContextUpdate, MailboxConfig, NewContextItem,
};
pub use item::ContextItem;
// ContextKind lives in agentmux-core; re-export for callers that only
// depend on agentmux-context.
pub use agentmux_core::{ContextKind, ContextScope, ContextSource, Visibility};
