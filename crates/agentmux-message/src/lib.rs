//! `agentmux-message` — Typed agent message bus.
//!
//! Responsibilities (see `docs/spec/02_system_architecture.md §4.7`):
//! - `AgentMessage` domain struct and associated enums
//! - inbox and delivery queue management
//! - prompt renderer (inline vs mailbox-file delivery per ADR-0005)

pub mod bus;
pub mod message;

pub use bus::{
    AgentDescriptor, DeliveryWait, DeliveryWaitReason, IdleDelivery, Inbox, MessageBus,
    PreparedInjection, PromptContext, PromptContextItem, initial_delivery_status, render_prompt,
};
pub use message::{AgentMessage, MessageKind, MessageSource, MessageTarget, NewAgentMessage};
