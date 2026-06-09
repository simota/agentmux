//! Clap command-line definitions for the `agentmux` CLI.

use std::path::PathBuf;

use agentmux_tui::state::AgentProviderChoice;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "agentmux",
    version,
    about = "Multi-agent coding cockpit for Claude Code, Codex, and shell agents",
    long_about = None,
)]
pub(crate) struct Cli {
    /// Override the daemon socket path (defaults to AGENTMUX_SOCKET or the runtime dir).
    #[arg(long, global = true)]
    pub(crate) socket: Option<PathBuf>,

    #[command(subcommand)]
    pub(crate) command: Option<Commands>,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
    /// Start the TUI, optionally spawning provider sessions first (e.g. "agy,codex").
    Start(StartArgs),

    /// Run environment diagnostics (socket / config / SQLite / claude / codex / PTY / worktree).
    Doctor(DoctorArgs),

    /// Manage the agentmux background daemon.
    Daemon(DaemonArgs),

    /// Manage projects (init / open / status).
    Project(ProjectArgs),

    /// Manage tasks (run / status / pause / resume / cancel / summary).
    Task(TaskArgs),

    /// Manage agent sessions (ls / spawn / stop / send / inject / focus / interrupt).
    Agent(AgentArgs),

    /// List running interactive sessions.
    Sessions(SessionsArgs),

    /// Manage messages (list / show / send / inject).
    Message(MessageArgs),

    /// Manage multi-party meeting threads (open / close / list).
    Meeting(MeetingArgs),

    /// Manage shared context items (add / list / show / search / attach / inject / export).
    Context(ContextArgs),

    /// Manage git worktrees (list / diff / test / promote / archive).
    Worktree(WorktreeArgs),

    /// Manage approval requests (list / approve / reject).
    Approval(ApprovalArgs),

    /// Attach (or re-attach) the TUI to an existing task or agent session.
    Attach(AttachArgs),

    /// Manage TUI layout presets (save / load / list).
    Layout(LayoutArgs),
}

// ---------------------------------------------------------------------------
// Per-subcommand arg structs (stubs — args will grow with implementation)
// ---------------------------------------------------------------------------

#[derive(Parser)]
pub(crate) struct StartArgs {
    /// Comma-separated panes to open before the TUI, e.g. "agy,codex,messages".
    pub(crate) providers: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StartupPaneChoice {
    Agent(AgentProviderChoice),
    Messages,
    Commands,
}

#[derive(Parser)]
pub(crate) struct DoctorArgs;

#[derive(Parser)]
pub(crate) struct DaemonArgs {
    #[command(subcommand)]
    pub(crate) action: DaemonAction,
}

#[derive(Subcommand)]
pub(crate) enum DaemonAction {
    /// Start the daemon in the background.
    Start,
    /// Stop the running daemon gracefully.
    Stop,
    /// Print daemon status and socket path.
    Status,
}

#[derive(Parser)]
pub(crate) struct ProjectArgs {
    #[command(subcommand)]
    pub(crate) action: ProjectAction,
}

#[derive(Subcommand)]
pub(crate) enum ProjectAction {
    /// Initialise a new `.agentmux/` directory in the current (or given) path.
    Init {
        #[arg(default_value = ".")]
        path: String,
    },
    /// Open an existing project by path.
    Open { path: String },
    /// Show current project status.
    Status,
    /// Add the AGENTMUX_RESULT protocol to local or global agent instruction files.
    InstallResultProtocol {
        /// Directory containing AGENTS.md / CLAUDE.md / GEMINI.md.
        #[arg(default_value = ".")]
        path: String,
        /// Install to global tool instruction files instead of a project directory.
        #[arg(long)]
        global: bool,
    },
}

#[derive(Parser)]
pub(crate) struct TaskArgs {
    #[command(subcommand)]
    pub(crate) action: TaskAction,
}

#[derive(Subcommand)]
pub(crate) enum TaskAction {
    /// Start a new task and attach the TUI.
    Run {
        /// Natural-language task description.
        description: String,
        /// Team template name (defined in `config.toml`).
        #[arg(long)]
        team: Option<String>,
        /// Comma-separated providers to run in isolated arena worktrees.
        #[cfg(feature = "arena")]
        #[arg(long)]
        arena: Option<String>,
        /// Base branch for arena worktrees.
        #[cfg(feature = "arena")]
        #[arg(long)]
        base_branch: Option<String>,
    },
    /// Show task status.
    Status { task_id: String },
    /// Pause a running task.
    Pause { task_id: String },
    /// Resume a paused task.
    Resume { task_id: String },
    /// Cancel a task.
    Cancel { task_id: String },
    /// Print a task summary.
    Summary { task_id: String },
}

#[derive(Parser)]
pub(crate) struct AgentArgs {
    #[command(subcommand)]
    pub(crate) action: AgentAction,
}

#[derive(Parser)]
pub(crate) struct SessionsArgs;

#[derive(Subcommand)]
pub(crate) enum AgentAction {
    /// List all agent sessions.
    Ls,
    /// Spawn a new agent session.
    Spawn {
        #[arg(long)]
        provider: String,
        #[arg(long)]
        role: String,
    },
    /// Stop an agent session.
    Stop { agent_id: String },
    /// Send a message to an agent.
    Send {
        /// Immediately inject the created message into the agent input. This is the default.
        #[arg(long)]
        inject: bool,
        /// Create the message without immediately injecting it.
        #[arg(long = "no-inject", conflicts_with = "inject")]
        no_inject: bool,
        agent_id: String,
        body: String,
    },
    /// Broadcast raw input to multiple agents at once (synchronize-panes).
    Broadcast {
        /// Target (broadcast, role:<role>, team:<team>, agent:<name|id>). Defaults to broadcast.
        #[arg(long, default_value = "broadcast")]
        to: String,
        /// Do not append a trailing Enter; the text is pasted without submitting.
        #[arg(long = "no-enter")]
        no_enter: bool,
        /// The text to inject into every resolved agent PTY.
        text: String,
    },
    /// Inject a queued message into an agent immediately.
    Inject {
        message_id: String,
        agent_id: String,
    },
    /// Focus the TUI pane on an agent.
    Focus { agent_id: String },
    /// Send an interrupt (Ctrl-C) to an agent.
    Interrupt { agent_id: String },
}

#[derive(Parser)]
pub(crate) struct MessageArgs {
    #[command(subcommand)]
    pub(crate) action: MessageAction,
}

#[derive(Subcommand)]
pub(crate) enum MessageAction {
    /// List messages.
    List,
    /// Show message history as a human-readable table.
    History {
        /// Maximum number of messages to print.
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Filter by task ID.
        #[arg(long)]
        task: Option<String>,
        /// Filter by meeting thread ID.
        #[arg(long)]
        thread: Option<String>,
        /// Filter by source or target agent/session/role label.
        #[arg(long)]
        agent: Option<String>,
        /// Filter by message kind, e.g. handoff or test_result.
        #[arg(long)]
        kind: Option<String>,
        /// Filter by delivery status, e.g. queued or delivered.
        #[arg(long)]
        status: Option<String>,
    },
    /// Show a message by ID.
    Show { message_id: String },
    /// Send a new message.
    Send {
        /// Immediately inject the created message into the resolved agent input. This is the default.
        #[arg(long)]
        inject: bool,
        /// Create the message without immediately injecting it.
        #[arg(long = "no-inject", conflicts_with = "inject")]
        no_inject: bool,
        /// Target (agent:<name>, role:<role>, team:<team>, thread:<id>, broadcast).
        /// Optional when --thread is given (the thread becomes the target).
        #[arg(long, required_unless_present = "thread")]
        to: Option<String>,
        /// Meeting thread to post into (delivers to all participants except you).
        #[arg(long)]
        thread: Option<String>,
        /// Message kind. One of: TaskAssignment, Question, Finding, PatchProposal,
        /// ReviewComment, TestResult, FailureReport, Decision, Handoff,
        /// ApprovalRequest, ContextUpdate, StatusProbe. Defaults to Handoff.
        #[arg(long)]
        kind: Option<String>,
        /// Message priority: low, normal, high, or urgent. Defaults to normal.
        #[arg(long)]
        priority: Option<String>,
        body: String,
    },
    /// Inject a message, bypassing delivery policy.
    Inject { message_id: String },
}

#[derive(Parser)]
pub(crate) struct MeetingArgs {
    #[command(subcommand)]
    pub(crate) action: MeetingAction,
}

#[derive(Subcommand)]
pub(crate) enum MeetingAction {
    /// Open a meeting thread and inject the agenda to every participant.
    Open {
        /// Agenda of the meeting (becomes the kickoff message).
        topic: String,
        /// Comma-separated participant session names or ids
        /// (e.g. "claude-a,codex-b,agy-c"; check with `agentmux sessions`).
        #[arg(long)]
        participants: String,
        /// Per-participant message limit (loop guard). Defaults to 5.
        #[arg(long = "max-turns")]
        max_turns: Option<u32>,
        /// Kickoff message kind. Defaults to Question.
        #[arg(long)]
        kind: Option<String>,
        /// Kickoff message priority: low, normal, high, or urgent. Defaults to normal.
        #[arg(long)]
        priority: Option<String>,
        /// Custom kickoff body. Defaults to a standard agenda prompt.
        #[arg(long)]
        body: Option<String>,
    },
    /// Close a meeting thread (further messages are rejected).
    Close { thread_id: String },
    /// List meeting threads.
    List,
}

#[derive(Parser)]
pub(crate) struct ContextArgs {
    #[command(subcommand)]
    pub(crate) action: ContextAction,
}

#[derive(Subcommand)]
pub(crate) enum ContextAction {
    /// Add a new context item.
    Add { title: String },
    /// List context items.
    List,
    /// Show a context item by ID.
    Show { context_id: String },
    /// Search context items by keyword.
    Search { query: String },
    /// Attach a context item to a message.
    Attach {
        context_id: String,
        message_id: String,
    },
    /// Inject a context item into an agent's next prompt.
    Inject {
        context_id: String,
        agent_id: String,
    },
    /// Export context items to a file.
    Export { output: String },
}

#[derive(Parser)]
pub(crate) struct WorktreeArgs {
    #[command(subcommand)]
    pub(crate) action: WorktreeAction,
}

#[derive(Subcommand)]
pub(crate) enum WorktreeAction {
    /// List worktrees.
    List,
    /// Show git diff for a worktree.
    Diff { worktree_id: String },
    /// Run tests in a worktree.
    Test { worktree_id: String },
    /// Promote a worktree branch (merge / PR).
    Promote { worktree_id: String },
    /// Archive (remove) a worktree.
    Archive { worktree_id: String },
}

#[derive(Parser)]
pub(crate) struct ApprovalArgs {
    #[command(subcommand)]
    pub(crate) action: ApprovalAction,
}

#[derive(Subcommand)]
pub(crate) enum ApprovalAction {
    /// List pending approval requests.
    List,
    /// Approve a request.
    Approve { approval_id: String },
    /// Reject a request.
    Reject { approval_id: String },
}

#[derive(Parser)]
pub(crate) struct AttachArgs {
    /// Task ID or agent session ID to attach to.
    pub(crate) target: String,
}

#[derive(Parser)]
pub(crate) struct LayoutArgs {
    #[command(subcommand)]
    pub(crate) action: LayoutAction,
}

#[derive(Subcommand)]
pub(crate) enum LayoutAction {
    /// Save the current TUI layout as a named preset.
    Save { name: String },
    /// Load a saved layout preset.
    Load { name: String },
    /// List saved layout presets.
    List,
}
