//! Pure TUI session state updated from daemon events.
//!
//! This module intentionally contains no terminal I/O. The interactive run loop
//! can apply daemon events here, then ask `layout`/`render` to draw the result.

use std::collections::BTreeMap;

use agentmux_ipc::protocol::{DaemonEvent, IpcEventKind};
use agentmux_terminal::{CellStyle, ScreenGrid, TerminalParser};
use serde_json::Value;
use unicode_width::UnicodeWidthChar;

use crate::keymap::{FocusDirection, TuiCommand};
use crate::layout::{PaneLayout, SplitDirection};

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
    messages: Vec<MessageListItem>,
    provider_picker_visible: bool,
    provider_picker_selected: usize,
    copy_selection: Option<CopySelection>,
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
            messages: Vec::new(),
            provider_picker_visible: false,
            provider_picker_selected: 0,
            copy_selection: None,
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
                .selected_provider()
                .map(|provider| {
                    self.provider_picker_visible = false;
                    CommandEffect::SpawnAgentPane(provider)
                })
                .unwrap_or(CommandEffect::Continue),
            TuiCommand::ClosePane => self
                .focused_pane()
                .map(|pane| CommandEffect::StopPane(pane.agent_id().to_string()))
                .unwrap_or(CommandEffect::Continue),
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
                }
                CommandEffect::Continue
            }
            TuiCommand::ShowSessionList => {
                self.session_list_visible = !self.session_list_visible;
                if self.session_list_visible {
                    self.keybinding_help_visible = false;
                    self.message_bus_visible = false;
                    self.provider_picker_visible = false;
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
                    CommandEffect::RefreshMessages
                } else {
                    CommandEffect::Continue
                }
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
        if self.provider_picker_selected >= PROVIDER_OPTIONS.len() {
            self.provider_picker_selected = 0;
        }
    }

    fn move_provider_selection(&mut self, delta: isize) {
        let count = PROVIDER_OPTIONS.len() as isize;
        let current = isize::try_from(self.provider_picker_selected).unwrap_or(0);
        self.provider_picker_selected = (current + delta).rem_euclid(count) as usize;
    }

    fn selected_provider(&self) -> Option<AgentProviderChoice> {
        PROVIDER_OPTIONS
            .get(self.provider_picker_selected)
            .map(|option| option.provider)
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
        let Some(agents) = payload.get("agents").and_then(|value| value.as_array()) else {
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
            } else {
                let mut pane = AgentPaneState::new(
                    agent_id.clone(),
                    name,
                    process_id,
                    self.default_terminal_size,
                );
                pane.role = role;
                pane.status = status;
                pane.last_event = None;
                self.layout.add_pane(agent_id.clone());
                self.panes.insert(agent_id.clone(), pane);
            }
            applied += 1;
        }

        self.clamp_session_list_selection();
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
            _ => StateChange::Ignored,
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
        self.clamp_session_list_selection();
        StateChange::RemovedPane(agent_id)
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
pub struct ProviderOption {
    pub provider: AgentProviderChoice,
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
        provider: AgentProviderChoice::Claude,
        hint: "Claude Code",
    },
    ProviderOption {
        provider: AgentProviderChoice::Codex,
        hint: "OpenAI Codex",
    },
    ProviderOption {
        provider: AgentProviderChoice::Agy,
        hint: "Google Antigravity CLI",
    },
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageListItem {
    pub message_id: String,
    pub created_at: String,
    pub delivery_status: String,
    pub kind: String,
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
        assert_eq!(pane.grid().cursor().col, 3);
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
