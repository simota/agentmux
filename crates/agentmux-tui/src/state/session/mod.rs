//! Pure per-client TUI session state and the daemon-event application logic.

use std::collections::BTreeMap;
#[cfg(feature = "activity-feed")]
use std::collections::VecDeque;

use agentmux_ipc::protocol::{DaemonEvent, IpcEventKind};
use agentmux_terminal::{CellStyle, TerminalParser};
use serde_json::Value;
use unicode_width::UnicodeWidthChar;

use crate::keymap::{FocusDirection, TuiCommand};
use crate::layout::{LayoutNode, PaneLayout, SplitDirection};

use super::choices::{
    CommandEffect, NewPaneChoice, ProviderOption, StateChange, PROVIDER_OPTIONS,
};
use super::copy::CopySelection;
#[cfg(feature = "activity-feed")]
use super::feed::needs_attention_status;
use super::feed::{output_bytes, string_field};
#[cfg(feature = "activity-feed")]
use super::feed::{EventFeedFilter, FeedEntry, SitrepEntry};
use super::message::MessageListItem;
#[cfg(feature = "arena")]
use super::pane::ArenaCandidateState;
use super::pane::{AgentPaneState, TerminalSize};
use super::CONVERSATION_LIST_PANE_ID;
#[cfg(feature = "activity-feed")]
use super::ACTIVITY_FEED_PANE_ID;
#[cfg(feature = "activity-feed")]
use super::MAX_FEED_ENTRIES;

mod accessors;
mod apply;
mod commands;

pub struct TuiSessionState {
    panes: BTreeMap<String, AgentPaneState>,
    layout: PaneLayout,
    default_terminal_size: TerminalSize,
    last_event: Option<IpcEventKind>,
    keybinding_help_visible: bool,
    session_list_visible: bool,
    session_list_selected: usize,
    message_bus_visible: bool,
    message_details_visible: bool,
    messages: Vec<MessageListItem>,
    provider_picker_visible: bool,
    provider_picker_selected: usize,
    copy_selection: Option<CopySelection>,
    #[cfg(feature = "activity-feed")]
    activity_feed_visible: bool,
    #[cfg(feature = "activity-feed")]
    feed_entries: VecDeque<FeedEntry>,
    #[cfg(feature = "activity-feed")]
    sitrep: Vec<SitrepEntry>,
    #[cfg(feature = "activity-feed")]
    feed_scroll: usize,
    #[cfg(feature = "activity-feed")]
    activity_feed_selected: usize,
    #[cfg(feature = "activity-feed")]
    feed_filter: EventFeedFilter,
    #[cfg(feature = "arena")]
    arena_overlay_visible: bool,
    #[cfg(feature = "arena")]
    arena_candidates: Vec<ArenaCandidateState>,
    #[cfg(feature = "arena")]
    arena_selected: usize,
    daemon_protocol_version: Option<u32>,
    runtime_notice: Option<String>,
    /// Render mirror of the keymap dispatcher's `awaiting_prefix_command` flag.
    ///
    /// The dispatcher remains the single source of truth; the driver refreshes
    /// this each frame via [`TuiSessionState::set_prefix_active`] so the render
    /// layer can surface a prefix-mode indicator.
    prefix_active: bool,
}

impl Default for TuiSessionState {
    fn default() -> Self {
        Self::new(SplitDirection::Vertical)
    }
}
