//! Command dispatch plus picker/pane and selection helpers for `TuiSessionState`.

use super::*;

impl TuiSessionState {
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
    pub(super) fn sync_activity_feed_scroll_to_selection(&mut self) {
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
    pub(super) fn clamp_arena_selection(&mut self) {
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

    pub(super) fn clamp_session_list_selection(&mut self) {
        let count = self.running_session_ids().len();
        if count == 0 {
            self.session_list_selected = 0;
        } else if self.session_list_selected >= count {
            self.session_list_selected = count - 1;
        }
    }
}
