//! Core domain enumerations shared across all crates.
//!
//! Keep variants in sync with `docs/spec/03_domain_model.md`.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Agent
// ---------------------------------------------------------------------------

/// Which AI/shell provider backs an `AgentSession`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentProvider {
    ClaudeCode,
    Codex,
    Shell,
    Custom(String),
}

/// Functional role the agent plays within a task team.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    Planner,
    Implementer,
    Reviewer,
    Tester,
    Debugger,
    Refactorer,
    SecurityReviewer,
    DocsWriter,
    Integrator,
    ContextManager,
    Custom(String),
}

/// Lifecycle state of a running agent session as detected by agentmux.
///
/// State transitions are driven by `StateSignal` priority order:
/// `HumanOverride > ExplicitMarker > HookEvent > Process >
///  FileSystemEvent > PtyActivity > ScreenPattern`
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Starting,
    InteractiveReady,
    RunningTurn,
    RunningCommand,
    AwaitingInput,
    AwaitingApproval,
    NeedsHuman,
    Blocked,
    CompletedTurn,
    Stalled,
    Exited,
    Failed,
}

/// How the agent process was launched (interactive TUI vs plain shell).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMode {
    InteractiveTui,
    InteractiveShell,
}

// ---------------------------------------------------------------------------
// Task
// ---------------------------------------------------------------------------

/// High-level lifecycle state of a `Task`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Created,
    Starting,
    Running,
    WaitingForHuman,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

// ---------------------------------------------------------------------------
// Worktree
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeStatus {
    Creating,
    Ready,
    Dirty,
    Testing,
    ReviewReady,
    Promoted,
    Archived,
    Failed,
}

// ---------------------------------------------------------------------------
// Approval
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalKind {
    AutoInput,
    FileWrite,
    ShellCommand,
    GitCommit,
    GitPush,
    NetworkAccess,
    SecretAccess,
    FullAccess,
    ExternalTool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Rejected,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

// ---------------------------------------------------------------------------
// Policy / automation
// ---------------------------------------------------------------------------

/// Governs how agentmux handles automated actions (write, command, push, …).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationLevel {
    /// Agent processes may be observed, but agentmux sends no automated input.
    ObserveOnly,
    /// Prompt, message, and context handoff injection is allowed.
    AutoPrompt,
    /// Safe read-only and configured test commands may be approved.
    AutoPromptAndApproveSafe,
    /// Workspace-local writes and tests may be approved under policy.
    AutoWorkspaceWrite,
    /// Full access is explicit and discouraged for v0.1.
    AutoFullAccess,
}

// ---------------------------------------------------------------------------
// Background job
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

// ---------------------------------------------------------------------------
// Pane / TUI
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InternalView {
    MessageBus,
    ActivityFeed,
    ContextBoard,
    ApprovalQueue,
    TaskTimeline,
    WorktreeDiff,
    TestResults,
    AgentList,
    Help,
}

// ---------------------------------------------------------------------------
// Artifact
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    TestLog,
    DiffPatch,
    ScreenSnapshot,
    Transcript,
    AgentResult,
    ContextBundle,
    FileList,
    CommandOutput,
}

// ---------------------------------------------------------------------------
// Priority & delivery
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    Normal,
    High,
    Urgent,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMode {
    InboxOnly,
    InjectWhenIdle,
    InjectImmediately,
    RequireHumanApproval,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    Queued,
    Rendered,
    WaitingForAgent,
    WaitingForApproval,
    Injecting,
    Delivered,
    Failed,
    Cancelled,
}

// ---------------------------------------------------------------------------
// Context
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextScope {
    /// Visible to all agents on the project.
    Project,
    /// Scoped to a specific task and its agents.
    Task,
    /// Private to a single agent session.
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextSource {
    Human,
    Agent(crate::ids::AgentSessionId),
    System,
    Import,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Public,
    Internal,
    Restricted,
}

// ---------------------------------------------------------------------------
// Context kind
// ---------------------------------------------------------------------------

/// Semantic category of a `ContextItem`.
///
/// See `docs/spec/03_domain_model.md §9`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextKind {
    ProjectSummary,
    ArchitectureNote,
    CodingRule,
    TaskBrief,
    FileReference,
    DiffSummary,
    TestResult,
    ErrorLog,
    AgentFinding,
    Decision,
    Risk,
    OpenQuestion,
    HandoffSummary,
}

// ---------------------------------------------------------------------------
// State signal
// ---------------------------------------------------------------------------

/// Source of a `StateSignal`; used to resolve conflicting signals by priority.
///
/// Priority (highest → lowest):
/// `HumanOverride > ExplicitMarker > HookEvent > Process >
///  FileSystemEvent > PtyActivity > ScreenPattern`
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateSignalSource {
    ScreenPattern,
    PtyActivity,
    FileSystemEvent,
    Process,
    HookEvent,
    ExplicitMarker,
    HumanOverride,
}
