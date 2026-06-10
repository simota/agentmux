use super::*;
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static FIXTURE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
fn builds_safe_agentmux_branch_name() {
    let branch = agentmux_branch_name("Task 123: Refresh Token", "Impl Codex").unwrap();

    assert_eq!(branch, "agentmux/task-123-refresh-token-impl-codex");
}

#[test]
fn rejects_empty_branch_name_segments() {
    let error = agentmux_branch_name("!!!", "codex").unwrap_err();

    assert!(error.to_string().contains("task_slug must not be empty"));
}

#[test]
fn parses_git_worktree_porcelain_output() {
    let output = "\
worktree /repo
HEAD abc123
branch refs/heads/main

worktree /repo/.agentmux/worktrees/task-codex
HEAD def456
branch refs/heads/agentmux/task-codex

";

    let worktrees = parse_worktree_list(output).unwrap();

    assert_eq!(worktrees.len(), 2);
    assert_eq!(worktrees[0].path, PathBuf::from("/repo"));
    assert_eq!(worktrees[0].branch.as_deref(), Some("main"));
    assert_eq!(worktrees[1].branch.as_deref(), Some("agentmux/task-codex"));
}

#[test]
fn creates_lists_and_removes_git_worktree() {
    let fixture = GitFixture::new();
    fixture.git(["init", "-b", "main"]);
    fs::write(fixture.repo.join("README.md"), "hello\n").unwrap();
    fixture.git(["add", "README.md"]);
    fixture.git([
        "-c",
        "user.name=Agentmux Test",
        "-c",
        "user.email=agentmux@example.invalid",
        "commit",
        "-m",
        "initial",
    ]);

    let project_id = ProjectId::new();
    let task_id = TaskId::new();
    let manager = WorktreeManager::new(
        project_id.clone(),
        fixture.repo.clone(),
        fixture.repo.join(".agentmux/worktrees"),
    )
    .unwrap();

    let worktree = manager
        .create_worktree(CreateWorktree {
            task_id: task_id.clone(),
            task_slug: "Task 123 Refresh Token".to_string(),
            agent_name: "Codex".to_string(),
            owner_agent_id: None,
            base_branch: "main".to_string(),
        })
        .unwrap();

    assert_eq!(worktree.project_id, project_id);
    assert_eq!(worktree.task_id, task_id);
    assert_eq!(
        worktree.branch_name,
        "agentmux/task-123-refresh-token-codex"
    );
    assert_eq!(worktree.status, WorktreeStatus::Ready);
    assert!(worktree.path.join("README.md").exists());

    let listed = manager.list_worktrees().unwrap();
    assert!(listed.iter().any(|entry| {
        entry.path == worktree.path
            && entry.branch.as_deref() == Some("agentmux/task-123-refresh-token-codex")
    }));

    manager.remove_worktree(&worktree.path, false).unwrap();

    assert!(!worktree.path.exists());
    let listed = manager.list_worktrees().unwrap();
    assert!(!listed.iter().any(|entry| entry.path == worktree.path));
}

#[test]
fn merge_to_integration_branch_returns_clean_when_branch_has_no_changes() {
    let fixture = GitFixture::new();
    fixture.init_with_readme();
    let manager = WorktreeManager::new(
        ProjectId::new(),
        fixture.repo.clone(),
        fixture.repo.join(".agentmux/worktrees"),
    )
    .unwrap();
    let worktree = manager
        .create_worktree(CreateWorktree {
            task_id: TaskId::new(),
            task_slug: "Task 123".to_string(),
            agent_name: "Codex".to_string(),
            owner_agent_id: None,
            base_branch: "main".to_string(),
        })
        .unwrap();

    let outcome = manager
        .merge_to_integration_branch(&worktree, "agentmux/integration")
        .unwrap();

    assert_eq!(outcome, MergeOutcome::Clean);
    assert_eq!(fixture.git_stdout(["branch", "--show-current"]), "main\n");
    assert_eq!(
        fixture.git_stdout(["status", "--porcelain", "--untracked-files=no"]),
        ""
    );
}

#[test]
fn merge_to_integration_branch_returns_dirty_for_successful_merge() {
    let fixture = GitFixture::new();
    fixture.init_with_readme();
    let manager = WorktreeManager::new(
        ProjectId::new(),
        fixture.repo.clone(),
        fixture.repo.join(".agentmux/worktrees"),
    )
    .unwrap();
    let worktree = manager
        .create_worktree(CreateWorktree {
            task_id: TaskId::new(),
            task_slug: "Task 123".to_string(),
            agent_name: "Codex".to_string(),
            owner_agent_id: None,
            base_branch: "main".to_string(),
        })
        .unwrap();
    fs::write(worktree.path.join("feature.txt"), "candidate\n").unwrap();
    fixture.git_in(&worktree.path, ["add", "feature.txt"]);
    fixture.git_in(
        &worktree.path,
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

    let outcome = manager
        .merge_to_integration_branch(&worktree, "agentmux/integration")
        .unwrap();

    assert_eq!(outcome, MergeOutcome::Dirty);
    assert_eq!(fixture.git_stdout(["branch", "--show-current"]), "main\n");
    assert_eq!(
        fixture.git_stdout(["status", "--porcelain", "--untracked-files=no"]),
        ""
    );
    assert!(!fixture.repo.join("feature.txt").exists());
    fixture.git(["checkout", "agentmux/integration"]);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("feature.txt")).unwrap(),
        "candidate\n"
    );
    assert_eq!(
        fixture.git_stdout(["diff", "--name-only", "--diff-filter=U"]),
        ""
    );
}

#[test]
fn merge_to_integration_branch_aborts_on_conflict() {
    let fixture = GitFixture::new();
    fixture.init_with_readme();
    let manager = WorktreeManager::new(
        ProjectId::new(),
        fixture.repo.clone(),
        fixture.repo.join(".agentmux/worktrees"),
    )
    .unwrap();
    let worktree = manager
        .create_worktree(CreateWorktree {
            task_id: TaskId::new(),
            task_slug: "Task 123".to_string(),
            agent_name: "Codex".to_string(),
            owner_agent_id: None,
            base_branch: "main".to_string(),
        })
        .unwrap();
    fs::write(worktree.path.join("README.md"), "candidate\n").unwrap();
    fixture.git_in(&worktree.path, ["add", "README.md"]);
    fixture.git_in(
        &worktree.path,
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
    fixture.git(["checkout", "main"]);
    fs::write(fixture.repo.join("README.md"), "base advanced\n").unwrap();
    fixture.git(["add", "README.md"]);
    fixture.git([
        "-c",
        "user.name=Agentmux Test",
        "-c",
        "user.email=agentmux@example.invalid",
        "commit",
        "-m",
        "base advanced",
    ]);

    let outcome = manager
        .merge_to_integration_branch(&worktree, "agentmux/integration")
        .unwrap();

    assert_eq!(outcome, MergeOutcome::Conflict);
    assert_eq!(fixture.git_stdout(["branch", "--show-current"]), "main\n");
    assert_eq!(
        fixture.git_stdout(["status", "--porcelain", "--untracked-files=no"]),
        ""
    );
    assert_eq!(
        fs::read_to_string(fixture.repo.join("README.md")).unwrap(),
        "base advanced\n"
    );
    fixture.git(["checkout", "agentmux/integration"]);
    assert_eq!(
        fixture.git_stdout(["diff", "--name-only", "--diff-filter=U"]),
        ""
    );
    assert!(!fixture.repo.join(".git/MERGE_HEAD").exists());
}

#[test]
fn merge_to_integration_branch_errors_when_repo_root_is_dirty_before_starting() {
    let fixture = GitFixture::new();
    fixture.init_with_readme();
    let manager = WorktreeManager::new(
        ProjectId::new(),
        fixture.repo.clone(),
        fixture.repo.join(".agentmux/worktrees"),
    )
    .unwrap();
    let first = manager
        .create_worktree(CreateWorktree {
            task_id: TaskId::new(),
            task_slug: "Task 123".to_string(),
            agent_name: "Codex".to_string(),
            owner_agent_id: None,
            base_branch: "main".to_string(),
        })
        .unwrap();
    fs::write(first.path.join("feature-a.txt"), "candidate a\n").unwrap();
    fixture.git_in(&first.path, ["add", "feature-a.txt"]);
    fixture.git_in(
        &first.path,
        [
            "-c",
            "user.name=Agentmux Test",
            "-c",
            "user.email=agentmux@example.invalid",
            "commit",
            "-m",
            "candidate-a",
        ],
    );
    fs::write(fixture.repo.join("README.md"), "dirty\n").unwrap();

    let error = manager
        .merge_to_integration_branch(&first, "agentmux/integration")
        .expect_err("dirty repo_root is rejected");

    assert!(error.to_string().contains("repo_root must be clean"));
    assert_eq!(fixture.git_stdout(["branch", "--show-current"]), "main\n");
    assert_eq!(
        fixture.git_stdout(["diff", "--name-only", "--diff-filter=U"]),
        ""
    );
}

#[test]
fn merge_to_integration_branch_refreshes_existing_integration_from_base() {
    let fixture = GitFixture::new();
    fixture.init_with_readme();
    let manager = WorktreeManager::new(
        ProjectId::new(),
        fixture.repo.clone(),
        fixture.repo.join(".agentmux/worktrees"),
    )
    .unwrap();
    let worktree = manager
        .create_worktree(CreateWorktree {
            task_id: TaskId::new(),
            task_slug: "Task 123".to_string(),
            agent_name: "Codex".to_string(),
            owner_agent_id: None,
            base_branch: "main".to_string(),
        })
        .unwrap();
    fixture.git(["checkout", "-b", "agentmux/integration", "main"]);
    fixture.git(["checkout", "main"]);
    fs::write(fixture.repo.join("base.txt"), "advanced\n").unwrap();
    fixture.git(["add", "base.txt"]);
    fixture.git([
        "-c",
        "user.name=Agentmux Test",
        "-c",
        "user.email=agentmux@example.invalid",
        "commit",
        "-m",
        "base advanced",
    ]);
    fs::write(worktree.path.join("feature.txt"), "candidate\n").unwrap();
    fixture.git_in(&worktree.path, ["add", "feature.txt"]);
    fixture.git_in(
        &worktree.path,
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

    let outcome = manager
        .merge_to_integration_branch(&worktree, "agentmux/integration")
        .unwrap();

    assert_eq!(outcome, MergeOutcome::Dirty);
    assert_eq!(fixture.git_stdout(["branch", "--show-current"]), "main\n");
    fixture.git(["checkout", "agentmux/integration"]);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("base.txt")).unwrap(),
        "advanced\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.repo.join("feature.txt")).unwrap(),
        "candidate\n"
    );
}

#[test]
fn captures_diff_patch_artifact_with_stat() {
    let fixture = GitFixture::new();
    fixture.init_with_readme();
    fs::write(fixture.repo.join("README.md"), "hello\nchanged\n").unwrap();

    let manager = WorktreeManager::new(
        ProjectId::new(),
        fixture.repo.clone(),
        fixture.repo.join(".agentmux/worktrees"),
    )
    .unwrap();
    let task_id = TaskId::new();

    let captured = manager
        .capture_diff_artifact(
            CaptureDiff {
                task_id: task_id.clone(),
                agent_name: "Impl Codex".to_string(),
                worktree_path: fixture.repo.clone(),
                base_branch: "main".to_string(),
            },
            fixture.repo.join(".agentmux/artifacts"),
        )
        .unwrap();

    assert_eq!(captured.patch.kind, ArtifactKind::DiffPatch);
    assert_eq!(captured.patch.task_id, Some(task_id.clone()));
    assert!(
        captured
            .patch
            .path
            .ends_with(format!("{}/diff-impl-codex-001.patch", task_id))
    );
    assert!(captured.stat.contains("README.md"));
    let patch = fs::read_to_string(captured.patch.path).unwrap();
    assert!(patch.contains("+changed"));
    assert!(captured.patch.size_bytes > 0);
}

#[test]
fn stores_test_command_log_artifact_for_test_pane() {
    let fixture = GitFixture::new();
    fixture.init_with_readme();
    let manager = WorktreeManager::new(
        ProjectId::new(),
        fixture.repo.clone(),
        fixture.repo.join(".agentmux/worktrees"),
    )
    .unwrap();
    let task_id = TaskId::new();

    let passed = manager
        .run_test_command_artifact(
            task_id.clone(),
            "impl-codex",
            &fixture.repo,
            TestCommand {
                name: "unit".to_string(),
                command: "printf 'ok\\n'".to_string(),
            },
            fixture.repo.join(".agentmux/artifacts"),
        )
        .unwrap();
    let failed = manager
        .run_test_command_artifact(
            task_id.clone(),
            "impl-codex",
            &fixture.repo,
            TestCommand {
                name: "unit".to_string(),
                command: "printf 'bad\\n' >&2; exit 7".to_string(),
            },
            fixture.repo.join(".agentmux/artifacts"),
        )
        .unwrap();

    assert_eq!(passed.status, TestRunStatus::Passed);
    assert_eq!(passed.exit_code, Some(0));
    assert_eq!(failed.status, TestRunStatus::Failed);
    assert_eq!(failed.exit_code, Some(7));
    assert!(
        passed
            .artifact
            .path
            .ends_with(format!("{}/test-impl-codex-001.log", task_id))
    );
    assert!(
        failed
            .artifact
            .path
            .ends_with(format!("{}/test-impl-codex-002.log", task_id))
    );
    let passed_log = fs::read_to_string(passed.artifact.path).unwrap();
    let failed_log = fs::read_to_string(failed.artifact.path).unwrap();
    assert!(passed_log.contains("command: printf 'ok"));
    assert!(passed_log.contains("--- stdout ---\nok"));
    assert!(failed_log.contains("exit_code: 7"));
    assert!(failed_log.contains("--- stderr ---\nbad"));
}

#[test]
fn rejects_empty_test_command_before_writing_artifact() {
    let fixture = GitFixture::new();
    fixture.init_with_readme();
    let manager = WorktreeManager::new(
        ProjectId::new(),
        fixture.repo.clone(),
        fixture.repo.join(".agentmux/worktrees"),
    )
    .unwrap();

    let error = manager
        .run_test_command_artifact(
            TaskId::new(),
            "impl-codex",
            &fixture.repo,
            TestCommand {
                name: "unit".to_string(),
                command: " ".to_string(),
            },
            fixture.repo.join(".agentmux/artifacts"),
        )
        .unwrap_err();

    assert!(error.to_string().contains("test command must not be empty"));
}

/// Regression: a hung test command must be killed at the timeout instead of
/// occupying its caller forever (the daemon runs these on blocking workers).
#[test]
fn shell_command_times_out_and_kills_the_child() {
    let fixture = GitFixture::new();
    let started = std::time::Instant::now();

    let error = run_shell_command_with_timeout(
        &fixture.repo,
        "sleep 30",
        std::time::Duration::from_millis(200),
    )
    .unwrap_err();

    assert!(
        error.to_string().contains("timed out"),
        "timeout error mentions the cause, got: {error}"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "the hung command is killed promptly, not awaited to completion"
    );
}

#[test]
fn shell_command_within_timeout_captures_output_and_status() {
    let fixture = GitFixture::new();

    let output = run_shell_command_with_timeout(
        &fixture.repo,
        "printf out-marker; printf err-marker >&2; exit 3",
        std::time::Duration::from_secs(10),
    )
    .unwrap();

    assert_eq!(output.status.code(), Some(3));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "out-marker");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "err-marker");
}

struct GitFixture {
    root: PathBuf,
    repo: PathBuf,
}

impl GitFixture {
    fn new() -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = FIXTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "agentmux-worktree-test-{}-{unique}-{sequence}",
            std::process::id()
        ));
        let repo = root.join("repo");
        fs::create_dir_all(&repo).unwrap();
        Self { root, repo }
    }

    fn init_with_readme(&self) {
        self.git(["init", "-b", "main"]);
        fs::write(self.repo.join("README.md"), "hello\n").unwrap();
        self.git(["add", "README.md"]);
        self.git([
            "-c",
            "user.name=Agentmux Test",
            "-c",
            "user.email=agentmux@example.invalid",
            "commit",
            "-m",
            "initial",
        ]);
    }

    fn git<const N: usize>(&self, args: [&str; N]) {
        self.git_in(&self.repo, args);
    }

    fn git_in<const N: usize>(&self, cwd: &Path, args: [&str; N]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git command failed: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_stdout<const N: usize>(&self, args: [&str; N]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git command failed: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
}

impl Drop for GitFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
