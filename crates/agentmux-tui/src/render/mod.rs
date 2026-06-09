//! Pane and view rendering.
//!
//! Split into submodules by responsibility:
//! - [`pane`]: agent pane chrome and grid rendering
//! - [`session`]: top-level frame orchestration
//! - [`overlays`]: centered overlays (session list, picker, message bus, feed, arena, help)
//! - [`messages`]: message-bus / conversation-list panel and message line formatting
//! - [`util`]: text/geometry helpers and terminal-to-ratatui style conversion

mod commands;
mod messages;
mod overlays;
mod pane;
mod session;
mod util;

#[cfg(test)]
mod tests;

pub use pane::{AgentPaneRenderer, PaneChrome};
pub use session::TuiSessionRenderer;
pub use util::{to_ratatui_color, to_ratatui_style};

#[cfg(feature = "activity-feed")]
pub use overlays::render_activity_feed;
#[cfg(feature = "arena")]
pub use overlays::render_arena_overlay;

// Re-imported so the `tests` submodule's `use super::*;` resolves the same
// names the original single-file module exposed.
#[cfg(test)]
use agentmux_terminal::ScreenGrid;
#[cfg(test)]
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier},
};
