//! Pure TUI session state updated from daemon events.
//!
//! This module intentionally contains no terminal I/O. The interactive run loop
//! can apply daemon events here, then ask `layout`/`render` to draw the result.

use std::collections::BTreeMap;
#[cfg(feature = "activity-feed")]
use std::collections::VecDeque;

use agentmux_ipc::protocol::{DaemonEvent, IpcEventKind};
use agentmux_terminal::{CellStyle, ScreenGrid, TerminalParser};
use serde_json::Value;
use unicode_width::UnicodeWidthChar;

use crate::keymap::{FocusDirection, TuiCommand};
use crate::layout::{PaneLayout, SplitDirection};

pub const CONVERSATION_LIST_PANE_ID: &str = "__agentmux_conversation_list__";
#[cfg(feature = "activity-feed")]
pub const ACTIVITY_FEED_PANE_ID: &str = "__agentmux_activity_feed__";
#[cfg(feature = "activity-feed")]
const MAX_FEED_ENTRIES: usize = 500;

/// Stable pane state derived from daemon agent/session events.
pub struct AgentPaneState {
    agent_id: String,
    name: String,
    role: Option<String>,
    process_id: Option<u32>,
    status: Option<String>,
    terminal: TerminalParser,
    scroll_offset: usize,
    last_event: Option<IpcEventKind>,
}

impl AgentPaneState {
    fn new(agent_id: String, name: String, process_id: Option<u32>, size: TerminalSize) -> Self {
        Self {
            agent_id,
            name,
            role: None,
            process_id,
            status: None,
            terminal: TerminalParser::new(size.rows, size.cols),
            scroll_offset: 0,
            last_event: Some(IpcEventKind::AgentSpawned),
        }
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn process_id(&self) -> Option<u32> {
        self.process_id
    }

    pub fn role(&self) -> Option<&str> {
        self.role.as_deref()
    }

    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub fn grid(&self) -> &ScreenGrid {
        self.terminal.grid()
    }

    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    pub fn last_event(&self) -> Option<&IpcEventKind> {
        self.last_event.as_ref()
    }

    pub fn chrome_title(&self) -> String {
        match (self.role.as_deref(), self.status.as_deref()) {
            (Some(role), Some(status)) => format!("{} ({}) | {}", self.name, role, status),
            (Some(role), None) => format!("{} ({})", self.name, role),
            (None, Some(status)) => format!("{} | {}", self.name, status),
            (None, None) => self.name.clone(),
        }
    }
}

/// Terminal dimensions used for new panes before the renderer supplies a real size.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalSize {
    pub rows: u16,
    pub cols: u16,
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self { rows: 24, cols: 80 }
    }
}

#[cfg(feature = "activity-feed")]
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct EventFeedFilter {
    pub task_id: Option<String>,
    pub roles: Vec<String>,
    pub kinds: Vec<String>,
}

#[cfg(feature = "activity-feed")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeedEntry {
    pub ts: String,
    pub actor: String,
    pub action: String,
    pub target: String,
    pub kind: String,
    pub focus_agent_id: Option<String>,
}

#[cfg(feature = "activity-feed")]
impl FeedEntry {
    pub fn from_event(event: &DaemonEvent) -> Option<Self> {
        match event.kind {
            IpcEventKind::PtyOutputChunk | IpcEventKind::ScreenDiff => None,
            IpcEventKind::AgentStatusChanged => {
                let agent_id = string_field(&event.payload, "agent_id")?;
                let status = string_field(&event.payload, "status")
                    .or_else(|| string_field(&event.payload, "new_status"))?;
                Some(Self::new(
                    event,
                    agent_id.clone(),
                    format!("status {status}"),
                    agent_id.clone(),
                    Some(agent_id),
                ))
            }
            IpcEventKind::MessageCreated | IpcEventKind::MessageDelivered => {
                let message_id = string_field(&event.payload, "message_id")?;
                let status = string_field(&event.payload, "delivery_status")
                    .unwrap_or_else(|| "created".to_string());
                let to = endpoint_label(event.payload.get("to"));
                Some(Self::new(
                    event,
                    endpoint_label(event.payload.get("from")),
                    format!("message {status}"),
                    to,
                    target_agent_id(event.payload.get("to")).or(Some(message_id)),
                ))
            }
            IpcEventKind::ApprovalCreated => {
                let approval_id = string_field(&event.payload, "approval_id")?;
                Some(Self::new(
                    event,
                    "policy".to_string(),
                    "approval requested".to_string(),
                    approval_id,
                    None,
                ))
            }
            _ => Some(Self::new(
                event,
                event_actor(&event.payload),
                event_action(&event.kind),
                event_target(&event.payload),
                string_field(&event.payload, "agent_id"),
            )),
        }
    }

    fn new(
        event: &DaemonEvent,
        actor: String,
        action: String,
        target: String,
        focus_agent_id: Option<String>,
    ) -> Self {
        Self {
            ts: string_field(&event.payload, "created_at")
                .or_else(|| string_field(&event.payload, "ts"))
                .unwrap_or_else(|| "-".to_string()),
            actor,
            action,
            target,
            kind: event_kind_label(&event.kind).to_string(),
            focus_agent_id,
        }
    }
}

#[cfg(feature = "activity-feed")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SitrepEntry {
    pub agent_id: String,
    pub name: String,
    pub status: String,
    pub needs_attention: bool,
}

#[cfg(feature = "arena")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArenaCandidateState {
    pub worktree_id: String,
    pub name: String,
    pub provider: String,
    pub diff_stat: String,
    pub test_status: String,
    pub summary: String,
}

#[cfg(feature = "arena")]
impl ArenaCandidateState {
    fn from_payload(payload: &Value) -> Option<Self> {
        let worktree_id = string_field(payload, "worktree_id")?;
        Some(Self {
            name: string_field(payload, "name")
                .or_else(|| string_field(payload, "branch_name"))
                .unwrap_or_else(|| worktree_id.clone()),
            worktree_id,
            provider: string_field(payload, "provider").unwrap_or_else(|| "-".to_string()),
            diff_stat: string_field(payload, "diff_stat").unwrap_or_else(|| "-".to_string()),
            test_status: string_field(payload, "test_status")
                .unwrap_or_else(|| "pending".to_string()),
            summary: string_field(payload, "summary").unwrap_or_default(),
        })
    }

    fn from_worktree_created(payload: &Value) -> Option<Self> {
        let worktree = payload.get("worktree").unwrap_or(payload);
        let worktree_id = string_field(worktree, "worktree_id")
            .or_else(|| string_field(payload, "worktree_id"))?;
        Some(Self {
            name: string_field(worktree, "branch_name").unwrap_or_else(|| worktree_id.clone()),
            worktree_id,
            provider: string_field(payload, "provider").unwrap_or_else(|| "-".to_string()),
            diff_stat: "-".to_string(),
            test_status: "pending".to_string(),
            summary: string_field(payload, "summary").unwrap_or_default(),
        })
    }
}

/// Pure state for one attached TUI client.
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
}

impl Default for TuiSessionState {
    fn default() -> Self {
        Self::new(SplitDirection::Vertical)
    }
}

impl TuiSessionState {
    pub fn new(split_direction: SplitDirection) -> Self {
        Self {
            panes: BTreeMap::new(),
            layout: PaneLayout::new(split_direction),
            default_terminal_size: TerminalSize::default(),
            last_event: None,
            keybinding_help_visible: false,
            session_list_visible: false,
            session_list_selected: 0,
            message_bus_visible: false,
            message_details_visible: false,
            messages: Vec::new(),
            provider_picker_visible: false,
            provider_picker_selected: 0,
            copy_selection: None,
            #[cfg(feature = "activity-feed")]
            activity_feed_visible: false,
            #[cfg(feature = "activity-feed")]
            feed_entries: VecDeque::new(),
            #[cfg(feature = "activity-feed")]
            sitrep: Vec::new(),
            #[cfg(feature = "activity-feed")]
            feed_scroll: 0,
            #[cfg(feature = "activity-feed")]
            activity_feed_selected: 0,
            #[cfg(feature = "activity-feed")]
            feed_filter: EventFeedFilter::default(),
            #[cfg(feature = "arena")]
            arena_overlay_visible: false,
            #[cfg(feature = "arena")]
            arena_candidates: Vec::new(),
            #[cfg(feature = "arena")]
            arena_selected: 0,
            daemon_protocol_version: None,
            runtime_notice: None,
        }
    }

    pub fn with_terminal_size(mut self, size: TerminalSize) -> Self {
        self.default_terminal_size = size;
        self
    }

    pub fn layout(&self) -> &PaneLayout {
        &self.layout
    }

    pub fn layout_mut(&mut self) -> &mut PaneLayout {
        &mut self.layout
    }

    pub fn pane(&self, agent_id: &str) -> Option<&AgentPaneState> {
        self.panes.get(agent_id)
    }

    pub fn is_conversation_list_pane(&self, pane_id: &str) -> bool {
        pane_id == CONVERSATION_LIST_PANE_ID
            && self
                .layout
                .panes()
                .iter()
                .any(|existing| existing == pane_id)
    }

    #[cfg(feature = "activity-feed")]
    pub fn is_activity_feed_pane(&self, pane_id: &str) -> bool {
        pane_id == ACTIVITY_FEED_PANE_ID
            && self
                .layout
                .panes()
                .iter()
                .any(|existing| existing == pane_id)
    }

    pub fn panes(&self) -> impl Iterator<Item = &AgentPaneState> {
        self.layout
            .panes()
            .iter()
            .filter_map(|pane_id| self.panes.get(pane_id))
    }

    pub fn focused_pane(&self) -> Option<&AgentPaneState> {
        self.layout.focused().and_then(|id| self.pane(id))
    }

    pub fn resize_pane(&mut self, agent_id: &str, size: TerminalSize) -> StateChange {
        if size.rows == 0 || size.cols == 0 {
            return StateChange::Ignored;
        }
        let Some(pane) = self.panes.get_mut(agent_id) else {
            return StateChange::Ignored;
        };
        pane.terminal.resize(size.rows, size.cols);
        StateChange::UpdatedPane(agent_id.to_string())
    }

    pub fn scroll_pane(&mut self, agent_id: &str, delta: isize) -> StateChange {
        let Some(pane) = self.panes.get_mut(agent_id) else {
            return StateChange::Ignored;
        };
        pane.scroll_offset = pane.scroll_offset.saturating_add_signed(delta);
        StateChange::UpdatedPane(agent_id.to_string())
    }

    pub fn scroll_focused_pane(&mut self, delta: isize) -> StateChange {
        let Some(agent_id) = self.layout.focused().map(ToOwned::to_owned) else {
            return StateChange::Ignored;
        };
        self.scroll_pane(&agent_id, delta)
    }

    pub fn reset_focused_pane_scroll(&mut self) -> StateChange {
        let Some(agent_id) = self.layout.focused().map(ToOwned::to_owned) else {
            return StateChange::Ignored;
        };
        let Some(pane) = self.panes.get_mut(&agent_id) else {
            return StateChange::Ignored;
        };
        pane.scroll_offset = 0;
        StateChange::UpdatedPane(agent_id)
    }

    pub fn last_event(&self) -> Option<&IpcEventKind> {
        self.last_event.as_ref()
    }

    pub fn keybinding_help_visible(&self) -> bool {
        self.keybinding_help_visible
    }

    pub fn session_list_visible(&self) -> bool {
        self.session_list_visible
    }

    pub fn session_list_selected_index(&self) -> usize {
        self.session_list_selected
    }

    pub fn message_bus_visible(&self) -> bool {
        self.message_bus_visible
    }

    pub fn messages(&self) -> &[MessageListItem] {
        &self.messages
    }

    pub fn message_details_visible(&self) -> bool {
        self.message_details_visible
    }

    pub fn provider_picker_visible(&self) -> bool {
        self.provider_picker_visible
    }

    pub fn provider_picker_selected_index(&self) -> usize {
        self.provider_picker_selected
    }

    pub fn provider_options(&self) -> &'static [ProviderOption] {
        PROVIDER_OPTIONS
    }

    pub fn copy_selection(&self) -> Option<&CopySelection> {
        self.copy_selection.as_ref()
    }

    #[cfg(feature = "activity-feed")]
    pub fn feed_entries(&self) -> &VecDeque<FeedEntry> {
        &self.feed_entries
    }

    #[cfg(feature = "activity-feed")]
    pub fn sitrep(&self) -> &[SitrepEntry] {
        &self.sitrep
    }

    #[cfg(feature = "activity-feed")]
    pub fn activity_feed_selected_index(&self) -> usize {
        self.activity_feed_selected
    }

    #[cfg(feature = "activity-feed")]
    pub fn feed_scroll(&self) -> usize {
        self.feed_scroll
    }

    #[cfg(feature = "activity-feed")]
    pub fn feed_filter(&self) -> &EventFeedFilter {
        &self.feed_filter
    }

    #[cfg(feature = "arena")]
    pub fn arena_overlay_visible(&self) -> bool {
        self.arena_overlay_visible
    }

    #[cfg(feature = "arena")]
    pub fn arena_candidates(&self) -> &[ArenaCandidateState] {
        &self.arena_candidates
    }

    #[cfg(feature = "arena")]
    pub fn arena_selected_index(&self) -> usize {
        self.arena_selected
    }

    #[cfg(feature = "activity-feed")]
    pub fn activity_feed_window_start(&self, visible_rows: usize) -> usize {
        let total = self.feed_entries.len();
        total
            .saturating_sub(visible_rows)
            .saturating_sub(self.feed_scroll.min(total.saturating_sub(visible_rows)))
    }

    pub fn daemon_protocol_version(&self) -> Option<u32> {
        self.daemon_protocol_version
    }

    pub fn runtime_notice(&self) -> Option<&str> {
        self.runtime_notice.as_deref()
    }

    pub fn set_runtime_notice(&mut self, notice: impl Into<String>) {
        self.runtime_notice = Some(notice.into());
    }

    pub fn set_copy_selection(&mut self, selection: CopySelection) {
        self.copy_selection = Some(selection);
    }

    pub fn clear_copy_selection(&mut self) {
        self.copy_selection = None;
    }

    pub fn focus_next(&mut self) {
        self.layout.focus_next();
    }

    pub fn focus_previous(&mut self) {
        self.layout.focus_previous();
    }

    pub fn toggle_zoom(&mut self) {
        self.layout.toggle_zoom();
    }

    pub fn apply_command(&mut self, command: TuiCommand) -> CommandEffect {
        match command {
            TuiCommand::Focus(FocusDirection::Right | FocusDirection::Down) => {
                self.clear_copy_selection();
                self.focus_next();
                CommandEffect::Continue
            }
            TuiCommand::Focus(FocusDirection::Left | FocusDirection::Up) => {
                self.clear_copy_selection();
                self.focus_previous();
                CommandEffect::Continue
            }
            TuiCommand::ToggleZoom => {
                self.toggle_zoom();
                CommandEffect::Continue
            }
            TuiCommand::SplitVertical => {
                self.layout.set_split_direction(SplitDirection::Vertical);
                self.open_provider_picker();
                CommandEffect::Continue
            }
            TuiCommand::SplitHorizontal => {
                self.layout.set_split_direction(SplitDirection::Horizontal);
                self.open_provider_picker();
                CommandEffect::Continue
            }
            TuiCommand::ProviderNext => {
                self.move_provider_selection(1);
                CommandEffect::Continue
            }
            TuiCommand::ProviderPrevious => {
                self.move_provider_selection(-1);
                CommandEffect::Continue
            }
            TuiCommand::SelectProvider => self
                .selected_new_pane_choice()
                .map(|choice| {
                    self.provider_picker_visible = false;
                    match choice {
                        NewPaneChoice::Agent(provider) => CommandEffect::SpawnAgentPane(provider),
                        NewPaneChoice::ConversationList => {
                            self.open_conversation_list_pane();
                            CommandEffect::OpenConversationListPane
                        }
                    }
                })
                .unwrap_or(CommandEffect::Continue),
            TuiCommand::ClosePane => {
                let Some(focused) = self.layout.focused().map(ToOwned::to_owned) else {
                    return CommandEffect::Continue;
                };
                if focused == CONVERSATION_LIST_PANE_ID {
                    self.layout.remove_pane(&focused);
                    CommandEffect::Continue
                } else {
                    #[cfg(feature = "activity-feed")]
                    if focused == ACTIVITY_FEED_PANE_ID {
                        self.close_activity_feed_pane();
                        return CommandEffect::Continue;
                    }
                    self.pane(&focused)
                        .map(|pane| CommandEffect::StopPane(pane.agent_id().to_string()))
                        .unwrap_or(CommandEffect::Continue)
                }
            }
            TuiCommand::RotateLayout => {
                self.layout.toggle_split_direction();
                CommandEffect::Continue
            }
            TuiCommand::Help => {
                self.keybinding_help_visible = !self.keybinding_help_visible;
                if self.keybinding_help_visible {
                    self.session_list_visible = false;
                    self.message_bus_visible = false;
                    self.provider_picker_visible = false;
                    #[cfg(feature = "activity-feed")]
                    {
                        self.activity_feed_visible = false;
                    }
                }
                CommandEffect::Continue
            }
            TuiCommand::ShowSessionList => {
                self.session_list_visible = !self.session_list_visible;
                if self.session_list_visible {
                    self.keybinding_help_visible = false;
                    self.message_bus_visible = false;
                    self.provider_picker_visible = false;
                    #[cfg(feature = "activity-feed")]
                    {
                        self.activity_feed_visible = false;
                    }
                    self.select_focused_running_session();
                }
                CommandEffect::Continue
            }
            TuiCommand::ShowMessageBus => {
                self.message_bus_visible = !self.message_bus_visible;
                if self.message_bus_visible {
                    self.keybinding_help_visible = false;
                    self.session_list_visible = false;
                    self.provider_picker_visible = false;
                    #[cfg(feature = "activity-feed")]
                    {
                        self.activity_feed_visible = false;
                    }
                    CommandEffect::RefreshMessages
                } else {
                    CommandEffect::Continue
                }
            }
            #[cfg(feature = "activity-feed")]
            TuiCommand::ShowActivityFeed => {
                self.toggle_activity_feed_pane();
                CommandEffect::ToggleActivityFeedPane {
                    visible: self.activity_feed_visible,
                }
            }
            #[cfg(feature = "activity-feed")]
            TuiCommand::ActivityFeedNext => {
                self.move_activity_feed_selection(1);
                CommandEffect::Continue
            }
            #[cfg(feature = "activity-feed")]
            TuiCommand::ActivityFeedPrevious => {
                self.move_activity_feed_selection(-1);
                CommandEffect::Continue
            }
            #[cfg(feature = "activity-feed")]
            TuiCommand::FocusFeedEntry => self
                .selected_feed_agent_id()
                .map(CommandEffect::FocusPaneById)
                .unwrap_or(CommandEffect::Continue),
            #[cfg(feature = "arena")]
            TuiCommand::ShowArenaOverlay => {
                self.arena_overlay_visible = !self.arena_overlay_visible;
                if self.arena_overlay_visible {
                    self.keybinding_help_visible = false;
                    self.session_list_visible = false;
                    self.message_bus_visible = false;
                    self.provider_picker_visible = false;
                    #[cfg(feature = "activity-feed")]
                    {
                        self.activity_feed_visible = false;
                    }
                    self.clamp_arena_selection();
                }
                CommandEffect::Continue
            }
            #[cfg(feature = "arena")]
            TuiCommand::ArenaNext => {
                self.move_arena_selection(1);
                CommandEffect::Continue
            }
            #[cfg(feature = "arena")]
            TuiCommand::ArenaPrevious => {
                self.move_arena_selection(-1);
                CommandEffect::Continue
            }
            #[cfg(feature = "arena")]
            TuiCommand::ArenaAdopt => self
                .selected_arena_worktree_id()
                .map(CommandEffect::ArenaAdopt)
                .unwrap_or(CommandEffect::Continue),
            TuiCommand::ToggleMessageDetails => {
                if self.message_bus_visible
                    || self
                        .layout
                        .focused()
                        .is_some_and(|pane_id| pane_id == CONVERSATION_LIST_PANE_ID)
                {
                    self.message_details_visible = !self.message_details_visible;
                }
                CommandEffect::Continue
            }
            TuiCommand::SessionListNext => {
                self.move_session_list_selection(1);
                CommandEffect::Continue
            }
            TuiCommand::SessionListPrevious => {
                self.move_session_list_selection(-1);
                CommandEffect::Continue
            }
            TuiCommand::FocusSelectedSession => {
                self.focus_selected_session();
                CommandEffect::Continue
            }
            TuiCommand::CloseOverlay => {
                self.keybinding_help_visible = false;
                self.session_list_visible = false;
                self.message_bus_visible = false;
                self.provider_picker_visible = false;
                #[cfg(feature = "activity-feed")]
                {
                    self.activity_feed_visible = false;
                }
                #[cfg(feature = "arena")]
                {
                    self.arena_overlay_visible = false;
                }
                self.clear_copy_selection();
                CommandEffect::Continue
            }
            TuiCommand::Detach => CommandEffect::Detach,
            TuiCommand::Quit => CommandEffect::Quit,
            _ => CommandEffect::Unhandled(command),
        }
    }

    pub fn open_provider_picker(&mut self) {
        self.provider_picker_visible = true;
        self.keybinding_help_visible = false;
        self.session_list_visible = false;
        self.message_bus_visible = false;
        #[cfg(feature = "activity-feed")]
        {
            self.activity_feed_visible = false;
        }
        #[cfg(feature = "arena")]
        {
            self.arena_overlay_visible = false;
        }
        if self.provider_picker_selected >= PROVIDER_OPTIONS.len() {
            self.provider_picker_selected = 0;
        }
    }

    pub fn open_conversation_list_pane(&mut self) {
        self.layout.add_pane(CONVERSATION_LIST_PANE_ID.to_string());
        self.layout.focus(CONVERSATION_LIST_PANE_ID);
        self.keybinding_help_visible = false;
        self.session_list_visible = false;
        self.message_bus_visible = false;
        self.provider_picker_visible = false;
        #[cfg(feature = "activity-feed")]
        {
            self.activity_feed_visible = false;
        }
        #[cfg(feature = "arena")]
        {
            self.arena_overlay_visible = false;
        }
        self.clear_copy_selection();
    }

    #[cfg(feature = "activity-feed")]
    pub fn open_activity_feed_pane(&mut self) {
        self.activity_feed_visible = true;
        self.layout.add_pane(ACTIVITY_FEED_PANE_ID.to_string());
        self.layout.focus(ACTIVITY_FEED_PANE_ID);
        self.keybinding_help_visible = false;
        self.session_list_visible = false;
        self.message_bus_visible = false;
        self.provider_picker_visible = false;
        self.clear_copy_selection();
    }

    #[cfg(feature = "activity-feed")]
    pub fn close_activity_feed_pane(&mut self) {
        self.activity_feed_visible = false;
        self.layout.remove_pane(ACTIVITY_FEED_PANE_ID);
    }

    #[cfg(feature = "activity-feed")]
    fn toggle_activity_feed_pane(&mut self) {
        if self.activity_feed_visible {
            self.close_activity_feed_pane();
        } else {
            self.open_activity_feed_pane();
        }
    }

    #[cfg(feature = "activity-feed")]
    fn move_activity_feed_selection(&mut self, delta: isize) {
        let count = self.feed_entries.len();
        if count == 0 {
            self.activity_feed_selected = 0;
            self.feed_scroll = 0;
            return;
        }
        let count = isize::try_from(count).unwrap_or(isize::MAX);
        let current = isize::try_from(self.activity_feed_selected).unwrap_or(0);
        self.activity_feed_selected = (current + delta).rem_euclid(count) as usize;
        self.sync_activity_feed_scroll_to_selection();
    }

    #[cfg(feature = "activity-feed")]
    fn sync_activity_feed_scroll_to_selection(&mut self) {
        let Some(tail_index) = self.feed_entries.len().checked_sub(1) else {
            self.feed_scroll = 0;
            return;
        };
        self.activity_feed_selected = self.activity_feed_selected.min(tail_index);
        self.feed_scroll = tail_index.saturating_sub(self.activity_feed_selected);
    }

    #[cfg(feature = "activity-feed")]
    fn selected_feed_agent_id(&self) -> Option<String> {
        self.feed_entries
            .get(self.activity_feed_selected)
            .and_then(|entry| entry.focus_agent_id.clone())
            .filter(|agent_id| self.pane(agent_id).is_some())
    }

    #[cfg(feature = "arena")]
    fn move_arena_selection(&mut self, delta: isize) {
        let count = self.arena_candidates.len();
        if count == 0 {
            self.arena_selected = 0;
            return;
        }
        let count = isize::try_from(count).unwrap_or(isize::MAX);
        let current = isize::try_from(self.arena_selected).unwrap_or(0);
        self.arena_selected = (current + delta).rem_euclid(count) as usize;
    }

    #[cfg(feature = "arena")]
    fn clamp_arena_selection(&mut self) {
        if self.arena_candidates.is_empty() {
            self.arena_selected = 0;
        } else if self.arena_selected >= self.arena_candidates.len() {
            self.arena_selected = self.arena_candidates.len() - 1;
        }
    }

    #[cfg(feature = "arena")]
    fn selected_arena_worktree_id(&self) -> Option<String> {
        self.arena_candidates
            .get(self.arena_selected)
            .map(|candidate| candidate.worktree_id.clone())
    }

    fn move_provider_selection(&mut self, delta: isize) {
        let count = PROVIDER_OPTIONS.len() as isize;
        let current = isize::try_from(self.provider_picker_selected).unwrap_or(0);
        self.provider_picker_selected = (current + delta).rem_euclid(count) as usize;
    }

    fn selected_new_pane_choice(&self) -> Option<NewPaneChoice> {
        PROVIDER_OPTIONS
            .get(self.provider_picker_selected)
            .map(|option| option.choice)
    }

    fn select_focused_running_session(&mut self) {
        let Some(focused) = self.layout.focused() else {
            self.session_list_selected = 0;
            return;
        };
        self.session_list_selected = self
            .running_session_ids()
            .iter()
            .position(|agent_id| agent_id == focused)
            .unwrap_or(0);
        self.clamp_session_list_selection();
    }

    fn move_session_list_selection(&mut self, delta: isize) {
        let count = self.running_session_ids().len();
        if count == 0 {
            self.session_list_selected = 0;
            return;
        }

        let count = isize::try_from(count).unwrap_or(isize::MAX);
        let current = isize::try_from(self.session_list_selected).unwrap_or(0);
        self.session_list_selected = (current + delta).rem_euclid(count) as usize;
    }

    fn focus_selected_session(&mut self) {
        let Some(agent_id) = self
            .running_session_ids()
            .get(self.session_list_selected)
            .cloned()
        else {
            self.session_list_visible = false;
            return;
        };
        self.layout.focus(&agent_id);
        self.session_list_visible = false;
    }

    fn running_session_ids(&self) -> Vec<String> {
        self.panes()
            .filter(|pane| pane.process_id().is_some())
            .map(|pane| pane.agent_id().to_string())
            .collect()
    }

    fn clamp_session_list_selection(&mut self) {
        let count = self.running_session_ids().len();
        if count == 0 {
            self.session_list_selected = 0;
        } else if self.session_list_selected >= count {
            self.session_list_selected = count - 1;
        }
    }

    /// Seed panes from a `daemon.status` response payload.
    ///
    /// This mirrors the daemon-owned agent list without doing any IPC itself.
    /// Unknown or malformed agent entries are skipped.
    pub fn apply_daemon_status(&mut self, payload: &serde_json::Value) -> usize {
        self.daemon_protocol_version = payload
            .get("protocol_version")
            .and_then(|value| value.as_u64())
            .and_then(|value| u32::try_from(value).ok())
            .or(self.daemon_protocol_version);

        let Some(agents) = payload.get("agents").and_then(|value| value.as_array()) else {
            #[cfg(feature = "arena")]
            self.apply_arena_candidates_payload(payload);
            return 0;
        };

        let mut applied = 0;
        for agent in agents {
            let Some(agent_id) =
                string_field(agent, "id").or_else(|| string_field(agent, "agent_id"))
            else {
                continue;
            };
            let name = string_field(agent, "name").unwrap_or_else(|| agent_id.clone());
            let process_id = agent
                .get("process_id")
                .and_then(|value| value.as_u64())
                .and_then(|value| u32::try_from(value).ok());
            let status = string_field(agent, "status");
            let role = string_field(agent, "role");

            if let Some(pane) = self.panes.get_mut(&agent_id) {
                pane.name = name;
                pane.role = role;
                pane.process_id = process_id;
                pane.status = status;
                pane.last_event = None;
                #[cfg(feature = "activity-feed")]
                {
                    let sitrep_name = pane.name.clone();
                    let sitrep_status = pane.status.clone();
                    let _ = pane;
                    self.upsert_sitrep(agent_id.clone(), sitrep_name, sitrep_status);
                }
            } else {
                let mut pane = AgentPaneState::new(
                    agent_id.clone(),
                    name,
                    process_id,
                    self.default_terminal_size,
                );
                pane.role = role;
                pane.status = status;
                #[cfg(feature = "activity-feed")]
                self.upsert_sitrep(agent_id.clone(), pane.name.clone(), pane.status.clone());
                pane.last_event = None;
                self.layout.add_pane(agent_id.clone());
                self.panes.insert(agent_id.clone(), pane);
            }
            applied += 1;
        }

        self.clamp_session_list_selection();
        #[cfg(feature = "arena")]
        self.apply_arena_candidates_payload(payload);
        applied
    }

    pub fn apply_message_list_payload(&mut self, payload: &Value) -> usize {
        let Some(messages) = payload.get("messages").and_then(Value::as_array) else {
            self.messages.clear();
            return 0;
        };

        self.messages = messages
            .iter()
            .filter_map(MessageListItem::from_payload)
            .collect();
        self.messages.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.message_id.cmp(&left.message_id))
        });
        self.messages.len()
    }

    /// Restore a full pane snapshot returned by `agent.snapshot`.
    pub fn apply_snapshot(&mut self, payload: &serde_json::Value) -> StateChange {
        let Some(agent_id) =
            string_field(payload, "agent_id").or_else(|| string_field(payload, "pane_id"))
        else {
            return StateChange::Ignored;
        };
        let rows = payload
            .get("rows")
            .and_then(|value| value.as_u64())
            .and_then(|value| u16::try_from(value).ok())
            .filter(|value| *value > 0)
            .unwrap_or(self.default_terminal_size.rows);
        let cols = payload
            .get("cols")
            .and_then(|value| value.as_u64())
            .and_then(|value| u16::try_from(value).ok())
            .filter(|value| *value > 0)
            .unwrap_or(self.default_terminal_size.cols);
        let name = string_field(payload, "name").unwrap_or_else(|| agent_id.clone());
        let role = string_field(payload, "role");
        let process_id = payload
            .get("process_id")
            .and_then(|value| value.as_u64())
            .and_then(|value| u32::try_from(value).ok());

        if !self.panes.contains_key(&agent_id) {
            self.layout.add_pane(agent_id.clone());
            self.panes.insert(
                agent_id.clone(),
                AgentPaneState::new(
                    agent_id.clone(),
                    name.clone(),
                    process_id,
                    TerminalSize { rows, cols },
                ),
            );
        }

        let Some(pane) = self.panes.get_mut(&agent_id) else {
            return StateChange::Ignored;
        };
        pane.name = name;
        pane.role = role;
        pane.process_id = process_id;
        pane.terminal = TerminalParser::new(rows, cols);
        pane.scroll_offset = 0;
        if let Some(lines) = payload.get("lines").and_then(|value| value.as_array()) {
            for (row, line) in lines.iter().enumerate().take(usize::from(rows)) {
                let Some(text) = line.as_str() else {
                    continue;
                };
                let Ok(row) = u16::try_from(row) else {
                    continue;
                };
                let grid = pane.terminal.grid_mut();
                grid.set_cursor(row, 0);
                let mut display_cols = 0_u16;
                for ch in text.chars() {
                    let width = UnicodeWidthChar::width(ch).unwrap_or(0);
                    if width == 0 {
                        continue;
                    }
                    let width = if width > 1 { 2 } else { 1 };
                    if display_cols.saturating_add(width) > cols {
                        break;
                    }
                    grid.write_char(ch, CellStyle::default());
                    display_cols += width;
                }
            }
        }
        pane.last_event = Some(IpcEventKind::TerminalSnapshotSaved);
        StateChange::UpdatedPane(agent_id)
    }

    /// Apply one daemon event. Malformed or unrelated event payloads are ignored.
    pub fn apply_event(&mut self, event: &DaemonEvent) -> StateChange {
        self.last_event = Some(event.kind.clone());
        #[cfg(feature = "activity-feed")]
        self.record_feed_event(event);

        match event.kind {
            IpcEventKind::AgentSpawned => self.apply_agent_spawned(event),
            IpcEventKind::ClientAttached => self.apply_client_attached(event),
            IpcEventKind::AgentStatusChanged | IpcEventKind::AgentStatusSignal => {
                self.apply_agent_status(event)
            }
            IpcEventKind::PtyOutputChunk | IpcEventKind::ScreenDiff => self.apply_output(event),
            IpcEventKind::TerminalSnapshotSaved => self.apply_snapshot(&event.payload),
            IpcEventKind::AgentExited => self.apply_agent_exited(event),
            IpcEventKind::MessageCreated | IpcEventKind::MessageDelivered => {
                self.apply_message_event(event)
            }
            #[cfg(feature = "arena")]
            IpcEventKind::WorktreeCreated
            | IpcEventKind::WorktreeDiffCaptured
            | IpcEventKind::WorktreeTestCompleted
            | IpcEventKind::WorktreeAdoptRequested => self.apply_arena_event(event),
            _ => StateChange::Ignored,
        }
    }

    #[cfg(feature = "arena")]
    fn apply_arena_candidates_payload(&mut self, payload: &Value) {
        let Some(candidates) = payload.get("arena_candidates").and_then(Value::as_array) else {
            return;
        };
        self.arena_candidates = candidates
            .iter()
            .filter_map(ArenaCandidateState::from_payload)
            .collect();
        self.clamp_arena_selection();
    }

    #[cfg(feature = "arena")]
    fn apply_arena_event(&mut self, event: &DaemonEvent) -> StateChange {
        match event.kind {
            IpcEventKind::WorktreeCreated => {
                let Some(candidate) = ArenaCandidateState::from_worktree_created(&event.payload)
                else {
                    return StateChange::Ignored;
                };
                self.upsert_arena_candidate(candidate);
                StateChange::UpdatedMessages
            }
            IpcEventKind::WorktreeDiffCaptured => {
                let Some(worktree_id) = string_field(&event.payload, "worktree_id") else {
                    return StateChange::Ignored;
                };
                let stat = string_field(&event.payload, "stat").unwrap_or_else(|| "-".to_string());
                self.update_arena_candidate(&worktree_id, |candidate| candidate.diff_stat = stat);
                StateChange::UpdatedMessages
            }
            IpcEventKind::WorktreeTestCompleted => {
                let Some(worktree_id) = string_field(&event.payload, "worktree_id") else {
                    return StateChange::Ignored;
                };
                let status =
                    string_field(&event.payload, "status").unwrap_or_else(|| "-".to_string());
                self.update_arena_candidate(&worktree_id, |candidate| {
                    candidate.test_status = status
                });
                StateChange::UpdatedMessages
            }
            IpcEventKind::WorktreeAdoptRequested => {
                let Some(worktree_id) = string_field(&event.payload, "worktree_id") else {
                    return StateChange::Ignored;
                };
                let approval_id =
                    string_field(&event.payload, "approval_id").unwrap_or_else(|| "-".to_string());
                self.update_arena_candidate(&worktree_id, |candidate| {
                    candidate.summary = format!("approval {approval_id}")
                });
                StateChange::UpdatedMessages
            }
            _ => StateChange::Ignored,
        }
    }

    #[cfg(feature = "arena")]
    fn upsert_arena_candidate(&mut self, candidate: ArenaCandidateState) {
        if let Some(existing) = self
            .arena_candidates
            .iter_mut()
            .find(|existing| existing.worktree_id == candidate.worktree_id)
        {
            *existing = candidate;
        } else {
            self.arena_candidates.push(candidate);
        }
        self.clamp_arena_selection();
    }

    #[cfg(feature = "arena")]
    fn update_arena_candidate<F>(&mut self, worktree_id: &str, update: F)
    where
        F: FnOnce(&mut ArenaCandidateState),
    {
        if let Some(candidate) = self
            .arena_candidates
            .iter_mut()
            .find(|candidate| candidate.worktree_id == worktree_id)
        {
            update(candidate);
        }
    }

    fn apply_message_event(&mut self, event: &DaemonEvent) -> StateChange {
        let Some(message) = MessageListItem::from_payload(&event.payload) else {
            return StateChange::Ignored;
        };
        if let Some(existing) = self
            .messages
            .iter_mut()
            .find(|existing| existing.message_id == message.message_id)
        {
            *existing = message;
        } else {
            self.messages.push(message);
        }
        self.messages.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.message_id.cmp(&left.message_id))
        });
        StateChange::UpdatedMessages
    }

    fn apply_agent_spawned(&mut self, event: &DaemonEvent) -> StateChange {
        let Some(agent_id) = string_field(&event.payload, "agent_id") else {
            return StateChange::Ignored;
        };
        let name = string_field(&event.payload, "name").unwrap_or_else(|| agent_id.clone());
        let role = string_field(&event.payload, "role");
        let process_id = event
            .payload
            .get("process_id")
            .and_then(|value| value.as_u64())
            .and_then(|value| u32::try_from(value).ok());

        if let Some(pane) = self.panes.get_mut(&agent_id) {
            pane.name = name;
            pane.role = role;
            pane.process_id = process_id;
            pane.last_event = Some(IpcEventKind::AgentSpawned);
            return StateChange::UpdatedPane(agent_id);
        }

        let mut pane = AgentPaneState::new(
            agent_id.clone(),
            name,
            process_id,
            self.default_terminal_size,
        );
        pane.role = role;
        self.layout.add_pane(agent_id.clone());
        self.layout.focus(&agent_id);
        self.panes.insert(agent_id.clone(), pane);
        StateChange::AddedPane(agent_id)
    }

    fn apply_client_attached(&mut self, event: &DaemonEvent) -> StateChange {
        let Some(agent_id) = string_field(&event.payload, "agent_id") else {
            return StateChange::Ignored;
        };

        if self.layout.focus(&agent_id) {
            StateChange::FocusedPane(agent_id)
        } else {
            StateChange::Ignored
        }
    }

    fn apply_agent_status(&mut self, event: &DaemonEvent) -> StateChange {
        let Some(agent_id) = string_field(&event.payload, "agent_id") else {
            return StateChange::Ignored;
        };
        let Some(status) = string_field(&event.payload, "status")
            .or_else(|| string_field(&event.payload, "new_status"))
            .or_else(|| string_field(&event.payload, "signal"))
        else {
            return StateChange::Ignored;
        };

        let Some(pane) = self.panes.get_mut(&agent_id) else {
            return StateChange::Ignored;
        };
        pane.status = Some(status);
        #[cfg(feature = "activity-feed")]
        {
            let sitrep_name = pane.name.clone();
            let sitrep_status = pane.status.clone();
            let _ = pane;
            self.upsert_sitrep(agent_id.clone(), sitrep_name, sitrep_status);
        }
        let Some(pane) = self.panes.get_mut(&agent_id) else {
            return StateChange::Ignored;
        };
        pane.last_event = Some(event.kind.clone());
        StateChange::UpdatedPane(agent_id)
    }

    fn apply_output(&mut self, event: &DaemonEvent) -> StateChange {
        let Some(agent_id) = string_field(&event.payload, "agent_id")
            .or_else(|| string_field(&event.payload, "pane_id"))
        else {
            return StateChange::Ignored;
        };
        let Some(bytes) = output_bytes(&event.payload) else {
            return StateChange::Ignored;
        };

        let Some(pane) = self.panes.get_mut(&agent_id) else {
            return StateChange::Ignored;
        };
        pane.terminal.advance(&bytes);
        if pane.scroll_offset > 0 {
            let max_offset = pane.terminal.grid().scrollback().len();
            pane.scroll_offset = pane.scroll_offset.min(max_offset);
        }
        pane.last_event = Some(event.kind.clone());
        StateChange::UpdatedPane(agent_id)
    }

    fn apply_agent_exited(&mut self, event: &DaemonEvent) -> StateChange {
        let Some(agent_id) = string_field(&event.payload, "agent_id") else {
            return StateChange::Ignored;
        };

        if self.panes.remove(&agent_id).is_none() {
            return StateChange::Ignored;
        }
        self.layout.remove_pane(&agent_id);
        #[cfg(feature = "activity-feed")]
        self.remove_sitrep(&agent_id);
        self.clamp_session_list_selection();
        StateChange::RemovedPane(agent_id)
    }

    #[cfg(feature = "activity-feed")]
    fn record_feed_event(&mut self, event: &DaemonEvent) {
        let Some(entry) = FeedEntry::from_event(event) else {
            return;
        };
        let was_following_tail = self
            .feed_entries
            .len()
            .checked_sub(1)
            .is_none_or(|tail| self.activity_feed_selected == tail && self.feed_scroll == 0);
        if self.feed_entries.len() == MAX_FEED_ENTRIES {
            self.feed_entries.pop_front();
            self.activity_feed_selected = self.activity_feed_selected.saturating_sub(1);
        }
        self.feed_entries.push_back(entry);
        if was_following_tail {
            self.activity_feed_selected = self.feed_entries.len().saturating_sub(1);
            self.feed_scroll = 0;
        } else {
            self.sync_activity_feed_scroll_to_selection();
        }
    }

    #[cfg(feature = "activity-feed")]
    fn upsert_sitrep(&mut self, agent_id: String, name: String, status: Option<String>) {
        let status = status.unwrap_or_else(|| "-".to_string());
        let needs_attention = needs_attention_status(&status);
        if let Some(entry) = self
            .sitrep
            .iter_mut()
            .find(|entry| entry.agent_id == agent_id)
        {
            entry.name = name;
            entry.status = status;
            entry.needs_attention = needs_attention;
        } else {
            self.sitrep.push(SitrepEntry {
                agent_id,
                name,
                status,
                needs_attention,
            });
        }
        self.sitrep.sort_by(|left, right| {
            right
                .needs_attention
                .cmp(&left.needs_attention)
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.agent_id.cmp(&right.agent_id))
        });
    }

    #[cfg(feature = "activity-feed")]
    fn remove_sitrep(&mut self, agent_id: &str) {
        self.sitrep.retain(|entry| entry.agent_id != agent_id);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StateChange {
    AddedPane(String),
    UpdatedPane(String),
    FocusedPane(String),
    RemovedPane(String),
    UpdatedMessages,
    Ignored,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandEffect {
    Continue,
    Detach,
    Quit,
    SpawnAgentPane(AgentProviderChoice),
    OpenConversationListPane,
    #[cfg(feature = "activity-feed")]
    ToggleActivityFeedPane {
        visible: bool,
    },
    #[cfg(feature = "activity-feed")]
    FocusPaneById(String),
    #[cfg(feature = "arena")]
    ArenaAdopt(String),
    StopPane(String),
    RefreshMessages,
    Unhandled(TuiCommand),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentProviderChoice {
    Claude,
    Codex,
    Agy,
}

impl AgentProviderChoice {
    pub fn provider(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Agy => "agy",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Agy => "Antigravity",
        }
    }

    pub fn default_name(self) -> &'static str {
        match self {
            Self::Claude => "claude-code",
            Self::Codex => "codex",
            Self::Agy => "agy",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NewPaneChoice {
    Agent(AgentProviderChoice),
    ConversationList,
}

impl NewPaneChoice {
    pub fn label(self) -> &'static str {
        match self {
            Self::Agent(provider) => provider.label(),
            Self::ConversationList => "Conversation List",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderOption {
    pub choice: NewPaneChoice,
    pub hint: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CopySelection {
    pub agent_id: String,
    pub start: CopyPoint,
    pub end: CopyPoint,
}

impl CopySelection {
    pub fn new(agent_id: impl Into<String>, start: CopyPoint, end: CopyPoint) -> Self {
        Self {
            agent_id: agent_id.into(),
            start,
            end,
        }
    }

    pub fn normalized(&self) -> (CopyPoint, CopyPoint) {
        if (self.end.row, self.end.col) < (self.start.row, self.start.col) {
            (self.end, self.start)
        } else {
            (self.start, self.end)
        }
    }

    pub fn contains(&self, row: u16, col: u16) -> bool {
        let (start, end) = self.normalized();
        if row < start.row || row > end.row {
            return false;
        }
        if start.row == end.row {
            return col >= start.col && col <= end.col;
        }
        if row == start.row {
            return col >= start.col;
        }
        if row == end.row {
            return col <= end.col;
        }
        true
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CopyPoint {
    pub row: u16,
    pub col: u16,
}

const PROVIDER_OPTIONS: &[ProviderOption] = &[
    ProviderOption {
        choice: NewPaneChoice::Agent(AgentProviderChoice::Claude),
        hint: "Claude Code",
    },
    ProviderOption {
        choice: NewPaneChoice::Agent(AgentProviderChoice::Codex),
        hint: "OpenAI Codex",
    },
    ProviderOption {
        choice: NewPaneChoice::Agent(AgentProviderChoice::Agy),
        hint: "Google Antigravity CLI",
    },
    ProviderOption {
        choice: NewPaneChoice::ConversationList,
        hint: "Message history panel",
    },
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageListItem {
    pub message_id: String,
    pub created_at: String,
    pub delivery_status: String,
    pub kind: String,
    pub thread_id: Option<String>,
    pub from: String,
    pub to: String,
    pub body: String,
}

impl MessageListItem {
    fn from_payload(payload: &Value) -> Option<Self> {
        let message_id = string_field(payload, "message_id")?;
        Some(Self {
            message_id,
            created_at: string_field(payload, "created_at").unwrap_or_else(|| "-".to_string()),
            delivery_status: string_field(payload, "delivery_status")
                .unwrap_or_else(|| "-".to_string()),
            kind: string_field(payload, "kind").unwrap_or_else(|| "-".to_string()),
            thread_id: string_field(payload, "thread_id"),
            from: endpoint_label(payload.get("from")),
            to: endpoint_label(payload.get("to")),
            body: string_field(payload, "body").unwrap_or_default(),
        })
    }
}

fn string_field(payload: &serde_json::Value, field: &str) -> Option<String> {
    payload
        .get(field)
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned)
}

#[cfg(feature = "activity-feed")]
fn needs_attention_status(status: &str) -> bool {
    matches!(
        status,
        "awaiting_input" | "needs_human" | "awaiting_approval" | "blocked" | "stalled"
    )
}

#[cfg(feature = "activity-feed")]
fn target_agent_id(value: Option<&Value>) -> Option<String> {
    let value = value?;
    let kind = value.get("kind").and_then(Value::as_str)?;
    if kind != "agent" {
        return None;
    }
    string_field(value, "id")
}

#[cfg(feature = "activity-feed")]
fn event_actor(payload: &Value) -> String {
    string_field(payload, "agent_id")
        .or_else(|| string_field(payload, "client_id"))
        .or_else(|| string_field(payload, "task_id"))
        .unwrap_or_else(|| "daemon".to_string())
}

#[cfg(feature = "activity-feed")]
fn event_target(payload: &Value) -> String {
    string_field(payload, "agent_id")
        .or_else(|| string_field(payload, "task_id"))
        .or_else(|| string_field(payload, "message_id"))
        .or_else(|| string_field(payload, "approval_id"))
        .unwrap_or_else(|| "-".to_string())
}

#[cfg(feature = "activity-feed")]
fn event_action(kind: &IpcEventKind) -> String {
    event_kind_label(kind)
        .rsplit('.')
        .next()
        .unwrap_or("event")
        .replace('_', " ")
}

#[cfg(feature = "activity-feed")]
fn event_kind_label(kind: &IpcEventKind) -> &'static str {
    match kind {
        IpcEventKind::DaemonStarted => "daemon.started",
        IpcEventKind::DaemonStopped => "daemon.stopped",
        IpcEventKind::ClientAttached => "client.attached",
        IpcEventKind::ClientDetached => "client.detached",
        IpcEventKind::TaskCreated => "task.created",
        IpcEventKind::TaskStatusChanged => "task.status_changed",
        IpcEventKind::AgentSpawned => "agent.spawned",
        IpcEventKind::AgentStatusSignal => "agent.status_signal",
        IpcEventKind::AgentStatusChanged => "agent.status_changed",
        IpcEventKind::AgentExited => "agent.exited",
        IpcEventKind::PtyOutputChunk => "pty.output_chunk",
        IpcEventKind::ScreenDiff => "screen.diff",
        IpcEventKind::TerminalSnapshotSaved => "terminal.snapshot_saved",
        IpcEventKind::InputScriptCreated => "input_script.created",
        IpcEventKind::InputScriptInjected => "input_script.injected",
        IpcEventKind::InputInjected => "input.injected",
        IpcEventKind::MessageCreated => "message.created",
        IpcEventKind::MessageDelivered => "message.delivered",
        IpcEventKind::ContextCreated => "context.created",
        IpcEventKind::ContextInjected => "context.injected",
        IpcEventKind::MailboxWritten => "mailbox.written",
        IpcEventKind::ArtifactCreated => "artifact.created",
        IpcEventKind::ApprovalCreated => "approval.created",
        IpcEventKind::ApprovalDecided => "approval.decided",
        IpcEventKind::WorktreeCreated => "worktree.created",
        IpcEventKind::WorktreeDiffCaptured => "worktree.diff_captured",
        IpcEventKind::WorktreeAdoptRequested => "worktree.adopt_requested",
        IpcEventKind::WorktreeTestCompleted => "worktree.test_completed",
        IpcEventKind::PolicyDenied => "policy.denied",
        IpcEventKind::Error => "error",
    }
}

fn endpoint_label(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return "-".to_string();
    };
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let id = value.get("id").and_then(Value::as_str).unwrap_or("-");
    if id == "-" {
        kind.to_string()
    } else {
        format!("{kind}:{id}")
    }
}

fn output_bytes(payload: &serde_json::Value) -> Option<Vec<u8>> {
    if let Some(bytes) = payload
        .get("bytes")
        .and_then(|value| value.as_array())
        .map(|bytes| {
            bytes
                .iter()
                .filter_map(|value| value.as_u64())
                .filter_map(|value| u8::try_from(value).ok())
                .collect::<Vec<_>>()
        })
        .filter(|bytes| !bytes.is_empty())
    {
        return Some(bytes);
    }

    if let Some(text) = payload
        .get("text")
        .or_else(|| payload.get("data"))
        .or_else(|| payload.get("chunk"))
        .and_then(|value| value.as_str())
    {
        return Some(text.as_bytes().to_vec());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentmux_ipc::protocol::IpcEventKind;
    use serde_json::json;

    fn event(kind: IpcEventKind, payload: serde_json::Value) -> DaemonEvent {
        DaemonEvent::new(kind, payload)
    }

    #[test]
    fn spawned_agent_adds_pane_and_initial_focus() {
        let mut state = TuiSessionState::default();

        let change = state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({
                "agent_id": "agent_001",
                "name": "impl-codex",
                "role": "implementer",
                "process_id": 42
            }),
        ));

        assert_eq!(change, StateChange::AddedPane("agent_001".to_string()));
        assert_eq!(state.layout().panes(), &["agent_001".to_string()]);
        assert_eq!(state.layout().focused(), Some("agent_001"));
        let pane = state.pane("agent_001").expect("pane");
        assert_eq!(pane.name(), "impl-codex");
        assert_eq!(pane.role(), Some("implementer"));
        assert_eq!(pane.process_id(), Some(42));
        assert_eq!(pane.chrome_title(), "impl-codex (implementer)");
    }

    #[test]
    fn duplicate_spawn_updates_existing_pane_without_reordering_layout() {
        let mut state = TuiSessionState::default();
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_001", "name": "old" }),
        ));

        let change = state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_001", "name": "new", "process_id": 7 }),
        ));

        assert_eq!(change, StateChange::UpdatedPane("agent_001".to_string()));
        assert_eq!(state.layout().panes(), &["agent_001".to_string()]);
        let pane = state.pane("agent_001").expect("pane");
        assert_eq!(pane.name(), "new");
        assert_eq!(pane.process_id(), Some(7));
    }

    #[test]
    fn client_attached_focuses_existing_pane() {
        let mut state = TuiSessionState::default();
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_a", "name": "a" }),
        ));
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_b", "name": "b" }),
        ));

        let change = state.apply_event(&event(
            IpcEventKind::ClientAttached,
            json!({ "client_id": "client_001", "agent_id": "agent_b" }),
        ));

        assert_eq!(change, StateChange::FocusedPane("agent_b".to_string()));
        assert_eq!(state.focused_pane().expect("focused").agent_id(), "agent_b");
    }

    #[test]
    fn status_event_updates_chrome_title() {
        let mut state = TuiSessionState::default();
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_001", "name": "impl" }),
        ));

        let change = state.apply_event(&event(
            IpcEventKind::AgentStatusChanged,
            json!({ "agent_id": "agent_001", "status": "awaiting_input" }),
        ));

        assert_eq!(change, StateChange::UpdatedPane("agent_001".to_string()));
        let pane = state.pane("agent_001").expect("pane");
        assert_eq!(pane.status(), Some("awaiting_input"));
        assert_eq!(pane.chrome_title(), "impl | awaiting_input");
    }

    #[cfg(feature = "activity-feed")]
    #[test]
    fn feed_entry_from_agent_status_changed_event() {
        let entry = FeedEntry::from_event(&event(
            IpcEventKind::AgentStatusChanged,
            json!({ "agent_id": "agent_001", "status": "awaiting_input" }),
        ))
        .expect("entry");

        assert_eq!(entry.actor, "agent_001");
        assert_eq!(entry.action, "status awaiting_input");
        assert_eq!(entry.target, "agent_001");
        assert_eq!(entry.kind, "agent.status_changed");
        assert_eq!(entry.focus_agent_id.as_deref(), Some("agent_001"));
    }

    #[cfg(feature = "activity-feed")]
    #[test]
    fn feed_entry_from_message_created_event_includes_delivery_status() {
        let entry = FeedEntry::from_event(&event(
            IpcEventKind::MessageCreated,
            json!({
                "message_id": "msg_001",
                "from": {"kind": "user", "id": "client_001"},
                "to": {"kind": "agent", "id": "agent_001"},
                "delivery_status": "pending",
                "created_at": "2026-06-04T12:34:56+00:00"
            }),
        ))
        .expect("entry");

        assert_eq!(entry.ts, "2026-06-04T12:34:56+00:00");
        assert_eq!(entry.actor, "user:client_001");
        assert_eq!(entry.action, "message pending");
        assert_eq!(entry.target, "agent:agent_001");
        assert_eq!(entry.kind, "message.created");
        assert_eq!(entry.focus_agent_id.as_deref(), Some("agent_001"));
    }

    #[cfg(feature = "activity-feed")]
    #[test]
    fn feed_entry_from_approval_created_event() {
        let entry = FeedEntry::from_event(&event(
            IpcEventKind::ApprovalCreated,
            json!({
                "approval_id": "approval_001",
                "kind": "tool",
                "risk": "medium",
                "title": "Run command"
            }),
        ))
        .expect("entry");

        assert_eq!(entry.actor, "policy");
        assert_eq!(entry.action, "approval requested");
        assert_eq!(entry.target, "approval_001");
        assert_eq!(entry.kind, "approval.created");
        assert_eq!(entry.focus_agent_id, None);
    }

    #[cfg(feature = "activity-feed")]
    #[test]
    fn feed_entry_from_daemon_event_uses_sensible_daemon_actor() {
        let entry = FeedEntry::from_event(&event(
            IpcEventKind::DaemonStopped,
            json!({ "socket_path": "/tmp/agentmux.sock" }),
        ))
        .expect("entry");

        assert_eq!(entry.actor, "daemon");
        assert_eq!(entry.action, "stopped");
        assert_eq!(entry.target, "-");
        assert_eq!(entry.kind, "daemon.stopped");
        assert_eq!(entry.focus_agent_id, None);
    }

    #[cfg(feature = "activity-feed")]
    #[test]
    fn activity_feed_ignores_high_frequency_output_events() {
        assert!(
            FeedEntry::from_event(&event(
                IpcEventKind::PtyOutputChunk,
                json!({ "agent_id": "agent_001", "text": "hello" }),
            ))
            .is_none()
        );
        assert!(
            FeedEntry::from_event(&event(
                IpcEventKind::ScreenDiff,
                json!({ "agent_id": "agent_001", "text": "hello" }),
            ))
            .is_none()
        );
    }

    #[cfg(feature = "activity-feed")]
    #[test]
    fn sitrep_sorts_agents_needing_attention_first() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "agents": [
                {"id": "agent_ready", "name": "ready", "status": "ready"},
                {"id": "agent_waiting", "name": "waiting", "status": "awaiting_input"}
            ]
        }));

        assert_eq!(state.sitrep()[0].agent_id, "agent_waiting");
        assert!(state.sitrep()[0].needs_attention);
        assert_eq!(state.sitrep()[1].agent_id, "agent_ready");
    }

    #[cfg(feature = "activity-feed")]
    #[test]
    fn agent_exit_removes_sitrep_entry_that_needed_attention() {
        let mut state = TuiSessionState::default();
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_001", "name": "impl" }),
        ));
        state.apply_event(&event(
            IpcEventKind::AgentStatusChanged,
            json!({ "agent_id": "agent_001", "status": "awaiting_input" }),
        ));

        let change = state.apply_event(&event(
            IpcEventKind::AgentExited,
            json!({ "agent_id": "agent_001", "name": "impl" }),
        ));

        assert_eq!(change, StateChange::RemovedPane("agent_001".to_string()));
        assert!(state.sitrep().is_empty());
    }

    #[cfg(feature = "activity-feed")]
    #[test]
    fn feed_caps_at_500_entries_and_keeps_indices_valid() {
        let mut state = TuiSessionState::default();

        for index in 0..501 {
            state.apply_event(&event(
                IpcEventKind::TaskCreated,
                json!({ "task_id": format!("task_{index:03}") }),
            ));
        }

        assert_eq!(state.feed_entries().len(), 500);
        assert_eq!(
            state.feed_entries().front().expect("front").target,
            "task_001"
        );
        assert_eq!(
            state.feed_entries().back().expect("back").target,
            "task_500"
        );
        assert!(state.activity_feed_selected_index() < state.feed_entries().len());
        assert!(state.feed_scroll() <= state.feed_entries().len());
    }

    #[cfg(feature = "activity-feed")]
    #[test]
    fn feed_navigation_on_empty_feed_is_noop() {
        let mut state = TuiSessionState::default();

        assert_eq!(
            state.apply_command(TuiCommand::ActivityFeedNext),
            CommandEffect::Continue
        );
        assert_eq!(
            state.apply_command(TuiCommand::ActivityFeedPrevious),
            CommandEffect::Continue
        );
        assert_eq!(
            state.apply_command(TuiCommand::FocusFeedEntry),
            CommandEffect::Continue
        );
        assert_eq!(state.activity_feed_selected_index(), 0);
    }

    #[cfg(feature = "activity-feed")]
    #[test]
    fn activity_feed_navigation_updates_scroll_to_keep_selection_visible() {
        let mut state = TuiSessionState::default();
        for index in 0..8 {
            state.apply_event(&event(
                IpcEventKind::TaskCreated,
                json!({ "task_id": format!("task_{index:03}") }),
            ));
        }

        for _ in 0..5 {
            state.apply_command(TuiCommand::ActivityFeedPrevious);
        }

        assert_eq!(state.activity_feed_selected_index(), 2);
        assert_eq!(state.feed_scroll(), 5);
        assert_eq!(state.activity_feed_window_start(5), 0);

        state.apply_command(TuiCommand::ActivityFeedNext);

        assert_eq!(state.activity_feed_selected_index(), 3);
        assert_eq!(state.feed_scroll(), 4);
        assert_eq!(state.activity_feed_window_start(5), 0);
    }

    #[cfg(feature = "activity-feed")]
    #[test]
    fn incoming_feed_event_does_not_steal_non_tail_selection() {
        let mut state = TuiSessionState::default();
        for index in 0..3 {
            state.apply_event(&event(
                IpcEventKind::TaskCreated,
                json!({ "task_id": format!("task_{index:03}") }),
            ));
        }
        state.apply_command(TuiCommand::ActivityFeedPrevious);

        state.apply_event(&event(
            IpcEventKind::TaskCreated,
            json!({ "task_id": "task_003" }),
        ));

        assert_eq!(state.activity_feed_selected_index(), 1);
        assert_eq!(state.feed_scroll(), 2);
    }

    #[cfg(feature = "activity-feed")]
    #[test]
    fn focus_feed_entry_for_removed_agent_is_noop() {
        let mut state = TuiSessionState::default();
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_001", "name": "impl" }),
        ));
        state.apply_event(&event(
            IpcEventKind::AgentExited,
            json!({ "agent_id": "agent_001", "name": "impl" }),
        ));

        assert!(state.pane("agent_001").is_none());
        assert_eq!(
            state.apply_command(TuiCommand::FocusFeedEntry),
            CommandEffect::Continue
        );
    }

    #[cfg(feature = "activity-feed")]
    #[test]
    fn focus_feed_entry_returns_focus_pane_effect() {
        let mut state = TuiSessionState::default();
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_001", "name": "impl" }),
        ));
        state.apply_event(&event(
            IpcEventKind::AgentStatusChanged,
            json!({ "agent_id": "agent_001", "status": "awaiting_input" }),
        ));

        assert_eq!(
            state.apply_command(TuiCommand::FocusFeedEntry),
            CommandEffect::FocusPaneById("agent_001".to_string())
        );
    }

    #[cfg(feature = "arena")]
    #[test]
    fn arena_events_update_candidates_and_adopt_effect() {
        let mut state = TuiSessionState::default();
        state.apply_event(&event(
            IpcEventKind::WorktreeCreated,
            json!({
                "worktree": {
                    "worktree_id": "wt_001",
                    "branch_name": "agentmux/task-a"
                },
                "provider": "codex"
            }),
        ));
        state.apply_event(&event(
            IpcEventKind::WorktreeDiffCaptured,
            json!({ "worktree_id": "wt_001", "stat": "1 file changed" }),
        ));
        state.apply_event(&event(
            IpcEventKind::WorktreeTestCompleted,
            json!({ "worktree_id": "wt_001", "status": "passed" }),
        ));

        assert_eq!(state.arena_candidates().len(), 1);
        assert_eq!(state.arena_candidates()[0].provider, "codex");
        assert_eq!(state.arena_candidates()[0].diff_stat, "1 file changed");
        assert_eq!(state.arena_candidates()[0].test_status, "passed");
        assert_eq!(
            state.apply_command(TuiCommand::ArenaAdopt),
            CommandEffect::ArenaAdopt("wt_001".to_string())
        );
    }

    #[cfg(feature = "arena")]
    #[test]
    fn arena_adopt_with_empty_selection_is_noop() {
        let mut state = TuiSessionState::default();

        assert_eq!(
            state.apply_command(TuiCommand::ShowArenaOverlay),
            CommandEffect::Continue
        );
        assert!(state.arena_overlay_visible());
        assert_eq!(
            state.apply_command(TuiCommand::ArenaAdopt),
            CommandEffect::Continue
        );
        assert_eq!(state.arena_selected_index(), 0);
    }

    #[cfg(feature = "arena")]
    #[test]
    fn arena_candidate_refresh_clamps_selection_while_overlay_is_open() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "protocol_version": 3,
            "agents": [],
            "arena_candidates": [
                { "worktree_id": "wt_001", "provider": "claude" },
                { "worktree_id": "wt_002", "provider": "codex" }
            ]
        }));
        state.apply_command(TuiCommand::ShowArenaOverlay);
        state.apply_command(TuiCommand::ArenaPrevious);

        assert_eq!(state.arena_selected_index(), 1);

        state.apply_daemon_status(&json!({
            "protocol_version": 3,
            "agents": [],
            "arena_candidates": [
                { "worktree_id": "wt_001", "provider": "claude" }
            ]
        }));

        assert_eq!(state.arena_selected_index(), 0);
        assert_eq!(
            state.apply_command(TuiCommand::ArenaAdopt),
            CommandEffect::ArenaAdopt("wt_001".to_string())
        );
    }

    #[cfg(feature = "arena")]
    #[test]
    fn daemon_status_seeds_arena_candidates() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "protocol_version": 3,
            "agents": [],
            "arena_candidates": [
                {
                    "worktree_id": "wt_001",
                    "agent_id": "agent_001",
                    "provider": "claude",
                    "diff_stat": "2 files changed",
                    "test_status": "failed"
                }
            ]
        }));

        assert_eq!(state.arena_candidates().len(), 1);
        assert_eq!(state.arena_candidates()[0].worktree_id, "wt_001");
        assert_eq!(state.arena_candidates()[0].test_status, "failed");
    }

    #[test]
    fn pty_output_chunk_advances_terminal_grid() {
        let mut state =
            TuiSessionState::default().with_terminal_size(TerminalSize { rows: 2, cols: 8 });
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_001", "name": "impl" }),
        ));

        let change = state.apply_event(&event(
            IpcEventKind::PtyOutputChunk,
            json!({ "agent_id": "agent_001", "text": "hello" }),
        ));

        assert_eq!(change, StateChange::UpdatedPane("agent_001".to_string()));
        let line = state
            .pane("agent_001")
            .expect("pane")
            .grid()
            .line_text(0)
            .expect("line");
        assert_eq!(line, "hello   ");
    }

    #[test]
    fn focused_pane_scroll_offset_tracks_mouse_history_navigation() {
        let mut state =
            TuiSessionState::default().with_terminal_size(TerminalSize { rows: 2, cols: 4 });
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_001", "name": "impl" }),
        ));
        state.apply_event(&event(
            IpcEventKind::PtyOutputChunk,
            json!({ "agent_id": "agent_001", "text": "aaaa\nbbbb\ncccc\n" }),
        ));

        let change = state.scroll_focused_pane(3);

        assert_eq!(change, StateChange::UpdatedPane("agent_001".to_string()));
        assert_eq!(state.pane("agent_001").expect("pane").scroll_offset(), 3);

        state.apply_event(&event(
            IpcEventKind::PtyOutputChunk,
            json!({ "agent_id": "agent_001", "text": "dddd\n" }),
        ));

        let pane = state.pane("agent_001").expect("pane");
        assert_eq!(pane.scroll_offset(), pane.grid().scrollback().len());

        let previous = pane.scroll_offset();
        state.scroll_focused_pane(-1);
        assert_eq!(
            state.pane("agent_001").expect("pane").scroll_offset(),
            previous.saturating_sub(1)
        );
    }

    #[test]
    fn resize_pane_updates_terminal_grid_dimensions() {
        let mut state =
            TuiSessionState::default().with_terminal_size(TerminalSize { rows: 2, cols: 8 });
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_001", "name": "impl" }),
        ));
        state.apply_event(&event(
            IpcEventKind::PtyOutputChunk,
            json!({ "agent_id": "agent_001", "text": "hello" }),
        ));

        let change = state.resize_pane("agent_001", TerminalSize { rows: 4, cols: 12 });

        assert_eq!(change, StateChange::UpdatedPane("agent_001".to_string()));
        let pane = state.pane("agent_001").expect("pane");
        assert_eq!(pane.grid().rows(), 4);
        assert_eq!(pane.grid().cols(), 12);
        assert_eq!(pane.grid().line_text(0).as_deref(), Some("hello       "));
    }

    #[test]
    fn output_bytes_payload_is_supported() {
        let mut state =
            TuiSessionState::default().with_terminal_size(TerminalSize { rows: 1, cols: 3 });
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_001", "name": "impl" }),
        ));

        state.apply_event(&event(
            IpcEventKind::ScreenDiff,
            json!({ "pane_id": "agent_001", "bytes": [65, 66, 300, 67] }),
        ));

        assert_eq!(
            state
                .pane("agent_001")
                .expect("pane")
                .grid()
                .line_text(0)
                .expect("line"),
            "ABC"
        );
    }

    #[test]
    fn output_bytes_preserve_split_utf8_sequences() {
        let mut state =
            TuiSessionState::default().with_terminal_size(TerminalSize { rows: 1, cols: 4 });
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_001", "name": "impl" }),
        ));

        state.apply_event(&event(
            IpcEventKind::PtyOutputChunk,
            json!({ "agent_id": "agent_001", "bytes": [0xE2] }),
        ));
        state.apply_event(&event(
            IpcEventKind::PtyOutputChunk,
            json!({ "agent_id": "agent_001", "bytes": [0x94, 0x80] }),
        ));

        assert_eq!(
            state
                .pane("agent_001")
                .expect("pane")
                .grid()
                .line_text(0)
                .expect("line"),
            "─   "
        );
    }

    #[test]
    fn full_snapshot_restores_existing_pane_grid() {
        let mut state =
            TuiSessionState::default().with_terminal_size(TerminalSize { rows: 1, cols: 4 });
        state.apply_daemon_status(&json!({
            "agents": [
                {"id": "agent_001", "name": "impl", "process_id": 7}
            ]
        }));

        let change = state.apply_snapshot(&json!({
            "agent_id": "agent_001",
            "name": "impl",
            "process_id": 7,
            "rows": 2,
            "cols": 5,
            "lines": ["hello", "bye  "]
        }));

        assert_eq!(change, StateChange::UpdatedPane("agent_001".to_string()));
        let pane = state.pane("agent_001").expect("pane");
        assert_eq!(pane.grid().rows(), 2);
        assert_eq!(pane.grid().cols(), 5);
        assert_eq!(pane.grid().line_text(0).as_deref(), Some("hello"));
        assert_eq!(pane.grid().line_text(1).as_deref(), Some("bye  "));
        assert_eq!(
            pane.last_event(),
            Some(&IpcEventKind::TerminalSnapshotSaved)
        );
    }

    #[test]
    fn snapshot_restore_clips_lines_by_display_width() {
        let mut state =
            TuiSessionState::default().with_terminal_size(TerminalSize { rows: 1, cols: 3 });
        state.apply_daemon_status(&json!({
            "agents": [
                {"id": "agent_001", "name": "impl", "process_id": 7}
            ]
        }));

        let change = state.apply_snapshot(&json!({
            "agent_id": "agent_001",
            "name": "impl",
            "process_id": 7,
            "rows": 1,
            "cols": 3,
            "lines": ["A変B"]
        }));

        assert_eq!(change, StateChange::UpdatedPane("agent_001".to_string()));
        let pane = state.pane("agent_001").expect("pane");
        assert_eq!(pane.grid().line_text(0).as_deref(), Some("A変"));
        assert_eq!(pane.grid().cursor().row, 0);
        // The wide glyph fills up to the right margin; the cursor parks on the
        // last column (wrap pending) instead of going past the grid.
        assert_eq!(pane.grid().cursor().col, 2);
    }

    #[test]
    fn exited_event_removes_pane_and_moves_focus() {
        let mut state = TuiSessionState::default();
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_001", "name": "impl", "process_id": 42 }),
        ));
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_002", "name": "shell", "process_id": 43 }),
        ));

        let change = state.apply_event(&event(
            IpcEventKind::AgentExited,
            json!({ "agent_id": "agent_001", "exit_status": 0 }),
        ));

        assert_eq!(change, StateChange::RemovedPane("agent_001".to_string()));
        assert!(state.pane("agent_001").is_none());
        assert_eq!(state.layout().panes(), &["agent_002".to_string()]);
        assert_eq!(state.layout().focused(), Some("agent_002"));
    }

    #[test]
    fn malformed_or_unknown_events_do_not_mutate_panes() {
        let mut state = TuiSessionState::default();

        assert_eq!(
            state.apply_event(&event(
                IpcEventKind::AgentSpawned,
                json!({ "name": "missing" })
            )),
            StateChange::Ignored
        );
        assert_eq!(
            state.apply_event(&event(
                IpcEventKind::MessageCreated,
                json!({ "body": "missing id" })
            )),
            StateChange::Ignored
        );

        assert_eq!(state.layout().panes(), &Vec::<String>::new());
    }

    #[test]
    fn focus_next_previous_and_zoom_delegate_to_layout_state() {
        let mut state = TuiSessionState::default();
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_a", "name": "a" }),
        ));
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_b", "name": "b" }),
        ));
        state.layout_mut().focus("agent_a");

        state.focus_next();
        state.focus_previous();
        state.toggle_zoom();

        assert_eq!(state.layout().focused(), Some("agent_a"));
        assert!(state.layout().is_zoomed());
    }

    #[test]
    fn apply_prefix_commands_updates_state_or_returns_session_effect() {
        let mut state = TuiSessionState::default();
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_a", "name": "a" }),
        ));
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_b", "name": "b" }),
        ));
        state.layout_mut().focus("agent_a");

        assert_eq!(
            state.apply_command(TuiCommand::Focus(FocusDirection::Right)),
            CommandEffect::Continue
        );
        assert_eq!(state.layout().focused(), Some("agent_b"));

        assert_eq!(
            state.apply_command(TuiCommand::Focus(FocusDirection::Left)),
            CommandEffect::Continue
        );
        assert_eq!(state.layout().focused(), Some("agent_a"));

        assert_eq!(
            state.apply_command(TuiCommand::ToggleZoom),
            CommandEffect::Continue
        );
        assert!(state.layout().is_zoomed());
        assert_eq!(
            state.apply_command(TuiCommand::SplitVertical),
            CommandEffect::Continue
        );
        assert!(state.provider_picker_visible());
        assert_eq!(
            state.apply_command(TuiCommand::SelectProvider),
            CommandEffect::SpawnAgentPane(AgentProviderChoice::Claude)
        );
        assert!(!state.provider_picker_visible());
        assert_eq!(
            state.provider_options()[3].choice,
            NewPaneChoice::ConversationList
        );
        assert_eq!(
            state.apply_command(TuiCommand::ClosePane),
            CommandEffect::StopPane("agent_a".to_string())
        );
        assert_eq!(
            state.apply_command(TuiCommand::RotateLayout),
            CommandEffect::Continue
        );
        assert_eq!(
            state.layout().split_direction(),
            crate::layout::SplitDirection::Horizontal
        );
        assert_eq!(
            state.apply_command(TuiCommand::Detach),
            CommandEffect::Detach
        );
        assert_eq!(state.apply_command(TuiCommand::Quit), CommandEffect::Quit);
        assert_eq!(
            state.apply_command(TuiCommand::Help),
            CommandEffect::Continue
        );
        assert!(state.keybinding_help_visible());
        assert_eq!(
            state.apply_command(TuiCommand::Help),
            CommandEffect::Continue
        );
        assert!(!state.keybinding_help_visible());
        assert_eq!(
            state.apply_command(TuiCommand::ShowSessionList),
            CommandEffect::Continue
        );
        assert!(state.session_list_visible());
        assert!(!state.keybinding_help_visible());
        assert_eq!(
            state.apply_command(TuiCommand::ShowSessionList),
            CommandEffect::Continue
        );
        assert!(!state.session_list_visible());
        assert_eq!(
            state.apply_command(TuiCommand::ShowMessageBus),
            CommandEffect::RefreshMessages
        );
        assert!(state.message_bus_visible());
        assert_eq!(
            state.apply_command(TuiCommand::CloseOverlay),
            CommandEffect::Continue
        );
        assert!(!state.message_bus_visible());
    }

    #[test]
    fn provider_picker_can_open_and_close_conversation_list_pane() {
        let mut state = TuiSessionState::default();
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_a", "name": "a" }),
        ));
        state.layout_mut().focus("agent_a");
        state.open_provider_picker();
        state.apply_command(TuiCommand::ProviderPrevious);

        assert_eq!(
            state.apply_command(TuiCommand::SelectProvider),
            CommandEffect::OpenConversationListPane
        );
        assert!(!state.provider_picker_visible());
        assert!(state.is_conversation_list_pane(CONVERSATION_LIST_PANE_ID));
        assert_eq!(state.layout().focused(), Some(CONVERSATION_LIST_PANE_ID));

        assert!(!state.message_details_visible());
        assert_eq!(
            state.apply_command(TuiCommand::ToggleMessageDetails),
            CommandEffect::Continue
        );
        assert!(state.message_details_visible());

        assert_eq!(
            state.apply_command(TuiCommand::ClosePane),
            CommandEffect::Continue
        );
        assert!(!state.is_conversation_list_pane(CONVERSATION_LIST_PANE_ID));
        assert_eq!(state.layout().focused(), Some("agent_a"));
    }

    #[test]
    fn session_list_selection_focuses_selected_running_session() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "agents": [
                {
                    "id": "agent_a",
                    "name": "a",
                    "process_id": 100
                },
                {
                    "id": "agent_b",
                    "name": "b",
                    "process_id": 200
                },
                {
                    "id": "agent_restored",
                    "name": "restored",
                    "process_id": null
                }
            ]
        }));
        state.layout_mut().focus("agent_a");

        assert_eq!(
            state.apply_command(TuiCommand::ShowSessionList),
            CommandEffect::Continue
        );
        assert!(state.session_list_visible());
        assert_eq!(state.session_list_selected_index(), 0);

        assert_eq!(
            state.apply_command(TuiCommand::SessionListNext),
            CommandEffect::Continue
        );
        assert_eq!(state.session_list_selected_index(), 1);
        assert_eq!(
            state.apply_command(TuiCommand::FocusSelectedSession),
            CommandEffect::Continue
        );

        assert_eq!(state.layout().focused(), Some("agent_b"));
        assert!(!state.session_list_visible());
    }

    #[test]
    fn session_list_selection_wraps_and_closes() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "agents": [
                {
                    "id": "agent_a",
                    "name": "a",
                    "process_id": 100
                },
                {
                    "id": "agent_b",
                    "name": "b",
                    "process_id": 200
                }
            ]
        }));

        state.apply_command(TuiCommand::ShowSessionList);
        state.apply_command(TuiCommand::SessionListPrevious);
        assert_eq!(state.session_list_selected_index(), 1);

        state.apply_command(TuiCommand::CloseOverlay);
        assert!(!state.session_list_visible());
    }

    #[test]
    fn daemon_status_payload_seeds_agent_panes_in_daemon_order() {
        let mut state = TuiSessionState::default();

        let applied = state.apply_daemon_status(&json!({
            "agents": [
                {
                    "id": "agent-a",
                    "name": "planner",
                    "process_id": 7,
                    "status": "interactive_ready"
                },
                {
                    "id": "agent-b",
                    "name": "impl",
                    "process_id": null
                },
                {
                    "name": "malformed"
                }
            ]
        }));

        assert_eq!(applied, 2);
        assert_eq!(
            state.layout().panes(),
            &["agent-a".to_owned(), "agent-b".to_owned()]
        );
        assert_eq!(state.layout().focused(), Some("agent-a"));

        let first = state.pane("agent-a").expect("first pane exists");
        assert_eq!(first.name(), "planner");
        assert_eq!(first.process_id(), Some(7));
        assert_eq!(first.status(), Some("interactive_ready"));
        assert_eq!(first.last_event(), None);

        let second = state.pane("agent-b").expect("second pane exists");
        assert_eq!(second.name(), "impl");
        assert_eq!(second.process_id(), None);
    }

    #[test]
    fn daemon_status_payload_updates_existing_panes_without_reordering() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "agents": [
                {"id": "a", "name": "old-a"},
                {"id": "b", "name": "old-b"}
            ]
        }));

        let applied = state.apply_daemon_status(&json!({
            "agents": [
                {"id": "b", "name": "new-b", "process_id": 9},
                {"id": "a", "name": "new-a", "status": "busy"}
            ]
        }));

        assert_eq!(applied, 2);
        assert_eq!(state.layout().panes(), &["a".to_owned(), "b".to_owned()]);
        assert_eq!(state.pane("a").expect("a pane").name(), "new-a");
        assert_eq!(state.pane("a").expect("a pane").status(), Some("busy"));
        assert_eq!(state.pane("b").expect("b pane").name(), "new-b");
        assert_eq!(state.pane("b").expect("b pane").process_id(), Some(9));
    }

    #[test]
    fn message_list_payload_updates_message_bus_state_newest_first() {
        let mut state = TuiSessionState::default();

        let applied = state.apply_message_list_payload(&json!({
            "messages": [
                {
                    "message_id": "msg_old",
                    "created_at": "2026-06-04T01:00:00+00:00",
                    "delivery_status": "queued",
                    "kind": "handoff",
                    "from": { "kind": "agent", "id": "planner" },
                    "to": { "kind": "agent", "id": "impl" },
                    "body": "old"
                },
                {
                    "message_id": "msg_new",
                    "created_at": "2026-06-04T02:00:00+00:00",
                    "delivery_status": "delivered",
                    "kind": "test_result",
                    "from": { "kind": "orchestrator" },
                    "to": { "kind": "role", "id": "tester" },
                    "body": "new"
                }
            ]
        }));

        assert_eq!(applied, 2);
        assert_eq!(state.messages()[0].message_id, "msg_new");
        assert_eq!(state.messages()[0].from, "orchestrator");
        assert_eq!(state.messages()[0].to, "role:tester");
    }

    #[test]
    fn message_events_upsert_message_bus_state() {
        let mut state = TuiSessionState::default();

        assert_eq!(
            state.apply_event(&event(
                IpcEventKind::MessageCreated,
                json!({
                    "message_id": "msg_1",
                    "created_at": "2026-06-04T01:00:00+00:00",
                    "delivery_status": "queued",
                    "kind": "handoff",
                    "from": { "kind": "agent", "id": "planner" },
                    "to": { "kind": "agent", "id": "impl" },
                    "body": "review this"
                })
            )),
            StateChange::UpdatedMessages
        );
        assert_eq!(state.messages()[0].delivery_status, "queued");

        assert_eq!(
            state.apply_event(&event(
                IpcEventKind::MessageDelivered,
                json!({
                    "message_id": "msg_1",
                    "created_at": "2026-06-04T01:00:00+00:00",
                    "delivery_status": "delivered",
                    "kind": "handoff",
                    "from": { "kind": "agent", "id": "planner" },
                    "to": { "kind": "agent", "id": "impl" },
                    "body": "review this"
                })
            )),
            StateChange::UpdatedMessages
        );
        assert_eq!(state.messages().len(), 1);
        assert_eq!(state.messages()[0].delivery_status, "delivered");
    }
}
