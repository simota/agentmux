//! Git command and shell process helpers.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output};

use agentmux_core::{AgentmuxError, error::Result};

use super::types::TestCommand;

pub(crate) fn utf8_stdout(output: Output, command: &str) -> Result<String> {
    String::from_utf8(output.stdout).map_err(|error| {
        AgentmuxError::Internal(format!("{command} output was not utf-8: {error}"))
    })
}

pub(crate) fn checkout_or_create_branch(
    repo_root: &Path,
    branch: &str,
    base_branch: &str,
) -> Result<()> {
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

pub(crate) fn current_head(repo_root: &Path) -> Result<String> {
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

pub(crate) fn restore_head(repo_root: &Path, head: &str) -> Result<()> {
    run_git(repo_root, [OsStr::new("checkout"), OsStr::new(head)])?;
    Ok(())
}

pub(crate) fn ensure_repo_root_clean(repo_root: &Path) -> Result<()> {
    if integration_branch_is_dirty(repo_root)? {
        return Err(AgentmuxError::UserError(
            "repo_root must be clean before promoting a worktree".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn unresolved_conflicts(repo_root: &Path) -> Result<String> {
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

pub(crate) fn abort_conflicted_merge(repo_root: &Path, conflicts: &str) -> Result<()> {
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

pub(crate) fn integration_branch_is_dirty(repo_root: &Path) -> Result<bool> {
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

pub(crate) fn run_git<'a>(
    repo_root: &Path,
    args: impl IntoIterator<Item = &'a OsStr>,
) -> Result<Output> {
    let output = run_git_raw(repo_root, args)?;
    if !output.status.success() {
        return Err(git_failure("git worktree failed", output));
    }

    Ok(output)
}

pub(crate) fn run_git_raw<'a>(
    repo_root: &Path,
    args: impl IntoIterator<Item = &'a OsStr>,
) -> Result<Output> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .map_err(|error| AgentmuxError::ProviderError(format!("failed to run git: {error}")))?;

    Ok(output)
}

pub(crate) fn git_failure(prefix: &str, output: Output) -> AgentmuxError {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if stderr.is_empty() { stdout } else { stderr };
    AgentmuxError::UserError(format!("{prefix}: {detail}"))
}

pub(crate) fn run_shell_command(worktree_path: &Path, command: &str) -> Result<Output> {
    Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .current_dir(worktree_path)
        .output()
        .map_err(|error| {
            AgentmuxError::ProviderError(format!("failed to run test command: {error}"))
        })
}

pub(crate) fn test_log_contents(command: &TestCommand, output: &Output) -> String {
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
