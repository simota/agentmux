use super::*;

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
