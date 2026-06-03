//! `agentmux-worktree` — Git worktree lifecycle management.
//!
//! Responsibilities (see `docs/spec/02_system_architecture.md §4.9`):
//! - create and remove git worktrees (`git worktree add / remove`)
//! - branch naming conventions (e.g. `agentmux/<task-slug>-<agent-name>`)
//! - capture diff artifacts (`git diff`) and store as `Artifact`
//! - test-target tracking (which worktree owns a test run)
//!
//! v0.1 shells out to `git` via `std::process::Command`.
//! No libgit2/gitoxide dependency required at this stage.

pub mod worktree;

pub use worktree::{
    Artifact, CaptureDiff, CapturedDiff, CreateWorktree, GitWorktree, TestCommand, TestRunArtifact,
    TestRunStatus, Worktree, WorktreeManager, agentmux_branch_name,
};
