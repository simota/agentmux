//! Worktree domain structs and git shell-out lifecycle operations.
//!
//! See `docs/spec/03_domain_model.md §7`.

mod artifact;
mod branch;
mod git;
mod manager;
mod types;

#[cfg(test)]
mod tests;

pub use branch::agentmux_branch_name;
pub use manager::WorktreeManager;
pub use types::{
    Artifact, CaptureDiff, CapturedDiff, CreateWorktree, GitWorktree, MergeOutcome, TestCommand,
    TestRunArtifact, TestRunStatus, Worktree,
};

// Re-exports for the in-module test suite (`tests.rs` uses `super::*`).
#[cfg(test)]
pub(crate) use agentmux_core::{ArtifactKind, ProjectId, TaskId, WorktreeStatus};
#[cfg(test)]
pub(crate) use branch::parse_worktree_list;
#[cfg(test)]
pub(crate) use git::run_shell_command_with_timeout;
#[cfg(test)]
pub(crate) use std::path::{Path, PathBuf};
#[cfg(test)]
pub(crate) use std::process::Command;
