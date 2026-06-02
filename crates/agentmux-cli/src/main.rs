//! `agentmux` — CLI entry point.
//!
//! Top-level subcommands mirror `docs/spec/11_cli_tui_user_spec.md §2`.
//! Each subcommand module is a stub; full implementation follows the
//! Phase roadmap in `docs/spec/12_implementation_roadmap.md`.

use clap::{Parser, Subcommand};

// ---------------------------------------------------------------------------
// Top-level CLI
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(
    name = "agentmux",
    version,
    about = "Multi-agent coding cockpit for Claude Code, Codex, and shell agents",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
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

    /// Manage messages (list / show / send / inject).
    Message(MessageArgs),

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
struct DoctorArgs;

#[derive(Parser)]
struct DaemonArgs {
    #[command(subcommand)]
    action: DaemonAction,
}

#[derive(Subcommand)]
enum DaemonAction {
    /// Start the daemon in the background.
    Start,
    /// Stop the running daemon gracefully.
    Stop,
    /// Print daemon status and socket path.
    Status,
}

#[derive(Parser)]
struct ProjectArgs {
    #[command(subcommand)]
    action: ProjectAction,
}

#[derive(Subcommand)]
enum ProjectAction {
    /// Initialise a new `.agentmux/` directory in the current (or given) path.
    Init {
        #[arg(default_value = ".")]
        path: String,
    },
    /// Open an existing project by path.
    Open { path: String },
    /// Show current project status.
    Status,
}

#[derive(Parser)]
struct TaskArgs {
    #[command(subcommand)]
    action: TaskAction,
}

#[derive(Subcommand)]
enum TaskAction {
    /// Start a new task and attach the TUI.
    Run {
        /// Natural-language task description.
        description: String,
        /// Team template name (defined in `config.toml`).
        #[arg(long)]
        team: Option<String>,
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
struct AgentArgs {
    #[command(subcommand)]
    action: AgentAction,
}

#[derive(Subcommand)]
enum AgentAction {
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
    Send { agent_id: String, body: String },
    /// Inject a queued message into an agent immediately.
    Inject { message_id: String, agent_id: String },
    /// Focus the TUI pane on an agent.
    Focus { agent_id: String },
    /// Send an interrupt (Ctrl-C) to an agent.
    Interrupt { agent_id: String },
}

#[derive(Parser)]
struct MessageArgs {
    #[command(subcommand)]
    action: MessageAction,
}

#[derive(Subcommand)]
enum MessageAction {
    /// List messages.
    List,
    /// Show a message by ID.
    Show { message_id: String },
    /// Send a new message.
    Send {
        #[arg(long)]
        to: String,
        body: String,
    },
    /// Inject a message, bypassing delivery policy.
    Inject { message_id: String },
}

#[derive(Parser)]
struct ContextArgs {
    #[command(subcommand)]
    action: ContextAction,
}

#[derive(Subcommand)]
enum ContextAction {
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
struct WorktreeArgs {
    #[command(subcommand)]
    action: WorktreeAction,
}

#[derive(Subcommand)]
enum WorktreeAction {
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
struct ApprovalArgs {
    #[command(subcommand)]
    action: ApprovalAction,
}

#[derive(Subcommand)]
enum ApprovalAction {
    /// List pending approval requests.
    List,
    /// Approve a request.
    Approve { approval_id: String },
    /// Reject a request.
    Reject { approval_id: String },
}

#[derive(Parser)]
struct AttachArgs {
    /// Task ID or agent session ID to attach to.
    target: String,
}

#[derive(Parser)]
struct LayoutArgs {
    #[command(subcommand)]
    action: LayoutAction,
}

#[derive(Subcommand)]
enum LayoutAction {
    /// Save the current TUI layout as a named preset.
    Save { name: String },
    /// Load a saved layout preset.
    Load { name: String },
    /// List saved layout presets.
    List,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Doctor(_) => {
            // #TODO(agent): implement doctor checks (socket / sqlite / claude / codex / PTY)
            println!("agentmux doctor — not yet implemented");
        }
        Commands::Daemon(args) => match args.action {
            DaemonAction::Start => println!("daemon start — not yet implemented"),
            DaemonAction::Stop => println!("daemon stop — not yet implemented"),
            DaemonAction::Status => println!("daemon status — not yet implemented"),
        },
        Commands::Project(args) => match args.action {
            ProjectAction::Init { path } => {
                println!("project init {path} — not yet implemented");
            }
            ProjectAction::Open { path } => {
                println!("project open {path} — not yet implemented");
            }
            ProjectAction::Status => println!("project status — not yet implemented"),
        },
        Commands::Task(args) => match args.action {
            TaskAction::Run { description, team } => {
                println!(
                    "task run {:?} team={} — not yet implemented",
                    description,
                    team.as_deref().unwrap_or("(none)")
                );
            }
            TaskAction::Status { task_id } => println!("task status {task_id} — not yet implemented"),
            TaskAction::Pause { task_id } => println!("task pause {task_id} — not yet implemented"),
            TaskAction::Resume { task_id } => println!("task resume {task_id} — not yet implemented"),
            TaskAction::Cancel { task_id } => println!("task cancel {task_id} — not yet implemented"),
            TaskAction::Summary { task_id } => println!("task summary {task_id} — not yet implemented"),
        },
        Commands::Agent(args) => match args.action {
            AgentAction::Ls => println!("agent ls — not yet implemented"),
            AgentAction::Spawn { provider, role } => {
                println!("agent spawn provider={provider} role={role} — not yet implemented");
            }
            AgentAction::Stop { agent_id } => println!("agent stop {agent_id} — not yet implemented"),
            AgentAction::Send { agent_id, body } => {
                println!("agent send {agent_id} {body:?} — not yet implemented");
            }
            AgentAction::Inject { message_id, agent_id } => {
                println!("agent inject {message_id} → {agent_id} — not yet implemented");
            }
            AgentAction::Focus { agent_id } => println!("agent focus {agent_id} — not yet implemented"),
            AgentAction::Interrupt { agent_id } => {
                println!("agent interrupt {agent_id} — not yet implemented");
            }
        },
        Commands::Message(args) => match args.action {
            MessageAction::List => println!("message list — not yet implemented"),
            MessageAction::Show { message_id } => println!("message show {message_id} — not yet implemented"),
            MessageAction::Send { to, body } => {
                println!("message send --to {to} {body:?} — not yet implemented");
            }
            MessageAction::Inject { message_id } => {
                println!("message inject {message_id} — not yet implemented");
            }
        },
        Commands::Context(args) => match args.action {
            ContextAction::Add { title } => println!("context add {title:?} — not yet implemented"),
            ContextAction::List => println!("context list — not yet implemented"),
            ContextAction::Show { context_id } => println!("context show {context_id} — not yet implemented"),
            ContextAction::Search { query } => println!("context search {query:?} — not yet implemented"),
            ContextAction::Attach { context_id, message_id } => {
                println!("context attach {context_id} → {message_id} — not yet implemented");
            }
            ContextAction::Inject { context_id, agent_id } => {
                println!("context inject {context_id} → {agent_id} — not yet implemented");
            }
            ContextAction::Export { output } => println!("context export {output} — not yet implemented"),
        },
        Commands::Worktree(args) => match args.action {
            WorktreeAction::List => println!("worktree list — not yet implemented"),
            WorktreeAction::Diff { worktree_id } => println!("worktree diff {worktree_id} — not yet implemented"),
            WorktreeAction::Test { worktree_id } => println!("worktree test {worktree_id} — not yet implemented"),
            WorktreeAction::Promote { worktree_id } => {
                println!("worktree promote {worktree_id} — not yet implemented");
            }
            WorktreeAction::Archive { worktree_id } => {
                println!("worktree archive {worktree_id} — not yet implemented");
            }
        },
        Commands::Approval(args) => match args.action {
            ApprovalAction::List => println!("approval list — not yet implemented"),
            ApprovalAction::Approve { approval_id } => {
                println!("approval approve {approval_id} — not yet implemented");
            }
            ApprovalAction::Reject { approval_id } => {
                println!("approval reject {approval_id} — not yet implemented");
            }
        },
        Commands::Attach(args) => {
            println!("attach {} — not yet implemented", args.target);
        }
        Commands::Layout(args) => match args.action {
            LayoutAction::Save { name } => println!("layout save {name} — not yet implemented"),
            LayoutAction::Load { name } => println!("layout load {name} — not yet implemented"),
            LayoutAction::List => println!("layout list — not yet implemented"),
        },
    }
}
