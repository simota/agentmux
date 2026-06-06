use super::*;

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

