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

    #[test]
    fn prefix_active_mirrors_dispatcher_flag() {
        let mut state = TuiSessionState::default();
        assert!(!state.prefix_active());

        state.set_prefix_active(true);
        assert!(state.prefix_active());

        state.set_prefix_active(false);
        assert!(!state.prefix_active());
    }

    #[test]
    fn focus_position_label_reports_name_and_one_based_index() {
        let mut state = TuiSessionState::default();
        assert_eq!(state.focus_position_label(), None);

        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_a", "name": "planner" }),
        ));
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_b", "name": "impl" }),
        ));

        assert!(state.layout_mut().focus("agent_a"));
        assert_eq!(
            state.focus_position_label().as_deref(),
            Some("focus: planner [1/2]")
        );

        assert!(state.layout_mut().focus("agent_b"));
        assert_eq!(
            state.focus_position_label().as_deref(),
            Some("focus: impl [2/2]")
        );
    }

    // ── Radar edge-case additions ──────────────────────────────────────────
    //
    // Cases 1–3 below guard behaviours the happy-path suite didn't fully cover:
    // (1) focus_previous also clears the unseen marker (not just focus_next /
    //     Focus(Right)), (2) focus_position_label over 3 panes with wrap-around
    //     navigation, and (3) prefix_active stays false after a normal key cycle.

    /// focus_previous (Focus::Left) must clear the unseen-output marker on the
    /// newly focused pane, just like focus_next / Focus::Right does.
    #[test]
    fn focus_previous_clears_unseen_output_on_newly_focused_pane() {
        let mut state = TuiSessionState::default();
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_a", "name": "a" }),
        ));
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_b", "name": "b" }),
        ));
        // Land focus on agent_b so that agent_a is the unfocused peer.
        assert!(state.focus_pane("agent_b"));

        // Output arrives on unfocused agent_a → marker must be set.
        state.apply_event(&event(
            IpcEventKind::PtyOutputChunk,
            json!({ "agent_id": "agent_a", "text": "background work" }),
        ));
        assert!(
            state.pane("agent_a").unwrap().has_unseen_output(),
            "unfocused pane must have unseen-output marker set"
        );

        // Navigate left (focus_previous) → agent_a gains focus; marker cleared.
        state.apply_command(TuiCommand::Focus(FocusDirection::Left));
        assert_eq!(
            state.layout().focused(),
            Some("agent_a"),
            "focus_previous must move to agent_a"
        );
        assert!(
            !state.pane("agent_a").unwrap().has_unseen_output(),
            "focus_previous must clear the unseen-output marker"
        );
    }

    /// focus_position_label must report a correct 1-based index/N as focus
    /// advances through 3 panes and wraps from last back to first.
    #[test]
    fn focus_position_label_wraps_correctly_across_three_panes() {
        let mut state = TuiSessionState::default();
        for (id, name) in [("a1", "alpha"), ("b2", "beta"), ("c3", "gamma")] {
            state.apply_event(&event(
                IpcEventKind::AgentSpawned,
                json!({ "agent_id": id, "name": name }),
            ));
        }

        // After spawn the last-added pane (c3) is focused by apply_agent_spawned.
        // Explicitly focus a1 to start from index 1.
        assert!(state.layout_mut().focus("a1"));
        assert_eq!(
            state.focus_position_label().as_deref(),
            Some("focus: alpha [1/3]")
        );

        state.focus_next(); // → b2
        assert_eq!(
            state.focus_position_label().as_deref(),
            Some("focus: beta [2/3]")
        );

        state.focus_next(); // → c3
        assert_eq!(
            state.focus_position_label().as_deref(),
            Some("focus: gamma [3/3]")
        );

        // Wrap: focus_next on the last pane must return to the first.
        state.focus_next(); // → a1 (wrap)
        assert_eq!(
            state.focus_position_label().as_deref(),
            Some("focus: alpha [1/3]"),
            "focus_next must wrap back to index 1"
        );

        // Reverse wrap: focus_previous on the first pane must jump to the last.
        state.focus_previous(); // → c3 (wrap)
        assert_eq!(
            state.focus_position_label().as_deref(),
            Some("focus: gamma [3/3]"),
            "focus_previous must wrap back to index 3"
        );
    }

    /// set_prefix_active(true) followed by set_prefix_active(false) must return
    /// false.  A call sequence that never activates the prefix must also stay
    /// false — the flag must not toggle spontaneously.
    #[test]
    fn prefix_active_stays_false_after_reset_and_without_activation() {
        let mut state = TuiSessionState::default();

        // Never activated → stays false.
        assert!(!state.prefix_active(), "fresh state has prefix_active = false");

        // Full Ctrl-g → command cycle: true then back to false.
        state.set_prefix_active(true);
        assert!(state.prefix_active());
        state.set_prefix_active(false);
        assert!(!state.prefix_active(), "prefix_active must be false after reset");

        // A second reset without a prior activation must still be false.
        state.set_prefix_active(false);
        assert!(!state.prefix_active(), "repeated false sets must stay false");
    }

    #[test]
    fn unseen_output_flag_sets_on_unfocused_pane_and_clears_on_focus() {
        let mut state = TuiSessionState::default();
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_a", "name": "a" }),
        ));
        state.apply_event(&event(
            IpcEventKind::AgentSpawned,
            json!({ "agent_id": "agent_b", "name": "b" }),
        ));
        // Focus agent_a explicitly so the unfocused pane is agent_b.
        assert!(state.focus_pane("agent_a"));
        assert_eq!(state.layout().focused(), Some("agent_a"));

        // Output to the focused pane does NOT raise the marker.
        state.apply_event(&event(
            IpcEventKind::PtyOutputChunk,
            json!({ "agent_id": "agent_a", "text": "hi" }),
        ));
        assert!(!state.pane("agent_a").expect("pane a").has_unseen_output());

        // Output to the unfocused pane raises the marker.
        state.apply_event(&event(
            IpcEventKind::PtyOutputChunk,
            json!({ "agent_id": "agent_b", "text": "work" }),
        ));
        assert!(state.pane("agent_b").expect("pane b").has_unseen_output());

        // Focusing the pane clears it.
        assert!(state.focus_pane("agent_b"));
        assert!(!state.pane("agent_b").expect("pane b").has_unseen_output());

        // Re-raise on agent_b while focus rests on agent_a, then verify
        // directional focus navigation also clears the marker.
        assert!(state.focus_pane("agent_a"));
        state.apply_event(&event(
            IpcEventKind::PtyOutputChunk,
            json!({ "agent_id": "agent_b", "text": "more" }),
        ));
        assert!(state.pane("agent_b").expect("pane b").has_unseen_output());
        state.apply_command(TuiCommand::Focus(FocusDirection::Right));
        assert_eq!(state.layout().focused(), Some("agent_b"));
        assert!(!state.pane("agent_b").expect("pane b").has_unseen_output());
    }

