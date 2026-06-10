//! Git command and shell process helpers.

use std::ffi::OsStr;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use agentmux_core::{AgentmuxError, error::Result};

use super::types::TestCommand;

/// Upper bound on a single git invocation. Git can hang indefinitely (e.g. a
/// credential prompt, a wedged filesystem); a stuck call must not pin its
/// caller forever.
pub(crate) const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

/// Upper bound on a worktree test command (`worktree.test`). Test suites are
/// slow but must still terminate; a hung test is killed and reported.
pub(crate) const TEST_COMMAND_TIMEOUT: Duration = Duration::from_secs(600);

/// Poll interval for the bounded child wait loop.
const CHILD_WAIT_POLL: Duration = Duration::from_millis(25);

/// Run `command` to completion with captured output, killing the child if it
/// does not exit within `timeout`.
///
/// Equivalent to `Command::output()` (stdin null, stdout/stderr piped) but
/// bounded: the wait is a `try_wait` poll loop with a deadline, and on timeout
/// the child is killed (SIGKILL) and reaped before the error is returned.
/// Output pipes are drained on dedicated threads so a chatty child can never
/// deadlock against a full pipe buffer.
pub(crate) fn run_command_with_timeout(
    command: &mut Command,
    timeout: Duration,
    label: &str,
) -> Result<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| AgentmuxError::ProviderError(format!("failed to run {label}: {error}")))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_thread = std::thread::spawn(move || read_pipe_to_end(stdout));
    let stderr_thread = std::thread::spawn(move || read_pipe_to_end(stderr));

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                // SIGKILL, then reap so no zombie is left behind. The reader
                // threads finish once the pipe write ends close with the child.
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                return Err(AgentmuxError::ProviderError(format!(
                    "{label} timed out after {}s and was killed",
                    timeout.as_secs()
                )));
            }
            Ok(None) => std::thread::sleep(CHILD_WAIT_POLL),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                return Err(AgentmuxError::ProviderError(format!(
                    "failed to wait for {label}: {error}"
                )));
            }
        }
    };

    let stdout = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn read_pipe_to_end(pipe: Option<impl Read>) -> Vec<u8> {
    let mut buffer = Vec::new();
    if let Some(mut pipe) = pipe {
        let _ = pipe.read_to_end(&mut buffer);
    }
    buffer
}

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
    let mut command = Command::new("git");
    command.arg("-C").arg(repo_root).args(args);
    run_command_with_timeout(&mut command, GIT_COMMAND_TIMEOUT, "git")
}

pub(crate) fn git_failure(prefix: &str, output: Output) -> AgentmuxError {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if stderr.is_empty() { stdout } else { stderr };
    AgentmuxError::UserError(format!("{prefix}: {detail}"))
}

pub(crate) fn run_shell_command(worktree_path: &Path, command: &str) -> Result<Output> {
    run_shell_command_with_timeout(worktree_path, command, TEST_COMMAND_TIMEOUT)
}

pub(crate) fn run_shell_command_with_timeout(
    worktree_path: &Path,
    command: &str,
    timeout: Duration,
) -> Result<Output> {
    let mut shell = Command::new("/bin/sh");
    shell.arg("-c").arg(command).current_dir(worktree_path);
    run_command_with_timeout(&mut shell, timeout, "test command")
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
