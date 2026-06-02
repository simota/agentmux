//! `agentmux-worktree` — Git worktree lifecycle management.
//!
//! Responsibilities (see `docs/spec/02_system_architecture.md §4.9`):
//! - create and remove git worktrees (`git worktree add / remove`)
//! - branch naming conventions (e.g. `agentmux/<task-id>/<role>`)
//! - capture diff artifacts (`git diff`) and store as `Artifact`
//! - test-target tracking (which worktree owns a test run)
//!
//! v0.1 shells out to `git` via `std::process::Command`.
//! No libgit2/gitoxide dependency required at this stage.
//!
//! #TODO(agent): implement WorktreeManager struct
//! #TODO(agent): implement create_worktree() shelling out to `git worktree add`
//! #TODO(agent): implement diff_artifact() shelling out to `git diff`
//! #TODO(agent): implement branch naming helper

pub mod worktree;

pub use worktree::Worktree;
