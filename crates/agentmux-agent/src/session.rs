//! `AgentSession` domain struct.
//!
//! See `docs/spec/03_domain_model.md §5`.
//!
//! #TODO(agent): implement full AgentSession with in-memory state management

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use agentmux_core::{
    AgentMode, AgentProvider, AgentRole, AgentSessionId, AgentStatus, ContextScopeId, DateTimeUtc,
    InboxId, PaneId, ProjectId, PtyId, TaskId, TerminalBufferId, WorktreeId, ids::ClientId,
};
use serde::{Deserialize, Serialize};

use crate::capabilities::AgentCapabilities;

/// Represents a live (or recently-exited) agent process managed by agentmux.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSession {
    pub id: AgentSessionId,
    pub project_id: ProjectId,
    pub task_id: Option<TaskId>,
    pub name: String,
    pub provider: AgentProvider,
    pub role: AgentRole,
    pub mode: AgentMode,
    pub pty_id: PtyId,
    pub process_id: Option<u32>,
    pub pane_id: Option<PaneId>,
    pub terminal_buffer_id: TerminalBufferId,
    pub worktree_id: Option<WorktreeId>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub status: AgentStatus,
    pub capabilities: AgentCapabilities,
    pub inbox_id: InboxId,
    pub context_scope_id: ContextScopeId,
    pub created_at: DateTimeUtc,
    pub last_activity_at: DateTimeUtc,
    pub exited_at: Option<DateTimeUtc>,
}

/// Current owner of a pane's input stream.
///
/// Automatic injection must acquire this lock before writing to the PTY. The
/// TTL prevents a crashed automation path from blocking future human input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct InputLock {
    pub owner: Option<InputOwner>,
    pub acquired_at: Option<DateTimeUtc>,
    pub expires_at: Option<DateTimeUtc>,
}

impl InputLock {
    pub const fn unlocked() -> Self {
        Self {
            owner: None,
            acquired_at: None,
            expires_at: None,
        }
    }

    pub fn is_available_at(&self, now: DateTimeUtc) -> bool {
        match self.expires_at {
            Some(expires_at) if expires_at <= now => true,
            _ => self.owner.is_none(),
        }
    }

    pub fn acquire(
        &mut self,
        owner: InputOwner,
        now: DateTimeUtc,
        ttl: Duration,
    ) -> Result<(), InputLockError> {
        self.expire_if_needed(now);
        if self.owner.is_some() {
            return Err(InputLockError::AlreadyHeld);
        }

        self.owner = Some(owner);
        self.acquired_at = Some(now);
        self.expires_at = Some(add_std_duration(now, ttl));
        Ok(())
    }

    pub fn release(&mut self, owner: &InputOwner) -> Result<(), InputLockError> {
        match &self.owner {
            Some(current_owner) if current_owner == owner => {
                *self = Self::unlocked();
                Ok(())
            }
            Some(_) => Err(InputLockError::OwnedByAnother),
            None => Ok(()),
        }
    }

    pub fn expire_if_needed(&mut self, now: DateTimeUtc) {
        if matches!(self.expires_at, Some(expires_at) if expires_at <= now) {
            *self = Self::unlocked();
        }
    }
}

impl Default for InputLock {
    fn default() -> Self {
        Self::unlocked()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputOwner {
    HumanClient(ClientId),
    Orchestrator,
    MessageBus,
    RecoveryAgent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputLockError {
    AlreadyHeld,
    OwnedByAnother,
}

/// Human and PTY activity timestamps used by precondition checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputActivity {
    pub last_human_input_at: Option<DateTimeUtc>,
    pub last_pty_output_at: Option<DateTimeUtc>,
}

impl InputActivity {
    pub const fn new() -> Self {
        Self {
            last_human_input_at: None,
            last_pty_output_at: None,
        }
    }

    pub fn record_human_input(&mut self, at: DateTimeUtc) {
        self.last_human_input_at = Some(at);
    }

    pub fn record_pty_output(&mut self, at: DateTimeUtc) {
        self.last_pty_output_at = Some(at);
    }

    pub fn quiet_for_at(&self, now: DateTimeUtc, quiet_period: Duration) -> bool {
        self.last_human_input_at
            .and_then(|last| elapsed_since(last, now))
            .is_none_or(|elapsed| elapsed >= quiet_period)
    }
}

impl Default for InputActivity {
    fn default() -> Self {
        Self::new()
    }
}

fn add_std_duration(at: DateTimeUtc, duration: Duration) -> DateTimeUtc {
    at + time::Duration::try_from(duration).unwrap_or(time::Duration::MAX)
}

fn elapsed_since(earlier: DateTimeUtc, later: DateTimeUtc) -> Option<Duration> {
    let nanos = later.unix_timestamp_nanos() - earlier.unix_timestamp_nanos();
    if nanos < 0 {
        return None;
    }

    u64::try_from(nanos).ok().map(Duration::from_nanos)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTimeUtc {
        DateTimeUtc::UNIX_EPOCH + time::Duration::seconds(100)
    }

    #[test]
    fn lock_acquire_release_and_ttl_expiry() {
        let mut lock = InputLock::default();
        let owner = InputOwner::Orchestrator;
        let acquired_at = now();

        lock.acquire(owner.clone(), acquired_at, Duration::from_secs(5))
            .expect("acquire lock");

        assert!(!lock.is_available_at(acquired_at + time::Duration::seconds(4)));
        assert!(lock.is_available_at(acquired_at + time::Duration::seconds(5)));

        lock.expire_if_needed(acquired_at + time::Duration::seconds(5));
        assert_eq!(lock.owner, None);

        lock.acquire(owner.clone(), acquired_at, Duration::from_secs(5))
            .expect("re-acquire lock");
        lock.release(&owner).expect("release lock");
        assert!(lock.is_available_at(acquired_at));
    }

    #[test]
    fn lock_rejects_second_owner_before_ttl() {
        let mut lock = InputLock::default();
        lock.acquire(InputOwner::Orchestrator, now(), Duration::from_secs(5))
            .expect("acquire lock");

        let error = lock
            .acquire(InputOwner::MessageBus, now(), Duration::from_secs(5))
            .expect_err("second owner should fail");

        assert_eq!(error, InputLockError::AlreadyHeld);
    }

    #[test]
    fn activity_is_not_quiet_until_human_input_ages_out() {
        let mut activity = InputActivity::new();
        let base = now();

        assert!(activity.quiet_for_at(base, Duration::from_secs(2)));

        activity.record_human_input(base);

        assert!(!activity.quiet_for_at(base + time::Duration::seconds(1), Duration::from_secs(2)));
        assert!(activity.quiet_for_at(base + time::Duration::seconds(2), Duration::from_secs(2)));
    }
}
