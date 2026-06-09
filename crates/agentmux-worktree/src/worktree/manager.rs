//! `WorktreeManager` — thin wrapper around `git worktree` for one project repo.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use agentmux_core::{
    AgentmuxError, ArtifactKind, DateTimeUtc, ProjectId, TaskId, WorktreeId, WorktreeStatus,
    error::Result,
};

use super::artifact::{
    artifact_metadata, artifact_path, next_artifact_sequence, write_artifact_file,
};
use super::branch::{
    agentmux_branch_name, parse_worktree_list, slug_segment, validate_git_ref_name,
    validate_test_command, validate_worktree_path,
};
use super::git::{
    abort_conflicted_merge, checkout_or_create_branch, current_head, ensure_repo_root_clean,
    git_failure, integration_branch_is_dirty, restore_head, run_git, run_git_raw,
    run_shell_command, test_log_contents, unresolved_conflicts, utf8_stdout,
};
use super::types::{
    CaptureDiff, CapturedDiff, CreateWorktree, GitWorktree, MergeOutcome, TestCommand,
    TestRunArtifact, TestRunStatus, Worktree,
};

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
            (Ok(_), Err(error)) => Err(error),
            // Both failed: the merge/promote error is the root cause and must
            // not be discarded; append the restore failure so neither is lost.
            (Err(primary), Err(restore)) => Err(AgentmuxError::Internal(format!(
                "{primary} (and failed to restore HEAD: {restore})"
            ))),
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
