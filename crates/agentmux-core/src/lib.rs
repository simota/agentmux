//! `agentmux-core` — Domain IDs, common error type, enums, and time helpers.
//!
//! Every other crate in the workspace depends on this crate. Keep it lean:
//! no async, no I/O, no heavy deps. Only data types and error definitions.

pub mod enums;
pub mod error;
pub mod ids;

pub use enums::*;
pub use error::AgentmuxError;
pub use ids::*;

/// Re-export `time::OffsetDateTime` as the canonical wall-clock timestamp type.
pub type DateTimeUtc = time::OffsetDateTime;
