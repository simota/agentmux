use crate::*;

pub(crate) struct LiveAgentSession {
    pub(crate) metadata: RegisteredAgentSession,
    pub(crate) worktree_id: Option<WorktreeId>,
    pub(crate) pty: Option<Mutex<PtyHandle>>,
    pub(crate) terminal: Arc<Mutex<TerminalParser>>,
    /// Per-pane human/PTY activity timestamps. `last_human_input_at` is updated
    /// only by genuine human key-forwarding (`AgentSendInputScript` ->
    /// `send_input_script`), never by automated message injection, so the
    /// auto-injection quiet-window check reflects real keystrokes into this pane.
    pub(crate) input_activity: InputActivity,
}

#[derive(Debug, Clone)]
pub(crate) struct ArenaCandidate {
    pub(crate) worktree_id: WorktreeId,
    pub(crate) agent_id: AgentSessionId,
    pub(crate) provider: String,
    pub(crate) diff_stat: Option<String>,
    pub(crate) test_status: Option<TestRunStatus>,
}

/// Timing and size knobs that drive message injection into agent PTYs.
///
/// Sourced from `AutomationConfig` (+ `ContextConfig.max_inline_chars`) so the
/// previously hardcoded constants can be tuned per project. The `Default` impl
/// reproduces the historical production values; tests construct a zero-delay
/// variant via [`InjectionTiming::test`] so injection runs instantly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InjectionTiming {
    /// Settle delay before writing an injected message into the target PTY.
    pub(crate) send_delay: Duration,
    /// Delay between the bracketed-paste body and the trailing Enter.
    pub(crate) paste_enter_delay: Duration,
    /// Upper bound on the PTY tail scanned for `AGENTMUX_RESULT` markers.
    pub(crate) result_detection_tail_bytes: usize,
    /// Inline-context budget used when rendering an injected message prompt.
    pub(crate) max_inline_chars: usize,
    /// Quiet window after a human keystroke during which automated injection
    /// into that pane must be deferred. Enforces the spec invariant "do not
    /// auto-inject into a pane where a human is currently typing"
    /// (`automation.human_input_quiet_ms`).
    pub(crate) human_input_quiet: Duration,
}

impl InjectionTiming {
    /// Build timing from project config (`[automation]` + `[context]`).
    pub(crate) fn from_config(automation: &AutomationConfig, context: &ContextConfig) -> Self {
        Self {
            send_delay: Duration::from_millis(automation.message_inject_send_delay_ms),
            paste_enter_delay: Duration::from_millis(automation.message_paste_enter_delay_ms),
            result_detection_tail_bytes: automation.result_detection_tail_bytes,
            max_inline_chars: context.max_inline_chars,
            human_input_quiet: Duration::from_millis(automation.human_input_quiet_ms),
        }
    }

    /// Zero-delay variant used by tests so injection completes instantly while
    /// preserving byte ordering and size limits.
    #[cfg(test)]
    pub(crate) fn test() -> Self {
        Self {
            send_delay: Duration::ZERO,
            paste_enter_delay: Duration::ZERO,
            ..Self::default()
        }
    }
}

impl Default for InjectionTiming {
    fn default() -> Self {
        Self {
            send_delay: Duration::from_millis(DEFAULT_MESSAGE_INJECT_SEND_DELAY_MS),
            paste_enter_delay: Duration::from_millis(DEFAULT_MESSAGE_PASTE_ENTER_DELAY_MS),
            result_detection_tail_bytes: DEFAULT_RESULT_DETECTION_TAIL_BYTES,
            max_inline_chars: DEFAULT_MAX_INLINE_CHARS,
            human_input_quiet: Duration::from_millis(DEFAULT_HUMAN_INPUT_QUIET_MS),
        }
    }
}

/// Fallback human-typing quiet window when no `[automation]` config is wired in.
/// Matches the documented example (`docs/config/agentmux.config.example.toml`).
pub(crate) const DEFAULT_HUMAN_INPUT_QUIET_MS: u64 = 2500;

/// Fallback inline-context budget when no `[context]` config is wired in.
pub(crate) const DEFAULT_MAX_INLINE_CHARS: usize = 2048;

pub(crate) struct DaemonState {
    pub(crate) clients: BTreeMap<ClientSessionId, Option<AgentSessionId>>,
    pub(crate) agents: BTreeMap<AgentSessionId, LiveAgentSession>,
    pub(crate) worktrees: BTreeMap<WorktreeId, Worktree>,
    pub(crate) worktree_repo_roots: BTreeMap<WorktreeId, PathBuf>,
    pub(crate) arena_candidates: BTreeMap<WorktreeId, ArenaCandidate>,
    pub(crate) messages: MessageBus,
    pub(crate) contexts: ContextBroker,
    pub(crate) approvals: ApprovalQueue,
    pub(crate) layout_presets: BTreeMap<String, serde_json::Value>,
    pub(crate) default_project_id: ProjectId,
    pub(crate) injection_timing: InjectionTiming,
    /// Policy engine derived from `[policy]` + `[automation]` config. Gates
    /// command execution (`WorktreeTest`) and file writes against the configured
    /// automation level and `protected_paths`. The `Default` impl reproduces the
    /// spec defaults (`AutoPrompt` + `ApprovalPolicy::default()`); real config
    /// overrides it via `DaemonRuntime::with_policy_engine`.
    pub(crate) policy: PolicyEngine,
}

impl Default for DaemonState {
    fn default() -> Self {
        // Tests rely on instant injection; production uses the historical
        // 5s settle / 120ms paste-enter timing. Real config can override the
        // timing via `DaemonRuntime::with_injection_config`.
        #[cfg(test)]
        let injection_timing = InjectionTiming::test();
        #[cfg(not(test))]
        let injection_timing = InjectionTiming::default();

        Self {
            clients: BTreeMap::new(),
            agents: BTreeMap::new(),
            worktrees: BTreeMap::new(),
            worktree_repo_roots: BTreeMap::new(),
            arena_candidates: BTreeMap::new(),
            messages: MessageBus::new(),
            contexts: ContextBroker::new(),
            approvals: ApprovalQueue::new(),
            layout_presets: BTreeMap::new(),
            default_project_id: ProjectId::new(),
            injection_timing,
            policy: PolicyEngine::with_policy(
                AutomationLevel::AutoPrompt,
                agentmux_policy::ApprovalPolicy::default(),
            ),
        }
    }
}
impl LiveAgentSession {
    pub(crate) fn metadata(metadata: RegisteredAgentSession) -> Self {
        Self {
            metadata,
            worktree_id: None,
            pty: None,
            terminal: Arc::new(Mutex::new(TerminalParser::default())),
            input_activity: InputActivity::new(),
        }
    }
}
/// Outcome of attempting to persist a live `AGENTMUX_RESULT` from a PTY tail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LiveResultOutcome {
    /// A new, distinct result was parsed and persisted to the bus.
    Persisted,
    /// No marker present in the tail yet.
    NotFound,
    /// The same result content was already persisted (drip/repaint); skipped.
    Duplicate,
    /// A marker was present but its JSON could not be parsed; carries the reason
    /// so the caller can decide whether to surface a (deduplicated) probe event.
    NeedsProbe { reason: String },
}

/// Bounded LRU ring of recently persisted result content hashes.
///
/// A drip-rendering TUI re-emits the same marker block on every repaint, so the
/// forwarder must skip already-seen content while still admitting genuinely new
/// results across multiple turns.
pub(crate) struct SeenResultHashes {
    ring: std::collections::VecDeque<u64>,
    capacity: usize,
}

impl SeenResultHashes {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            ring: std::collections::VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub(crate) fn contains(&self, hash: u64) -> bool {
        self.ring.contains(&hash)
    }

    pub(crate) fn record(&mut self, hash: u64) {
        if self.ring.len() == self.capacity {
            self.ring.pop_front();
        }
        self.ring.push_back(hash);
    }
}

/// Input for `meeting.open`: topic + participant session names/ids.
pub struct OpenMeetingInput {
    pub topic: String,
    pub participants: Vec<String>,
    pub opened_by: MessageSource,
    pub max_messages_per_participant: Option<u32>,
    pub kind: MessageKind,
    pub priority: Priority,
    pub body: Option<String>,
}

pub(crate) enum ContextLookup {
    List,
    Show(ContextItemId),
    Search(String),
}
