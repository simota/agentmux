//! Constructors, getters/queries, and small setters for `TuiSessionState`.

use super::*;

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
            commands_input_buffer: String::new(),
            commands_target: "broadcast".to_string(),
            commands_history: Vec::new(),
            commands_pending_broadcast: None,
            commands_pending_role: None,
            daemon_protocol_version: None,
            runtime_notice: None,
            prefix_active: false,
            hide_result_marker: true,
        }
    }

    pub fn with_terminal_size(mut self, size: TerminalSize) -> Self {
        self.default_terminal_size = size;
        self
    }

    /// Replace the pane layout with a CLI-parsed startup tree.
    ///
    /// The tree's leaves must reference pane ids that already exist as registered
    /// panes (spawned agents) or the conversation-list pane id; callers resolve
    /// provider leaves to spawned agent ids before invoking this. Focus is reset
    /// to the first leaf in depth-first order. Used once at TUI bootstrap so the
    /// nested/sized structure from `agentmux start "<spec>"` takes effect.
    pub fn apply_startup_layout(&mut self, root: LayoutNode) {
        self.layout = PaneLayout::from_root(root);
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

    pub fn is_commands_pane(&self, pane_id: &str) -> bool {
        pane_id == COMMANDS_PANE_ID
            && self
                .layout
                .panes()
                .iter()
                .any(|existing| existing == pane_id)
    }

    /// Text the user is currently composing in the Commands panel.
    pub fn commands_input_buffer(&self) -> &str {
        &self.commands_input_buffer
    }

    /// Current broadcast target (`"broadcast"`, `"role:<role>"`, or `"agent:<name>"`).
    pub fn commands_target(&self) -> &str {
        &self.commands_target
    }

    /// Sent-broadcast history, oldest first.
    pub fn commands_history(&self) -> &[CommandsLogEntry] {
        &self.commands_history
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

    /// Mirror the keymap dispatcher's prefix state into render state.
    ///
    /// The driver calls this each frame with `dispatcher.is_awaiting_prefix_command()`.
    /// The dispatcher stays the single source of truth.
    pub fn set_prefix_active(&mut self, active: bool) {
        self.prefix_active = active;
    }

    /// Whether the `Ctrl-g` prefix is armed and awaiting its command key.
    pub fn prefix_active(&self) -> bool {
        self.prefix_active
    }

    /// Status-bar label describing the focused pane: `focus: <name> [i/N]`.
    ///
    /// Position is 1-based over the layout's pane order. Returns `None` when no
    /// pane is focused (empty layout).
    pub fn focus_position_label(&self) -> Option<String> {
        let focused = self.layout.focused()?;
        let panes = self.layout.panes();
        let total = panes.len();
        let index = panes.iter().position(|pane_id| pane_id == focused)?;
        let name = self
            .panes
            .get(focused)
            .map(|pane| pane.name().to_string())
            .unwrap_or_else(|| focused.to_string());
        Some(format!("focus: {name} [{}/{}]", index + 1, total))
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
        self.clear_focused_pane_unseen();
    }

    pub fn focus_previous(&mut self) {
        self.layout.focus_previous();
        self.clear_focused_pane_unseen();
    }

    /// Focus the pane with `agent_id`, clearing its unseen-output marker.
    ///
    /// Returns `false` (and changes nothing) if no pane has that id.
    pub fn focus_pane(&mut self, agent_id: &str) -> bool {
        if self.layout.focus(agent_id) {
            self.clear_focused_pane_unseen();
            true
        } else {
            false
        }
    }

    /// Clear the unseen-output marker on the currently focused pane.
    ///
    /// Called whenever focus changes so a pane the user is now looking at no
    /// longer advertises pending output.
    pub fn clear_focused_pane_unseen(&mut self) {
        let Some(agent_id) = self.layout.focused().map(ToOwned::to_owned) else {
            return;
        };
        if let Some(pane) = self.panes.get_mut(&agent_id) {
            pane.has_unseen_output = false;
        }
    }

    pub fn toggle_zoom(&mut self) {
        self.layout.toggle_zoom();
    }

    /// Whether `AGENTMUX_RESULT:` marker blocks are hidden in agent panes.
    /// Defaults to `true`. Display-only — orchestration is unaffected.
    pub fn hide_result_marker(&self) -> bool {
        self.hide_result_marker
    }

    /// Flip the `AGENTMUX_RESULT:` marker visibility for agent panes.
    pub fn toggle_result_marker(&mut self) {
        self.hide_result_marker = !self.hide_result_marker;
    }
}
