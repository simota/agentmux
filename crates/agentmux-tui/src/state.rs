//! Pure TUI session state updated from daemon events.
//!
//! This module intentionally contains no terminal I/O. The interactive run loop
//! can apply daemon events here, then ask `layout`/`render` to draw the result.

use std::collections::BTreeMap;

use agentmux_ipc::protocol::{DaemonEvent, IpcEventKind};
use agentmux_terminal::{CellStyle, ScreenGrid, TerminalParser};

use crate::keymap::{FocusDirection, TuiCommand};
use crate::layout::{PaneLayout, SplitDirection};

/// Stable pane state derived from daemon agent/session events.
pub struct AgentPaneState {
    agent_id: String,
    name: String,
    process_id: Option<u32>,
    status: Option<String>,
    terminal: TerminalParser,
    last_event: Option<IpcEventKind>,
}

impl AgentPaneState {
    fn new(agent_id: String, name: String, process_id: Option<u32>, size: TerminalSize) -> Self {
        Self {
            agent_id,
            name,
            process_id,
            status: None,
            terminal: TerminalParser::new(size.rows, size.cols),
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

    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub fn grid(&self) -> &ScreenGrid {
        self.terminal.grid()
    }

    pub fn last_event(&self) -> Option<&IpcEventKind> {
        self.last_event.as_ref()
    }

    pub fn chrome_title(&self) -> String {
        match self.status.as_deref() {
            Some(status) => format!("{} | {}", self.name, status),
            None => self.name.clone(),
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

    pub fn last_event(&self) -> Option<&IpcEventKind> {
        self.last_event.as_ref()
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
                self.focus_next();
                CommandEffect::Continue
            }
            TuiCommand::Focus(FocusDirection::Left | FocusDirection::Up) => {
                self.focus_previous();
                CommandEffect::Continue
            }
            TuiCommand::ToggleZoom => {
                self.toggle_zoom();
                CommandEffect::Continue
            }
            TuiCommand::Detach => CommandEffect::Detach,
            TuiCommand::Quit => CommandEffect::Quit,
            _ => CommandEffect::Unhandled(command),
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

            if let Some(pane) = self.panes.get_mut(&agent_id) {
                pane.name = name;
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
                pane.status = status;
                pane.last_event = None;
                self.layout.add_pane(agent_id.clone());
                self.panes.insert(agent_id.clone(), pane);
            }
            applied += 1;
        }

        applied
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
        pane.process_id = process_id;
        pane.terminal = TerminalParser::new(rows, cols);
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
                for ch in text.chars().take(usize::from(cols)) {
                    grid.write_char(ch, CellStyle::default());
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
            _ => StateChange::Ignored,
        }
    }

    fn apply_agent_spawned(&mut self, event: &DaemonEvent) -> StateChange {
        let Some(agent_id) = string_field(&event.payload, "agent_id") else {
            return StateChange::Ignored;
        };
        let name = string_field(&event.payload, "name").unwrap_or_else(|| agent_id.clone());
        let process_id = event
            .payload
            .get("process_id")
            .and_then(|value| value.as_u64())
            .and_then(|value| u32::try_from(value).ok());

        if let Some(pane) = self.panes.get_mut(&agent_id) {
            pane.name = name;
            pane.process_id = process_id;
            pane.last_event = Some(IpcEventKind::AgentSpawned);
            return StateChange::UpdatedPane(agent_id);
        }

        let pane = AgentPaneState::new(
            agent_id.clone(),
            name,
            process_id,
            self.default_terminal_size,
        );
        self.layout.add_pane(agent_id.clone());
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
        pane.last_event = Some(event.kind.clone());
        StateChange::UpdatedPane(agent_id)
    }

    fn apply_agent_exited(&mut self, event: &DaemonEvent) -> StateChange {
        let Some(agent_id) = string_field(&event.payload, "agent_id") else {
            return StateChange::Ignored;
        };

        let Some(pane) = self.panes.get_mut(&agent_id) else {
            return StateChange::Ignored;
        };
        pane.status = Some("exited".to_string());
        pane.process_id = None;
        pane.last_event = Some(IpcEventKind::AgentExited);
        StateChange::UpdatedPane(agent_id)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StateChange {
    AddedPane(String),
    UpdatedPane(String),
    FocusedPane(String),
    Ignored,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandEffect {
    Continue,
    Detach,
    Quit,
    Unhandled(TuiCommand),
}

fn string_field(payload: &serde_json::Value, field: &str) -> Option<String> {
    payload
        .get(field)
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned)
}

fn output_bytes(payload: &serde_json::Value) -> Option<Vec<u8>> {
    if let Some(text) = payload
        .get("text")
        .or_else(|| payload.get("data"))
        .or_else(|| payload.get("chunk"))
        .and_then(|value| value.as_str())
    {
        return Some(text.as_bytes().to_vec());
    }

    payload
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
                "process_id": 42
            }),
        ));

        assert_eq!(change, StateChange::AddedPane("agent_001".to_string()));
        assert_eq!(state.layout().panes(), &["agent_001".to_string()]);
        assert_eq!(state.layout().focused(), Some("agent_001"));
        let pane = state.pane("agent_001").expect("pane");
        assert_eq!(pane.name(), "impl-codex");
        assert_eq!(pane.process_id(), Some(42));
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
    fn exited_event_marks_pane_without_removing_it() {
        let mut state = TuiSessionState::default();
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_001", "name": "impl", "process_id": 42 }),
        ));

        let change = state.apply_event(&event(
            IpcEventKind::AgentExited,
            json!({ "agent_id": "agent_001", "exit_status": 0 }),
        ));

        assert_eq!(change, StateChange::UpdatedPane("agent_001".to_string()));
        let pane = state.pane("agent_001").expect("pane");
        assert_eq!(pane.status(), Some("exited"));
        assert_eq!(pane.process_id(), None);
        assert_eq!(state.layout().panes(), &["agent_001".to_string()]);
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
                json!({ "message_id": "m" })
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
            state.apply_command(TuiCommand::Detach),
            CommandEffect::Detach
        );
        assert_eq!(state.apply_command(TuiCommand::Quit), CommandEffect::Quit);
        assert_eq!(
            state.apply_command(TuiCommand::Help),
            CommandEffect::Unhandled(TuiCommand::Help)
        );
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
}
