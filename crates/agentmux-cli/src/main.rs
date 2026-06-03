//! `agentmux` — CLI entry point.
//!
//! Top-level subcommands mirror `docs/spec/11_cli_tui_user_spec.md §2`.
//! The CLI is a thin JSONL/Unix-socket client for the daemon. Interactive
//! control remains in the TUI.

use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use agentmux_core::{AgentmuxConfig, AgentmuxError, error::Result};
use agentmux_ipc::{
    ClientHello, ClientRequest, DaemonResponse, IpcCommand, JsonlReader, JsonlWriter,
};
use agentmux_pty::{PtyHandle, PtySpawnSpec, TerminalSize};
use agentmux_store::Store;
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use tokio::io::BufReader;
use tokio::net::UnixStream;

const DEFAULT_PROJECT_CONFIG: &str =
    include_str!("../../../docs/config/agentmux.config.example.toml");

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
    /// Override the daemon socket path (defaults to AGENTMUX_SOCKET or the runtime dir).
    #[arg(long, global = true)]
    socket: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Commands>,
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let socket_path = cli.socket.unwrap_or_else(default_socket_path);

    let Some(command) = cli.command else {
        // Bare `agentmux`: make sure the daemon is up, then show its status.
        ensure_daemon(&socket_path).await?;
        let response = send_daemon_request(&socket_path, daemon_status_request()).await?;
        print_response("agentmux", response)?;
        return Ok(());
    };

    match command {
        Commands::Doctor(_) => {
            let report = doctor_report(
                &socket_path,
                &std::env::current_dir().map_err(|error| {
                    AgentmuxError::Internal(format!("failed to resolve cwd: {error}"))
                })?,
            );
            print_doctor_report(&report);
        }
        Commands::Daemon(args) => match args.action {
            DaemonAction::Start => {
                ensure_daemon(&socket_path).await?;
                println!("daemon running ({})", socket_path.display());
            }
            DaemonAction::Stop => stop_daemon(&socket_path)?,
            DaemonAction::Status => {
                // Passive: report not-running rather than auto-starting.
                if daemon_running(&socket_path).await {
                    let response =
                        send_daemon_request(&socket_path, daemon_status_request()).await?;
                    print_response("daemon", response)?;
                } else {
                    println!("daemon: not running ({})", socket_path.display());
                }
            }
        },
        Commands::Project(args) => match args.action {
            ProjectAction::Init { path } => {
                // `project init` is a purely local operation — it creates the
                // `.agentmux/` directory and does not require a running daemon.
                let project_dir = init_project(Path::new(&path))?;
                println!("project initialised at {}", project_dir.display());
                println!("  created: .agentmux/config.toml");
                println!("  hint:    add '.agentmux/' to your .gitignore");
            }
            ProjectAction::Open { path } => {
                println!("project open {path} — not yet implemented");
            }
            ProjectAction::Status => {
                // Local-first: report project state from `.agentmux/` without
                // requiring a running daemon. Daemon connectivity is reported
                // best-effort and never fails the command.
                let cwd = std::env::current_dir().map_err(|error| {
                    AgentmuxError::UserError(format!("cannot resolve current directory: {error}"))
                })?;
                let agentmux_dir = cwd.join(".agentmux");
                if agentmux_dir.is_dir() {
                    println!("project root: {}", cwd.display());
                    let config_path = agentmux_dir.join("config.toml");
                    match AgentmuxConfig::load_from_path(&config_path) {
                        Ok(_) => println!("config:       {} (valid)", config_path.display()),
                        Err(error) => {
                            println!("config:       {} (invalid: {error})", config_path.display())
                        }
                    }
                    match UnixStream::connect(&socket_path).await {
                        Ok(_) => println!("daemon:       running ({})", socket_path.display()),
                        Err(_) => println!("daemon:       not running ({})", socket_path.display()),
                    }
                } else {
                    println!(
                        "project: not initialised (no .agentmux/ in {})",
                        cwd.display()
                    );
                    println!("  run: agentmux project init .");
                }
            }
        },
        Commands::Task(args) => match args.action {
            TaskAction::Run { description, team } => {
                let response =
                    send_daemon_request(&socket_path, task_run_request(description, team)?).await?;
                print_response("task", response)?;
            }
            TaskAction::Status { task_id } => {
                println!("task status {task_id} — not yet implemented")
            }
            TaskAction::Pause { task_id } => println!("task pause {task_id} — not yet implemented"),
            TaskAction::Resume { task_id } => {
                println!("task resume {task_id} — not yet implemented")
            }
            TaskAction::Cancel { task_id } => {
                println!("task cancel {task_id} — not yet implemented")
            }
            TaskAction::Summary { task_id } => {
                println!("task summary {task_id} — not yet implemented")
            }
        },
        Commands::Agent(args) => match args.action {
            AgentAction::Ls => {
                let response = send_daemon_request(&socket_path, agent_ls_request()).await?;
                print_response("agent", response)?;
            }
            AgentAction::Spawn { provider, role } => {
                let response =
                    send_daemon_request(&socket_path, agent_spawn_request(provider, role)?).await?;
                print_response("agent", response)?;
            }
            AgentAction::Stop { agent_id } => {
                let response =
                    send_daemon_request(&socket_path, agent_stop_request(agent_id)).await?;
                print_response("agent", response)?;
            }
            AgentAction::Send { agent_id, body } => {
                let response =
                    send_daemon_request(&socket_path, agent_send_request(agent_id, body)?).await?;
                print_response("agent", response)?;
            }
            AgentAction::Inject {
                message_id,
                agent_id,
            } => {
                let response =
                    send_daemon_request(&socket_path, agent_inject_request(message_id, agent_id))
                        .await?;
                print_response("agent", response)?;
            }
            AgentAction::Focus { agent_id } => {
                let response =
                    send_daemon_request(&socket_path, agent_focus_request(agent_id)).await?;
                print_response("agent", response)?;
            }
            AgentAction::Interrupt { agent_id } => {
                let response =
                    send_daemon_request(&socket_path, agent_interrupt_request(agent_id)).await?;
                print_response("agent", response)?;
            }
        },
        Commands::Message(args) => match args.action {
            MessageAction::List => {
                let response = send_daemon_request(&socket_path, message_list_request()).await?;
                print_response("message", response)?;
            }
            MessageAction::Show { message_id } => {
                let response =
                    send_daemon_request(&socket_path, message_show_request(message_id)).await?;
                print_response("message", response)?;
            }
            MessageAction::Send { to, body } => {
                let response =
                    send_daemon_request(&socket_path, message_send_request(to, body)?).await?;
                print_response("message", response)?;
            }
            MessageAction::Inject { message_id } => {
                let response =
                    send_daemon_request(&socket_path, message_inject_request(message_id)).await?;
                print_response("message", response)?;
            }
        },
        Commands::Context(args) => match args.action {
            ContextAction::Add { title } => {
                let response =
                    send_daemon_request(&socket_path, context_add_request(title)?).await?;
                print_response("context", response)?;
            }
            ContextAction::List => {
                let response = send_daemon_request(&socket_path, context_list_request()).await?;
                print_response("context", response)?;
            }
            ContextAction::Show { context_id } => {
                let response =
                    send_daemon_request(&socket_path, context_show_request(context_id)).await?;
                print_response("context", response)?;
            }
            ContextAction::Search { query } => {
                let response =
                    send_daemon_request(&socket_path, context_search_request(query)?).await?;
                print_response("context", response)?;
            }
            ContextAction::Attach {
                context_id,
                message_id,
            } => {
                let response = send_daemon_request(
                    &socket_path,
                    context_attach_request(context_id, message_id),
                )
                .await?;
                print_response("context", response)?;
            }
            ContextAction::Inject {
                context_id,
                agent_id,
            } => {
                let response =
                    send_daemon_request(&socket_path, context_inject_request(context_id, agent_id))
                        .await?;
                print_response("context", response)?;
            }
            ContextAction::Export { output } => {
                let response =
                    send_daemon_request(&socket_path, context_export_request(output)).await?;
                print_response("context", response)?;
            }
        },
        Commands::Worktree(args) => match args.action {
            WorktreeAction::List => {
                let response = send_daemon_request(&socket_path, worktree_list_request()).await?;
                print_response("worktree", response)?;
            }
            WorktreeAction::Diff { worktree_id } => {
                let response =
                    send_daemon_request(&socket_path, worktree_diff_request(worktree_id)).await?;
                print_response("worktree", response)?;
            }
            WorktreeAction::Test { worktree_id } => {
                let response =
                    send_daemon_request(&socket_path, worktree_test_request(worktree_id)).await?;
                print_response("worktree", response)?;
            }
            WorktreeAction::Promote { worktree_id } => {
                let response =
                    send_daemon_request(&socket_path, worktree_promote_request(worktree_id))
                        .await?;
                print_response("worktree", response)?;
            }
            WorktreeAction::Archive { worktree_id } => {
                let response =
                    send_daemon_request(&socket_path, worktree_archive_request(worktree_id))
                        .await?;
                print_response("worktree", response)?;
            }
        },
        Commands::Approval(args) => match args.action {
            ApprovalAction::List => {
                let response = send_daemon_request(&socket_path, approval_list_request()).await?;
                print_response("approval", response)?;
            }
            ApprovalAction::Approve { approval_id } => {
                let response =
                    send_daemon_request(&socket_path, approval_approve_request(approval_id))
                        .await?;
                print_response("approval", response)?;
            }
            ApprovalAction::Reject { approval_id } => {
                let response =
                    send_daemon_request(&socket_path, approval_reject_request(approval_id)).await?;
                print_response("approval", response)?;
            }
        },
        Commands::Attach(args) => {
            let response = send_daemon_request(&socket_path, attach_request(args.target)).await?;
            print_response("attach", response)?;
        }
        Commands::Layout(args) => match args.action {
            LayoutAction::Save { name } => {
                let response =
                    send_daemon_request(&socket_path, layout_save_request(name)?).await?;
                print_response("layout", response)?;
            }
            LayoutAction::Load { name } => {
                let response = send_daemon_request(&socket_path, layout_load_request(name)).await?;
                print_response("layout", response)?;
            }
            LayoutAction::List => {
                let response = send_daemon_request(&socket_path, layout_list_request()).await?;
                print_response("layout", response)?;
            }
        },
    };

    Ok(())
}

fn default_socket_path() -> PathBuf {
    if let Some(socket_path) = std::env::var_os("AGENTMUX_SOCKET") {
        return PathBuf::from(socket_path);
    }
    if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime_dir).join("agentmux/agentmux.sock");
    }

    let user = std::env::var_os("USER")
        .and_then(|value| value.into_string().ok())
        .unwrap_or_else(|| "current".to_string());
    std::env::temp_dir().join(format!("agentmux-{user}/agentmux.sock"))
}

fn daemon_status_request() -> ClientRequest {
    ClientRequest::new("req_daemon_status", IpcCommand::DaemonStatus, json!({}))
}

fn task_run_request(description: String, team: Option<String>) -> Result<ClientRequest> {
    let project_path = std::env::current_dir()
        .map_err(|error| AgentmuxError::Internal(format!("failed to resolve cwd: {error}")))?;

    Ok(ClientRequest::new(
        "req_task_run",
        IpcCommand::TaskRun,
        json!({
            "project_path": project_path,
            "body": description,
            "team": team.unwrap_or_else(|| "claude-codex".to_string()),
        }),
    ))
}

fn attach_request(target: String) -> ClientRequest {
    ClientRequest::new(
        "req_attach",
        IpcCommand::ClientAttach,
        json!({ "agent_id": target }),
    )
}

fn agent_ls_request() -> ClientRequest {
    ClientRequest::new("req_agent_ls", IpcCommand::DaemonStatus, json!({}))
}

fn agent_spawn_request(provider: String, role: String) -> Result<ClientRequest> {
    if provider.trim().is_empty() {
        return Err(AgentmuxError::UserError(
            "agent provider must not be empty".to_string(),
        ));
    }
    if role.trim().is_empty() {
        return Err(AgentmuxError::UserError(
            "agent role must not be empty".to_string(),
        ));
    }

    Ok(ClientRequest::new(
        "req_agent_spawn",
        IpcCommand::AgentSpawn,
        json!({
            "provider": provider,
            "role": role,
            "name": role,
        }),
    ))
}

fn agent_stop_request(agent_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_agent_stop",
        IpcCommand::AgentStop,
        json!({ "agent_id": agent_id }),
    )
}

fn agent_send_request(agent_id: String, body: String) -> Result<ClientRequest> {
    if body.trim().is_empty() {
        return Err(AgentmuxError::UserError(
            "agent message body must not be empty".to_string(),
        ));
    }
    Ok(ClientRequest::new(
        "req_agent_send",
        IpcCommand::MessageCreate,
        json!({
            "to": agent_id,
            "body": body,
            "kind": "handoff",
            "priority": "normal",
            "delivery_mode": "inject_when_idle",
        }),
    ))
}

fn agent_inject_request(message_id: String, agent_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_agent_inject",
        IpcCommand::MessageInject,
        json!({ "message_id": message_id, "agent_id": agent_id }),
    )
}

fn agent_focus_request(agent_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_agent_focus",
        IpcCommand::AgentFocus,
        json!({ "agent_id": agent_id }),
    )
}

fn agent_interrupt_request(agent_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_agent_interrupt",
        IpcCommand::AgentInterrupt,
        json!({ "agent_id": agent_id }),
    )
}

fn message_list_request() -> ClientRequest {
    ClientRequest::new("req_message_list", IpcCommand::MessageList, json!({}))
}

fn message_show_request(message_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_message_show",
        IpcCommand::MessageShow,
        json!({ "message_id": message_id }),
    )
}

fn message_send_request(to: String, body: String) -> Result<ClientRequest> {
    if body.trim().is_empty() {
        return Err(AgentmuxError::UserError(
            "message body must not be empty".to_string(),
        ));
    }

    Ok(ClientRequest::new(
        "req_message_send",
        IpcCommand::MessageCreate,
        json!({
            "to": to,
            "body": body,
            "kind": "handoff",
            "priority": "normal",
            "delivery_mode": "inject_when_idle",
        }),
    ))
}

fn message_inject_request(message_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_message_inject",
        IpcCommand::MessageInject,
        json!({ "message_id": message_id }),
    )
}

fn context_add_request(title: String) -> Result<ClientRequest> {
    if title.trim().is_empty() {
        return Err(AgentmuxError::UserError(
            "context title must not be empty".to_string(),
        ));
    }

    Ok(ClientRequest::new(
        "req_context_add",
        IpcCommand::ContextCreate,
        json!({
            "title": title,
            "kind": "handoff_summary",
            "visibility": "internal",
        }),
    ))
}

fn context_list_request() -> ClientRequest {
    ClientRequest::new("req_context_list", IpcCommand::ContextSearch, json!({}))
}

fn context_show_request(context_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_context_show",
        IpcCommand::ContextSearch,
        json!({ "context_id": context_id }),
    )
}

fn context_search_request(query: String) -> Result<ClientRequest> {
    if query.trim().is_empty() {
        return Err(AgentmuxError::UserError(
            "context search query must not be empty".to_string(),
        ));
    }

    Ok(ClientRequest::new(
        "req_context_search",
        IpcCommand::ContextSearch,
        json!({ "query": query }),
    ))
}

fn context_attach_request(context_id: String, message_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_context_attach",
        IpcCommand::ContextAttach,
        json!({ "context_id": context_id, "message_id": message_id }),
    )
}

fn context_inject_request(context_id: String, agent_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_context_inject",
        IpcCommand::ContextInject,
        json!({ "context_id": context_id, "agent_id": agent_id }),
    )
}

fn context_export_request(output: String) -> ClientRequest {
    ClientRequest::new(
        "req_context_export",
        IpcCommand::ContextExport,
        json!({ "output": output }),
    )
}

fn worktree_list_request() -> ClientRequest {
    ClientRequest::new("req_worktree_list", IpcCommand::WorktreeList, json!({}))
}

fn worktree_diff_request(worktree_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_worktree_diff",
        IpcCommand::WorktreeDiff,
        json!({ "worktree_id": worktree_id }),
    )
}

fn worktree_test_request(worktree_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_worktree_test",
        IpcCommand::WorktreeTest,
        json!({ "worktree_id": worktree_id }),
    )
}

fn worktree_promote_request(worktree_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_worktree_promote",
        IpcCommand::WorktreePromote,
        json!({ "worktree_id": worktree_id }),
    )
}

fn worktree_archive_request(worktree_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_worktree_archive",
        IpcCommand::WorktreeArchive,
        json!({ "worktree_id": worktree_id }),
    )
}

fn approval_list_request() -> ClientRequest {
    ClientRequest::new("req_approval_list", IpcCommand::ApprovalList, json!({}))
}

fn approval_approve_request(approval_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_approval_approve",
        IpcCommand::ApprovalApprove,
        json!({ "approval_id": approval_id }),
    )
}

fn approval_reject_request(approval_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_approval_reject",
        IpcCommand::ApprovalReject,
        json!({ "approval_id": approval_id }),
    )
}

fn layout_save_request(name: String) -> Result<ClientRequest> {
    if name.trim().is_empty() {
        return Err(AgentmuxError::UserError(
            "layout name must not be empty".to_string(),
        ));
    }

    Ok(ClientRequest::new(
        "req_layout_save",
        IpcCommand::LayoutSet,
        json!({ "name": name }),
    ))
}

fn layout_load_request(name: String) -> ClientRequest {
    ClientRequest::new(
        "req_layout_load",
        IpcCommand::LayoutGet,
        json!({ "name": name }),
    )
}

fn layout_list_request() -> ClientRequest {
    ClientRequest::new("req_layout_list", IpcCommand::LayoutGet, json!({}))
}

fn init_project(path: &Path) -> Result<PathBuf> {
    let project_dir = path.canonicalize().map_err(|error| {
        AgentmuxError::UserError(format!(
            "failed to resolve project path '{}': {error}",
            path.display()
        ))
    })?;
    let agentmux_dir = project_dir.join(".agentmux");
    std::fs::create_dir_all(&agentmux_dir).map_err(|error| {
        AgentmuxError::StoreError(format!(
            "failed to create '{}': {error}",
            agentmux_dir.display()
        ))
    })?;

    let config_path = agentmux_dir.join("config.toml");
    if !config_path.exists() {
        std::fs::write(&config_path, DEFAULT_PROJECT_CONFIG).map_err(|error| {
            AgentmuxError::StoreError(format!(
                "failed to write '{}': {error}",
                config_path.display()
            ))
        })?;
    }

    Ok(project_dir)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DoctorStatus {
    Ok,
    Warn,
    Fail,
}

impl DoctorStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DoctorCheck {
    name: &'static str,
    status: DoctorStatus,
    detail: String,
}

impl DoctorCheck {
    fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: DoctorStatus::Ok,
            detail: detail.into(),
        }
    }

    fn warn(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: DoctorStatus::Warn,
            detail: detail.into(),
        }
    }

    fn fail(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: DoctorStatus::Fail,
            detail: detail.into(),
        }
    }
}

fn doctor_report(socket_path: &Path, project_dir: &Path) -> Vec<DoctorCheck> {
    vec![
        check_daemon_socket(socket_path),
        check_config_parse(project_dir),
        check_sqlite_access(project_dir),
        check_command_available("claude"),
        check_command_available("codex"),
        check_pty_creation(project_dir),
        check_git_worktree(project_dir),
    ]
}

#[cfg(unix)]
fn check_daemon_socket(socket_path: &Path) -> DoctorCheck {
    use std::os::unix::fs::FileTypeExt;

    match std::fs::metadata(socket_path) {
        Ok(metadata) if metadata.file_type().is_socket() => {
            DoctorCheck::ok("daemon socket", socket_path.display().to_string())
        }
        Ok(_) => DoctorCheck::fail(
            "daemon socket",
            format!("path exists but is not a socket: {}", socket_path.display()),
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => DoctorCheck::warn(
            "daemon socket",
            format!("not found: {}", socket_path.display()),
        ),
        Err(error) => DoctorCheck::fail(
            "daemon socket",
            format!("cannot inspect {}: {error}", socket_path.display()),
        ),
    }
}

#[cfg(not(unix))]
fn check_daemon_socket(socket_path: &Path) -> DoctorCheck {
    if socket_path.exists() {
        DoctorCheck::ok("daemon socket", socket_path.display().to_string())
    } else {
        DoctorCheck::warn(
            "daemon socket",
            format!("not found: {}", socket_path.display()),
        )
    }
}

fn check_config_parse(project_dir: &Path) -> DoctorCheck {
    let config_path = project_dir.join(".agentmux/config.toml");
    match std::fs::read_to_string(&config_path) {
        Ok(contents) => match AgentmuxConfig::parse_str(&contents) {
            Ok(config) => {
                DoctorCheck::ok("config parse", format!("project={}", config.project.name))
            }
            Err(error) => DoctorCheck::fail("config parse", error.to_string()),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => DoctorCheck::warn(
            "config parse",
            format!("not found: {}", config_path.display()),
        ),
        Err(error) => DoctorCheck::fail(
            "config parse",
            format!("cannot read {}: {error}", config_path.display()),
        ),
    }
}

fn check_sqlite_access(project_dir: &Path) -> DoctorCheck {
    let db_path = project_dir.join(".agentmux/state.db");
    match Store::open(&db_path) {
        Ok(_) => DoctorCheck::ok("SQLite access", db_path.display().to_string()),
        Err(error) => DoctorCheck::fail("SQLite access", error.to_string()),
    }
}

fn check_command_available(command: &'static str) -> DoctorCheck {
    match find_command_in_path(command, std::env::var_os("PATH").as_deref()) {
        Some(path) => DoctorCheck::ok(command, path.display().to_string()),
        None => DoctorCheck::warn(command, format!("'{command}' not found in PATH")),
    }
}

fn find_command_in_path(command: &str, path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let path = path?;
    std::env::split_paths(path)
        .map(|dir| dir.join(command))
        .find(|candidate| candidate.is_file())
}

fn check_pty_creation(project_dir: &Path) -> DoctorCheck {
    let mut env = BTreeMap::new();
    env.insert("TERM".to_string(), "xterm-256color".to_string());
    let spec = PtySpawnSpec {
        command: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), "exit 0".to_string()],
        cwd: project_dir.to_path_buf(),
        env,
        size: TerminalSize::default(),
    };

    match PtyHandle::spawn(spec).and_then(|mut handle| handle.wait()) {
        Ok(status) if status.success => DoctorCheck::ok("PTY creation", status.display),
        Ok(status) => DoctorCheck::fail("PTY creation", status.display),
        Err(error) => DoctorCheck::fail("PTY creation", error.to_string()),
    }
}

fn check_git_worktree(project_dir: &Path) -> DoctorCheck {
    let output = Command::new("git")
        .arg("-C")
        .arg(project_dir)
        .args(["worktree", "list", "--porcelain"])
        .output();

    match output {
        Ok(output) if output.status.success() => {
            DoctorCheck::ok("git worktree", format!("{} bytes", output.stdout.len()))
        }
        Ok(output) => DoctorCheck::warn(
            "git worktree",
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ),
        Err(error) => DoctorCheck::warn("git worktree", format!("failed to run git: {error}")),
    }
}

fn print_doctor_report(report: &[DoctorCheck]) {
    for check in report {
        println!(
            "{:<14} {:<5} {}",
            check.name,
            check.status.label(),
            check.detail
        );
    }
}

/// Pidfile written by the daemon next to its socket; used by `daemon stop`.
fn daemon_pid_path(socket_path: &Path) -> Option<PathBuf> {
    socket_path
        .parent()
        .map(|parent| parent.join("agentmux.pid"))
}

/// Resolve the `agentmux-daemon` binary: prefer a sibling of the running CLI
/// (so dev `target/debug` and installed `bin/` both work), else fall back to PATH.
fn resolve_daemon_binary() -> std::ffi::OsString {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join("agentmux-daemon");
        if sibling.exists() {
            return sibling.into_os_string();
        }
    }
    std::ffi::OsString::from("agentmux-daemon")
}

/// Whether the daemon is currently accepting connections on `socket_path`.
async fn daemon_running(socket_path: &Path) -> bool {
    UnixStream::connect(socket_path).await.is_ok()
}

/// Ensure the daemon is reachable, auto-starting it in the background if not.
async fn ensure_daemon(socket_path: &Path) -> Result<()> {
    if daemon_running(socket_path).await {
        return Ok(());
    }

    let binary = resolve_daemon_binary();
    let mut command = Command::new(&binary);
    command
        .env("AGENTMUX_SOCKET", socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Detach into its own process group so it survives the CLI exiting and
        // is not killed by a terminal Ctrl-C aimed at the foreground command.
        .process_group(0);
    command.spawn().map_err(|error| {
        AgentmuxError::IpcError(format!(
            "failed to start agentmux-daemon ('{}'): {error}",
            binary.to_string_lossy()
        ))
    })?;

    // Poll for the socket to come up (~5s).
    for _ in 0..50 {
        if daemon_running(socket_path).await {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Err(AgentmuxError::IpcError(format!(
        "agentmux-daemon did not become ready at '{}' within 5s",
        socket_path.display()
    )))
}

/// Stop the running daemon via its pidfile (SIGTERM → graceful shutdown).
fn stop_daemon(socket_path: &Path) -> Result<()> {
    let Some(pid_path) = daemon_pid_path(socket_path) else {
        println!(
            "daemon: cannot resolve pidfile for {}",
            socket_path.display()
        );
        return Ok(());
    };
    match std::fs::read_to_string(&pid_path) {
        Ok(contents) => {
            let pid = contents.trim();
            let signalled = Command::new("kill")
                .arg(pid)
                .status()
                .map(|status| status.success())
                .unwrap_or(false);
            if signalled {
                let _ = std::fs::remove_file(&pid_path);
                println!("daemon stopped (pid {pid})");
            } else {
                let _ = std::fs::remove_file(&pid_path);
                println!("daemon stop: pid {pid} was not running (cleared stale pidfile)");
            }
        }
        Err(_) => println!("daemon: not running (no pidfile at {})", pid_path.display()),
    }
    Ok(())
}

async fn send_daemon_request(socket_path: &Path, request: ClientRequest) -> Result<DaemonResponse> {
    ensure_daemon(socket_path).await?;
    let stream = UnixStream::connect(socket_path).await.map_err(|error| {
        AgentmuxError::IpcError(format!(
            "failed to connect daemon socket '{}': {error}",
            socket_path.display()
        ))
    })?;
    let request_id = request.id.clone();
    let (reader, writer) = stream.into_split();
    let mut reader = JsonlReader::new(BufReader::new(reader));
    let mut writer = JsonlWriter::new(writer);

    writer
        .write(&ClientHello::new(env!("CARGO_PKG_VERSION")))
        .await?;
    writer.write(&request).await?;

    while let Some(frame) = reader.read::<Value>().await? {
        if frame.get("id").and_then(Value::as_str) == Some(request_id.as_str()) {
            return serde_json::from_value(frame).map_err(|error| {
                AgentmuxError::IpcError(format!("invalid daemon response: {error}"))
            });
        }
    }

    Err(AgentmuxError::IpcError(format!(
        "daemon closed before responding to {request_id}"
    )))
}

fn print_response(label: &str, response: DaemonResponse) -> Result<()> {
    if !response.ok {
        let error = response.error.ok_or_else(|| {
            AgentmuxError::IpcError("daemon returned an error without an error body".to_string())
        })?;
        return Err(AgentmuxError::UserError(format!(
            "{label} request failed: {} ({}){}",
            error.message,
            error.code,
            error
                .hint
                .map(|hint| format!("; hint: {hint}"))
                .unwrap_or_default()
        )));
    }

    let payload = response.payload.unwrap_or_else(|| json!({}));
    println!(
        "{}",
        serde_json::to_string_pretty(&payload).map_err(|error| AgentmuxError::IpcError(
            format!("invalid response payload: {error}")
        ))?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentmux_ipc::PROTOCOL_VERSION;

    #[test]
    fn daemon_pid_path_sits_next_to_socket() {
        let socket = PathBuf::from("/tmp/agentmux-test/agentmux.sock");
        assert_eq!(
            daemon_pid_path(&socket),
            Some(PathBuf::from("/tmp/agentmux-test/agentmux.pid"))
        );
    }

    #[test]
    fn resolve_daemon_binary_falls_back_to_bare_name() {
        // Always returns a non-empty program name (sibling path or PATH fallback).
        assert!(!resolve_daemon_binary().is_empty());
    }

    #[test]
    fn task_run_request_matches_spec_payload() {
        let request = task_run_request(
            "refresh token bugを修正".to_string(),
            Some("claude-codex".to_string()),
        )
        .unwrap();

        assert_eq!(request.version, PROTOCOL_VERSION);
        assert_eq!(request.command, IpcCommand::TaskRun);
        assert_eq!(request.payload["body"], "refresh token bugを修正");
        assert_eq!(request.payload["team"], "claude-codex");
        assert!(request.payload["project_path"].as_str().is_some());
    }

    #[test]
    fn task_run_defaults_to_claude_codex_team() {
        let request = task_run_request("fix failing tests".to_string(), None).unwrap();

        assert_eq!(request.payload["team"], "claude-codex");
    }

    #[test]
    fn attach_request_targets_agent_session_for_daemon_ipc() {
        let request = attach_request("agent_01HX".to_string());

        assert_eq!(request.command, IpcCommand::ClientAttach);
        assert_eq!(request.payload["agent_id"], "agent_01HX");
    }

    #[test]
    fn message_requests_target_daemon_ipc() {
        let list = message_list_request();
        assert_eq!(list.command, IpcCommand::MessageList);

        let show = message_show_request("msg_01HX".to_string());
        assert_eq!(show.command, IpcCommand::MessageShow);
        assert_eq!(show.payload["message_id"], "msg_01HX");

        let send = message_send_request("agent_01HX".to_string(), "hello".to_string()).unwrap();
        assert_eq!(send.command, IpcCommand::MessageCreate);
        assert_eq!(send.payload["to"], "agent_01HX");
        assert_eq!(send.payload["body"], "hello");
        assert_eq!(send.payload["kind"], "handoff");
        assert_eq!(send.payload["delivery_mode"], "inject_when_idle");

        let inject = message_inject_request("msg_01HX".to_string());
        assert_eq!(inject.command, IpcCommand::MessageInject);
        assert_eq!(inject.payload["message_id"], "msg_01HX");
    }

    #[test]
    fn message_send_rejects_empty_body_before_ipc() {
        let error = message_send_request("agent_01HX".to_string(), "  ".to_string()).unwrap_err();

        assert!(error.to_string().contains("message body must not be empty"));
    }

    #[test]
    fn context_requests_target_daemon_ipc() {
        let add = context_add_request("decision log".to_string()).unwrap();
        assert_eq!(add.command, IpcCommand::ContextCreate);
        assert_eq!(add.payload["title"], "decision log");
        assert_eq!(add.payload["kind"], "handoff_summary");

        let list = context_list_request();
        assert_eq!(list.command, IpcCommand::ContextSearch);
        assert_eq!(list.payload, json!({}));

        let show = context_show_request("ctx_01HX".to_string());
        assert_eq!(show.command, IpcCommand::ContextSearch);
        assert_eq!(show.payload["context_id"], "ctx_01HX");

        let search = context_search_request("risk".to_string()).unwrap();
        assert_eq!(search.command, IpcCommand::ContextSearch);
        assert_eq!(search.payload["query"], "risk");

        let attach = context_attach_request("ctx_01HX".to_string(), "msg_01HX".to_string());
        assert_eq!(attach.command, IpcCommand::ContextAttach);
        assert_eq!(attach.payload["context_id"], "ctx_01HX");
        assert_eq!(attach.payload["message_id"], "msg_01HX");

        let inject = context_inject_request("ctx_01HX".to_string(), "agent_01HX".to_string());
        assert_eq!(inject.command, IpcCommand::ContextInject);
        assert_eq!(inject.payload["context_id"], "ctx_01HX");
        assert_eq!(inject.payload["agent_id"], "agent_01HX");

        let export = context_export_request("contexts.json".to_string());
        assert_eq!(export.command, IpcCommand::ContextExport);
        assert_eq!(export.payload["output"], "contexts.json");
    }

    #[test]
    fn context_request_builders_reject_empty_text_before_ipc() {
        let add_error = context_add_request("  ".to_string()).unwrap_err();
        assert!(
            add_error
                .to_string()
                .contains("context title must not be empty")
        );

        let search_error = context_search_request("  ".to_string()).unwrap_err();
        assert!(
            search_error
                .to_string()
                .contains("context search query must not be empty")
        );
    }

    #[test]
    fn worktree_requests_target_daemon_ipc() {
        let list = worktree_list_request();
        assert_eq!(list.command, IpcCommand::WorktreeList);
        assert_eq!(list.payload, json!({}));

        let diff = worktree_diff_request("wt_01HX".to_string());
        assert_eq!(diff.command, IpcCommand::WorktreeDiff);
        assert_eq!(diff.payload["worktree_id"], "wt_01HX");

        let test = worktree_test_request("wt_01HX".to_string());
        assert_eq!(test.command, IpcCommand::WorktreeTest);
        assert_eq!(test.payload["worktree_id"], "wt_01HX");

        let promote = worktree_promote_request("wt_01HX".to_string());
        assert_eq!(promote.command, IpcCommand::WorktreePromote);
        assert_eq!(promote.payload["worktree_id"], "wt_01HX");

        let archive = worktree_archive_request("wt_01HX".to_string());
        assert_eq!(archive.command, IpcCommand::WorktreeArchive);
        assert_eq!(archive.payload["worktree_id"], "wt_01HX");
    }

    #[test]
    fn approval_requests_target_daemon_ipc() {
        let list = approval_list_request();
        assert_eq!(list.command, IpcCommand::ApprovalList);
        assert_eq!(list.payload, json!({}));

        let approve = approval_approve_request("appr_01HX".to_string());
        assert_eq!(approve.command, IpcCommand::ApprovalApprove);
        assert_eq!(approve.payload["approval_id"], "appr_01HX");

        let reject = approval_reject_request("appr_01HY".to_string());
        assert_eq!(reject.command, IpcCommand::ApprovalReject);
        assert_eq!(reject.payload["approval_id"], "appr_01HY");
    }

    #[test]
    fn agent_requests_target_daemon_ipc() {
        let list = agent_ls_request();
        assert_eq!(list.command, IpcCommand::DaemonStatus);
        assert_eq!(list.payload, json!({}));

        let spawn = agent_spawn_request("codex".to_string(), "implementer".to_string()).unwrap();
        assert_eq!(spawn.command, IpcCommand::AgentSpawn);
        assert_eq!(spawn.payload["provider"], "codex");
        assert_eq!(spawn.payload["role"], "implementer");
        assert_eq!(spawn.payload["name"], "implementer");

        let stop = agent_stop_request("agent_01HX".to_string());
        assert_eq!(stop.command, IpcCommand::AgentStop);
        assert_eq!(stop.payload["agent_id"], "agent_01HX");

        let send = agent_send_request("agent_01HX".to_string(), "hello".to_string()).unwrap();
        assert_eq!(send.command, IpcCommand::MessageCreate);
        assert_eq!(send.payload["to"], "agent_01HX");
        assert_eq!(send.payload["body"], "hello");

        let inject = agent_inject_request("msg_01HX".to_string(), "agent_01HX".to_string());
        assert_eq!(inject.command, IpcCommand::MessageInject);
        assert_eq!(inject.payload["message_id"], "msg_01HX");
        assert_eq!(inject.payload["agent_id"], "agent_01HX");

        let focus = agent_focus_request("agent_01HX".to_string());
        assert_eq!(focus.command, IpcCommand::AgentFocus);
        assert_eq!(focus.payload["agent_id"], "agent_01HX");

        let interrupt = agent_interrupt_request("agent_01HX".to_string());
        assert_eq!(interrupt.command, IpcCommand::AgentInterrupt);
        assert_eq!(interrupt.payload["agent_id"], "agent_01HX");
    }

    #[test]
    fn agent_request_builders_reject_empty_values_before_ipc() {
        let provider_error =
            agent_spawn_request(" ".to_string(), "implementer".to_string()).unwrap_err();
        assert!(provider_error.to_string().contains("provider"));

        let role_error = agent_spawn_request("codex".to_string(), " ".to_string()).unwrap_err();
        assert!(role_error.to_string().contains("role"));

        let body_error = agent_send_request("agent_01HX".to_string(), " ".to_string()).unwrap_err();
        assert!(body_error.to_string().contains("agent message body"));
    }

    #[test]
    fn layout_requests_target_daemon_ipc() {
        let save = layout_save_request("default".to_string()).unwrap();
        assert_eq!(save.command, IpcCommand::LayoutSet);
        assert_eq!(save.payload["name"], "default");

        let load = layout_load_request("default".to_string());
        assert_eq!(load.command, IpcCommand::LayoutGet);
        assert_eq!(load.payload["name"], "default");

        let list = layout_list_request();
        assert_eq!(list.command, IpcCommand::LayoutGet);
        assert_eq!(list.payload, json!({}));
    }

    #[test]
    fn layout_save_rejects_empty_name_before_ipc() {
        let error = layout_save_request("  ".to_string()).unwrap_err();

        assert!(error.to_string().contains("layout name must not be empty"));
    }

    #[test]
    fn project_init_creates_agentmux_config_without_overwriting_existing_file() {
        let root = std::env::temp_dir().join(format!("agentmux-cli-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let project_dir = init_project(&root).unwrap();
        let config_path = project_dir.join(".agentmux/config.toml");
        let contents = std::fs::read_to_string(&config_path).unwrap();
        let config = AgentmuxConfig::parse_str(&contents).unwrap();
        assert_eq!(config.project.name, "example");

        std::fs::write(
            &config_path,
            DEFAULT_PROJECT_CONFIG.replace("example", "custom"),
        )
        .unwrap();
        init_project(&root).unwrap();
        assert!(
            std::fs::read_to_string(&config_path)
                .unwrap()
                .contains("custom")
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn config_parser_accepts_docs_example_and_rejects_invalid_values() {
        let config = AgentmuxConfig::parse_str(DEFAULT_PROJECT_CONFIG).unwrap();
        assert_eq!(config.team["claude-codex"].agents.len(), 5);

        let invalid =
            DEFAULT_PROJECT_CONFIG.replace("prefix_key = \"Ctrl-g\"", "prefix_key = \"F12\"");
        let error = AgentmuxConfig::parse_str(&invalid).unwrap_err();
        assert!(error.to_string().contains("tui.prefix_key"));
    }

    #[test]
    fn command_lookup_searches_path_entries() {
        let root =
            std::env::temp_dir().join(format!("agentmux-cli-path-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let command_path = root.join("codex");
        std::fs::write(&command_path, "").unwrap();

        assert_eq!(
            find_command_in_path("codex", Some(root.as_os_str())),
            Some(command_path)
        );
        assert_eq!(find_command_in_path("claude", Some(root.as_os_str())), None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn doctor_report_includes_required_v0_1_checks() {
        let root =
            std::env::temp_dir().join(format!("agentmux-cli-doctor-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".agentmux")).unwrap();
        std::fs::write(root.join(".agentmux/config.toml"), DEFAULT_PROJECT_CONFIG).unwrap();

        let report = doctor_report(&root.join("agentmux.sock"), &root);
        let names = report.iter().map(|check| check.name).collect::<Vec<_>>();

        assert_eq!(
            names,
            vec![
                "daemon socket",
                "config parse",
                "SQLite access",
                "claude",
                "codex",
                "PTY creation",
                "git worktree"
            ]
        );
        assert!(report.iter().any(|check| {
            check.name == "config parse"
                && check.status == DoctorStatus::Ok
                && check.detail == "project=example"
        }));

        std::fs::remove_dir_all(&root).unwrap();
    }
}
