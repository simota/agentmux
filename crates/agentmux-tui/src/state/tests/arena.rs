use super::*;

#[cfg(feature = "arena")]
#[test]
fn arena_events_update_candidates_and_adopt_effect() {
    let mut state = TuiSessionState::default();
    state.apply_event(&event(
        IpcEventKind::WorktreeCreated,
        json!({
            "worktree": {
                "worktree_id": "wt_001",
                "branch_name": "agentmux/task-a"
            },
            "provider": "codex"
        }),
    ));
    state.apply_event(&event(
        IpcEventKind::WorktreeDiffCaptured,
        json!({ "worktree_id": "wt_001", "stat": "1 file changed" }),
    ));
    state.apply_event(&event(
        IpcEventKind::WorktreeTestCompleted,
        json!({ "worktree_id": "wt_001", "status": "passed" }),
    ));

    assert_eq!(state.arena_candidates().len(), 1);
    assert_eq!(state.arena_candidates()[0].provider, "codex");
    assert_eq!(state.arena_candidates()[0].diff_stat, "1 file changed");
    assert_eq!(state.arena_candidates()[0].test_status, "passed");
    assert_eq!(
        state.apply_command(TuiCommand::ArenaAdopt),
        CommandEffect::ArenaAdopt("wt_001".to_string())
    );
}

#[cfg(feature = "arena")]
#[test]
fn arena_adopt_with_empty_selection_is_noop() {
    let mut state = TuiSessionState::default();

    assert_eq!(
        state.apply_command(TuiCommand::ShowArenaOverlay),
        CommandEffect::Continue
    );
    assert!(state.arena_overlay_visible());
    assert_eq!(
        state.apply_command(TuiCommand::ArenaAdopt),
        CommandEffect::Continue
    );
    assert_eq!(state.arena_selected_index(), 0);
}

#[cfg(feature = "arena")]
#[test]
fn arena_candidate_refresh_clamps_selection_while_overlay_is_open() {
    let mut state = TuiSessionState::default();
    state.apply_daemon_status(&json!({
        "protocol_version": 3,
        "agents": [],
        "arena_candidates": [
            { "worktree_id": "wt_001", "provider": "claude" },
            { "worktree_id": "wt_002", "provider": "codex" }
        ]
    }));
    state.apply_command(TuiCommand::ShowArenaOverlay);
    state.apply_command(TuiCommand::ArenaPrevious);

    assert_eq!(state.arena_selected_index(), 1);

    state.apply_daemon_status(&json!({
        "protocol_version": 3,
        "agents": [],
        "arena_candidates": [
            { "worktree_id": "wt_001", "provider": "claude" }
        ]
    }));

    assert_eq!(state.arena_selected_index(), 0);
    assert_eq!(
        state.apply_command(TuiCommand::ArenaAdopt),
        CommandEffect::ArenaAdopt("wt_001".to_string())
    );
}

#[cfg(feature = "arena")]
#[test]
fn daemon_status_seeds_arena_candidates() {
    let mut state = TuiSessionState::default();
    state.apply_daemon_status(&json!({
        "protocol_version": 3,
        "agents": [],
        "arena_candidates": [
            {
                "worktree_id": "wt_001",
                "agent_id": "agent_001",
                "provider": "claude",
                "diff_stat": "2 files changed",
                "test_status": "failed"
            }
        ]
    }));

    assert_eq!(state.arena_candidates().len(), 1);
    assert_eq!(state.arena_candidates()[0].worktree_id, "wt_001");
    assert_eq!(state.arena_candidates()[0].test_status, "failed");
}
