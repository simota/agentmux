use super::*;

#[test]
fn context_requests_target_daemon_ipc() {
    let add = context_add_request("decision log".to_string()).unwrap();
    assert_eq!(add.command, IpcCommand::ContextCreate);
    assert_eq!(add.payload["title"], "decision log");
    assert_eq!(add.payload["kind"], "handoff_summary");

    let list = context_list_request();
    assert_eq!(list.command, IpcCommand::ContextSearch);
    assert_eq!(list.payload, json!({}));

    let show = context_show_request("ctx_01HX".to_string());
    assert_eq!(show.command, IpcCommand::ContextSearch);
    assert_eq!(show.payload["context_id"], "ctx_01HX");

    let search = context_search_request("risk".to_string()).unwrap();
    assert_eq!(search.command, IpcCommand::ContextSearch);
    assert_eq!(search.payload["query"], "risk");

    let attach = context_attach_request("ctx_01HX".to_string(), "msg_01HX".to_string());
    assert_eq!(attach.command, IpcCommand::ContextAttach);
    assert_eq!(attach.payload["context_id"], "ctx_01HX");
    assert_eq!(attach.payload["message_id"], "msg_01HX");

    let inject = context_inject_request("ctx_01HX".to_string(), "agent_01HX".to_string());
    assert_eq!(inject.command, IpcCommand::ContextInject);
    assert_eq!(inject.payload["context_id"], "ctx_01HX");
    assert_eq!(inject.payload["agent_id"], "agent_01HX");

    let export = context_export_request("contexts.json".to_string());
    assert_eq!(export.command, IpcCommand::ContextExport);
    assert_eq!(export.payload["output"], "contexts.json");
}

#[test]
fn context_request_builders_reject_empty_text_before_ipc() {
    let add_error = context_add_request("  ".to_string()).unwrap_err();
    assert!(
        add_error
            .to_string()
            .contains("context title must not be empty")
    );

    let search_error = context_search_request("  ".to_string()).unwrap_err();
    assert!(
        search_error
            .to_string()
            .contains("context search query must not be empty")
    );
}

#[test]
fn worktree_requests_target_daemon_ipc() {
    let list = worktree_list_request();
    assert_eq!(list.command, IpcCommand::WorktreeList);
    assert_eq!(list.payload, json!({}));

    let diff = worktree_diff_request("wt_01HX".to_string());
    assert_eq!(diff.command, IpcCommand::WorktreeDiff);
    assert_eq!(diff.payload["worktree_id"], "wt_01HX");

    let test = worktree_test_request("wt_01HX".to_string());
    assert_eq!(test.command, IpcCommand::WorktreeTest);
    assert_eq!(test.payload["worktree_id"], "wt_01HX");

    let promote = worktree_promote_request("wt_01HX".to_string());
    assert_eq!(promote.command, IpcCommand::WorktreePromote);
    assert_eq!(promote.payload["worktree_id"], "wt_01HX");

    let archive = worktree_archive_request("wt_01HX".to_string());
    assert_eq!(archive.command, IpcCommand::WorktreeArchive);
    assert_eq!(archive.payload["worktree_id"], "wt_01HX");
}

#[test]
fn approval_requests_target_daemon_ipc() {
    let list = approval_list_request();
    assert_eq!(list.command, IpcCommand::ApprovalList);
    assert_eq!(list.payload, json!({}));

    let approve = approval_approve_request("appr_01HX".to_string());
    assert_eq!(approve.command, IpcCommand::ApprovalApprove);
    assert_eq!(approve.payload["approval_id"], "appr_01HX");

    let reject = approval_reject_request("appr_01HY".to_string());
    assert_eq!(reject.command, IpcCommand::ApprovalReject);
    assert_eq!(reject.payload["approval_id"], "appr_01HY");
}

#[test]
fn agent_requests_target_daemon_ipc() {
    let list = agent_ls_request();
    assert_eq!(list.command, IpcCommand::DaemonStatus);
    assert_eq!(list.payload, json!({}));

    let spawn = agent_spawn_request("codex".to_string(), "implementer".to_string()).unwrap();
    assert_eq!(spawn.command, IpcCommand::AgentSpawn);
    assert_eq!(spawn.payload["provider"], "codex");
    assert_eq!(spawn.payload["role"], "implementer");
    let spawn_name = spawn.payload["name"].as_str().unwrap();
    assert!(spawn_name.starts_with("implementer-"));
    assert_eq!(spawn_name.len(), "implementer-".len() + 6);

    let second_spawn = agent_spawn_request("codex".to_string(), "implementer".to_string()).unwrap();
    assert_ne!(spawn.payload["name"], second_spawn.payload["name"]);

    let stop = agent_stop_request("agent_01HX".to_string());
    assert_eq!(stop.command, IpcCommand::AgentStop);
    assert_eq!(stop.payload["agent_id"], "agent_01HX");

    let send = agent_send_request("agent_01HX".to_string(), "hello".to_string()).unwrap();
    assert_eq!(send.command, IpcCommand::MessageCreate);
    assert_eq!(send.payload["to"], "agent:agent_01HX");
    assert_eq!(send.payload["body"], "hello");

    let send_by_name = agent_send_request("codex-a1b2c3".to_string(), "hello".to_string()).unwrap();
    assert_eq!(send_by_name.payload["to"], "agent:codex-a1b2c3");

    let inject = agent_inject_request("msg_01HX".to_string(), "agent_01HX".to_string());
    assert_eq!(inject.command, IpcCommand::MessageInject);
    assert_eq!(inject.payload["message_id"], "msg_01HX");
    assert_eq!(inject.payload["agent_id"], "agent_01HX");

    let focus = agent_focus_request("agent_01HX".to_string());
    assert_eq!(focus.command, IpcCommand::AgentFocus);
    assert_eq!(focus.payload["agent_id"], "agent_01HX");

    let interrupt = agent_interrupt_request("agent_01HX".to_string());
    assert_eq!(interrupt.command, IpcCommand::AgentInterrupt);
    assert_eq!(interrupt.payload["agent_id"], "agent_01HX");
}

#[test]
fn agent_broadcast_input_request_builds_target_and_actions() {
    // Default: paste the text and submit it with a trailing PressEnter.
    let with_enter =
        agent_broadcast_input_request("broadcast".to_string(), "git status".to_string(), true)
            .unwrap();
    assert_eq!(with_enter.command, IpcCommand::AgentBroadcastInput);
    assert_eq!(with_enter.payload["target"], "broadcast");
    assert_eq!(with_enter.payload["actions"][0]["paste_text"], "git status");
    assert_eq!(with_enter.payload["actions"][1], "press_enter");

    // --no-enter: paste only, no trailing PressEnter action.
    let no_enter =
        agent_broadcast_input_request("role:tester".to_string(), "echo hi".to_string(), false)
            .unwrap();
    assert_eq!(no_enter.payload["target"], "role:tester");
    assert_eq!(no_enter.payload["actions"].as_array().unwrap().len(), 1);
    assert_eq!(no_enter.payload["actions"][0]["paste_text"], "echo hi");

    // Empty target and empty text are rejected up front.
    assert!(agent_broadcast_input_request("   ".to_string(), "x".to_string(), true).is_err());
    assert!(agent_broadcast_input_request("broadcast".to_string(), String::new(), true).is_err());
}

#[test]
fn sessions_list_request_targets_daemon_status() {
    let request = sessions_list_request();

    assert_eq!(request.id, "req_sessions_list");
    assert_eq!(request.command, IpcCommand::DaemonStatus);
    assert_eq!(request.payload, json!({}));
}

#[test]
fn format_sessions_payload_lists_only_running_sessions() {
    let payload = json!({
        "agents": [
            {
                "id": "agent_live",
                "name": "shell",
                "role": "tester",
                "status": "awaiting_input",
                "input_ready": true,
                "process_id": 1234,
                "has_process": true,
                "attached_clients": ["csess_1", "csess_2"]
            },
            {
                "id": "agent_restored",
                "name": "restored",
                "process_id": null,
                "has_process": false,
                "attached_clients": []
            }
        ]
    });

    assert_eq!(
        format_sessions_payload(&payload),
        "ID NAME ROLE STATUS INPUT PID CLIENTS\nagent_live shell tester awaiting_input ready 1234 2\n"
    );
}

#[test]
fn format_sessions_payload_reports_empty_running_sessions() {
    assert_eq!(
        format_sessions_payload(&json!({ "agents": [] })),
        "no running sessions\n"
    );
    assert_eq!(format_sessions_payload(&json!({})), "no running sessions\n");
}

#[test]
fn agent_request_builders_reject_empty_values_before_ipc() {
    let provider_error =
        agent_spawn_request(" ".to_string(), "implementer".to_string()).unwrap_err();
    assert!(provider_error.to_string().contains("provider"));

    let role_error = agent_spawn_request("codex".to_string(), " ".to_string()).unwrap_err();
    assert!(role_error.to_string().contains("role"));

    let body_error = agent_send_request("agent_01HX".to_string(), " ".to_string()).unwrap_err();
    assert!(body_error.to_string().contains("agent message body"));
}

#[test]
fn layout_requests_target_daemon_ipc() {
    let save = layout_save_request("default".to_string()).unwrap();
    assert_eq!(save.command, IpcCommand::LayoutSet);
    assert_eq!(save.payload["name"], "default");

    let load = layout_load_request("default".to_string());
    assert_eq!(load.command, IpcCommand::LayoutGet);
    assert_eq!(load.payload["name"], "default");

    let list = layout_list_request();
    assert_eq!(list.command, IpcCommand::LayoutGet);
    assert_eq!(list.payload, json!({}));
}

#[test]
fn layout_save_rejects_empty_name_before_ipc() {
    let error = layout_save_request("  ".to_string()).unwrap_err();

    assert!(error.to_string().contains("layout name must not be empty"));
}
