//! In-memory typed message bus.
//!
//! The daemon owns process and persistence boundaries; this module keeps the
//! v0.1 message behavior pure and unit-testable.

mod core;
mod render;
mod status;
mod types;

pub use core::MessageBus;
pub use render::render_prompt;
pub use status::initial_delivery_status;
pub use types::{
    AgentDescriptor, DeliveryWait, DeliveryWaitReason, IdleDelivery, Inbox, PreparedInjection,
    PromptContext, PromptContextItem,
};

#[cfg(test)]
mod tests;
