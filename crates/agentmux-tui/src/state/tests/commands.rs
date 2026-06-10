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
fn result_marker_hidden_by_default_and_toggle_flips_it() {
    let mut state = TuiSessionState::default();
    assert!(state.hide_result_marker());

    state.toggle_result_marker();
    assert!(!state.hide_result_marker());

    state.toggle_result_marker();
    assert!(state.hide_result_marker());
}

#[test]
fn apply_toggle_result_marker_command_flips_flag() {
    let mut state = TuiSessionState::default();
    assert!(state.hide_result_marker());

    assert_eq!(
        state.apply_command(TuiCommand::ToggleResultMarker),
        CommandEffect::Continue
    );
    assert!(!state.hide_result_marker());
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
    // ConversationList is the second-to-last provider option (Commands is the
    // last), so step back twice from index 0: wrap to Commands, then land on
    // ConversationList.
    state.apply_command(TuiCommand::ProviderPrevious);
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
fn provider_picker_can_open_and_close_commands_pane() {
    let mut state = TuiSessionState::default();
    state.apply_event(&event(
        IpcEventKind::AgentSpawned,
        json!({ "agent_id": "agent_a", "name": "a" }),
    ));
    state.layout_mut().focus("agent_a");
    state.open_provider_picker();
    // Commands is the last provider option.
    for _ in 0..(state.provider_options().len() - 1) {
        state.apply_command(TuiCommand::ProviderNext);
    }
    assert_eq!(
        state.provider_options().last().unwrap().choice,
        crate::state::NewPaneChoice::Commands
    );

    assert_eq!(
        state.apply_command(TuiCommand::SelectProvider),
        CommandEffect::OpenCommandsPane
    );
    assert!(!state.provider_picker_visible());
    assert!(state.is_commands_pane(crate::state::COMMANDS_PANE_ID));
    assert_eq!(
        state.layout().focused(),
        Some(crate::state::COMMANDS_PANE_ID)
    );

    assert_eq!(
        state.apply_command(TuiCommand::ClosePane),
        CommandEffect::Continue
    );
    assert!(!state.is_commands_pane(crate::state::COMMANDS_PANE_ID));
    assert_eq!(state.layout().focused(), Some("agent_a"));
}

#[test]
fn commands_input_editing_pushes_backspaces_and_clears() {
    let mut state = TuiSessionState::default();
    assert_eq!(state.commands_input_buffer(), "");

    state.commands_input_push('h');
    state.commands_input_push('i');
    state.commands_input_push('!');
    assert_eq!(state.commands_input_buffer(), "hi!");

    state.commands_input_backspace();
    assert_eq!(state.commands_input_buffer(), "hi");

    state.commands_input_clear();
    assert_eq!(state.commands_input_buffer(), "");
    assert_eq!(state.commands_input_cursor(), 0);
    // Backspace on an empty buffer is a no-op.
    state.commands_input_backspace();
    assert_eq!(state.commands_input_buffer(), "");
}

/// The caret moves with Left/Right/Home/End and clamps at both buffer ends.
#[test]
fn commands_input_cursor_moves_and_clamps() {
    let mut state = TuiSessionState::default();
    for ch in "abc".chars() {
        state.commands_input_push(ch);
    }
    assert_eq!(state.commands_input_cursor(), 3);

    state.commands_input_move_right(); // already at the end: no-op
    assert_eq!(state.commands_input_cursor(), 3);
    state.commands_input_move_left();
    state.commands_input_move_left();
    assert_eq!(state.commands_input_cursor(), 1);
    state.commands_input_move_home();
    assert_eq!(state.commands_input_cursor(), 0);
    state.commands_input_move_left(); // already at the start: no-op
    assert_eq!(state.commands_input_cursor(), 0);
    state.commands_input_move_right();
    assert_eq!(state.commands_input_cursor(), 1);
    state.commands_input_move_end();
    assert_eq!(state.commands_input_cursor(), 3);
}

/// Insertion and deletion happen at the caret, staying char-boundary safe for
/// multibyte text (Japanese and emoji).
#[test]
fn commands_input_edits_at_cursor_are_char_boundary_safe() {
    let mut state = TuiSessionState::default();
    for ch in "あい😀".chars() {
        state.commands_input_push(ch);
    }
    assert_eq!(state.commands_input_buffer(), "あい😀");

    // Insert in the middle: あ|い😀 → あxい😀
    state.commands_input_move_home();
    state.commands_input_move_right();
    state.commands_input_push('x');
    assert_eq!(state.commands_input_buffer(), "あxい😀");
    assert_eq!(state.commands_input_cursor(), 2);

    // Backspace removes the char before the caret (the inserted 'x').
    state.commands_input_backspace();
    assert_eq!(state.commands_input_buffer(), "あい😀");
    assert_eq!(state.commands_input_cursor(), 1);

    // Delete removes the char under the caret ('い').
    state.commands_input_delete();
    assert_eq!(state.commands_input_buffer(), "あ😀");
    assert_eq!(state.commands_input_cursor(), 1);

    // Delete at the end of the buffer is a no-op.
    state.commands_input_move_end();
    state.commands_input_delete();
    assert_eq!(state.commands_input_buffer(), "あ😀");
}

/// A pasted string is inserted whole at the caret and the caret lands after it.
#[test]
fn commands_input_insert_str_inserts_at_cursor() {
    let mut state = TuiSessionState::default();
    for ch in "ad".chars() {
        state.commands_input_push(ch);
    }
    state.commands_input_move_left();
    state.commands_input_insert_str("bあc");
    assert_eq!(state.commands_input_buffer(), "abあcd");
    assert_eq!(state.commands_input_cursor(), 4);

    // Empty paste is a no-op.
    state.commands_input_insert_str("");
    assert_eq!(state.commands_input_buffer(), "abあcd");
    assert_eq!(state.commands_input_cursor(), 4);
}

/// Every caret operation on an empty buffer is a safe no-op: no panic and no
/// caret drift away from index 0.
#[test]
fn commands_input_empty_buffer_ops_are_noops() {
    let mut state = TuiSessionState::default();

    state.commands_input_delete();
    state.commands_input_move_left();
    state.commands_input_move_right();
    state.commands_input_move_end();
    state.commands_input_move_home();

    assert_eq!(state.commands_input_buffer(), "");
    assert_eq!(state.commands_input_cursor(), 0);

    // Insert after the no-op storm lands at index 0 as usual.
    state.commands_input_push('a');
    assert_eq!(state.commands_input_buffer(), "a");
    assert_eq!(state.commands_input_cursor(), 1);
}

/// Editing is Unicode-scalar based: deleting after a ZWJ emoji sequence or a
/// combining mark removes exactly one scalar per keypress, never splitting a
/// char (no byte-boundary panic).
#[test]
fn commands_input_backspace_removes_one_scalar_from_zwj_and_combining_sequences() {
    let mut state = TuiSessionState::default();
    for ch in "👩\u{200D}🚀".chars() {
        state.commands_input_push(ch); // astronaut: 3 scalars joined by ZWJ
    }
    assert_eq!(state.commands_input_cursor(), 3);

    state.commands_input_backspace(); // removes 🚀
    assert_eq!(state.commands_input_buffer(), "👩\u{200D}");
    state.commands_input_backspace(); // removes the ZWJ
    assert_eq!(state.commands_input_buffer(), "👩");
    state.commands_input_backspace();
    assert_eq!(state.commands_input_buffer(), "");

    // Combining mark: backspace removes the mark first, then the base char.
    state.commands_input_push('e');
    state.commands_input_push('\u{301}'); // e + combining acute = é
    state.commands_input_backspace();
    assert_eq!(state.commands_input_buffer(), "e");

    // Delete (forward) is scalar-based too: caret before the mark removes it.
    state.commands_input_push('\u{301}');
    state.commands_input_move_home();
    state.commands_input_move_right(); // caret between 'e' and the mark
    state.commands_input_delete();
    assert_eq!(state.commands_input_buffer(), "e");
    assert_eq!(state.commands_input_cursor(), 1);
}

/// A multi-kilobyte insert lands whole at the caret and the caret advances by
/// the char count (not the byte count) of the inserted text.
#[test]
fn commands_input_insert_str_handles_multi_kilobyte_text() {
    let mut state = TuiSessionState::default();
    state.commands_input_push('<');
    state.commands_input_push('>');
    state.commands_input_move_left(); // caret between '<' and '>'

    let huge = "あ0".repeat(1024); // 2048 chars, 4 KB of multibyte payload
    state.commands_input_insert_str(&huge);

    assert_eq!(state.commands_input_cursor(), 1 + 2048);
    assert_eq!(state.commands_input_buffer().chars().count(), 2050);
    assert!(state.commands_input_buffer().starts_with("<あ0"));
    assert!(state.commands_input_buffer().ends_with("あ0>"));
}

#[test]
fn cycle_commands_target_walks_broadcast_then_distinct_roles() {
    let mut state = TuiSessionState::default();
    // No live agents → only "broadcast" exists; cycling stays put.
    assert_eq!(state.commands_target(), "broadcast");
    state.cycle_commands_target();
    assert_eq!(state.commands_target(), "broadcast");

    // Two implementers (same role) and one reviewer.
    // Cycle order: broadcast → role:implementer → role:reviewer →
    //              agent:impl-1 → agent:impl-2 → agent:rev-1 → broadcast.
    state.apply_event(&event(
        IpcEventKind::AgentSpawned,
        json!({ "agent_id": "a1", "name": "impl-1", "role": "implementer", "process_id": 1 }),
    ));
    state.apply_event(&event(
        IpcEventKind::AgentSpawned,
        json!({ "agent_id": "a2", "name": "impl-2", "role": "implementer", "process_id": 2 }),
    ));
    state.apply_event(&event(
        IpcEventKind::AgentSpawned,
        json!({ "agent_id": "r1", "name": "rev-1", "role": "reviewer", "process_id": 3 }),
    ));

    state.cycle_commands_target();
    assert_eq!(state.commands_target(), "role:implementer");
    state.cycle_commands_target();
    assert_eq!(state.commands_target(), "role:reviewer");
    state.cycle_commands_target();
    assert_eq!(state.commands_target(), "agent:impl-1");
    state.cycle_commands_target();
    assert_eq!(state.commands_target(), "agent:impl-2");
    state.cycle_commands_target();
    assert_eq!(state.commands_target(), "agent:rev-1");
    state.cycle_commands_target();
    assert_eq!(state.commands_target(), "broadcast");
}

#[test]
fn commands_broadcast_effect_skips_empty_and_carries_target() {
    let mut state = TuiSessionState::default();
    // Empty (and whitespace-only) buffers produce no effect.
    assert_eq!(state.commands_broadcast_effect(), None);
    state.commands_input_push(' ');
    assert_eq!(state.commands_broadcast_effect(), None);

    state.commands_input_clear();
    state.commands_input_push('g');
    state.commands_input_push('o');
    assert_eq!(
        state.commands_broadcast_effect(),
        Some(CommandEffect::BroadcastInput {
            target: "broadcast".to_string(),
            text: "go".to_string(),
        })
    );
}

#[test]
fn push_commands_history_accumulates_entries() {
    let mut state = TuiSessionState::default();
    assert!(state.commands_history().is_empty());

    state.push_commands_history("broadcast", "run tests", 2, 1);
    state.push_commands_history("role:tester", "again", 1, 0);

    let history = state.commands_history();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].target, "broadcast");
    assert_eq!(history[0].text, "run tests");
    assert_eq!(
        history[0].kind,
        CommandsLogKind::Broadcast {
            delivered: 2,
            skipped: 1
        }
    );
    assert_eq!(history[1].target, "role:tester");
    assert_eq!(
        history[1].kind,
        CommandsLogKind::Broadcast {
            delivered: 1,
            skipped: 0
        }
    );
}

#[test]
fn broadcast_response_pairs_pending_request_into_history() {
    let mut state = TuiSessionState::default();
    state.commands_input_push('x');
    state.begin_commands_broadcast("broadcast", "x");
    // begin_* clears the editor buffer (request handed to daemon).
    assert_eq!(state.commands_input_buffer(), "");

    let applied = state.apply_commands_broadcast_response(&json!({
        "delivered": ["a1", "a2"],
        "skipped": ["a3"]
    }));
    assert!(applied);

    let history = state.commands_history();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].target, "broadcast");
    assert_eq!(history[0].text, "x");
    assert_eq!(
        history[0].kind,
        CommandsLogKind::Broadcast {
            delivered: 2,
            skipped: 1
        }
    );

    // A second response with no pending request is a no-op.
    assert!(!state.apply_commands_broadcast_response(&json!({ "delivered": [] })));
    assert_eq!(state.commands_history().len(), 1);
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
    assert!(
        !state.prefix_active(),
        "fresh state has prefix_active = false"
    );

    // Full Ctrl-g → command cycle: true then back to false.
    state.set_prefix_active(true);
    assert!(state.prefix_active());
    state.set_prefix_active(false);
    assert!(
        !state.prefix_active(),
        "prefix_active must be false after reset"
    );

    // A second reset without a prior activation must still be false.
    state.set_prefix_active(false);
    assert!(
        !state.prefix_active(),
        "repeated false sets must stay false"
    );
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

// ── Session-aware target cycling ───────────────────────────────────────

/// With two sessions (one with a role, one without), the cycle order is:
/// broadcast → role:<r> → agent:<name1> → agent:<name2> → broadcast.
#[test]
fn cycle_commands_target_includes_agent_targets() {
    let mut state = TuiSessionState::default();

    state.apply_event(&event(
        IpcEventKind::AgentSpawned,
        json!({ "agent_id": "x1", "name": "coder", "role": "implementer", "process_id": 10 }),
    ));
    state.apply_event(&event(
        IpcEventKind::AgentSpawned,
        json!({ "agent_id": "x2", "name": "tester", "process_id": 11 }),
    ));

    // Expected order: broadcast → role:implementer → agent:coder → agent:tester → broadcast
    assert_eq!(state.commands_target(), "broadcast");
    state.cycle_commands_target();
    assert_eq!(state.commands_target(), "role:implementer");
    state.cycle_commands_target();
    assert_eq!(state.commands_target(), "agent:coder");
    state.cycle_commands_target();
    assert_eq!(state.commands_target(), "agent:tester");
    state.cycle_commands_target();
    assert_eq!(state.commands_target(), "broadcast");
}

/// With no sessions, cycling keeps the target at "broadcast".
#[test]
fn cycle_commands_target_stays_at_broadcast_with_no_sessions() {
    let mut state = TuiSessionState::default();
    assert_eq!(state.commands_target(), "broadcast");
    for _ in 0..3 {
        state.cycle_commands_target();
        assert_eq!(state.commands_target(), "broadcast");
    }
}

/// When the active target disappears (session exited), the next cycle
/// resolves safely to broadcast without panicking.
#[test]
fn cycle_commands_target_recovers_from_stale_agent_target() {
    let mut state = TuiSessionState::default();

    state.apply_event(&event(
        IpcEventKind::AgentSpawned,
        json!({ "agent_id": "z1", "name": "ghost", "process_id": 99 }),
    ));
    state.cycle_commands_target(); // → agent:ghost
    assert_eq!(state.commands_target(), "agent:ghost");

    // Session exits.
    state.apply_event(&event(
        IpcEventKind::AgentExited,
        json!({ "agent_id": "z1" }),
    ));
    // Target is now stale; options contain only ["broadcast"].
    assert_eq!(state.commands_target(), "agent:ghost");

    state.cycle_commands_target();
    assert_eq!(state.commands_target(), "broadcast");
}

/// `commands_target_options` returns broadcast + sorted roles + agents in pane order.
#[test]
fn commands_target_options_returns_broadcast_then_roles_then_agents() {
    let mut state = TuiSessionState::default();

    state.apply_event(&event(
        IpcEventKind::AgentSpawned,
        json!({ "agent_id": "p1", "name": "plan", "role": "planner", "process_id": 1 }),
    ));
    state.apply_event(&event(
        IpcEventKind::AgentSpawned,
        json!({ "agent_id": "i1", "name": "impl", "role": "implementer", "process_id": 2 }),
    ));

    let options = state.commands_target_options();
    assert_eq!(options[0], "broadcast");
    // Roles are sorted: implementer < planner.
    assert_eq!(options[1], "role:implementer");
    assert_eq!(options[2], "role:planner");
    // Agent entries follow in pane/spawn order.
    assert!(options.contains(&"agent:plan".to_string()));
    assert!(options.contains(&"agent:impl".to_string()));
    assert_eq!(options.len(), 5); // broadcast + 2 roles + 2 agents
}

/// Cycle the broadcast target until it equals `want` (panics if unreachable).
fn select_target(state: &mut TuiSessionState, want: &str) {
    for _ in 0..state.commands_target_options().len() {
        if state.commands_target() == want {
            return;
        }
        state.cycle_commands_target();
    }
    assert_eq!(state.commands_target(), want, "target {want} not reachable");
}

fn type_input(state: &mut TuiSessionState, text: &str) {
    for ch in text.chars() {
        state.commands_input_push(ch);
    }
}

#[test]
fn parse_commands_input_returns_none_when_buffer_empty() {
    let mut state = TuiSessionState::default();
    assert_eq!(state.parse_commands_input(), None);
    state.commands_input_push(' ');
    assert_eq!(state.parse_commands_input(), None);
}

#[test]
fn parse_commands_input_role_assigns_to_resolved_live_agent() {
    let mut state = TuiSessionState::default();
    state.apply_event(&event(
        IpcEventKind::AgentSpawned,
        json!({ "agent_id": "agent_foo", "name": "foo", "role": "implementer", "process_id": 7 }),
    ));
    select_target(&mut state, "agent:foo");

    type_input(&mut state, "/role qa-lead");
    assert_eq!(
        state.parse_commands_input(),
        Some(CommandsSubmit::AssignRole {
            agent_id: "agent_foo".to_string(),
            role: "qa-lead".to_string(),
        })
    );
}

#[test]
fn parse_commands_input_empty_role_is_error() {
    let mut state = TuiSessionState::default();
    state.apply_event(&event(
        IpcEventKind::AgentSpawned,
        json!({ "agent_id": "agent_foo", "name": "foo", "role": "implementer", "process_id": 7 }),
    ));
    select_target(&mut state, "agent:foo");

    type_input(&mut state, "/role ");
    assert!(matches!(
        state.parse_commands_input(),
        Some(CommandsSubmit::Error(_))
    ));
}

#[test]
fn parse_commands_input_role_against_broadcast_target_is_error() {
    let mut state = TuiSessionState::default();
    assert_eq!(state.commands_target(), "broadcast");
    type_input(&mut state, "/role x");
    match state.parse_commands_input() {
        Some(CommandsSubmit::Error(message)) => {
            assert!(message.contains("agent:<name>"), "message: {message}");
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

#[test]
fn parse_commands_input_role_against_unresolved_agent_is_error() {
    let mut state = TuiSessionState::default();
    // Spawn a live pane so the target group is non-empty, then point the
    // target at a ghost name that no pane carries (manually, since cycling
    // only ever yields live names).
    state.apply_event(&event(
        IpcEventKind::AgentSpawned,
        json!({ "agent_id": "agent_foo", "name": "foo", "role": "implementer", "process_id": 7 }),
    ));
    select_target(&mut state, "agent:foo");
    // Remove the only live session so `agent:foo` no longer resolves.
    state.apply_event(&event(
        IpcEventKind::AgentExited,
        json!({ "agent_id": "agent_foo" }),
    ));

    type_input(&mut state, "/role qa");
    assert!(matches!(
        state.parse_commands_input(),
        Some(CommandsSubmit::Error(_))
    ));
}

#[test]
fn parse_commands_input_send_broadcasts_quoted_and_unquoted() {
    let mut state = TuiSessionState::default();
    type_input(&mut state, "/send \"hello world\"");
    assert_eq!(
        state.parse_commands_input(),
        Some(CommandsSubmit::Broadcast("hello world".to_string()))
    );

    state.commands_input_clear();
    type_input(&mut state, "/send hi");
    assert_eq!(
        state.parse_commands_input(),
        Some(CommandsSubmit::Broadcast("hi".to_string()))
    );
}

#[test]
fn parse_commands_input_send_without_text_is_error() {
    let mut state = TuiSessionState::default();
    type_input(&mut state, "/send");
    assert!(matches!(
        state.parse_commands_input(),
        Some(CommandsSubmit::Error(_))
    ));
}

#[test]
fn parse_commands_input_plain_text_is_error() {
    let mut state = TuiSessionState::default();
    // Plain text is never broadcast implicitly — it must go through /send.
    type_input(&mut state, "hello world");
    assert!(matches!(
        state.parse_commands_input(),
        Some(CommandsSubmit::Error(_))
    ));
}

#[test]
fn parse_commands_input_unknown_command_is_error() {
    let mut state = TuiSessionState::default();
    // `/rolex foo` parses to the unknown command `rolex`, not a broadcast.
    type_input(&mut state, "/rolex foo");
    assert!(matches!(
        state.parse_commands_input(),
        Some(CommandsSubmit::Error(_))
    ));
}

#[test]
fn apply_agent_role_changed_updates_matching_pane() {
    let mut state = TuiSessionState::default();
    state.apply_event(&event(
        IpcEventKind::AgentSpawned,
        json!({ "agent_id": "agent_foo", "name": "foo", "role": "implementer", "process_id": 7 }),
    ));
    assert_eq!(
        state.pane("agent_foo").and_then(|p| p.role()),
        Some("implementer")
    );

    let change = state.apply_event(&event(
        IpcEventKind::AgentRoleChanged,
        json!({ "agent_id": "agent_foo", "role": "qa-lead" }),
    ));
    assert_eq!(change, StateChange::UpdatedPane("agent_foo".to_string()));
    assert_eq!(
        state.pane("agent_foo").and_then(|p| p.role()),
        Some("qa-lead")
    );
    // The role group of the target options now reflects the new role.
    assert!(
        state
            .commands_target_options()
            .contains(&"role:qa-lead".to_string())
    );
}

#[test]
fn apply_agent_role_changed_ignores_unknown_agent() {
    let mut state = TuiSessionState::default();
    let change = state.apply_event(&event(
        IpcEventKind::AgentRoleChanged,
        json!({ "agent_id": "ghost", "role": "qa-lead" }),
    ));
    assert_eq!(change, StateChange::Ignored);
    assert!(state.pane("ghost").is_none());
}

#[test]
fn role_assign_response_records_success_history_entry() {
    let mut state = TuiSessionState::default();
    state.apply_event(&event(
        IpcEventKind::AgentSpawned,
        json!({ "agent_id": "agent_foo", "name": "foo", "role": "implementer", "process_id": 7 }),
    ));
    select_target(&mut state, "agent:foo");

    state.begin_commands_role_assign("qa-lead");
    // begin_* clears the editor buffer (request handed to daemon).
    assert_eq!(state.commands_input_buffer(), "");

    let applied = state.apply_commands_role_response(&json!({
        "agent_id": "agent_foo",
        "role": "qa-lead"
    }));
    assert!(applied);

    let history = state.commands_history();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].target, "agent:foo");
    assert_eq!(
        history[0].kind,
        CommandsLogKind::RoleAssigned {
            role: "qa-lead".to_string()
        }
    );

    // A second response with no pending assignment is a no-op.
    assert!(!state.apply_commands_role_response(&json!({ "role": "x" })));
    assert_eq!(state.commands_history().len(), 1);
}

#[test]
fn fail_commands_role_assign_records_error_history_entry() {
    let mut state = TuiSessionState::default();
    type_input(&mut state, "/role x");
    state.fail_commands_role_assign("select a single session (agent:<name>) first");

    let history = state.commands_history();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].kind, CommandsLogKind::Error);
    assert_eq!(state.commands_input_buffer(), "");
}

#[test]
fn parse_commands_input_keys_sends_to_resolved_live_agent() {
    let mut state = TuiSessionState::default();
    state.apply_event(&event(
        IpcEventKind::AgentSpawned,
        json!({ "agent_id": "agent_foo", "name": "foo", "role": "implementer", "process_id": 7 }),
    ));
    select_target(&mut state, "agent:foo");

    type_input(&mut state, "/keys C-c|enter");
    assert_eq!(
        state.parse_commands_input(),
        Some(CommandsSubmit::SendKeys {
            agent_id: "agent_foo".to_string(),
            spec: "C-c|enter".to_string(),
        })
    );
}

#[test]
fn parse_commands_input_empty_keys_spec_is_error() {
    let mut state = TuiSessionState::default();
    state.apply_event(&event(
        IpcEventKind::AgentSpawned,
        json!({ "agent_id": "agent_foo", "name": "foo", "role": "implementer", "process_id": 7 }),
    ));
    select_target(&mut state, "agent:foo");

    type_input(&mut state, "/keys ");
    assert!(matches!(
        state.parse_commands_input(),
        Some(CommandsSubmit::Error(_))
    ));
}

#[test]
fn parse_commands_input_keys_against_broadcast_target_is_error() {
    let mut state = TuiSessionState::default();
    assert_eq!(state.commands_target(), "broadcast");
    type_input(&mut state, "/keys C-c");
    match state.parse_commands_input() {
        Some(CommandsSubmit::Error(message)) => {
            assert!(message.contains("agent:<name>"), "message: {message}");
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

#[test]
fn parse_commands_input_keys_against_role_target_is_error() {
    let mut state = TuiSessionState::default();
    state.apply_event(&event(
        IpcEventKind::AgentSpawned,
        json!({ "agent_id": "agent_foo", "name": "foo", "role": "implementer", "process_id": 7 }),
    ));
    select_target(&mut state, "role:implementer");

    type_input(&mut state, "/keys C-c");
    assert!(matches!(
        state.parse_commands_input(),
        Some(CommandsSubmit::Error(_))
    ));
}

#[test]
fn parse_commands_input_keys_against_unresolved_agent_is_error() {
    let mut state = TuiSessionState::default();
    state.apply_event(&event(
        IpcEventKind::AgentSpawned,
        json!({ "agent_id": "agent_foo", "name": "foo", "role": "implementer", "process_id": 7 }),
    ));
    select_target(&mut state, "agent:foo");
    state.apply_event(&event(
        IpcEventKind::AgentExited,
        json!({ "agent_id": "agent_foo" }),
    ));

    type_input(&mut state, "/keys C-c");
    assert!(matches!(
        state.parse_commands_input(),
        Some(CommandsSubmit::Error(_))
    ));
}

#[test]
fn begin_commands_keys_records_keys_history_via_broadcast_response() {
    let mut state = TuiSessionState::default();
    state.apply_event(&event(
        IpcEventKind::AgentSpawned,
        json!({ "agent_id": "agent_foo", "name": "foo", "role": "implementer", "process_id": 7 }),
    ));
    select_target(&mut state, "agent:foo");

    type_input(&mut state, "/keys C-c|enter");
    state.begin_commands_keys("C-c|enter");
    // begin_* clears the editor buffer (request handed to daemon).
    assert_eq!(state.commands_input_buffer(), "");

    // The daemon's agent.broadcast_input response drives the history entry.
    let applied = state.apply_commands_broadcast_response(&json!({
        "delivered": ["agent_foo"],
        "skipped": []
    }));
    assert!(applied);

    let history = state.commands_history();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].target, "agent:foo");
    assert_eq!(history[0].text, "keys: C-c|enter");
    assert_eq!(
        history[0].kind,
        CommandsLogKind::Broadcast {
            delivered: 1,
            skipped: 0
        }
    );
}

#[test]
fn fail_commands_keys_records_error_history_entry() {
    let mut state = TuiSessionState::default();
    type_input(&mut state, "/keys bogus");
    state.fail_commands_keys("unknown key step 'bogus'");

    let history = state.commands_history();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].kind, CommandsLogKind::Error);
    assert_eq!(state.commands_input_buffer(), "");
}
