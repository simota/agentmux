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
            TuiCommand::ToggleResultMarker => {
                self.toggle_result_marker();
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
                        NewPaneChoice::Commands => {
                            self.open_commands_pane();
                            CommandEffect::OpenCommandsPane
                        }
                    }
                })
                .unwrap_or(CommandEffect::Continue),
            TuiCommand::ClosePane => {
                let Some(focused) = self.layout.focused().map(ToOwned::to_owned) else {
                    return CommandEffect::Continue;
                };
                if focused == CONVERSATION_LIST_PANE_ID || focused == COMMANDS_PANE_ID {
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

    pub fn open_commands_pane(&mut self) {
        self.layout.add_pane(COMMANDS_PANE_ID.to_string());
        self.layout.focus(COMMANDS_PANE_ID);
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

    /// Append a printable character to the Commands input buffer.
    pub fn commands_input_push(&mut self, ch: char) {
        self.commands_input_buffer.push(ch);
    }

    /// Remove the last character from the Commands input buffer (no-op if empty).
    pub fn commands_input_backspace(&mut self) {
        self.commands_input_buffer.pop();
    }

    /// Clear the Commands input buffer.
    pub fn commands_input_clear(&mut self) {
        self.commands_input_buffer.clear();
    }

    /// Cycle the broadcast target through:
    ///
    /// `"broadcast"` → each distinct live-agent role as `"role:<r>"` (sorted) →
    /// each live agent as `"agent:<name>"` (in pane order) → back to `"broadcast"`.
    ///
    /// If the current target has been removed (e.g. the session exited), the next
    /// Tab safely resolves to the first entry (`"broadcast"`).
    pub fn cycle_commands_target(&mut self) {
        let targets = self.commands_target_options();
        let next = targets
            .iter()
            .position(|candidate| candidate == &self.commands_target)
            .map(|index| (index + 1) % targets.len())
            .unwrap_or(0);
        self.commands_target = targets
            .get(next)
            .cloned()
            .unwrap_or_else(|| "broadcast".to_string());
    }

    /// Build the complete ordered list of broadcast-target options:
    ///
    /// 1. `"broadcast"` (always first)
    /// 2. `"role:<r>"` for each distinct role among live panes, sorted ascending
    /// 3. `"agent:<name>"` for each live pane in layout order
    ///
    /// A pane is considered live when its `process_id` is `Some`.  Panes without
    /// a role are excluded from the role group but always appear in the agent group.
    pub fn commands_target_options(&self) -> Vec<String> {
        let live_panes: Vec<&crate::state::AgentPaneState> = self
            .panes()
            .filter(|pane| pane.process_id().is_some())
            .collect();

        let mut roles: Vec<String> = live_panes
            .iter()
            .filter_map(|pane| pane.role().map(ToOwned::to_owned))
            .collect();
        roles.sort();
        roles.dedup();

        let mut options = Vec::with_capacity(1 + roles.len() + live_panes.len());
        options.push("broadcast".to_string());
        options.extend(roles.into_iter().map(|role| format!("role:{role}")));
        options.extend(
            live_panes
                .iter()
                .map(|pane| format!("agent:{}", pane.name())),
        );
        options
    }

    /// Record a sent broadcast (with its delivery outcome) in the history log.
    pub fn push_commands_history(
        &mut self,
        target: impl Into<String>,
        text: impl Into<String>,
        delivered: usize,
        skipped: usize,
    ) {
        self.commands_history.push(CommandsLogEntry {
            target: target.into(),
            text: text.into(),
            kind: CommandsLogKind::Broadcast { delivered, skipped },
        });
    }

    /// Record a successful role assignment in the history log.
    pub fn push_commands_role_history(
        &mut self,
        target: impl Into<String>,
        role: impl Into<String>,
    ) {
        let role = role.into();
        self.commands_history.push(CommandsLogEntry {
            target: target.into(),
            text: format!("/role {role}"),
            kind: CommandsLogKind::RoleAssigned { role },
        });
    }

    /// Record an error line (rejected `/role` submission etc.) in the history log.
    pub fn push_commands_error_history(
        &mut self,
        target: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.commands_history.push(CommandsLogEntry {
            target: target.into(),
            text: message.into(),
            kind: CommandsLogKind::Error,
        });
    }

    /// Record an in-flight broadcast so its daemon response can be paired with
    /// the right `target`/`text` when recorded into history. Also clears the
    /// input buffer, since the request has been handed off to the daemon.
    pub fn begin_commands_broadcast(&mut self, target: impl Into<String>, text: impl Into<String>) {
        self.commands_pending_broadcast = Some((target.into(), text.into()));
        self.commands_input_clear();
    }

    /// Apply an `AgentBroadcastInput` response payload (`{delivered, skipped}`)
    /// by pushing a history entry for the matching in-flight broadcast.
    ///
    /// Returns `true` when a pending broadcast was matched and recorded.
    pub fn apply_commands_broadcast_response(&mut self, payload: &Value) -> bool {
        let Some((target, text)) = self.commands_pending_broadcast.take() else {
            return false;
        };
        let delivered = count_field(payload, "delivered");
        let skipped = count_field(payload, "skipped");
        self.push_commands_history(target, text, delivered, skipped);
        true
    }

    /// Build a [`CommandEffect::BroadcastInput`] from the current buffer/target,
    /// returning `None` when the buffer is empty (nothing to send).
    ///
    /// The buffer is left untouched; the client loop clears it via
    /// [`Self::commands_input_clear`] once the daemon round-trip succeeds.
    pub fn commands_broadcast_effect(&self) -> Option<CommandEffect> {
        let text = self.commands_input_buffer.trim();
        if text.is_empty() {
            return None;
        }
        Some(CommandEffect::BroadcastInput {
            target: self.commands_target.clone(),
            text: self.commands_input_buffer.clone(),
        })
    }

    /// Interpret the current Commands-panel input against the current target.
    ///
    /// The panel is command-oriented: every submission is a slash command.
    ///
    /// - `/send "<text>"` (quotes optional) broadcasts the raw text to the
    ///   current target. Empty text -> [`CommandsSubmit::Error`].
    /// - `/role <newrole>` assigns a role to the selected session. The target
    ///   must be a single live session (`agent:<name>` resolving to a live
    ///   pane); anything else (`broadcast`, `role:<…>`, or an unresolved
    ///   `agent:<name>`) -> [`CommandsSubmit::Error`]. Empty role -> Error.
    /// - Plain (non-slash) text or an unknown `/command` -> [`CommandsSubmit::Error`];
    ///   text is never broadcast implicitly — use `/send`.
    /// - An empty buffer yields `None` (nothing to submit).
    pub fn parse_commands_input(&self) -> Option<CommandsSubmit> {
        let trimmed = self.commands_input_buffer.trim();
        if trimmed.is_empty() {
            return None;
        }

        let Some(without_slash) = trimmed.strip_prefix('/') else {
            return Some(CommandsSubmit::Error(
                "commands start with '/'; use /send \"<text>\" to broadcast".to_string(),
            ));
        };

        let (command, rest) = match without_slash.split_once(char::is_whitespace) {
            Some((command, rest)) => (command, rest.trim()),
            None => (without_slash, ""),
        };

        match command {
            "send" => {
                let text = unquote(rest);
                if text.is_empty() {
                    return Some(CommandsSubmit::Error(
                        "/send requires text, e.g. /send \"hello\"".to_string(),
                    ));
                }
                Some(CommandsSubmit::Broadcast(text.to_string()))
            }
            "role" => {
                if rest.is_empty() {
                    return Some(CommandsSubmit::Error(
                        "/role requires a role name".to_string(),
                    ));
                }
                let Some(name) = self.commands_target.strip_prefix("agent:") else {
                    return Some(CommandsSubmit::Error(
                        "select a single session (agent:<name>) first".to_string(),
                    ));
                };
                match self.live_agent_id_by_name(name) {
                    Some(agent_id) => Some(CommandsSubmit::AssignRole {
                        agent_id,
                        role: rest.to_string(),
                    }),
                    None => Some(CommandsSubmit::Error(format!("no live session named {name}"))),
                }
            }
            other => Some(CommandsSubmit::Error(format!("unknown command: /{other}"))),
        }
    }

    /// Resolve a live pane's `agent_id` from its session name. Returns `None`
    /// when no live (process-backed) pane carries that exact name.
    fn live_agent_id_by_name(&self, name: &str) -> Option<String> {
        self.panes()
            .filter(|pane| pane.process_id().is_some())
            .find(|pane| pane.name() == name)
            .map(|pane| pane.agent_id().to_string())
    }

    /// Record an in-flight role assignment so its `agent.set_role` response can
    /// be paired with the right `target`/`role` when recorded into history. Also
    /// clears the input buffer, since the request has been handed off.
    pub fn begin_commands_role_assign(&mut self, role: impl Into<String>) {
        self.commands_pending_role = Some((self.commands_target.clone(), role.into()));
        self.commands_input_clear();
    }

    /// Apply an `agent.set_role` response payload (`{agent_id, role}`) by pushing
    /// a success history entry for the matching in-flight role assignment. The
    /// response `role` (the canonical label) is preferred over the requested one.
    ///
    /// Returns `true` when a pending role assignment was matched and recorded.
    pub fn apply_commands_role_response(&mut self, payload: &Value) -> bool {
        let Some((target, requested_role)) = self.commands_pending_role.take() else {
            return false;
        };
        let role = string_field(payload, "role").unwrap_or(requested_role);
        self.push_commands_role_history(target, role);
        true
    }

    /// Record an error line for a rejected role assignment and clear the input.
    pub fn fail_commands_role_assign(&mut self, message: impl Into<String>) {
        let target = self.commands_target.clone();
        self.push_commands_error_history(target, message);
        self.commands_pending_role = None;
        self.commands_input_clear();
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
        self.clear_focused_pane_unseen();
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

/// Count the entries of a JSON array field (e.g. `delivered` / `skipped`),
/// treating a missing or non-array field as zero.
fn count_field(payload: &Value, field: &str) -> usize {
    payload
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0)
}

/// Strip one pair of matching surrounding quotes (`"` or `'`) from an already
/// trimmed string; otherwise return it unchanged. Lets `/send "hi"` and
/// `/send hi` both broadcast `hi`.
fn unquote(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &value[1..value.len() - 1];
        }
    }
    value
}
