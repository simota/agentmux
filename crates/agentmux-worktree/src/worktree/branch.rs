//! Branch naming, git-ref validation, and worktree-list parsing helpers.

use std::path::{Path, PathBuf};

use agentmux_core::{AgentmuxError, error::Result};

use super::types::{GitWorktree, TestCommand};

/// Build the v0.1 branch name: `agentmux/{task_slug}-{agent_name}`.
pub fn agentmux_branch_name(task_slug: &str, agent_name: &str) -> Result<String> {
    Ok(format!(
        "agentmux/{}-{}",
        slug_segment(task_slug, "task_slug")?,
        slug_segment(agent_name, "agent_name")?
    ))
}

pub(crate) fn slug_segment(value: &str, field: &str) -> Result<String> {
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

pub(crate) fn validate_git_ref_segment(value: &str, field: &str) -> Result<()> {
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

pub(crate) fn validate_git_ref_name(value: &str, field: &str) -> Result<()> {
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

pub(crate) fn parse_worktree_list(output: &str) -> Result<Vec<GitWorktree>> {
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

pub(crate) fn validate_worktree_path(path: &Path) -> Result<()> {
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

pub(crate) fn validate_test_command(command: &TestCommand) -> Result<()> {
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
