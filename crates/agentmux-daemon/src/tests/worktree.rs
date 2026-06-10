use super::*;

#[tokio::test]
async fn ipc_worktree_commands_list_test_promote_and_archive() {
    let root = std::env::temp_dir().join(format!("agentmux-worktree-ipc-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&root).expect("temporary worktree root is created");
    test_git(&root, ["init", "-b", "main"]);
    std::fs::write(root.join("README.md"), "hello\n").unwrap();
    test_git(&root, ["add", "README.md"]);
    test_git(
        &root,
        [
            "-c",
            "user.name=Agentmux Test",
            "-c",
            "user.email=agentmux@example.invalid",
            "commit",
            "-m",
            "initial",
        ],
    );
    test_git(&root, ["branch", "agentmux/task-impl"]);
    let runtime = DaemonRuntime::new(16);
    let attached_agent = runtime
        .register_agent("worktree-observer".to_string())
        .await;
    let worktree = Worktree {
        id: WorktreeId::new(),
        project_id: ProjectId::new(),
        task_id: agentmux_core::TaskId::new(),
        owner_agent_id: None,
        path: root.clone(),
        branch_name: "agentmux/task-impl".to_string(),
        base_branch: "main".to_string(),
        status: WorktreeStatus::Ready,
        created_at: DateTimeUtc::UNIX_EPOCH,
    };
    let worktree_id = worktree.id.to_string();
    let parsed_worktree_id = worktree.id.clone();
    runtime.register_worktree(worktree).await;

    let (client_stream, server_stream) = UnixStream::pair().unwrap();
    let server_runtime = runtime.clone();
    let server = tokio::spawn(async move { handle_client(server_stream, server_runtime).await });

    let (reader, writer) = client_stream.into_split();
    let mut reader = JsonlReader::new(BufReader::new(reader));
    let mut writer = JsonlWriter::new(writer);

    writer.write(&ClientHello::new("0.1.0")).await.unwrap();
    writer
        .write(&ClientRequest::new(
            "req_attach",
            IpcCommand::ClientAttach,
            json!({ "agent_id": attached_agent.id.to_string() }),
        ))
        .await
        .unwrap();
    let (attach_response, attach_event) = read_response_and_event(&mut reader).await;
    assert!(attach_response.ok);
    assert_eq!(attach_event.kind, IpcEventKind::ClientAttached);

    writer
        .write(&ClientRequest::new(
            "req_worktree_list",
            IpcCommand::WorktreeList,
            json!({}),
        ))
        .await
        .unwrap();
    let list_response: DaemonResponse = reader.read().await.unwrap().unwrap();
    assert!(list_response.ok);
    let list_payload = list_response.payload.unwrap();
    assert_eq!(list_payload["worktrees"].as_array().unwrap().len(), 1);
    assert_eq!(list_payload["worktrees"][0]["worktree_id"], worktree_id);

    writer
        .write(&ClientRequest::new(
            "req_worktree_test",
            IpcCommand::WorktreeTest,
            json!({
                "worktree_id": worktree_id,
                "name": "smoke",
                "command": "printf test-ok",
            }),
        ))
        .await
        .unwrap();
    let (test_response, artifact_event) = read_response_and_event(&mut reader).await;
    assert!(test_response.ok);
    assert_eq!(artifact_event.kind, IpcEventKind::ArtifactCreated);
    let test_event: DaemonEvent = reader.read().await.unwrap().unwrap();
    assert_eq!(test_event.kind, IpcEventKind::WorktreeTestCompleted);
    let test_payload = test_response.payload.unwrap();
    assert_eq!(test_payload["worktree"]["status"], "review_ready");
    assert_eq!(test_payload["test"]["status"], "passed");
    assert!(
        std::fs::read_to_string(test_payload["test"]["artifact"]["path"].as_str().unwrap())
            .unwrap()
            .contains("test-ok")
    );
    mark_arena_candidate(
        &runtime,
        parsed_worktree_id.clone(),
        Some("README.md | 0".to_string()),
        Some(TestRunStatus::Passed),
    )
    .await;

    writer
        .write(&ClientRequest::new(
            "req_worktree_promote",
            IpcCommand::WorktreePromote,
            json!({ "worktree_id": test_payload["worktree"]["worktree_id"] }),
        ))
        .await
        .unwrap();
    let promote_response: DaemonResponse = reader.read().await.unwrap().unwrap();
    assert!(promote_response.ok);
    let promote_payload = promote_response.payload.unwrap();
    assert_eq!(promote_payload["status"], "pending");
    assert_eq!(promote_payload["worktree_id"], worktree_id);
    assert_eq!(
        runtime
            .worktree_by_id(&parsed_worktree_id)
            .await
            .unwrap()
            .status,
        WorktreeStatus::ReviewReady
    );
    let approval_event: DaemonEvent = reader.read().await.unwrap().unwrap();
    assert_eq!(approval_event.kind, IpcEventKind::ApprovalCreated);
    let adopt_event: DaemonEvent = reader.read().await.unwrap().unwrap();
    assert_eq!(adopt_event.kind, IpcEventKind::WorktreeAdoptRequested);

    writer
        .write(&ClientRequest::new(
            "req_worktree_archive",
            IpcCommand::WorktreeArchive,
            json!({ "worktree_id": test_payload["worktree"]["worktree_id"] }),
        ))
        .await
        .unwrap();
    let archive_response: DaemonResponse = reader.read().await.unwrap().unwrap();
    assert!(archive_response.ok);
    assert_eq!(archive_response.payload.unwrap()["status"], "archived");

    server.abort();
    std::fs::remove_dir_all(root).expect("temporary worktree root is removed");
}

#[tokio::test]
async fn ipc_worktree_diff_reports_unknown_worktree() {
    let runtime = DaemonRuntime::new(16);
    let (client_stream, server_stream) = UnixStream::pair().unwrap();
    let server_runtime = runtime.clone();
    let server = tokio::spawn(async move { handle_client(server_stream, server_runtime).await });

    let (reader, writer) = client_stream.into_split();
    let mut reader = JsonlReader::new(BufReader::new(reader));
    let mut writer = JsonlWriter::new(writer);

    writer.write(&ClientHello::new("0.1.0")).await.unwrap();
    writer
        .write(&ClientRequest::new(
            "req_worktree_diff",
            IpcCommand::WorktreeDiff,
            json!({ "worktree_id": WorktreeId::new().to_string() }),
        ))
        .await
        .unwrap();

    let response: DaemonResponse = reader.read().await.unwrap().unwrap();
    assert!(!response.ok);
    assert_eq!(response.error.unwrap().code, "WORKTREE_DIFF_FAILED");

    server.abort();
}

#[tokio::test]
async fn worktree_adopt_requires_approval_and_reject_archives() {
    let runtime = DaemonRuntime::new(16);
    let worktree = Worktree {
        id: WorktreeId::new(),
        project_id: ProjectId::new(),
        task_id: TaskId::new(),
        owner_agent_id: None,
        path: std::env::temp_dir(),
        branch_name: "agentmux/task-impl".to_string(),
        base_branch: "main".to_string(),
        status: WorktreeStatus::ReviewReady,
        created_at: DateTimeUtc::UNIX_EPOCH,
    };
    let worktree_id = worktree.id.clone();
    runtime.register_worktree(worktree).await;
    mark_arena_candidate(
        &runtime,
        worktree_id.clone(),
        Some("README.md | 0".to_string()),
        Some(TestRunStatus::Passed),
    )
    .await;

    let approval = runtime
        .request_worktree_adoption(worktree_id.clone())
        .await
        .expect("adoption approval is queued");

    assert_eq!(approval.worktree_id, Some(worktree_id.clone()));
    assert_eq!(
        runtime.worktree_by_id(&worktree_id).await.unwrap().status,
        WorktreeStatus::ReviewReady
    );

    runtime
        .reject_approval(&approval.id)
        .await
        .expect("approval is rejected");
    wait_for_worktree_status(&runtime, &worktree_id, WorktreeStatus::Archived).await;
}

#[tokio::test]
async fn ipc_worktree_promote_without_approval_queues_request_and_does_not_merge() {
    let root =
        std::env::temp_dir().join(format!("agentmux-unapproved-promote-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&root).expect("temporary worktree root is created");
    test_git(&root, ["init", "-b", "main"]);
    std::fs::write(root.join("README.md"), "hello\n").unwrap();
    test_git(&root, ["add", "README.md"]);
    test_git(
        &root,
        [
            "-c",
            "user.name=Agentmux Test",
            "-c",
            "user.email=agentmux@example.invalid",
            "commit",
            "-m",
            "initial",
        ],
    );
    test_git(&root, ["checkout", "-b", "agentmux/task-impl"]);
    std::fs::write(root.join("feature.txt"), "candidate\n").unwrap();
    test_git(&root, ["add", "feature.txt"]);
    test_git(
        &root,
        [
            "-c",
            "user.name=Agentmux Test",
            "-c",
            "user.email=agentmux@example.invalid",
            "commit",
            "-m",
            "candidate",
        ],
    );
    test_git(&root, ["checkout", "main"]);
    let runtime = DaemonRuntime::new(16);
    let worktree = Worktree {
        id: WorktreeId::new(),
        project_id: ProjectId::new(),
        task_id: TaskId::new(),
        owner_agent_id: None,
        path: root.clone(),
        branch_name: "agentmux/task-impl".to_string(),
        base_branch: "main".to_string(),
        status: WorktreeStatus::ReviewReady,
        created_at: DateTimeUtc::UNIX_EPOCH,
    };
    let worktree_id = worktree.id.clone();
    runtime
        .register_worktree_with_repo_root(worktree, root.clone())
        .await;
    mark_arena_candidate(
        &runtime,
        worktree_id.clone(),
        Some("feature.txt | 1".to_string()),
        Some(TestRunStatus::Passed),
    )
    .await;
    let (client_stream, server_stream) = UnixStream::pair().unwrap();
    let server_runtime = runtime.clone();
    let server = tokio::spawn(async move { handle_client(server_stream, server_runtime).await });
    let (reader, writer) = client_stream.into_split();
    let mut reader = JsonlReader::new(BufReader::new(reader));
    let mut writer = JsonlWriter::new(writer);
    writer.write(&ClientHello::new("0.1.0")).await.unwrap();

    writer
        .write(&ClientRequest::new(
            "req_worktree_promote",
            IpcCommand::WorktreePromote,
            json!({ "worktree_id": worktree_id.to_string() }),
        ))
        .await
        .unwrap();
    let promote_response: DaemonResponse = reader.read().await.unwrap().unwrap();

    assert!(promote_response.ok);
    let promote_payload = promote_response.payload.unwrap();
    assert_eq!(promote_payload["status"], "pending");
    assert_eq!(
        runtime.worktree_by_id(&worktree_id).await.unwrap().status,
        WorktreeStatus::ReviewReady
    );
    assert!(runtime.list_approvals().await.len() == 1);
    assert_eq!(git_stdout(&root, ["branch", "--show-current"]), "main\n");
    assert!(!root.join("feature.txt").exists());
    assert!(git_stdout(&root, ["branch", "--list", "agentmux/integration"]).is_empty());

    server.abort();
    std::fs::remove_dir_all(root).expect("temporary worktree root is removed");
}

#[tokio::test]
async fn worktree_adopt_unknown_worktree_does_not_queue_approval() {
    let runtime = DaemonRuntime::new(16);
    let unknown_id = WorktreeId::new();

    let error = runtime
        .request_worktree_adoption(unknown_id.clone())
        .await
        .expect_err("unknown worktree adoption is rejected");

    assert!(error.to_string().contains(&unknown_id.to_string()));
    assert!(runtime.list_approvals().await.is_empty());
}

#[tokio::test]
async fn worktree_adopt_before_diff_capture_is_rejected() {
    let runtime = DaemonRuntime::new(16);
    let worktree = Worktree {
        id: WorktreeId::new(),
        project_id: ProjectId::new(),
        task_id: TaskId::new(),
        owner_agent_id: None,
        path: std::env::temp_dir(),
        branch_name: "agentmux/task-impl".to_string(),
        base_branch: "main".to_string(),
        status: WorktreeStatus::ReviewReady,
        created_at: DateTimeUtc::UNIX_EPOCH,
    };
    let worktree_id = worktree.id.clone();
    runtime.register_worktree(worktree).await;
    mark_arena_candidate(
        &runtime,
        worktree_id.clone(),
        None,
        Some(TestRunStatus::Passed),
    )
    .await;

    let error = runtime
        .request_worktree_adoption(worktree_id)
        .await
        .expect_err("adoption before diff capture is rejected");

    assert!(error.to_string().contains("captured diff"));
    assert!(runtime.list_approvals().await.is_empty());
}

#[tokio::test]
async fn worktree_adopt_after_test_failure_is_rejected() {
    let runtime = DaemonRuntime::new(16);
    let worktree = Worktree {
        id: WorktreeId::new(),
        project_id: ProjectId::new(),
        task_id: TaskId::new(),
        owner_agent_id: None,
        path: std::env::temp_dir(),
        branch_name: "agentmux/task-impl".to_string(),
        base_branch: "main".to_string(),
        status: WorktreeStatus::Failed,
        created_at: DateTimeUtc::UNIX_EPOCH,
    };
    let worktree_id = worktree.id.clone();
    runtime.register_worktree(worktree).await;
    mark_arena_candidate(
        &runtime,
        worktree_id.clone(),
        Some("README.md | 1".to_string()),
        Some(TestRunStatus::Failed),
    )
    .await;

    let error = runtime
        .request_worktree_adoption(worktree_id)
        .await
        .expect_err("adoption after failed tests is rejected");

    assert!(error.to_string().contains("passed tests"));
    assert!(runtime.list_approvals().await.is_empty());
}

#[tokio::test]
async fn worktree_adopt_rejects_second_pending_approval() {
    let runtime = DaemonRuntime::new(16);
    let first = Worktree {
        id: WorktreeId::new(),
        project_id: ProjectId::new(),
        task_id: TaskId::new(),
        owner_agent_id: None,
        path: std::env::temp_dir(),
        branch_name: "agentmux/task-first".to_string(),
        base_branch: "main".to_string(),
        status: WorktreeStatus::ReviewReady,
        created_at: DateTimeUtc::UNIX_EPOCH,
    };
    let second = Worktree {
        id: WorktreeId::new(),
        branch_name: "agentmux/task-second".to_string(),
        ..first.clone()
    };
    let first_id = first.id.clone();
    let second_id = second.id.clone();
    runtime.register_worktree(first).await;
    runtime.register_worktree(second).await;
    mark_arena_candidate(
        &runtime,
        first_id.clone(),
        Some("first".to_string()),
        Some(TestRunStatus::Passed),
    )
    .await;
    mark_arena_candidate(
        &runtime,
        second_id.clone(),
        Some("second".to_string()),
        Some(TestRunStatus::Passed),
    )
    .await;

    let first_approval = runtime
        .request_worktree_adoption(first_id.clone())
        .await
        .expect("first adoption approval is queued");
    let second_error = runtime
        .request_worktree_adoption(second_id.clone())
        .await
        .expect_err("second pending adoption is rejected");

    assert!(second_error.to_string().contains("already pending"));
    assert_eq!(runtime.list_approvals().await.len(), 1);

    runtime
        .reject_approval(&first_approval.id)
        .await
        .expect("first approval is rejected");
    wait_for_worktree_status(&runtime, &first_id, WorktreeStatus::Archived).await;

    let second_approval = runtime
        .request_worktree_adoption(second_id.clone())
        .await
        .expect("adoption is allowed after pending approval is decided");
    assert_eq!(second_approval.worktree_id, Some(second_id));
}

#[tokio::test]
async fn rejecting_one_worktree_adoption_keeps_other_candidate_ready() {
    let runtime = DaemonRuntime::new(16);
    let first = Worktree {
        id: WorktreeId::new(),
        project_id: ProjectId::new(),
        task_id: TaskId::new(),
        owner_agent_id: None,
        path: std::env::temp_dir(),
        branch_name: "agentmux/task-first".to_string(),
        base_branch: "main".to_string(),
        status: WorktreeStatus::ReviewReady,
        created_at: DateTimeUtc::UNIX_EPOCH,
    };
    let second = Worktree {
        id: WorktreeId::new(),
        branch_name: "agentmux/task-second".to_string(),
        ..first.clone()
    };
    let first_id = first.id.clone();
    let second_id = second.id.clone();
    runtime.register_worktree(first).await;
    runtime.register_worktree(second).await;
    mark_arena_candidate(
        &runtime,
        first_id.clone(),
        Some("first".to_string()),
        Some(TestRunStatus::Passed),
    )
    .await;
    mark_arena_candidate(
        &runtime,
        second_id.clone(),
        Some("second".to_string()),
        Some(TestRunStatus::Passed),
    )
    .await;

    let approval = runtime
        .request_worktree_adoption(first_id.clone())
        .await
        .expect("adoption approval is queued");
    runtime
        .reject_approval(&approval.id)
        .await
        .expect("approval is rejected");

    wait_for_worktree_status(&runtime, &first_id, WorktreeStatus::Archived).await;
    assert_eq!(
        runtime.worktree_by_id(&second_id).await.unwrap().status,
        WorktreeStatus::ReviewReady
    );
}

#[tokio::test]
async fn approving_adoption_for_missing_repo_reports_error_without_status_change() {
    let runtime = DaemonRuntime::new(16);
    let mut events = runtime.subscribe();
    let root = std::env::temp_dir().join(format!("agentmux-missing-promote-{}", ulid::Ulid::new()));
    let worktree = Worktree {
        id: WorktreeId::new(),
        project_id: ProjectId::new(),
        task_id: TaskId::new(),
        owner_agent_id: None,
        path: root.join("worktree"),
        branch_name: "agentmux/task-missing".to_string(),
        base_branch: "main".to_string(),
        status: WorktreeStatus::ReviewReady,
        created_at: DateTimeUtc::UNIX_EPOCH,
    };
    let worktree_id = worktree.id.clone();
    runtime
        .register_worktree_with_repo_root(worktree, root.join("repo"))
        .await;
    mark_arena_candidate(
        &runtime,
        worktree_id.clone(),
        Some("missing".to_string()),
        Some(TestRunStatus::Passed),
    )
    .await;

    let approval = runtime
        .request_worktree_adoption(worktree_id.clone())
        .await
        .expect("adoption approval is queued");
    runtime
        .approve_approval(&approval.id)
        .await
        .expect("approval decision is accepted");

    for _ in 0..20 {
        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("daemon emits promote failure")
            .expect("event is available");
        if event.kind == IpcEventKind::Error && event.payload["signal"] == "worktree_promote_failed"
        {
            assert_eq!(event.payload["worktree_id"], worktree_id.to_string());
            assert_eq!(
                runtime.worktree_by_id(&worktree_id).await.unwrap().status,
                WorktreeStatus::ReviewReady
            );
            return;
        }
    }
    panic!("promote failure event was not emitted");
}

#[tokio::test]
async fn approving_worktree_adopt_promotes_via_merge() {
    let root = std::env::temp_dir().join(format!("agentmux-worktree-adopt-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&root).expect("temporary worktree root is created");
    test_git(&root, ["init", "-b", "main"]);
    std::fs::write(root.join("README.md"), "hello\n").unwrap();
    test_git(&root, ["add", "README.md"]);
    test_git(
        &root,
        [
            "-c",
            "user.name=Agentmux Test",
            "-c",
            "user.email=agentmux@example.invalid",
            "commit",
            "-m",
            "initial",
        ],
    );
    test_git(&root, ["branch", "agentmux/task-impl"]);
    let runtime = DaemonRuntime::new(16);
    let worktree = Worktree {
        id: WorktreeId::new(),
        project_id: ProjectId::new(),
        task_id: TaskId::new(),
        owner_agent_id: None,
        path: root.clone(),
        branch_name: "agentmux/task-impl".to_string(),
        base_branch: "main".to_string(),
        status: WorktreeStatus::ReviewReady,
        created_at: DateTimeUtc::UNIX_EPOCH,
    };
    let worktree_id = worktree.id.clone();
    runtime
        .register_worktree_with_repo_root(worktree, root.clone())
        .await;
    mark_arena_candidate(
        &runtime,
        worktree_id.clone(),
        Some("README.md | 0".to_string()),
        Some(TestRunStatus::Passed),
    )
    .await;

    let approval = runtime
        .request_worktree_adoption(worktree_id.clone())
        .await
        .expect("adoption approval is queued");
    runtime
        .approve_approval(&approval.id)
        .await
        .expect("approval is accepted");

    wait_for_worktree_status(&runtime, &worktree_id, WorktreeStatus::Promoted).await;
    std::fs::remove_dir_all(root).expect("temporary worktree root is removed");
}

#[tokio::test]
async fn arena_run_rejects_duplicate_provider_labels_before_side_effects() {
    let root = std::env::temp_dir().join(format!(
        "agentmux-arena-duplicate-provider-{}",
        ulid::Ulid::new()
    ));
    std::fs::create_dir_all(&root).expect("temporary repo root is created");
    test_git(&root, ["init", "-b", "main"]);
    std::fs::write(root.join("README.md"), "hello\n").unwrap();
    test_git(&root, ["add", "README.md"]);
    test_git(
        &root,
        [
            "-c",
            "user.name=Agentmux Test",
            "-c",
            "user.email=agentmux@example.invalid",
            "commit",
            "-m",
            "initial",
        ],
    );
    let runtime = DaemonRuntime::new(16);

    let error = runtime
        .run_task_with_arena(
            "compare duplicate providers".to_string(),
            vec!["claude".to_string(), "claude".to_string()],
            root.clone(),
            "main".to_string(),
        )
        .await
        .expect_err("duplicate providers are rejected");

    assert!(error.to_string().contains("duplicated"));
    assert!(runtime.list_worktrees().await.is_empty());
    assert_eq!(runtime.status_payload().await["agent_count"], 0);
    assert_eq!(
        git_stdout(&root, ["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        1
    );

    std::fs::remove_dir_all(root).expect("temporary repo root is removed");
}

#[tokio::test]
async fn ipc_worktree_commands_reject_invalid_worktree_id() {
    let runtime = DaemonRuntime::new(16);
    let (client_stream, server_stream) = UnixStream::pair().unwrap();
    let server_runtime = runtime.clone();
    let server = tokio::spawn(async move { handle_client(server_stream, server_runtime).await });

    let (reader, writer) = client_stream.into_split();
    let mut reader = JsonlReader::new(BufReader::new(reader));
    let mut writer = JsonlWriter::new(writer);

    writer.write(&ClientHello::new("0.1.0")).await.unwrap();
    writer
        .write(&ClientRequest::new(
            "req_worktree_test",
            IpcCommand::WorktreeTest,
            json!({ "worktree_id": "not-a-worktree-id" }),
        ))
        .await
        .unwrap();

    let response: DaemonResponse = reader.read().await.unwrap().unwrap();
    assert!(!response.ok);
    assert_eq!(response.error.unwrap().code, "INVALID_WORKTREE_ID");

    server.abort();
}

#[tokio::test]
async fn ipc_worktree_test_rejects_policy_denied_command() {
    // #14: a command the policy engine denies (default policy denies
    // `git push`) must be rejected before it can reach `/bin/sh -c`. The
    // gate fires after worktree-id parsing but before the worktree lookup,
    // so a well-formed (non-existent) worktree id is enough to exercise it.
    let runtime = DaemonRuntime::new(16);
    let (client_stream, server_stream) = UnixStream::pair().unwrap();
    let server_runtime = runtime.clone();
    let server = tokio::spawn(async move { handle_client(server_stream, server_runtime).await });

    let (reader, writer) = client_stream.into_split();
    let mut reader = JsonlReader::new(BufReader::new(reader));
    let mut writer = JsonlWriter::new(writer);

    writer.write(&ClientHello::new("0.1.0")).await.unwrap();
    writer
        .write(&ClientRequest::new(
            "req_worktree_test",
            IpcCommand::WorktreeTest,
            json!({
                "worktree_id": WorktreeId::new().to_string(),
                "name": "smoke",
                "command": "git push origin main",
            }),
        ))
        .await
        .unwrap();

    let response: DaemonResponse = reader.read().await.unwrap().unwrap();
    assert!(!response.ok);
    let error = response.error.unwrap();
    assert_eq!(error.code, "WORKTREE_TEST_DENIED");
    assert!(
        error.message.contains("git push"),
        "denial message names the rejected command: {}",
        error.message
    );

    server.abort();
}

#[tokio::test]
async fn ipc_worktree_test_allows_non_denied_command() {
    // A command the policy engine does not deny (the default `printf` is
    // classified `Ask`, not `Deny`) must pass the gate and fail later only
    // on the missing worktree — confirming the gate does not over-reject.
    let runtime = DaemonRuntime::new(16);
    let (client_stream, server_stream) = UnixStream::pair().unwrap();
    let server_runtime = runtime.clone();
    let server = tokio::spawn(async move { handle_client(server_stream, server_runtime).await });

    let (reader, writer) = client_stream.into_split();
    let mut reader = JsonlReader::new(BufReader::new(reader));
    let mut writer = JsonlWriter::new(writer);

    writer.write(&ClientHello::new("0.1.0")).await.unwrap();
    writer
        .write(&ClientRequest::new(
            "req_worktree_test",
            IpcCommand::WorktreeTest,
            json!({
                "worktree_id": WorktreeId::new().to_string(),
                "name": "smoke",
                "command": "printf test-ok",
            }),
        ))
        .await
        .unwrap();

    let response: DaemonResponse = reader.read().await.unwrap().unwrap();
    assert!(!response.ok);
    // Not denied by policy: the request proceeds past the gate and fails on
    // the unknown worktree instead.
    assert_ne!(response.error.unwrap().code, "WORKTREE_TEST_DENIED");

    server.abort();
}

/// Regression: `worktree.test` runs a synchronous `std::process` command. On
/// the (current-thread) runtime it used to pin the only async worker for the
/// whole command duration; it must now run on the blocking pool so concurrent
/// daemon work keeps progressing.
#[tokio::test]
async fn worktree_test_command_does_not_block_the_async_worker() {
    let root = std::env::temp_dir().join(format!("agentmux-worktree-block-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&root).expect("temporary worktree root is created");
    test_git(&root, ["init", "-b", "main"]);
    std::fs::write(root.join("README.md"), "hello\n").unwrap();
    test_git(&root, ["add", "README.md"]);
    test_git(
        &root,
        [
            "-c",
            "user.name=Agentmux Test",
            "-c",
            "user.email=agentmux@example.invalid",
            "commit",
            "-m",
            "initial",
        ],
    );
    let runtime = DaemonRuntime::new(16);
    let worktree = Worktree {
        id: WorktreeId::new(),
        project_id: ProjectId::new(),
        task_id: agentmux_core::TaskId::new(),
        owner_agent_id: None,
        path: root.clone(),
        branch_name: "main".to_string(),
        base_branch: "main".to_string(),
        status: WorktreeStatus::Ready,
        created_at: DateTimeUtc::UNIX_EPOCH,
    };
    let worktree_id = worktree.id.clone();
    runtime.register_worktree(worktree).await;

    let test_runtime = runtime.clone();
    let test_worktree_id = worktree_id.clone();
    let test_task = tokio::spawn(async move {
        test_runtime
            .run_worktree_test(
                &test_worktree_id,
                TestCommand {
                    name: "slow".to_string(),
                    command: "sleep 1; printf slow-ok".to_string(),
                },
            )
            .await
    });

    // While the slow test command runs, an unrelated runtime call must
    // complete promptly: the async worker is not pinned by the child process.
    tokio::time::sleep(Duration::from_millis(150)).await;
    let status = tokio::time::timeout(Duration::from_millis(500), runtime.status_payload())
        .await
        .expect("daemon stays responsive while a worktree test command runs");
    assert_eq!(status["agent_count"], 0);

    let payload = tokio::time::timeout(Duration::from_secs(10), test_task)
        .await
        .expect("worktree test completes")
        .expect("worktree test task does not panic")
        .expect("worktree test succeeds");
    assert_eq!(payload["test"]["status"], "passed");

    std::fs::remove_dir_all(root).expect("temporary worktree root is removed");
}
