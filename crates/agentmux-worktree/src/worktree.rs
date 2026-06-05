//! Worktree domain structs and git shell-out lifecycle operations.
//!
//! See `docs/spec/03_domain_model.md §7`.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use agentmux_core::{
    AgentSessionId, AgentmuxError, ArtifactId, ArtifactKind, DateTimeUtc, ProjectId, TaskId,
    WorktreeId, WorktreeStatus, error::Result,
};
use serde::{Deserialize, Serialize};

/// A git worktree dedicated to a specific agent session within a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Worktree {
    pub id: WorktreeId,
    pub project_id: ProjectId,
    pub task_id: TaskId,
    pub owner_agent_id: Option<AgentSessionId>,
    /// Absolute path to the worktree checkout on disk.
    pub path: PathBuf,
    pub branch_name: String,
    pub base_branch: String,
    pub status: WorktreeStatus,
    pub created_at: DateTimeUtc,
}

/// Request to create a dedicated git worktree for an agent.
#[derive(Debug, Clone)]
pub struct CreateWorktree {
    pub task_id: TaskId,
    pub task_slug: String,
    pub agent_name: String,
    pub owner_agent_id: Option<AgentSessionId>,
    pub base_branch: String,
}

/// Raw worktree entry returned by `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitWorktree {
    pub path: PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub detached: bool,
    pub bare: bool,
}

/// Persisted artifact metadata for files under `.agentmux/artifacts/<task>/`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub id: ArtifactId,
    pub project_id: ProjectId,
    pub task_id: Option<TaskId>,
    pub kind: ArtifactKind,
    pub path: PathBuf,
    pub title: String,
    pub mime_type: Option<String>,
    pub size_bytes: u64,
    pub checksum: Option<String>,
    pub created_at: DateTimeUtc,
}

/// Request to capture a worktree diff as reviewable artifacts.
#[derive(Debug, Clone)]
pub struct CaptureDiff {
    pub task_id: TaskId,
    pub agent_name: String,
    pub worktree_path: PathBuf,
    pub base_branch: String,
}

/// Diff capture result: full patch artifact plus stat output for summaries.
#[derive(Debug, Clone)]
pub struct CapturedDiff {
    pub patch: Artifact,
    pub stat: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeOutcome {
    Clean,
    Dirty,
    Conflict,
}

/// Project-configured test command to run in a target worktree.
#[derive(Debug, Clone)]
pub struct TestCommand {
    pub name: String,
    pub command: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestRunStatus {
    Passed,
    Failed,
}

/// Captured test command output for a test pane.
#[derive(Debug, Clone)]
pub struct TestRunArtifact {
    pub artifact: Artifact,
    pub command: String,
    pub status: TestRunStatus,
    pub exit_code: Option<i32>,
}

/// Thin manager around `git worktree` commands for one project repository.
#[derive(Debug, Clone)]
pub struct WorktreeManager {
    project_id: ProjectId,
    repo_root: PathBuf,
    worktrees_root: PathBuf,
}

impl WorktreeManager {
    pub fn new(
        project_id: ProjectId,
        repo_root: impl Into<PathBuf>,
        worktrees_root: impl Into<PathBuf>,
    ) -> Result<Self> {
        let repo_root = repo_root.into();
        let worktrees_root = worktrees_root.into();
        if repo_root.as_os_str().is_empty() {
            return Err(AgentmuxError::UserError(
                "repo_root must not be empty".to_string(),
            ));
        }
        if worktrees_root.as_os_str().is_empty() {
            return Err(AgentmuxError::UserError(
                "worktrees_root must not be empty".to_string(),
            ));
        }

        Ok(Self {
            project_id,
            repo_root,
            worktrees_root,
        })
    }

    pub fn create_worktree(&self, input: CreateWorktree) -> Result<Worktree> {
        validate_git_ref_name(&input.base_branch, "base_branch")?;
        let branch_name = agentmux_branch_name(&input.task_slug, &input.agent_name)?;
        let dir_name = format!(
            "{}-{}",
            slug_segment(&input.task_slug, "task_slug")?,
            slug_segment(&input.agent_name, "agent_name")?
        );
        let path = self.worktrees_root.join(dir_name);

        let args = [
            OsStr::new("worktree"),
            OsStr::new("add"),
            path.as_os_str(),
            OsStr::new("-b"),
            OsStr::new(&branch_name),
            OsStr::new(&input.base_branch),
        ];
        run_git(&self.repo_root, args)?;
        let path = fs::canonicalize(&path).map_err(|error| {
            AgentmuxError::Internal(format!(
                "created worktree path could not be resolved: {error}"
            ))
        })?;

        Ok(Worktree {
            id: WorktreeId::new(),
            project_id: self.project_id.clone(),
            task_id: input.task_id,
            owner_agent_id: input.owner_agent_id,
            path,
            branch_name,
            base_branch: input.base_branch,
            status: WorktreeStatus::Ready,
            created_at: DateTimeUtc::now_utc(),
        })
    }

    pub fn list_worktrees(&self) -> Result<Vec<GitWorktree>> {
        let output = run_git(
            &self.repo_root,
            [
                OsStr::new("worktree"),
                OsStr::new("list"),
                OsStr::new("--porcelain"),
            ],
        )?;
        let stdout = String::from_utf8(output.stdout).map_err(|error| {
            AgentmuxError::Internal(format!("git worktree list output was not utf-8: {error}"))
        })?;
        parse_worktree_list(&stdout)
    }

    pub fn remove_worktree(&self, path: impl AsRef<Path>, force: bool) -> Result<()> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            return Err(AgentmuxError::UserError(
                "worktree path must not be empty".to_string(),
            ));
        }

        let mut args = vec![
            OsStr::new("worktree"),
            OsStr::new("remove"),
            path.as_os_str(),
        ];
        if force {
            args.push(OsStr::new("--force"));
        }
        run_git(&self.repo_root, args)?;
        Ok(())
    }

    pub fn merge_to_integration_branch(
        &self,
        worktree: &Worktree,
        integration_branch: &str,
    ) -> Result<MergeOutcome> {
        validate_git_ref_name(&worktree.branch_name, "worktree.branch_name")?;
        validate_git_ref_name(&worktree.base_branch, "worktree.base_branch")?;
        validate_git_ref_name(integration_branch, "integration_branch")?;
        validate_worktree_path(&worktree.path)?;

        ensure_repo_root_clean(&self.repo_root)?;
        let original_head = current_head(&self.repo_root)?;
        let result = (|| {
            checkout_or_create_branch(&self.repo_root, integration_branch, &worktree.base_branch)?;
            run_git(
                &self.repo_root,
                [
                    OsStr::new("reset"),
                    OsStr::new("--hard"),
                    OsStr::new(&worktree.base_branch),
                ],
            )?;
            let merge_output = run_git_raw(
                &self.repo_root,
                [
                    OsStr::new("merge"),
                    OsStr::new("--no-commit"),
                    OsStr::new("--no-ff"),
                    OsStr::new(&worktree.branch_name),
                ],
            )?;

            if !merge_output.status.success() {
                let conflicts = unresolved_conflicts(&self.repo_root)?;
                if !conflicts.is_empty() {
                    abort_conflicted_merge(&self.repo_root, &conflicts)?;
                    return Ok(MergeOutcome::Conflict);
                }
                return Err(git_failure("git merge failed", merge_output));
            }

            if integration_branch_is_dirty(&self.repo_root)? {
                run_git(
                    &self.repo_root,
                    [
                        OsStr::new("-c"),
                        OsStr::new("user.name=Agentmux"),
                        OsStr::new("-c"),
                        OsStr::new("user.email=agentmux@example.invalid"),
                        OsStr::new("commit"),
                        OsStr::new("-m"),
                        OsStr::new(&format!(
                            "Promote worktree {} into {integration_branch}",
                            worktree.id
                        )),
                    ],
                )?;
                return Ok(MergeOutcome::Dirty);
            }

            Ok(MergeOutcome::Clean)
        })();
        let restore = restore_head(&self.repo_root, &original_head);
        match (result, restore) {
            (Ok(outcome), Ok(())) => Ok(outcome),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(error)) | (Err(_), Err(error)) => Err(error),
        }
    }

    pub fn capture_diff_artifact(
        &self,
        input: CaptureDiff,
        artifacts_root: impl AsRef<Path>,
    ) -> Result<CapturedDiff> {
        validate_git_ref_name(&input.base_branch, "base_branch")?;
        validate_worktree_path(&input.worktree_path)?;
        let agent_segment = slug_segment(&input.agent_name, "agent_name")?;
        let artifacts_dir = artifacts_root.as_ref().join(input.task_id.to_string());
        let sequence = next_artifact_sequence(&artifacts_dir, "diff", &agent_segment, "patch")?;
        let patch_path = artifact_path(&artifacts_dir, "diff", &agent_segment, sequence, "patch");

        let base_spec = input.base_branch.clone();
        let stat_output = run_git(
            &input.worktree_path,
            [
                OsStr::new("diff"),
                OsStr::new("--stat"),
                OsStr::new(&base_spec),
            ],
        )?;
        let patch_output = run_git(
            &input.worktree_path,
            [OsStr::new("diff"), OsStr::new(&base_spec)],
        )?;
        let stat = utf8_stdout(stat_output, "git diff --stat")?;
        write_artifact_file(&patch_path, &patch_output.stdout)?;

        let patch = artifact_metadata(
            self.project_id.clone(),
            Some(input.task_id),
            ArtifactKind::DiffPatch,
            patch_path,
            format!("Diff patch: {}", input.agent_name),
            Some("text/x-patch".to_string()),
        )?;

        Ok(CapturedDiff { patch, stat })
    }

    pub fn run_test_command_artifact(
        &self,
        task_id: TaskId,
        agent_name: &str,
        worktree_path: impl AsRef<Path>,
        test_command: TestCommand,
        artifacts_root: impl AsRef<Path>,
    ) -> Result<TestRunArtifact> {
        validate_worktree_path(worktree_path.as_ref())?;
        validate_test_command(&test_command)?;
        let agent_segment = slug_segment(agent_name, "agent_name")?;
        let artifacts_dir = artifacts_root.as_ref().join(task_id.to_string());
        let sequence = next_artifact_sequence(&artifacts_dir, "test", &agent_segment, "log")?;
        let log_path = artifact_path(&artifacts_dir, "test", &agent_segment, sequence, "log");

        let output = run_shell_command(worktree_path.as_ref(), &test_command.command)?;
        let status = if output.status.success() {
            TestRunStatus::Passed
        } else {
            TestRunStatus::Failed
        };
        let log = test_log_contents(&test_command, &output);
        write_artifact_file(&log_path, log.as_bytes())?;

        let artifact = artifact_metadata(
            self.project_id.clone(),
            Some(task_id),
            ArtifactKind::TestLog,
            log_path,
            format!("Test log: {agent_name} / {}", test_command.name),
            Some("text/plain".to_string()),
        )?;

        Ok(TestRunArtifact {
            artifact,
            command: test_command.command,
            status,
            exit_code: output.status.code(),
        })
    }
}

/// Build the v0.1 branch name: `agentmux/{task_slug}-{agent_name}`.
pub fn agentmux_branch_name(task_slug: &str, agent_name: &str) -> Result<String> {
    Ok(format!(
        "agentmux/{}-{}",
        slug_segment(task_slug, "task_slug")?,
        slug_segment(agent_name, "agent_name")?
    ))
}

fn slug_segment(value: &str, field: &str) -> Result<String> {
    let slug = value
        .trim()
        .chars()
        .flat_map(char::to_lowercase)
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");

    validate_git_ref_segment(&slug, field)?;
    Ok(slug)
}

fn validate_git_ref_segment(value: &str, field: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(AgentmuxError::UserError(format!(
            "{field} must not be empty"
        )));
    }
    if value.starts_with('-')
        || value.starts_with('.')
        || value.ends_with('.')
        || value.contains("..")
        || value.contains('@')
        || value.contains('\\')
        || value.contains('/')
        || value.chars().any(|ch| ch.is_control())
    {
        return Err(AgentmuxError::UserError(format!(
            "{field} contains characters that are unsafe for a git ref segment"
        )));
    }
    Ok(())
}

fn validate_git_ref_name(value: &str, field: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(AgentmuxError::UserError(format!(
            "{field} must not be empty"
        )));
    }
    if value.starts_with('-')
        || value.starts_with('/')
        || value.ends_with('/')
        || value.ends_with('.')
        || value.contains("..")
        || value.contains("//")
        || value.contains('@')
        || value.contains('\\')
        || value.chars().any(|ch| ch.is_control())
    {
        return Err(AgentmuxError::UserError(format!(
            "{field} contains characters that are unsafe for a git ref"
        )));
    }
    Ok(())
}

fn parse_worktree_list(output: &str) -> Result<Vec<GitWorktree>> {
    let mut worktrees = Vec::new();
    let mut current: Option<GitWorktree> = None;

    for line in output.lines() {
        if line.is_empty() {
            if let Some(worktree) = current.take() {
                worktrees.push(worktree);
            }
            continue;
        }

        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(worktree) = current.take() {
                worktrees.push(worktree);
            }
            current = Some(GitWorktree {
                path: PathBuf::from(path),
                head: None,
                branch: None,
                detached: false,
                bare: false,
            });
            continue;
        }

        let Some(worktree) = current.as_mut() else {
            return Err(AgentmuxError::Internal(format!(
                "git worktree list entry started without worktree path: {line}"
            )));
        };

        if let Some(head) = line.strip_prefix("HEAD ") {
            worktree.head = Some(head.to_string());
        } else if let Some(branch) = line.strip_prefix("branch refs/heads/") {
            worktree.branch = Some(branch.to_string());
        } else if line == "detached" {
            worktree.detached = true;
        } else if line == "bare" {
            worktree.bare = true;
        }
    }

    if let Some(worktree) = current {
        worktrees.push(worktree);
    }

    Ok(worktrees)
}

fn validate_worktree_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() {
        return Err(AgentmuxError::UserError(
            "worktree path must not be empty".to_string(),
        ));
    }
    if !path.exists() {
        return Err(AgentmuxError::UserError(format!(
            "worktree path does not exist: {}",
            path.display()
        )));
    }
    Ok(())
}

fn validate_test_command(command: &TestCommand) -> Result<()> {
    if command.name.trim().is_empty() {
        return Err(AgentmuxError::UserError(
            "test command name must not be empty".to_string(),
        ));
    }
    if command.command.trim().is_empty() {
        return Err(AgentmuxError::UserError(
            "test command must not be empty".to_string(),
        ));
    }
    Ok(())
}

fn artifact_path(
    artifacts_dir: &Path,
    kind: &str,
    agent_segment: &str,
    sequence: u32,
    ext: &str,
) -> PathBuf {
    artifacts_dir.join(format!("{kind}-{agent_segment}-{sequence:03}.{ext}"))
}

fn next_artifact_sequence(
    artifacts_dir: &Path,
    kind: &str,
    agent_segment: &str,
    ext: &str,
) -> Result<u32> {
    let mut sequence = 1;
    if !artifacts_dir.exists() {
        return Ok(sequence);
    }

    let prefix = format!("{kind}-{agent_segment}-");
    let suffix = format!(".{ext}");
    for entry in fs::read_dir(artifacts_dir).map_err(|error| {
        AgentmuxError::Internal(format!(
            "failed to read artifacts dir {}: {error}",
            artifacts_dir.display()
        ))
    })? {
        let entry = entry.map_err(|error| {
            AgentmuxError::Internal(format!("failed to read artifact dir entry: {error}"))
        })?;
        let Some(file_name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some(raw_sequence) = file_name
            .strip_prefix(&prefix)
            .and_then(|value| value.strip_suffix(&suffix))
        else {
            continue;
        };
        if let Ok(found) = raw_sequence.parse::<u32>() {
            sequence = sequence.max(found.saturating_add(1));
        }
    }

    Ok(sequence)
}

fn write_artifact_file(path: &Path, contents: &[u8]) -> Result<()> {
    let Some(parent) = path.parent() else {
        return Err(AgentmuxError::Internal(format!(
            "artifact path has no parent: {}",
            path.display()
        )));
    };
    fs::create_dir_all(parent).map_err(|error| {
        AgentmuxError::Internal(format!(
            "failed to create artifact dir {}: {error}",
            parent.display()
        ))
    })?;
    fs::write(path, contents).map_err(|error| {
        AgentmuxError::Internal(format!(
            "failed to write artifact {}: {error}",
            path.display()
        ))
    })
}

fn artifact_metadata(
    project_id: ProjectId,
    task_id: Option<TaskId>,
    kind: ArtifactKind,
    path: PathBuf,
    title: String,
    mime_type: Option<String>,
) -> Result<Artifact> {
    let metadata = fs::metadata(&path).map_err(|error| {
        AgentmuxError::Internal(format!(
            "failed to read artifact metadata {}: {error}",
            path.display()
        ))
    })?;
    Ok(Artifact {
        id: ArtifactId::new(),
        project_id,
        task_id,
        kind,
        path,
        title,
        mime_type,
        size_bytes: metadata.len(),
        checksum: None,
        created_at: DateTimeUtc::now_utc(),
    })
}

fn utf8_stdout(output: Output, command: &str) -> Result<String> {
    String::from_utf8(output.stdout).map_err(|error| {
        AgentmuxError::Internal(format!("{command} output was not utf-8: {error}"))
    })
}

fn checkout_or_create_branch(repo_root: &Path, branch: &str, base_branch: &str) -> Result<()> {
    let checkout = run_git_raw(repo_root, [OsStr::new("checkout"), OsStr::new(branch)])?;
    if checkout.status.success() {
        return Ok(());
    }

    run_git(
        repo_root,
        [
            OsStr::new("checkout"),
            OsStr::new("-b"),
            OsStr::new(branch),
            OsStr::new(base_branch),
        ],
    )?;
    Ok(())
}

fn current_head(repo_root: &Path) -> Result<String> {
    let branch = run_git_raw(
        repo_root,
        [
            OsStr::new("symbolic-ref"),
            OsStr::new("--quiet"),
            OsStr::new("--short"),
            OsStr::new("HEAD"),
        ],
    )?;
    if branch.status.success() {
        return Ok(utf8_stdout(branch, "git symbolic-ref --short HEAD")?
            .trim()
            .to_string());
    }

    Ok(utf8_stdout(
        run_git(repo_root, [OsStr::new("rev-parse"), OsStr::new("HEAD")])?,
        "git rev-parse HEAD",
    )?
    .trim()
    .to_string())
}

fn restore_head(repo_root: &Path, head: &str) -> Result<()> {
    run_git(repo_root, [OsStr::new("checkout"), OsStr::new(head)])?;
    Ok(())
}

fn ensure_repo_root_clean(repo_root: &Path) -> Result<()> {
    if integration_branch_is_dirty(repo_root)? {
        return Err(AgentmuxError::UserError(
            "repo_root must be clean before promoting a worktree".to_string(),
        ));
    }
    Ok(())
}

fn unresolved_conflicts(repo_root: &Path) -> Result<String> {
    let output = run_git(
        repo_root,
        [
            OsStr::new("diff"),
            OsStr::new("--name-only"),
            OsStr::new("--diff-filter=U"),
        ],
    )?;
    utf8_stdout(output, "git diff --name-only --diff-filter=U")
}

fn abort_conflicted_merge(repo_root: &Path, conflicts: &str) -> Result<()> {
    let abort = run_git_raw(repo_root, [OsStr::new("merge"), OsStr::new("--abort")])?;
    if !abort.status.success() {
        return Err(AgentmuxError::UserError(format!(
            "git merge conflicted in {} and merge --abort failed: {}",
            conflicts.trim(),
            git_failure("git merge --abort failed", abort)
        )));
    }
    if repo_root.join(".git/MERGE_HEAD").exists() {
        return Err(AgentmuxError::UserError(format!(
            "git merge conflicted in {} and merge --abort left MERGE_HEAD",
            conflicts.trim()
        )));
    }
    Ok(())
}

fn integration_branch_is_dirty(repo_root: &Path) -> Result<bool> {
    let output = run_git(
        repo_root,
        [
            OsStr::new("status"),
            OsStr::new("--porcelain"),
            OsStr::new("--untracked-files=no"),
        ],
    )?;
    Ok(!utf8_stdout(output, "git status --porcelain")?
        .trim()
        .is_empty())
}

fn run_git<'a>(repo_root: &Path, args: impl IntoIterator<Item = &'a OsStr>) -> Result<Output> {
    let output = run_git_raw(repo_root, args)?;
    if !output.status.success() {
        return Err(git_failure("git worktree failed", output));
    }

    Ok(output)
}

fn run_git_raw<'a>(repo_root: &Path, args: impl IntoIterator<Item = &'a OsStr>) -> Result<Output> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .map_err(|error| AgentmuxError::ProviderError(format!("failed to run git: {error}")))?;

    Ok(output)
}

fn git_failure(prefix: &str, output: Output) -> AgentmuxError {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if stderr.is_empty() { stdout } else { stderr };
    AgentmuxError::UserError(format!("{prefix}: {detail}"))
}

fn run_shell_command(worktree_path: &Path, command: &str) -> Result<Output> {
    Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .current_dir(worktree_path)
        .output()
        .map_err(|error| {
            AgentmuxError::ProviderError(format!("failed to run test command: {error}"))
        })
}

fn test_log_contents(command: &TestCommand, output: &Output) -> String {
    format!(
        "command: {}\nstatus: {}\nexit_code: {}\n\n--- stdout ---\n{}\n--- stderr ---\n{}",
        command.command,
        output.status,
        output
            .status
            .code()
            .map(|code| code.to_string())
            .unwrap_or_else(|| "signal".to_string()),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[cfg(test)]
mod tests {
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
}
