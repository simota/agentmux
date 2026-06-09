use super::*;

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
