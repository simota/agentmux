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
use super::COMMANDS_PANE_ID;
#[cfg(feature = "activity-feed")]
use super::ACTIVITY_FEED_PANE_ID;
#[cfg(feature = "activity-feed")]
use super::MAX_FEED_ENTRIES;

mod accessors;
mod apply;
mod commands;

/// One entry recorded in the Commands panel's history log.
///
/// An entry is either a broadcast (with its delivery outcome), a successful
/// role assignment, or an error line. `target` / `text` carry the human-facing
/// head of the entry; `kind` distinguishes how the tail outcome renders.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandsLogEntry {
    pub target: String,
    pub text: String,
    pub kind: CommandsLogKind,
}

/// Outcome variant of a [`CommandsLogEntry`].
///
/// `Broadcast.delivered` / `Broadcast.skipped` mirror the daemon's
/// `AgentBroadcastInput` response: `skipped` counts agents that were not
/// injected because a human was typing into them (the daemon owns that safety
/// decision; the UI only reports it).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandsLogKind {
    Broadcast { delivered: usize, skipped: usize },
    RoleAssigned { role: String },
    Error,
}

/// Outcome of interpreting a Commands-panel submission
/// ([`TuiSessionState::parse_commands_input`]).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandsSubmit {
    /// Send the (verbatim) text to the current broadcast target.
    Broadcast(String),
    /// Assign `role` to the single resolved live session `agent_id`.
    AssignRole { agent_id: String, role: String },
    /// Send the raw key sequence `spec` (a `/keys` DSL string) to the single
    /// resolved live session `agent_id`. The spec is forwarded verbatim; the
    /// client validates and parses it when building the request.
    SendKeys { agent_id: String, spec: String },
    /// The submission was rejected; show `message` as an error history line.
    Error(String),
}

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
    /// Commands-panel input editor buffer (text the user is composing).
    commands_input_buffer: String,
    /// Current broadcast target: `"broadcast"`, `"role:<role>"`, or `"agent:<name>"`.
    commands_target: String,
    /// Sent-broadcast history, oldest first.
    commands_history: Vec<CommandsLogEntry>,
    /// `(target, text)` of an in-flight broadcast awaiting its daemon response.
    /// The client loop records the request before sending, then the response
    /// handler pairs it with the `delivered`/`skipped` counts for the history.
    commands_pending_broadcast: Option<(String, String)>,
    /// `(target, role)` of an in-flight role assignment awaiting its
    /// `agent.set_role` response, paired with the right history entry on reply.
    commands_pending_role: Option<(String, String)>,
    daemon_protocol_version: Option<u32>,
    runtime_notice: Option<String>,
    /// Render mirror of the keymap dispatcher's `awaiting_prefix_command` flag.
    ///
    /// The dispatcher remains the single source of truth; the driver refreshes
    /// this each frame via [`TuiSessionState::set_prefix_active`] so the render
    /// layer can surface a prefix-mode indicator.
    prefix_active: bool,
    /// When `true`, `AGENTMUX_RESULT:` marker blocks are blanked in agent panes.
    /// Defaults to `true` (hidden); toggled by the user. This is display-only:
    /// the daemon detects the marker from raw PTY bytes regardless of this flag.
    hide_result_marker: bool,
}

impl Default for TuiSessionState {
    fn default() -> Self {
        Self::new(SplitDirection::Vertical)
    }
}
