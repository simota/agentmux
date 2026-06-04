//! Client-side keymap dispatching.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

const CTRL_C: &[u8] = b"\x03";

/// Pane focus direction selected from a prefixed arrow key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FocusDirection {
    Left,
    Right,
    Up,
    Down,
}

/// TUI command produced by the prefix keymap.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TuiCommand {
    Detach,
    Quit,
    Help,
    ToggleZoom,
    Focus(FocusDirection),
    ShowSessionList,
    ShowAgentList,
    ShowMessageBus,
    ShowContextBoard,
    ShowApprovalQueue,
    SplitVertical,
    SplitHorizontal,
    ProviderNext,
    ProviderPrevious,
    SelectProvider,
    ClosePane,
    ResizeMode,
    RotateLayout,
    PasteQueuedMessage,
    InjectSelectedMessage,
    RequestStatus,
    AttachContext,
    RunTests,
    InterruptAgent,
    CommandPalette,
    EnterCopyMode,
    SessionListNext,
    SessionListPrevious,
    FocusSelectedSession,
    CloseOverlay,
}

/// Result of routing one terminal key event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KeyDispatch {
    /// Forward raw bytes to the currently focused agent pane.
    ForwardToFocusedPane(Vec<u8>),
    /// Execute a client-side TUI command.
    Command(TuiCommand),
    /// Prefix key was consumed; the next key decides the command.
    PrefixStarted,
    /// Event was consumed or ignored without producing an action.
    Consumed,
}

/// Stateful dispatcher for the tmux-like `Ctrl-g` prefix keymap.
#[derive(Clone, Debug)]
pub struct KeymapDispatcher {
    prefix: KeyBinding,
    awaiting_prefix_command: bool,
}

impl Default for KeymapDispatcher {
    fn default() -> Self {
        Self::new(KeyBinding::ctrl_char('g'))
    }
}

impl KeymapDispatcher {
    pub fn new(prefix: KeyBinding) -> Self {
        Self {
            prefix,
            awaiting_prefix_command: false,
        }
    }

    pub fn is_awaiting_prefix_command(&self) -> bool {
        self.awaiting_prefix_command
    }

    pub fn dispatch(&mut self, key: KeyEvent) -> KeyDispatch {
        self.dispatch_with_session_list(key, false)
    }

    pub fn dispatch_with_session_list(
        &mut self,
        key: KeyEvent,
        session_list_visible: bool,
    ) -> KeyDispatch {
        self.dispatch_with_overlays(key, session_list_visible, false, false)
    }

    pub fn dispatch_with_overlays(
        &mut self,
        key: KeyEvent,
        session_list_visible: bool,
        message_bus_visible: bool,
        provider_picker_visible: bool,
    ) -> KeyDispatch {
        if matches!(key.kind, KeyEventKind::Release) {
            return KeyDispatch::Consumed;
        }

        if self.prefix.matches(key) {
            self.awaiting_prefix_command = true;
            return KeyDispatch::PrefixStarted;
        }

        if self.awaiting_prefix_command {
            self.awaiting_prefix_command = false;
            return prefix_command(key)
                .map(KeyDispatch::Command)
                .unwrap_or(KeyDispatch::Consumed);
        }

        if session_list_visible {
            return overlay_navigation_command(key)
                .map(KeyDispatch::Command)
                .unwrap_or(KeyDispatch::Consumed);
        }

        if provider_picker_visible {
            return provider_picker_command(key)
                .map(KeyDispatch::Command)
                .unwrap_or(KeyDispatch::Consumed);
        }

        if message_bus_visible {
            return overlay_close_command(key)
                .map(KeyDispatch::Command)
                .unwrap_or(KeyDispatch::Consumed);
        }

        key_event_bytes(key)
            .map(KeyDispatch::ForwardToFocusedPane)
            .unwrap_or(KeyDispatch::Consumed)
    }
}

/// A key binding matched by code plus required modifiers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyBinding {
    code: KeyCode,
    modifiers: KeyModifiers,
}

impl KeyBinding {
    pub fn ctrl_char(ch: char) -> Self {
        Self {
            code: KeyCode::Char(ch.to_ascii_lowercase()),
            modifiers: KeyModifiers::CONTROL,
        }
    }

    fn matches(self, key: KeyEvent) -> bool {
        key.code == self.code && key.modifiers == self.modifiers
    }
}

fn overlay_navigation_command(key: KeyEvent) -> Option<TuiCommand> {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }

    match key.code {
        KeyCode::Down | KeyCode::Char('j') => Some(TuiCommand::SessionListNext),
        KeyCode::Up | KeyCode::Char('k') => Some(TuiCommand::SessionListPrevious),
        KeyCode::Enter => Some(TuiCommand::FocusSelectedSession),
        KeyCode::Esc | KeyCode::Char('q') => Some(TuiCommand::CloseOverlay),
        _ => None,
    }
}

fn provider_picker_command(key: KeyEvent) -> Option<TuiCommand> {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }

    match key.code {
        KeyCode::Down | KeyCode::Char('j') => Some(TuiCommand::ProviderNext),
        KeyCode::Up | KeyCode::Char('k') => Some(TuiCommand::ProviderPrevious),
        KeyCode::Char('1') => Some(TuiCommand::SelectProvider),
        KeyCode::Char('2') => Some(TuiCommand::ProviderNext),
        KeyCode::Char('3') => Some(TuiCommand::ProviderPrevious),
        KeyCode::Enter => Some(TuiCommand::SelectProvider),
        KeyCode::Esc | KeyCode::Char('q') => Some(TuiCommand::CloseOverlay),
        _ => None,
    }
}

fn overlay_close_command(key: KeyEvent) -> Option<TuiCommand> {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }

    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => Some(TuiCommand::CloseOverlay),
        _ => None,
    }
}

fn prefix_command(key: KeyEvent) -> Option<TuiCommand> {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }

    match key.code {
        KeyCode::Char('d') => Some(TuiCommand::Detach),
        KeyCode::Char('q') => Some(TuiCommand::Quit),
        KeyCode::Char('?') => Some(TuiCommand::Help),
        KeyCode::Char('z') => Some(TuiCommand::ToggleZoom),
        KeyCode::Left => Some(TuiCommand::Focus(FocusDirection::Left)),
        KeyCode::Right => Some(TuiCommand::Focus(FocusDirection::Right)),
        KeyCode::Up => Some(TuiCommand::Focus(FocusDirection::Up)),
        KeyCode::Down => Some(TuiCommand::Focus(FocusDirection::Down)),
        KeyCode::Char('s') => Some(TuiCommand::ShowSessionList),
        KeyCode::Char('a') => Some(TuiCommand::ShowAgentList),
        KeyCode::Char('m') => Some(TuiCommand::ShowMessageBus),
        KeyCode::Char('c') => Some(TuiCommand::ShowContextBoard),
        KeyCode::Char('A') => Some(TuiCommand::ShowApprovalQueue),
        KeyCode::Char('%') => Some(TuiCommand::SplitVertical),
        KeyCode::Char('"') => Some(TuiCommand::SplitHorizontal),
        KeyCode::Char('x') => Some(TuiCommand::ClosePane),
        KeyCode::Char('r') => Some(TuiCommand::ResizeMode),
        KeyCode::Char(' ') => Some(TuiCommand::RotateLayout),
        KeyCode::Char('p') => Some(TuiCommand::PasteQueuedMessage),
        KeyCode::Char('i') => Some(TuiCommand::InjectSelectedMessage),
        KeyCode::Char('R') => Some(TuiCommand::RequestStatus),
        KeyCode::Char('C') => Some(TuiCommand::AttachContext),
        KeyCode::Char('T') => Some(TuiCommand::RunTests),
        KeyCode::Char('I') => Some(TuiCommand::InterruptAgent),
        KeyCode::Char(':') => Some(TuiCommand::CommandPalette),
        KeyCode::Char('[') => Some(TuiCommand::EnterCopyMode),
        _ => None,
    }
}

fn key_event_bytes(key: KeyEvent) -> Option<Vec<u8>> {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('c') | KeyCode::Char('C') => Some(CTRL_C.to_vec()),
            KeyCode::Char(ch) if ch.is_ascii_alphabetic() => {
                Some(vec![ch.to_ascii_lowercase() as u8 - b'a' + 1])
            }
            _ => None,
        };
    }

    match key.code {
        KeyCode::Backspace => Some(vec![0x7f]),
        KeyCode::Enter => Some(b"\r".to_vec()),
        KeyCode::Left => Some(b"\x1b[D".to_vec()),
        KeyCode::Right => Some(b"\x1b[C".to_vec()),
        KeyCode::Up => Some(b"\x1b[A".to_vec()),
        KeyCode::Down => Some(b"\x1b[B".to_vec()),
        KeyCode::Home => Some(b"\x1b[H".to_vec()),
        KeyCode::End => Some(b"\x1b[F".to_vec()),
        KeyCode::PageUp => Some(b"\x1b[5~".to_vec()),
        KeyCode::PageDown => Some(b"\x1b[6~".to_vec()),
        KeyCode::Tab => Some(b"\t".to_vec()),
        KeyCode::Esc => Some(b"\x1b".to_vec()),
        KeyCode::Char(ch) => Some(ch.to_string().into_bytes()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventState;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn prefix_key_is_consumed_and_not_forwarded_to_agent() {
        let mut dispatcher = KeymapDispatcher::default();

        let dispatch = dispatcher.dispatch(key(KeyCode::Char('g'), KeyModifiers::CONTROL));

        assert_eq!(dispatch, KeyDispatch::PrefixStarted);
        assert!(dispatcher.is_awaiting_prefix_command());
    }

    #[test]
    fn prefixed_key_maps_to_tui_command() {
        let mut dispatcher = KeymapDispatcher::default();
        assert_eq!(
            dispatcher.dispatch(key(KeyCode::Char('g'), KeyModifiers::CONTROL)),
            KeyDispatch::PrefixStarted
        );

        let dispatch = dispatcher.dispatch(key(KeyCode::Char('z'), KeyModifiers::NONE));

        assert_eq!(dispatch, KeyDispatch::Command(TuiCommand::ToggleZoom));
        assert!(!dispatcher.is_awaiting_prefix_command());
    }

    #[test]
    fn prefixed_q_maps_to_quit_command() {
        let mut dispatcher = KeymapDispatcher::default();
        dispatcher.dispatch(key(KeyCode::Char('g'), KeyModifiers::CONTROL));

        let dispatch = dispatcher.dispatch(key(KeyCode::Char('q'), KeyModifiers::NONE));

        assert_eq!(dispatch, KeyDispatch::Command(TuiCommand::Quit));
    }

    #[test]
    fn prefixed_question_mark_maps_to_help_command() {
        let mut dispatcher = KeymapDispatcher::default();
        dispatcher.dispatch(key(KeyCode::Char('g'), KeyModifiers::CONTROL));

        let dispatch = dispatcher.dispatch(key(KeyCode::Char('?'), KeyModifiers::NONE));

        assert_eq!(dispatch, KeyDispatch::Command(TuiCommand::Help));
    }

    #[test]
    fn prefixed_s_maps_to_session_list_command() {
        let mut dispatcher = KeymapDispatcher::default();
        dispatcher.dispatch(key(KeyCode::Char('g'), KeyModifiers::CONTROL));

        let dispatch = dispatcher.dispatch(key(KeyCode::Char('s'), KeyModifiers::NONE));

        assert_eq!(dispatch, KeyDispatch::Command(TuiCommand::ShowSessionList));
    }

    #[test]
    fn bare_q_is_forwarded_to_focused_pane() {
        let mut dispatcher = KeymapDispatcher::default();

        let dispatch = dispatcher.dispatch(key(KeyCode::Char('q'), KeyModifiers::NONE));

        assert_eq!(dispatch, KeyDispatch::ForwardToFocusedPane(b"q".to_vec()));
    }

    #[test]
    fn bare_escape_is_forwarded_to_focused_pane() {
        let mut dispatcher = KeymapDispatcher::default();

        let dispatch = dispatcher.dispatch(key(KeyCode::Esc, KeyModifiers::NONE));

        assert_eq!(
            dispatch,
            KeyDispatch::ForwardToFocusedPane(b"\x1b".to_vec())
        );
    }

    #[test]
    fn session_list_keys_map_to_selection_commands_when_visible() {
        let mut dispatcher = KeymapDispatcher::default();

        assert_eq!(
            dispatcher.dispatch_with_session_list(key(KeyCode::Down, KeyModifiers::NONE), true),
            KeyDispatch::Command(TuiCommand::SessionListNext)
        );
        assert_eq!(
            dispatcher.dispatch_with_session_list(key(KeyCode::Up, KeyModifiers::NONE), true),
            KeyDispatch::Command(TuiCommand::SessionListPrevious)
        );
        assert_eq!(
            dispatcher.dispatch_with_session_list(key(KeyCode::Enter, KeyModifiers::NONE), true),
            KeyDispatch::Command(TuiCommand::FocusSelectedSession)
        );
        assert_eq!(
            dispatcher.dispatch_with_session_list(key(KeyCode::Esc, KeyModifiers::NONE), true),
            KeyDispatch::Command(TuiCommand::CloseOverlay)
        );
    }

    #[test]
    fn session_list_consumes_regular_keys_when_visible() {
        let mut dispatcher = KeymapDispatcher::default();

        let dispatch = dispatcher
            .dispatch_with_session_list(key(KeyCode::Char('a'), KeyModifiers::NONE), true);

        assert_eq!(dispatch, KeyDispatch::Consumed);
    }

    #[test]
    fn message_bus_overlay_closes_on_escape_or_q() {
        let mut dispatcher = KeymapDispatcher::default();

        assert_eq!(
            dispatcher.dispatch_with_overlays(
                key(KeyCode::Esc, KeyModifiers::NONE),
                false,
                true,
                false
            ),
            KeyDispatch::Command(TuiCommand::CloseOverlay)
        );
        assert_eq!(
            dispatcher.dispatch_with_overlays(
                key(KeyCode::Char('q'), KeyModifiers::NONE),
                false,
                true,
                false
            ),
            KeyDispatch::Command(TuiCommand::CloseOverlay)
        );
        assert_eq!(
            dispatcher.dispatch_with_overlays(
                key(KeyCode::Char('a'), KeyModifiers::NONE),
                false,
                true,
                false
            ),
            KeyDispatch::Consumed
        );
    }

    #[test]
    fn provider_picker_keys_map_to_selection_commands_when_visible() {
        let mut dispatcher = KeymapDispatcher::default();

        assert_eq!(
            dispatcher.dispatch_with_overlays(
                key(KeyCode::Down, KeyModifiers::NONE),
                false,
                false,
                true
            ),
            KeyDispatch::Command(TuiCommand::ProviderNext)
        );
        assert_eq!(
            dispatcher.dispatch_with_overlays(
                key(KeyCode::Up, KeyModifiers::NONE),
                false,
                false,
                true
            ),
            KeyDispatch::Command(TuiCommand::ProviderPrevious)
        );
        assert_eq!(
            dispatcher.dispatch_with_overlays(
                key(KeyCode::Enter, KeyModifiers::NONE),
                false,
                false,
                true
            ),
            KeyDispatch::Command(TuiCommand::SelectProvider)
        );
    }

    #[test]
    fn prefixed_x_maps_to_close_pane_command() {
        let mut dispatcher = KeymapDispatcher::default();
        dispatcher.dispatch(key(KeyCode::Char('g'), KeyModifiers::CONTROL));

        let dispatch = dispatcher.dispatch(key(KeyCode::Char('x'), KeyModifiers::NONE));

        assert_eq!(dispatch, KeyDispatch::Command(TuiCommand::ClosePane));
    }

    #[test]
    fn prefixed_bracket_maps_to_copy_mode_command() {
        let mut dispatcher = KeymapDispatcher::default();
        dispatcher.dispatch(key(KeyCode::Char('g'), KeyModifiers::CONTROL));

        let dispatch = dispatcher.dispatch(key(KeyCode::Char('['), KeyModifiers::NONE));

        assert_eq!(dispatch, KeyDispatch::Command(TuiCommand::EnterCopyMode));
    }

    #[test]
    fn prefixed_arrow_maps_to_focus_command() {
        let mut dispatcher = KeymapDispatcher::default();
        dispatcher.dispatch(key(KeyCode::Char('g'), KeyModifiers::CONTROL));

        let dispatch = dispatcher.dispatch(key(KeyCode::Right, KeyModifiers::NONE));

        assert_eq!(
            dispatch,
            KeyDispatch::Command(TuiCommand::Focus(FocusDirection::Right))
        );
    }

    #[test]
    fn shifted_prefixed_keys_map_to_commands() {
        let mut dispatcher = KeymapDispatcher::default();
        dispatcher.dispatch(key(KeyCode::Char('g'), KeyModifiers::CONTROL));
        assert_eq!(
            dispatcher.dispatch(key(KeyCode::Char('A'), KeyModifiers::SHIFT)),
            KeyDispatch::Command(TuiCommand::ShowApprovalQueue)
        );

        dispatcher.dispatch(key(KeyCode::Char('g'), KeyModifiers::CONTROL));
        assert_eq!(
            dispatcher.dispatch(key(KeyCode::Char('%'), KeyModifiers::SHIFT)),
            KeyDispatch::Command(TuiCommand::SplitVertical)
        );
    }

    #[test]
    fn regular_keys_are_forwarded_to_focused_pane() {
        let mut dispatcher = KeymapDispatcher::default();

        assert_eq!(
            dispatcher.dispatch(key(KeyCode::Char('a'), KeyModifiers::NONE)),
            KeyDispatch::ForwardToFocusedPane(b"a".to_vec())
        );
        assert_eq!(
            dispatcher.dispatch(key(KeyCode::Enter, KeyModifiers::NONE)),
            KeyDispatch::ForwardToFocusedPane(b"\r".to_vec())
        );
    }

    #[test]
    fn ctrl_c_is_forwarded_to_focused_pane() {
        let mut dispatcher = KeymapDispatcher::default();

        let dispatch = dispatcher.dispatch(key(KeyCode::Char('c'), KeyModifiers::CONTROL));

        assert_eq!(dispatch, KeyDispatch::ForwardToFocusedPane(CTRL_C.to_vec()));
    }

    #[test]
    fn unknown_prefixed_key_is_consumed() {
        let mut dispatcher = KeymapDispatcher::default();
        dispatcher.dispatch(key(KeyCode::Char('g'), KeyModifiers::CONTROL));

        let dispatch = dispatcher.dispatch(key(KeyCode::F(1), KeyModifiers::NONE));

        assert_eq!(dispatch, KeyDispatch::Consumed);
        assert!(!dispatcher.is_awaiting_prefix_command());
    }
}
