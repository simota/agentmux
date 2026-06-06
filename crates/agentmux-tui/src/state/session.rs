//! Pure per-client TUI session state and the daemon-event application logic.

use std::collections::BTreeMap;
#[cfg(feature = "activity-feed")]
use std::collections::VecDeque;

use agentmux_ipc::protocol::{DaemonEvent, IpcEventKind};
use agentmux_terminal::{CellStyle, TerminalParser};
use serde_json::Value;
use unicode_width::UnicodeWidthChar;

use crate::keymap::{FocusDirection, TuiCommand};
use crate::layout::{PaneLayout, SplitDirection};

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
