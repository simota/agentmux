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
    #[cfg(feature = "arena")]
    ShowArenaOverlay,
    #[cfg(feature = "activity-feed")]
    ShowActivityFeed,
    ToggleMessageDetails,
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
    ToggleResultMarker,
    SessionListNext,
    SessionListPrevious,
    FocusSelectedSession,
    #[cfg(feature = "activity-feed")]
    ActivityFeedNext,
    #[cfg(feature = "activity-feed")]
    ActivityFeedPrevious,
    #[cfg(feature = "activity-feed")]
    FocusFeedEntry,
    #[cfg(feature = "arena")]
    ArenaNext,
    #[cfg(feature = "arena")]
    ArenaPrevious,
    #[cfg(feature = "arena")]
    ArenaAdopt,
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
        self.dispatch_with_context(
            key,
            session_list_visible,
            message_bus_visible,
            provider_picker_visible,
            false,
        )
    }

    pub fn dispatch_with_context(
        &mut self,
        key: KeyEvent,
        session_list_visible: bool,
        message_bus_visible: bool,
        provider_picker_visible: bool,
        conversation_list_focused: bool,
    ) -> KeyDispatch {
        self.dispatch_with_activity_feed_context(
            key,
            session_list_visible,
            message_bus_visible,
            provider_picker_visible,
            conversation_list_focused,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn dispatch_with_activity_feed_context(
        &mut self,
        key: KeyEvent,
        session_list_visible: bool,
        message_bus_visible: bool,
        provider_picker_visible: bool,
        conversation_list_focused: bool,
        _activity_feed_focused: bool,
    ) -> KeyDispatch {
        self.dispatch_with_arena_context(
            key,
            session_list_visible,
            message_bus_visible,
            provider_picker_visible,
            conversation_list_focused,
            false,
            _activity_feed_focused,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn dispatch_with_arena_context(
        &mut self,
        key: KeyEvent,
        session_list_visible: bool,
        message_bus_visible: bool,
        provider_picker_visible: bool,
        conversation_list_focused: bool,
        _arena_overlay_visible: bool,
        _activity_feed_focused: bool,
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

        #[cfg(feature = "arena")]
        if _arena_overlay_visible {
            return arena_overlay_command(key)
                .map(KeyDispatch::Command)
                .unwrap_or(KeyDispatch::Consumed);
        }

        if provider_picker_visible {
            return provider_picker_command(key)
                .map(KeyDispatch::Command)
                .unwrap_or(KeyDispatch::Consumed);
        }

        if message_bus_visible {
            return message_list_command(key, true)
                .map(KeyDispatch::Command)
                .unwrap_or(KeyDispatch::Consumed);
        }

        if conversation_list_focused {
            return message_list_command(key, false)
                .map(KeyDispatch::Command)
                .unwrap_or(KeyDispatch::Consumed);
        }

        #[cfg(feature = "activity-feed")]
        if _activity_feed_focused {
            return activity_feed_command(key)
                .map(KeyDispatch::Command)
                .unwrap_or(KeyDispatch::Consumed);
        }

        key_event_bytes(key)
            .map(KeyDispatch::ForwardToFocusedPane)
            .unwrap_or(KeyDispatch::Consumed)
    }
}

#[cfg(feature = "arena")]
fn arena_overlay_command(key: KeyEvent) -> Option<TuiCommand> {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }

    match key.code {
        KeyCode::Down | KeyCode::Char('j') => Some(TuiCommand::ArenaNext),
        KeyCode::Up | KeyCode::Char('k') => Some(TuiCommand::ArenaPrevious),
        KeyCode::Char('a') | KeyCode::Enter => Some(TuiCommand::ArenaAdopt),
        KeyCode::Esc | KeyCode::Char('q') => Some(TuiCommand::CloseOverlay),
        _ => None,
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

fn message_list_command(key: KeyEvent, close_enabled: bool) -> Option<TuiCommand> {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }

    match key.code {
        KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Char('d') => {
            Some(TuiCommand::ToggleMessageDetails)
        }
        KeyCode::Esc | KeyCode::Char('q') if close_enabled => Some(TuiCommand::CloseOverlay),
        _ => None,
    }
}

#[cfg(feature = "activity-feed")]
fn activity_feed_command(key: KeyEvent) -> Option<TuiCommand> {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }

    match key.code {
        KeyCode::Down | KeyCode::Char('j') => Some(TuiCommand::ActivityFeedNext),
        KeyCode::Up | KeyCode::Char('k') => Some(TuiCommand::ActivityFeedPrevious),
        KeyCode::Enter => Some(TuiCommand::FocusFeedEntry),
        KeyCode::Esc | KeyCode::Char('q') => Some(TuiCommand::ClosePane),
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
        KeyCode::Char('?') => Some(TuiCommand::Help),
        KeyCode::Char('z') => Some(TuiCommand::ToggleZoom),
        KeyCode::Left => Some(TuiCommand::Focus(FocusDirection::Left)),
        KeyCode::Right => Some(TuiCommand::Focus(FocusDirection::Right)),
        KeyCode::Up => Some(TuiCommand::Focus(FocusDirection::Up)),
        KeyCode::Down => Some(TuiCommand::Focus(FocusDirection::Down)),
        KeyCode::Char('s') => Some(TuiCommand::ShowSessionList),
        #[cfg(feature = "arena")]
        KeyCode::Char('a') => Some(TuiCommand::ShowArenaOverlay),
        #[cfg(not(feature = "arena"))]
        KeyCode::Char('a') => Some(TuiCommand::ShowAgentList),
        KeyCode::Char('m') => Some(TuiCommand::ShowMessageBus),
        #[cfg(feature = "activity-feed")]
        KeyCode::Char('f') => Some(TuiCommand::ShowActivityFeed),
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
        // `r`/`R` are already bound (ResizeMode / RequestStatus); use `v`
        // ("visibility") for toggling AGENTMUX_RESULT marker display.
        KeyCode::Char('v') => Some(TuiCommand::ToggleResultMarker),
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
    fn prefixed_q_is_consumed_without_quitting() {
        let mut dispatcher = KeymapDispatcher::default();
        dispatcher.dispatch(key(KeyCode::Char('g'), KeyModifiers::CONTROL));

        let dispatch = dispatcher.dispatch(key(KeyCode::Char('q'), KeyModifiers::NONE));

        assert_eq!(dispatch, KeyDispatch::Consumed);
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

    #[cfg(feature = "activity-feed")]
    #[test]
    fn prefixed_f_maps_to_activity_feed_command() {
        let mut dispatcher = KeymapDispatcher::default();
        dispatcher.dispatch(key(KeyCode::Char('g'), KeyModifiers::CONTROL));

        let dispatch = dispatcher.dispatch(key(KeyCode::Char('f'), KeyModifiers::NONE));

        assert_eq!(dispatch, KeyDispatch::Command(TuiCommand::ShowActivityFeed));
    }

    #[cfg(feature = "activity-feed")]
    #[test]
    fn activity_feed_focus_maps_navigation_and_enter() {
        let mut dispatcher = KeymapDispatcher::default();

        assert_eq!(
            dispatcher.dispatch_with_activity_feed_context(
                key(KeyCode::Down, KeyModifiers::NONE),
                false,
                false,
                false,
                false,
                true
            ),
            KeyDispatch::Command(TuiCommand::ActivityFeedNext)
        );
        assert_eq!(
            dispatcher.dispatch_with_activity_feed_context(
                key(KeyCode::Up, KeyModifiers::NONE),
                false,
                false,
                false,
                false,
                true
            ),
            KeyDispatch::Command(TuiCommand::ActivityFeedPrevious)
        );
        assert_eq!(
            dispatcher.dispatch_with_activity_feed_context(
                key(KeyCode::Enter, KeyModifiers::NONE),
                false,
                false,
                false,
                false,
                true
            ),
            KeyDispatch::Command(TuiCommand::FocusFeedEntry)
        );
    }

    #[cfg(feature = "arena")]
    #[test]
    fn prefixed_a_maps_to_arena_overlay_command() {
        let mut dispatcher = KeymapDispatcher::default();
        assert_eq!(
            dispatcher.dispatch(key(KeyCode::Char('g'), KeyModifiers::CONTROL)),
            KeyDispatch::PrefixStarted
        );

        let dispatch = dispatcher.dispatch(key(KeyCode::Char('a'), KeyModifiers::NONE));

        assert_eq!(dispatch, KeyDispatch::Command(TuiCommand::ShowArenaOverlay));
    }

    #[cfg(feature = "arena")]
    #[test]
    fn arena_overlay_maps_navigation_and_adopt() {
        let mut dispatcher = KeymapDispatcher::default();

        assert_eq!(
            dispatcher.dispatch_with_arena_context(
                key(KeyCode::Char('j'), KeyModifiers::NONE),
                false,
                false,
                false,
                false,
                true,
                false,
            ),
            KeyDispatch::Command(TuiCommand::ArenaNext)
        );
        assert_eq!(
            dispatcher.dispatch_with_arena_context(
                key(KeyCode::Char('k'), KeyModifiers::NONE),
                false,
                false,
                false,
                false,
                true,
                false,
            ),
            KeyDispatch::Command(TuiCommand::ArenaPrevious)
        );
        assert_eq!(
            dispatcher.dispatch_with_arena_context(
                key(KeyCode::Char('a'), KeyModifiers::NONE),
                false,
                false,
                false,
                false,
                true,
                false,
            ),
            KeyDispatch::Command(TuiCommand::ArenaAdopt)
        );
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
    fn message_bus_overlay_closes_or_toggles_details() {
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
                key(KeyCode::Enter, KeyModifiers::NONE),
                false,
                true,
                false
            ),
            KeyDispatch::Command(TuiCommand::ToggleMessageDetails)
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
    fn conversation_list_focus_toggles_details_and_consumes_other_keys() {
        let mut dispatcher = KeymapDispatcher::default();

        assert_eq!(
            dispatcher.dispatch_with_context(
                key(KeyCode::Char('d'), KeyModifiers::NONE),
                false,
                false,
                false,
                true
            ),
            KeyDispatch::Command(TuiCommand::ToggleMessageDetails)
        );
        assert_eq!(
            dispatcher.dispatch_with_context(
                key(KeyCode::Char('a'), KeyModifiers::NONE),
                false,
                false,
                false,
                true
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
    fn prefixed_v_maps_to_toggle_result_marker_command() {
        let mut dispatcher = KeymapDispatcher::default();
        dispatcher.dispatch(key(KeyCode::Char('g'), KeyModifiers::CONTROL));

        let dispatch = dispatcher.dispatch(key(KeyCode::Char('v'), KeyModifiers::NONE));

        assert_eq!(dispatch, KeyDispatch::Command(TuiCommand::ToggleResultMarker));
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
