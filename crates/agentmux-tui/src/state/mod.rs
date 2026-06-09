//! Pure TUI session state updated from daemon events.
//!
//! This module intentionally contains no terminal I/O. The interactive run loop
//! can apply daemon events here, then ask `layout`/`render` to draw the result.

mod choices;
mod copy;
mod feed;
mod message;
mod pane;
mod session;

#[cfg(test)]
mod tests;

pub const CONVERSATION_LIST_PANE_ID: &str = "__agentmux_conversation_list__";
pub const COMMANDS_PANE_ID: &str = "__agentmux_commands__";
#[cfg(feature = "activity-feed")]
pub const ACTIVITY_FEED_PANE_ID: &str = "__agentmux_activity_feed__";
#[cfg(feature = "activity-feed")]
const MAX_FEED_ENTRIES: usize = 500;

pub use choices::{AgentProviderChoice, CommandEffect, NewPaneChoice, ProviderOption, StateChange};
pub use session::{CommandsLogEntry, CommandsLogKind, CommandsSubmit};
pub use copy::{CopyPoint, CopySelection};
#[cfg(feature = "activity-feed")]
pub use feed::{EventFeedFilter, FeedEntry, SitrepEntry};
pub use message::MessageListItem;
#[cfg(feature = "arena")]
pub use pane::ArenaCandidateState;
pub use pane::{AgentPaneState, TerminalSize};
pub use session::TuiSessionState;
