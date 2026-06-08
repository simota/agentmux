//! `AgentSession` domain struct.
//!
//! See `docs/spec/03_domain_model.md §5`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use agentmux_core::{
    AgentMode, AgentProvider, AgentRole, AgentSessionId, AgentStatus, ContextScopeId, DateTimeUtc,
    InboxId, PaneId, ProjectId, PtyId, StateSignalSource, TaskId, TerminalBufferId, WorktreeId,
    error::Result, ids::ClientId,
};
use serde::{Deserialize, Serialize};

use crate::capabilities::AgentCapabilities;
use crate::signal::StateSignal;

/// How long a winning `StateSignal` suppresses strictly-lower-priority
/// signals before they are allowed to override it. Keeps a `HumanOverride`
/// or `ExplicitMarker` verdict from being clobbered by a stray low-trust
/// `ScreenPattern`/`PtyActivity` signal, while still letting state recover
/// once the high-priority evidence is stale.
///
/// Currently a fixed default; a future change may surface this through
/// project policy configuration.
const STATE_SIGNAL_PRIORITY_TTL: Duration = Duration::from_secs(30);

/// Represents a live (or recently-exited) agent process managed by agentmux.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    pub input_lock: InputLock,
    pub inbox_id: InboxId,
    pub context_scope_id: ContextScopeId,
    pub created_at: DateTimeUtc,
    pub last_activity_at: DateTimeUtc,
    pub exited_at: Option<DateTimeUtc>,
    /// Source and observation time of the last `StateSignal` that won and set
    /// `status`. Used to enforce the `StateSignalSource` priority ordering: a
    /// lower-priority signal cannot override a higher-priority verdict until
    /// that verdict goes stale. See `apply_state_signal`.
    pub last_signal_source: Option<StateSignalSource>,
    pub last_signal_at: Option<DateTimeUtc>,
}

impl AgentSession {
    pub fn new(init: AgentSessionInit, now: DateTimeUtc) -> Self {
        Self {
            id: init.id.unwrap_or_default(),
            project_id: init.project_id,
            task_id: init.task_id,
            name: init.name,
            provider: init.provider,
            role: init.role,
            mode: init.mode,
            pty_id: init.pty_id,
            process_id: init.process_id,
            pane_id: init.pane_id,
            terminal_buffer_id: init.terminal_buffer_id.unwrap_or_default(),
            worktree_id: init.worktree_id,
            cwd: init.cwd,
            env: init.env,
            status: AgentStatus::Starting,
            capabilities: init.capabilities,
            input_lock: InputLock::default(),
            inbox_id: init.inbox_id.unwrap_or_default(),
            context_scope_id: init.context_scope_id.unwrap_or_default(),
            created_at: now,
            last_activity_at: now,
            exited_at: None,
            last_signal_source: None,
            last_signal_at: None,
        }
    }

    pub fn transition_status(&mut self, next: AgentStatus, now: DateTimeUtc) -> Result<()> {
        if !is_allowed_transition(&self.status, &next) {
            return Err(agentmux_core::AgentmuxError::UserError(format!(
                "invalid agent status transition: {:?} -> {:?}",
                self.status, next
            )));
        }

        self.status = next;
        self.last_activity_at = now;
        if matches!(self.status, AgentStatus::Exited | AgentStatus::Failed) {
            self.exited_at = Some(now);
            self.input_lock = InputLock::unlocked();
        }
        Ok(())
    }

    pub fn apply_state_signal(&mut self, signal: &StateSignal) -> Result<()> {
        if signal.agent_id != self.id {
            return Err(agentmux_core::AgentmuxError::UserError(format!(
                "state signal for '{}' cannot be applied to '{}'",
                signal.agent_id, self.id
            )));
        }

        // Enforce the StateSignalSource priority ordering
        // (HumanOverride > … > ScreenPattern). A signal whose source outranks
        // or ties the last winning signal always applies; a lower-priority
        // signal is ignored until the prior verdict goes stale.
        if !self.signal_should_apply(signal) {
            return Ok(());
        }

        // A signal that passed priority/staleness gating but is not a legal FSM
        // transition from the current status is a silent no-op, not a hard
        // error. Otherwise a stale lower-priority signal admitted via the TTL
        // path would `?`-propagate an `Err` every poll without updating
        // `last_signal_*`, re-failing forever (a permanent error loop). We do
        // not weaken `transition_status` itself — other callers still rely on
        // its error contract. Leaving `last_signal_*` untouched is correct: a
        // no-op signal did not win, so it must not reset the TTL window.
        if !is_allowed_transition(&self.status, &signal.value) {
            return Ok(());
        }

        self.transition_status(signal.value.clone(), signal.observed_at)?;
        self.last_signal_source = Some(signal.source.clone());
        self.last_signal_at = Some(signal.observed_at);
        Ok(())
    }

    fn signal_should_apply(&self, signal: &StateSignal) -> bool {
        match (&self.last_signal_source, self.last_signal_at) {
            (Some(last_source), Some(last_at)) => {
                // `StateSignalSource` derives `Ord` with ScreenPattern lowest
                // and HumanOverride highest, so `>=` is "at least as trusted".
                signal.source >= *last_source
                    || add_std_duration(last_at, STATE_SIGNAL_PRIORITY_TTL) <= signal.observed_at
            }
            _ => true,
        }
    }

    pub fn record_human_input(&mut self, at: DateTimeUtc) {
        self.last_activity_at = at;
    }

    pub fn record_pty_output(&mut self, at: DateTimeUtc) {
        self.last_activity_at = at;
    }

    pub fn set_process_id(&mut self, process_id: u32, at: DateTimeUtc) {
        self.process_id = Some(process_id);
        self.last_activity_at = at;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSessionInit {
    pub id: Option<AgentSessionId>,
    pub project_id: ProjectId,
    pub task_id: Option<TaskId>,
    pub name: String,
    pub provider: AgentProvider,
    pub role: AgentRole,
    pub mode: AgentMode,
    pub pty_id: PtyId,
    pub process_id: Option<u32>,
    pub pane_id: Option<PaneId>,
    pub terminal_buffer_id: Option<TerminalBufferId>,
    pub worktree_id: Option<WorktreeId>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub capabilities: AgentCapabilities,
    pub inbox_id: Option<InboxId>,
    pub context_scope_id: Option<ContextScopeId>,
}

fn is_allowed_transition(current: &AgentStatus, next: &AgentStatus) -> bool {
    if current == next {
        return true;
    }

    if matches!(current, AgentStatus::Exited | AgentStatus::Failed) {
        return false;
    }

    if matches!(
        next,
        AgentStatus::NeedsHuman | AgentStatus::Stalled | AgentStatus::Exited | AgentStatus::Failed
    ) {
        return true;
    }

    matches!(
        (current, next),
        (AgentStatus::Starting, AgentStatus::InteractiveReady)
            | (AgentStatus::InteractiveReady, AgentStatus::AwaitingInput)
            | (AgentStatus::AwaitingInput, AgentStatus::RunningTurn)
            | (AgentStatus::RunningTurn, AgentStatus::RunningCommand)
            | (AgentStatus::RunningCommand, AgentStatus::AwaitingApproval)
            | (AgentStatus::AwaitingApproval, AgentStatus::RunningCommand)
            | (AgentStatus::RunningCommand, AgentStatus::CompletedTurn)
            | (AgentStatus::RunningTurn, AgentStatus::CompletedTurn)
            | (AgentStatus::CompletedTurn, AgentStatus::AwaitingInput)
            | (AgentStatus::Stalled, AgentStatus::AwaitingInput)
            | (AgentStatus::NeedsHuman, AgentStatus::AwaitingInput)
    )
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
    use agentmux_core::StateSignalSource;

    fn now() -> DateTimeUtc {
        DateTimeUtc::UNIX_EPOCH + time::Duration::seconds(100)
    }

    fn session_init() -> AgentSessionInit {
        AgentSessionInit {
            id: Some(AgentSessionId::new()),
            project_id: ProjectId::new(),
            task_id: Some(TaskId::new()),
            name: "impl-codex".to_string(),
            provider: AgentProvider::Codex,
            role: AgentRole::Implementer,
            mode: AgentMode::InteractiveTui,
            pty_id: PtyId::new(),
            process_id: None,
            pane_id: Some(PaneId::new()),
            terminal_buffer_id: Some(TerminalBufferId::new()),
            worktree_id: Some(WorktreeId::new()),
            cwd: PathBuf::from("/tmp/project"),
            env: BTreeMap::from([("TERM".to_string(), "xterm-256color".to_string())]),
            capabilities: AgentCapabilities::codex(),
            inbox_id: Some(InboxId::new()),
            context_scope_id: Some(ContextScopeId::new()),
        }
    }

    #[test]
    fn agent_session_initializes_in_memory_state() {
        let created_at = now();
        let init = session_init();
        let expected_id = init.id.clone().expect("test id");

        let session = AgentSession::new(init, created_at);

        assert_eq!(session.id, expected_id);
        assert_eq!(session.status, AgentStatus::Starting);
        assert_eq!(session.input_lock, InputLock::unlocked());
        assert_eq!(session.created_at, created_at);
        assert_eq!(session.last_activity_at, created_at);
        assert_eq!(session.exited_at, None);
    }

    #[test]
    fn agent_session_applies_spec_status_transitions() {
        let mut session = AgentSession::new(session_init(), now());
        let ready_at = now() + time::Duration::seconds(1);

        session
            .transition_status(AgentStatus::InteractiveReady, ready_at)
            .expect("starting -> ready");
        session
            .transition_status(
                AgentStatus::AwaitingInput,
                ready_at + time::Duration::seconds(1),
            )
            .expect("ready -> awaiting input");
        session
            .transition_status(
                AgentStatus::RunningTurn,
                ready_at + time::Duration::seconds(2),
            )
            .expect("awaiting input -> running turn");
        session
            .transition_status(
                AgentStatus::CompletedTurn,
                ready_at + time::Duration::seconds(3),
            )
            .expect("running turn -> completed turn");

        assert_eq!(session.status, AgentStatus::CompletedTurn);
        assert_eq!(
            session.last_activity_at,
            ready_at + time::Duration::seconds(3)
        );
    }

    fn signal_at(
        session_id: &AgentSessionId,
        source: StateSignalSource,
        value: AgentStatus,
        observed_at: DateTimeUtc,
    ) -> StateSignal {
        StateSignal {
            agent_id: session_id.clone(),
            source,
            confidence: 1.0,
            value,
            evidence: "test".to_string(),
            observed_at,
        }
    }

    #[test]
    fn state_signal_priority_and_staleness_are_enforced() {
        let mut session = AgentSession::new(session_init(), now());
        let id = session.id.clone();
        let t0 = now();

        // High-priority human verdict wins from the initial state.
        session
            .apply_state_signal(&signal_at(
                &id,
                StateSignalSource::HumanOverride,
                AgentStatus::NeedsHuman,
                t0,
            ))
            .unwrap();
        assert_eq!(session.status, AgentStatus::NeedsHuman);

        // Lower-priority screen-scrape within the TTL is ignored, even though
        // NeedsHuman -> AwaitingInput is itself a legal transition.
        session
            .apply_state_signal(&signal_at(
                &id,
                StateSignalSource::ScreenPattern,
                AgentStatus::AwaitingInput,
                t0 + time::Duration::seconds(5),
            ))
            .unwrap();
        assert_eq!(session.status, AgentStatus::NeedsHuman);

        // Once the human verdict is stale, the lower-priority signal takes over.
        session
            .apply_state_signal(&signal_at(
                &id,
                StateSignalSource::ScreenPattern,
                AgentStatus::AwaitingInput,
                t0 + time::Duration::seconds(31),
            ))
            .unwrap();
        assert_eq!(session.status, AgentStatus::AwaitingInput);

        // A higher-priority signal always wins regardless of staleness.
        session
            .apply_state_signal(&signal_at(
                &id,
                StateSignalSource::HumanOverride,
                AgentStatus::NeedsHuman,
                t0 + time::Duration::seconds(32),
            ))
            .unwrap();
        assert_eq!(session.status, AgentStatus::NeedsHuman);
    }

    #[test]
    fn ttl_admitted_signal_with_illegal_transition_is_silent_no_op() {
        let mut session = AgentSession::new(session_init(), now());
        let id = session.id.clone();
        let t0 = now();

        // A high-priority verdict wins and parks the FSM in a terminal-ish
        // state. From CompletedTurn the only legal move is AwaitingInput.
        session
            .transition_status(AgentStatus::InteractiveReady, t0)
            .unwrap();
        session
            .apply_state_signal(&signal_at(
                &id,
                StateSignalSource::HumanOverride,
                AgentStatus::AwaitingInput,
                t0 + time::Duration::seconds(1),
            ))
            .unwrap();
        session
            .transition_status(
                AgentStatus::RunningTurn,
                t0 + time::Duration::seconds(2),
            )
            .unwrap();
        session
            .transition_status(
                AgentStatus::CompletedTurn,
                t0 + time::Duration::seconds(3),
            )
            .unwrap();
        // Re-stamp the winning verdict so the TTL window is anchored here.
        session
            .apply_state_signal(&signal_at(
                &id,
                StateSignalSource::HumanOverride,
                AgentStatus::CompletedTurn,
                t0 + time::Duration::seconds(3),
            ))
            .unwrap();
        assert_eq!(session.status, AgentStatus::CompletedTurn);

        // After the TTL elapses a lower-priority signal is admitted by the
        // staleness path, but RunningTurn is NOT a legal move from
        // CompletedTurn. This must be a no-op, NOT an error, and applying it
        // repeatedly must not loop or change anything.
        let stale_signal = signal_at(
            &id,
            StateSignalSource::ScreenPattern,
            AgentStatus::RunningTurn,
            t0 + time::Duration::seconds(40),
        );
        for _ in 0..3 {
            session
                .apply_state_signal(&stale_signal)
                .expect("illegal-but-admitted signal is a no-op, never an error");
            assert_eq!(session.status, AgentStatus::CompletedTurn);
        }

        // The winning verdict's bookkeeping is untouched, so a later legal
        // lower-priority signal still applies once stale.
        assert_eq!(
            session.last_signal_source,
            Some(StateSignalSource::HumanOverride)
        );
        session
            .apply_state_signal(&signal_at(
                &id,
                StateSignalSource::ScreenPattern,
                AgentStatus::AwaitingInput,
                t0 + time::Duration::seconds(41),
            ))
            .expect("legal stale signal applies");
        assert_eq!(session.status, AgentStatus::AwaitingInput);
    }

    #[test]
    fn agent_session_rejects_invalid_forward_jump() {
        let mut session = AgentSession::new(session_init(), now());

        let error = session
            .transition_status(AgentStatus::RunningCommand, now())
            .expect_err("starting cannot skip to running command");

        assert!(
            error
                .to_string()
                .contains("invalid agent status transition")
        );
        assert_eq!(session.status, AgentStatus::Starting);
    }

    #[test]
    fn agent_session_terminal_status_releases_input_lock_and_is_final() {
        let mut session = AgentSession::new(session_init(), now());
        session
            .input_lock
            .acquire(InputOwner::Orchestrator, now(), Duration::from_secs(30))
            .expect("lock acquired");

        session
            .transition_status(AgentStatus::Exited, now() + time::Duration::seconds(1))
            .expect("any non-terminal can exit");

        assert_eq!(session.input_lock, InputLock::unlocked());
        assert_eq!(session.exited_at, Some(now() + time::Duration::seconds(1)));
        assert!(
            session
                .transition_status(
                    AgentStatus::AwaitingInput,
                    now() + time::Duration::seconds(2)
                )
                .is_err()
        );
    }

    #[test]
    fn agent_session_applies_matching_state_signal_only() {
        let mut session = AgentSession::new(session_init(), now());
        let signal = StateSignal {
            agent_id: session.id.clone(),
            source: StateSignalSource::Process,
            confidence: 1.0,
            value: AgentStatus::Failed,
            evidence: "process exited non-zero".to_string(),
            observed_at: now() + time::Duration::seconds(5),
        };

        session
            .apply_state_signal(&signal)
            .expect("matching signal applies");

        assert_eq!(session.status, AgentStatus::Failed);
        assert_eq!(session.exited_at, Some(signal.observed_at));

        let mut other = AgentSession::new(session_init(), now());
        let error = other
            .apply_state_signal(&signal)
            .expect_err("signal belongs to a different session");
        assert!(error.to_string().contains("cannot be applied"));
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
