//! `agentmux` — CLI entry point.
//!
//! Top-level subcommands mirror `docs/spec/11_cli_tui_user_spec.md §2`.
//! The CLI is a thin JSONL/Unix-socket client for the daemon. Interactive
//! control remains in the TUI.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agentmux_core::{AgentmuxConfig, AgentmuxError, error::Result};
use agentmux_ipc::{
    ClientHello, ClientRequest, DaemonResponse, DaemonStreamFrame, IpcCommand, JsonlReader,
    JsonlWriter,
};
use agentmux_pty::{PtyHandle, PtySpawnSpec, TerminalSize};
use agentmux_store::Store;
use agentmux_tui::{
    input::{InputForwardError, dispatch_to_daemon_request},
    keymap::KeymapDispatcher,
    layout::{PaneLayout, Rect},
    render::TuiSessionRenderer,
    state::{
        AgentProviderChoice, CommandEffect, CopyPoint, CopySelection, StateChange,
        TerminalSize as TuiTerminalSize, TuiSessionState,
    },
    terminal::{CrosstermTerminalIo, TerminalIo, TerminalSession},
};
use clap::{Parser, Subcommand};
use crossterm::{
    event::{Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind},
    terminal as crossterm_terminal,
};
use serde_json::{Value, json};
use tokio::io::{AsyncWrite, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const DEFAULT_PROJECT_CONFIG: &str =
    include_str!("../../../docs/config/agentmux.config.example.toml");
const RESULT_PROTOCOL_MARKER_START: &str = "<!-- agentmux-result-protocol:start -->";
const RESULT_PROTOCOL_MARKER_END: &str = "<!-- agentmux-result-protocol:end -->";
static AGENT_NAME_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const RESULT_PROTOCOL_BLOCK: &str = r#"<!-- agentmux-result-protocol:start -->
## agentmux result protocol

When working inside an agentmux-managed session, end each completed turn with:

```text
AGENTMUX_RESULT:
{
  "status": "completed",
  "summary": "<short summary>",
  "changed_files": [],
  "messages": [],
  "context_updates": [],
  "needs": [],
  "next": null
}
```

Use `messages[]` to send work to another coding agent through the agentmux message bus. The whole `AGENTMUX_RESULT` block is not stored as a message; only entries inside `messages[]` are routed. Keep `messages: []` when no cross-agent message is needed.

Allowed `messages[].kind` values are: `TaskAssignment`, `Question`, `Finding`, `PatchProposal`, `ReviewComment`, `TestResult`, `FailureReport`, `Decision`, `Handoff`, `ApprovalRequest`, `ContextUpdate`, `StatusProbe`. Do not invent other kinds such as `Greeting`; an invalid kind prevents the result messages from being stored.

Agent sessions register a stable role and a unique session name at startup. Use role targets (`role:tester`, `role:implementer`, `role:reviewer`) when every session with that role should receive the message. Use `agent:<session-name>` or a session id when the message is for exactly one session. Check available sessions with `Ctrl-g s` in the TUI or `agentmux sessions`.

Each live session receives its own identity through environment variables: `AGENTMUX_AGENT_NAME`, `AGENTMUX_AGENT_ROLE`, and `AGENTMUX_AGENT_ID`. Use `AGENTMUX_AGENT_NAME` when another session needs to reply to exactly this session.

Common TUI workflows:

- Start multiple panes with `agentmux start "agy,codex"` or include message history with `agentmux start "agy,messages,codex"`.
- Inside the TUI, `Ctrl-g %` and `Ctrl-g "` open the new pane picker. Choose `Claude Code`, `Codex`, `Antigravity`, or `Conversation List`.
- `Conversation List` opens the message history as a normal pane. `Ctrl-g m` opens the same history as a temporary overlay.
- `Ctrl-g s` shows running sessions with their names, roles, and process IDs.
- `Ctrl-g x` closes the focused local pane or stops the focused agent pane.

Message inspection commands:

- `agentmux message list` shows stored bus messages newest first.
- `agentmux sessions` shows live agent sessions and their stable names/roles.
- `agentmux start "messages"` opens only the message history pane.

To inject an existing bus message into a live session, use `agentmux message inject <message_id>` only when the message target resolves to exactly one session. If the target can resolve to multiple sessions (for example `role:tester`) or you need a specific pane, use `agentmux agent inject <message_id> <agent_id>` after checking `agentmux sessions`; this explicitly selects the session that receives the PTY input.

Injection is asynchronous: the daemon records the message first, then waits briefly before writing the rendered message into the target PTY. If the TUI list updates before the text appears in the agent pane, wait a few seconds before retrying.

```json
{
  "to": "role:tester",
  "kind": "TestResult",
  "body": "Run the focused regression tests.",
  "priority": "normal"
}
```

Two-session exchange example:

```text
impl finishes work:
AGENTMUX_RESULT:
{
  "status": "completed",
  "summary": "Implemented copy mode.",
  "changed_files": ["crates/agentmux-cli/src/main.rs"],
  "messages": [
    {
      "to": "role:tester",
      "kind": "TestResult",
      "body": "Please verify copy mode: Ctrl-g [, drag inside the focused pane, release to copy, Esc/q to exit.",
      "priority": "normal"
    }
  ],
  "context_updates": [],
  "needs": [],
  "next": null
}
```

```text
tester replies:
AGENTMUX_RESULT:
{
  "status": "completed",
  "summary": "Copy mode verification completed.",
  "changed_files": [],
  "messages": [
    {
      "to": "agent:codex-a1b2c3",
      "kind": "Finding",
      "body": "Focused-pane drag selection worked. OSC52 clipboard support depends on the host terminal.",
      "priority": "normal"
    }
  ],
  "context_updates": [],
  "needs": [],
  "next": null
}
```

Check delivery with `Ctrl-g m` in the TUI or `agentmux message list`.
<!-- agentmux-result-protocol:end -->
"#;

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
struct StartArgs {
    /// Comma-separated panes to open before the TUI, e.g. "agy,codex,messages".
    providers: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StartupPaneChoice {
    Agent(AgentProviderChoice),
    Messages,
}

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

#[derive(Parser)]
struct SessionsArgs;

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
    Send {
        /// Immediately inject the created message into the agent input.
        #[arg(long)]
        inject: bool,
        agent_id: String,
        body: String,
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
struct MessageArgs {
    #[command(subcommand)]
    action: MessageAction,
}

#[derive(Subcommand)]
enum MessageAction {
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
        /// Immediately inject the created message into the resolved agent input.
        #[arg(long)]
        inject: bool,
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
        run_bare_tui_session(&socket_path).await?;
        return Ok(());
    };

    match command {
        Commands::Start(args) => {
            let panes = parse_start_panes(args.providers.as_deref())?;
            run_tui_session_with_startup_panes(&socket_path, panes).await?;
        }
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
            ProjectAction::InstallResultProtocol { path, global } => {
                let report = install_result_protocol(Path::new(&path), global)?;
                print_result_protocol_report(&report);
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
            AgentAction::Send {
                inject,
                agent_id,
                body,
            } => {
                send_message_and_maybe_inject(
                    &socket_path,
                    "agent",
                    agent_send_request(agent_id, body)?,
                    inject,
                )
                .await?;
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
        Commands::Sessions(_) => {
            let response = send_daemon_request(&socket_path, sessions_list_request()).await?;
            print_sessions_response(response)?;
        }
        Commands::Message(args) => match args.action {
            MessageAction::List => {
                let response = send_daemon_request(&socket_path, message_list_request()).await?;
                print_response("message", response)?;
            }
            MessageAction::History {
                limit,
                task,
                agent,
                kind,
                status,
            } => {
                let response = send_daemon_request(&socket_path, message_list_request()).await?;
                print_message_history_response(
                    response,
                    &MessageHistoryFilter {
                        limit,
                        task,
                        agent,
                        kind,
                        status,
                    },
                )?;
            }
            MessageAction::Show { message_id } => {
                let response =
                    send_daemon_request(&socket_path, message_show_request(message_id)).await?;
                print_response("message", response)?;
            }
            MessageAction::Send { inject, to, body } => {
                send_message_and_maybe_inject(
                    &socket_path,
                    "message",
                    message_send_request(to, body)?,
                    inject,
                )
                .await?;
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
            run_tui_session(&socket_path, Some(args.target)).await?;
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

fn tui_daemon_status_request() -> ClientRequest {
    ClientRequest::new("req_tui_status", IpcCommand::DaemonStatus, json!({}))
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

fn snapshot_request(target: String) -> ClientRequest {
    ClientRequest::new(
        "req_snapshot",
        IpcCommand::AgentSnapshot,
        json!({ "agent_id": target }),
    )
}

fn detach_request() -> ClientRequest {
    ClientRequest::new("req_detach", IpcCommand::ClientDetach, json!({}))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TuiSignal {
    Sigint,
}

fn tui_signal_effect(signal: TuiSignal) -> CommandEffect {
    match signal {
        TuiSignal::Sigint => CommandEffect::Detach,
    }
}

fn tui_close_request(effect: CommandEffect) -> Option<ClientRequest> {
    match effect {
        CommandEffect::Detach | CommandEffect::Quit => Some(detach_request()),
        _ => None,
    }
}

fn spawn_tui_signal_forwarder(signal_tx: mpsc::UnboundedSender<TuiSignal>) -> JoinHandle<()> {
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = signal_tx.send(TuiSignal::Sigint);
        }
    })
}

fn agent_ls_request() -> ClientRequest {
    ClientRequest::new("req_agent_ls", IpcCommand::DaemonStatus, json!({}))
}

fn sessions_list_request() -> ClientRequest {
    ClientRequest::new("req_sessions_list", IpcCommand::DaemonStatus, json!({}))
}

#[cfg(test)]
fn bare_session_spawn_request() -> ClientRequest {
    agent_spawn_for_provider_request(AgentProviderChoice::Codex)
}

#[cfg(test)]
fn agent_spawn_for_provider_request(provider: AgentProviderChoice) -> ClientRequest {
    agent_spawn_for_provider_request_with_size(provider, None)
}

fn agent_spawn_for_provider_request_with_size(
    provider: AgentProviderChoice,
    size: Option<TuiTerminalSize>,
) -> ClientRequest {
    agent_spawn_for_provider_request_with_id("req_agent_spawn_provider", provider, size)
}

fn agent_spawn_for_provider_request_with_id(
    request_id: impl Into<String>,
    provider: AgentProviderChoice,
    size: Option<TuiTerminalSize>,
) -> ClientRequest {
    let mut payload = json!({
        "provider": provider.provider(),
        "role": "implementer",
        "name": unique_agent_name(provider.default_name()),
    });
    if let Some(size) = size {
        payload["size"] = json!({
            "rows": size.rows,
            "cols": size.cols,
        });
    }

    ClientRequest::new(request_id, IpcCommand::AgentSpawn, payload)
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
            "name": unique_agent_name(&role),
        }),
    ))
}

fn unique_agent_name(prefix: &str) -> String {
    let prefix = sanitize_agent_name_prefix(prefix);
    let sequence = AGENT_NAME_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let entropy = nanos ^ ((std::process::id() as u64) << 32) ^ sequence;
    format!("{prefix}-{}", base36_suffix(entropy, 6))
}

fn sanitize_agent_name_prefix(prefix: &str) -> String {
    let mut output = String::new();
    let mut last_was_separator = false;
    for ch in prefix.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            output.push(ch.to_ascii_lowercase());
            last_was_separator = false;
        } else if !last_was_separator && !output.is_empty() {
            output.push('-');
            last_was_separator = true;
        }
    }
    while output.ends_with('-') {
        output.pop();
    }
    if output.is_empty() {
        "agent".to_string()
    } else {
        output
    }
}

fn base36_suffix(mut value: u64, len: usize) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut chars = vec!['0'; len];
    for slot in chars.iter_mut().rev() {
        *slot = DIGITS[(value % 36) as usize] as char;
        value /= 36;
    }
    chars.into_iter().collect()
}

fn agent_stop_request(agent_id: String) -> ClientRequest {
    ClientRequest::new(
        "req_agent_stop",
        IpcCommand::AgentStop,
        json!({ "agent_id": agent_id }),
    )
}

fn agent_send_request(agent_id: String, body: String) -> Result<ClientRequest> {
    if agent_id.trim().is_empty() {
        return Err(AgentmuxError::UserError(
            "agent message target must not be empty".to_string(),
        ));
    }
    if body.trim().is_empty() {
        return Err(AgentmuxError::UserError(
            "agent message body must not be empty".to_string(),
        ));
    }
    Ok(ClientRequest::new(
        "req_agent_send",
        IpcCommand::MessageCreate,
        json!({
            "to": normalize_agent_target(&agent_id),
            "body": body,
            "kind": "handoff",
            "priority": "normal",
            "delivery_mode": "inject_when_idle",
        }),
    ))
}

fn parse_start_panes(raw: Option<&str>) -> Result<Vec<StartupPaneChoice>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    raw.split(',')
        .map(str::trim)
        .filter(|pane| !pane.is_empty())
        .map(parse_start_pane_choice)
        .collect()
}

fn parse_start_pane_choice(raw: &str) -> Result<StartupPaneChoice> {
    match raw.to_ascii_lowercase().as_str() {
        "messages" | "message" | "message-bus" | "message_bus" | "conversation-list"
        | "conversation_list" => Ok(StartupPaneChoice::Messages),
        _ => parse_provider_choice(raw).map(StartupPaneChoice::Agent),
    }
}

fn parse_provider_choice(raw: &str) -> Result<AgentProviderChoice> {
    match raw.to_ascii_lowercase().as_str() {
        "claude" | "claude-code" | "claude_code" => Ok(AgentProviderChoice::Claude),
        "codex" => Ok(AgentProviderChoice::Codex),
        "agy" | "antigravity" => Ok(AgentProviderChoice::Agy),
        _ => Err(AgentmuxError::UserError(format!(
            "unknown start pane '{raw}' (expected claude, codex, agy, or messages)"
        ))),
    }
}

fn normalize_agent_target(raw: &str) -> String {
    let target = raw.trim();
    if target.starts_with("agent:") {
        target.to_string()
    } else {
        format!("agent:{target}")
    }
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

fn agent_resize_request(id: String, agent_id: String, size: TuiTerminalSize) -> ClientRequest {
    ClientRequest::new(
        id,
        IpcCommand::AgentResize,
        json!({
            "agent_id": agent_id,
            "rows": size.rows,
            "cols": size.cols,
        }),
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResultProtocolInstallReport {
    path: PathBuf,
    status: ResultProtocolInstallStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResultProtocolInstallStatus {
    Added,
    AlreadyPresent,
    Updated,
    Missing,
}

fn install_result_protocol(path: &Path, global: bool) -> Result<Vec<ResultProtocolInstallReport>> {
    let targets = if global {
        global_result_protocol_targets()?
    } else {
        local_result_protocol_targets(path)?
    };

    targets
        .into_iter()
        .map(|target| install_result_protocol_to_file(&target, global))
        .collect()
}

fn local_result_protocol_targets(path: &Path) -> Result<Vec<PathBuf>> {
    let dir = path.canonicalize().map_err(|error| {
        AgentmuxError::UserError(format!(
            "failed to resolve instruction directory '{}': {error}",
            path.display()
        ))
    })?;

    Ok(["AGENTS.md", "CLAUDE.md", "GEMINI.md"]
        .into_iter()
        .map(|name| dir.join(name))
        .collect())
}

fn global_result_protocol_targets() -> Result<Vec<PathBuf>> {
    let Some(home) = std::env::var_os("HOME") else {
        return Err(AgentmuxError::UserError(
            "HOME is not set; cannot install global result protocol".to_string(),
        ));
    };
    let home = PathBuf::from(home);
    Ok(vec![
        home.join(".codex/AGENTS.md"),
        home.join(".claude/CLAUDE.md"),
        home.join(".gemini/GEMINI.md"),
    ])
}

fn install_result_protocol_to_file(
    path: &Path,
    create_missing: bool,
) -> Result<ResultProtocolInstallReport> {
    if !path.exists() {
        if !create_missing {
            return Ok(ResultProtocolInstallReport {
                path: path.to_path_buf(),
                status: ResultProtocolInstallStatus::Missing,
            });
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                AgentmuxError::StoreError(format!(
                    "failed to create '{}': {error}",
                    parent.display()
                ))
            })?;
        }
        std::fs::write(path, format!("{RESULT_PROTOCOL_BLOCK}\n")).map_err(|error| {
            AgentmuxError::StoreError(format!("failed to write '{}': {error}", path.display()))
        })?;
        return Ok(ResultProtocolInstallReport {
            path: path.to_path_buf(),
            status: ResultProtocolInstallStatus::Added,
        });
    }

    let contents = std::fs::read_to_string(path).map_err(|error| {
        AgentmuxError::StoreError(format!("failed to read '{}': {error}", path.display()))
    })?;
    if let Some(next) = replace_result_protocol_block(&contents) {
        if next == contents {
            return Ok(ResultProtocolInstallReport {
                path: path.to_path_buf(),
                status: ResultProtocolInstallStatus::AlreadyPresent,
            });
        }
        std::fs::write(path, next).map_err(|error| {
            AgentmuxError::StoreError(format!("failed to write '{}': {error}", path.display()))
        })?;
        return Ok(ResultProtocolInstallReport {
            path: path.to_path_buf(),
            status: ResultProtocolInstallStatus::Updated,
        });
    }

    let mut next = contents;
    if !next.ends_with('\n') {
        next.push('\n');
    }
    next.push('\n');
    next.push_str(RESULT_PROTOCOL_BLOCK);
    next.push('\n');
    std::fs::write(path, next).map_err(|error| {
        AgentmuxError::StoreError(format!("failed to write '{}': {error}", path.display()))
    })?;

    Ok(ResultProtocolInstallReport {
        path: path.to_path_buf(),
        status: ResultProtocolInstallStatus::Added,
    })
}

fn replace_result_protocol_block(contents: &str) -> Option<String> {
    let start = contents.find(RESULT_PROTOCOL_MARKER_START)?;
    let after_start = start + RESULT_PROTOCOL_MARKER_START.len();
    let mut end = contents[after_start..]
        .find(RESULT_PROTOCOL_MARKER_END)
        .map(|relative| after_start + relative + RESULT_PROTOCOL_MARKER_END.len())
        .unwrap_or(contents.len());
    if contents[end..].starts_with('\n') {
        end += 1;
    }

    let mut next =
        String::with_capacity(contents.len() - (end - start) + RESULT_PROTOCOL_BLOCK.len() + 2);
    next.push_str(&contents[..start]);
    next.push_str(RESULT_PROTOCOL_BLOCK);
    next.push_str(&contents[end..]);
    Some(next)
}

fn print_result_protocol_report(report: &[ResultProtocolInstallReport]) {
    for entry in report {
        let status = match entry.status {
            ResultProtocolInstallStatus::Added => "added",
            ResultProtocolInstallStatus::AlreadyPresent => "already-present",
            ResultProtocolInstallStatus::Updated => "updated",
            ResultProtocolInstallStatus::Missing => "missing",
        };
        println!("{status}: {}", entry.path.display());
    }
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

async fn run_bare_tui_session(socket_path: &Path) -> Result<()> {
    run_tui_session(socket_path, None).await
}

#[cfg(test)]
fn agent_id_from_spawn_response(response: DaemonResponse) -> Result<String> {
    if !response.ok {
        return Err(response_error("agent.spawn", response));
    }

    response
        .payload
        .and_then(|payload| {
            payload
                .get("agent_id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .ok_or_else(|| AgentmuxError::IpcError("agent.spawn response missing agent_id".to_string()))
}

async fn run_tui_session(socket_path: &Path, target: Option<String>) -> Result<()> {
    run_tui_session_inner(socket_path, target, Vec::new()).await
}

async fn run_tui_session_with_startup_panes(
    socket_path: &Path,
    panes: Vec<StartupPaneChoice>,
) -> Result<()> {
    run_tui_session_inner(socket_path, None, panes).await
}

async fn run_tui_session_inner(
    socket_path: &Path,
    target: Option<String>,
    startup_panes: Vec<StartupPaneChoice>,
) -> Result<()> {
    ensure_daemon(socket_path).await?;
    let stream = UnixStream::connect(socket_path).await.map_err(|error| {
        AgentmuxError::IpcError(format!(
            "failed to connect daemon socket '{}': {error}",
            socket_path.display()
        ))
    })?;
    let (reader, writer) = stream.into_split();
    let mut reader = JsonlReader::new(BufReader::new(reader));
    let mut writer = JsonlWriter::new(writer);

    writer
        .write(&ClientHello::new(env!("CARGO_PKG_VERSION")))
        .await?;

    let status_request = tui_daemon_status_request();
    let status_request_id = status_request.id.clone();
    writer.write(&status_request).await?;
    let open_startup_messages = startup_panes
        .iter()
        .any(|pane| matches!(pane, StartupPaneChoice::Messages));
    let startup_spawn_requests = startup_panes
        .into_iter()
        .filter_map(|pane| match pane {
            StartupPaneChoice::Agent(provider) => Some(provider),
            StartupPaneChoice::Messages => None,
        })
        .enumerate()
        .map(|(index, provider)| {
            agent_spawn_for_provider_request_with_id(
                format!("req_start_agent_spawn_{index}"),
                provider,
                None,
            )
        })
        .collect::<Vec<_>>();
    for request in &startup_spawn_requests {
        writer.write(request).await?;
    }
    let startup_message_list_request = open_startup_messages.then(message_list_request);
    if let Some(request) = &startup_message_list_request {
        writer.write(request).await?;
    }
    let startup_spawn_request_ids = startup_spawn_requests
        .iter()
        .map(|request| request.id.clone())
        .collect::<Vec<_>>();
    let attach_and_snapshot = target.map(|target| {
        let snapshot_request = snapshot_request(target.clone());
        let attach_request = attach_request(target);
        (attach_request, snapshot_request)
    });
    if let Some((attach_request, snapshot_request)) = &attach_and_snapshot {
        writer.write(attach_request).await?;
        writer.write(snapshot_request).await?;
    }

    let mut state = TuiSessionState::default();
    let mut startup_agent_ids = Vec::new();
    if let Some((attach_request, snapshot_request)) = &attach_and_snapshot {
        let _startup_agent_ids = wait_for_tui_bootstrap(
            &mut reader,
            &mut state,
            &status_request_id,
            Some(&attach_request.id),
            Some(&snapshot_request.id),
            &startup_spawn_request_ids,
            startup_message_list_request
                .as_ref()
                .map(|request| request.id.as_str()),
        )
        .await?;
    } else {
        startup_agent_ids = wait_for_tui_bootstrap(
            &mut reader,
            &mut state,
            &status_request_id,
            None,
            None,
            &startup_spawn_request_ids,
            startup_message_list_request
                .as_ref()
                .map(|request| request.id.as_str()),
        )
        .await?;
        if open_startup_messages {
            state.open_conversation_list_pane();
        }
        if startup_agent_ids.is_empty() && !open_startup_messages {
            state.open_provider_picker();
        }
    }

    let terminal_io = CrosstermTerminalIo::new(io::stdout()).map_err(|error| {
        AgentmuxError::TerminalError(format!("failed to initialise terminal UI: {error}"))
    })?;
    let mut terminal = TerminalSession::enter(terminal_io).map_err(|error| {
        AgentmuxError::TerminalError(format!("failed to enter terminal UI: {error}"))
    })?;

    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            match reader.read::<DaemonStreamFrame>().await {
                Ok(Some(frame)) => {
                    if frame_tx.send(Ok(frame)).is_err() {
                        break;
                    }
                }
                Ok(None) => {
                    let _ = frame_tx.send(Err(AgentmuxError::IpcError(
                        "daemon closed the attached event stream".to_string(),
                    )));
                    break;
                }
                Err(error) => {
                    let _ = frame_tx.send(Err(error));
                    break;
                }
            }
        }
    });

    let renderer = TuiSessionRenderer::default();
    let mut keymap = KeymapDispatcher::default();
    let mut input_sequence = 0_u64;
    let mut resize_sequence = 0_u64;
    let mut copy_mode = false;
    let mut copy_drag_start: Option<CopyPoint> = None;
    let (signal_tx, mut signal_rx) = mpsc::unbounded_channel();
    let signal_task = spawn_tui_signal_forwarder(signal_tx);

    sync_current_terminal_pane_sizes(&mut writer, &mut state, &mut resize_sequence).await?;
    if let Some(first_agent_id) = startup_agent_ids.first() {
        writer
            .write(&attach_request(first_agent_id.clone()))
            .await?;
    }
    for agent_id in &startup_agent_ids {
        writer.write(&snapshot_request(agent_id.clone())).await?;
    }
    draw_tui_frame(&mut terminal, &renderer, &state)?;

    loop {
        while let Ok(frame) = frame_rx.try_recv() {
            // Stream-level errors (daemon closed / read failure) stay fatal via `?`.
            // A per-request response error during the session — e.g. a keystroke
            // forwarded to an agent with no live PTY — must NOT tear down the
            // cockpit; surface it as a notice and keep running.
            let frame = frame?;
            let spawned_agent_id = spawned_agent_id_from_frame(&frame);
            let _notice = apply_runtime_stream_frame(&mut state, frame);
            if let Some(agent_id) = spawned_agent_id {
                sync_current_terminal_pane_sizes(&mut writer, &mut state, &mut resize_sequence)
                    .await?;
                writer.write(&attach_request(agent_id.clone())).await?;
                writer.write(&snapshot_request(agent_id)).await?;
            }
            draw_tui_frame(&mut terminal, &renderer, &state)?;
        }

        if let Ok(signal) = signal_rx.try_recv() {
            if let Some(request) = tui_close_request(tui_signal_effect(signal)) {
                writer.write(&request).await?;
                break;
            }
        }

        if let Some(event) = terminal
            .io_mut()
            .poll_event(Duration::from_millis(16))
            .map_err(|error| AgentmuxError::TerminalError(format!("failed to read key: {error}")))?
        {
            let Event::Key(key) = event else {
                if let Event::Resize(cols, rows) = event {
                    for request in
                        resize_panes_for_terminal(&mut state, cols, rows, &mut resize_sequence)
                    {
                        writer.write(&request).await?;
                    }
                    draw_tui_frame(&mut terminal, &renderer, &state)?;
                } else if copy_mode && let Event::Mouse(mouse) = event {
                    let (cols, rows) = current_terminal_size()?;
                    if let Some(action) = copy_mode_mouse_action(
                        &mut state,
                        cols,
                        rows,
                        mouse.kind,
                        mouse.column,
                        mouse.row,
                        &mut copy_drag_start,
                    ) {
                        match action {
                            CopyModeAction::Redraw => {
                                draw_tui_frame(&mut terminal, &renderer, &state)?;
                            }
                            CopyModeAction::CopyAndExit(text) => {
                                terminal
                                    .io_mut()
                                    .copy_to_clipboard(&text)
                                    .map_err(|error| {
                                        AgentmuxError::TerminalError(format!(
                                            "failed to copy selection to clipboard: {error}"
                                        ))
                                    })?;
                                terminal
                                    .io_mut()
                                    .set_mouse_capture(false)
                                    .map_err(|error| {
                                        AgentmuxError::TerminalError(format!(
                                            "failed to disable mouse capture: {error}"
                                        ))
                                    })?;
                                copy_mode = false;
                                copy_drag_start = None;
                                state.reset_focused_pane_scroll();
                                state.clear_copy_selection();
                                draw_tui_frame(&mut terminal, &renderer, &state)?;
                            }
                        }
                    }
                } else if let Event::Mouse(mouse) = event
                    && let Some(delta) = mouse_scroll_delta(mouse.kind)
                {
                    let (cols, rows) = current_terminal_size()?;
                    if scroll_pane_at(&mut state, cols, rows, mouse.column, mouse.row, delta) {
                        draw_tui_frame(&mut terminal, &renderer, &state)?;
                    }
                }
                continue;
            };
            if copy_mode && copy_mode_key_exits(key.code, key.modifiers) {
                terminal
                    .io_mut()
                    .set_mouse_capture(false)
                    .map_err(|error| {
                        AgentmuxError::TerminalError(format!(
                            "failed to disable mouse capture: {error}"
                        ))
                    })?;
                copy_mode = false;
                copy_drag_start = None;
                state.reset_focused_pane_scroll();
                state.clear_copy_selection();
                draw_tui_frame(&mut terminal, &renderer, &state)?;
                continue;
            }
            let conversation_list_focused = state
                .layout()
                .focused()
                .is_some_and(|pane_id| state.is_conversation_list_pane(pane_id));
            let dispatch = keymap.dispatch_with_context(
                key,
                state.session_list_visible(),
                state.message_bus_visible(),
                state.provider_picker_visible(),
                conversation_list_focused,
            );
            if let Some(command) = match &dispatch {
                agentmux_tui::keymap::KeyDispatch::Command(command) => Some(*command),
                _ => None,
            } {
                match state.apply_command(command) {
                    CommandEffect::Continue => {
                        sync_current_terminal_pane_sizes(
                            &mut writer,
                            &mut state,
                            &mut resize_sequence,
                        )
                        .await?;
                        draw_tui_frame(&mut terminal, &renderer, &state)?;
                    }
                    CommandEffect::SpawnAgentPane(provider) => {
                        let spawn_size = current_terminal_size()
                            .ok()
                            .and_then(|(cols, rows)| pending_spawn_pane_size(&state, cols, rows));
                        writer
                            .write(&agent_spawn_for_provider_request_with_size(
                                provider, spawn_size,
                            ))
                            .await?;
                    }
                    CommandEffect::OpenConversationListPane => {
                        writer.write(&message_list_request()).await?;
                        sync_current_terminal_pane_sizes(
                            &mut writer,
                            &mut state,
                            &mut resize_sequence,
                        )
                        .await?;
                        draw_tui_frame(&mut terminal, &renderer, &state)?;
                    }
                    CommandEffect::StopPane(agent_id) => {
                        writer.write(&agent_stop_request(agent_id)).await?;
                    }
                    CommandEffect::RefreshMessages => {
                        writer.write(&message_list_request()).await?;
                        draw_tui_frame(&mut terminal, &renderer, &state)?;
                    }
                    CommandEffect::Unhandled(agentmux_tui::keymap::TuiCommand::EnterCopyMode) => {
                        if state.focused_pane().is_some() {
                            terminal.io_mut().set_mouse_capture(true).map_err(|error| {
                                AgentmuxError::TerminalError(format!(
                                    "failed to enable mouse capture: {error}"
                                ))
                            })?;
                            copy_mode = true;
                            copy_drag_start = None;
                            state.clear_copy_selection();
                            draw_tui_frame(&mut terminal, &renderer, &state)?;
                        }
                    }
                    CommandEffect::Detach => {
                        writer.write(&detach_request()).await?;
                        break;
                    }
                    CommandEffect::Quit => {
                        writer.write(&detach_request()).await?;
                        break;
                    }
                    CommandEffect::Unhandled(_) => {}
                }
                continue;
            }

            input_sequence = input_sequence.saturating_add(1);
            let request_id = format!("req_input_{input_sequence}");
            match dispatch_to_daemon_request(&state, request_id, dispatch) {
                Ok(Some(request)) => writer.write(&request).await?,
                Ok(None) | Err(InputForwardError::NoFocusedPane) => {}
                Err(error) => {
                    return Err(AgentmuxError::UserError(format!(
                        "failed to forward input to focused agent: {error:?}"
                    )));
                }
            }
        }
    }

    signal_task.abort();

    terminal.shutdown().map_err(|error| {
        AgentmuxError::TerminalError(format!("failed to restore terminal UI: {error}"))
    })
}

async fn wait_for_tui_bootstrap<R>(
    reader: &mut JsonlReader<R>,
    state: &mut TuiSessionState,
    status_request_id: &str,
    attach_request_id: Option<&str>,
    snapshot_request_id: Option<&str>,
    startup_spawn_request_ids: &[String],
    startup_message_list_request_id: Option<&str>,
) -> Result<Vec<String>>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let mut status_received = false;
    let mut attach_received = attach_request_id.is_none();
    let mut snapshot_received = snapshot_request_id.is_none();
    let mut startup_messages_received = startup_message_list_request_id.is_none();
    let mut startup_spawn_received = BTreeSet::new();
    let mut startup_agent_ids = Vec::new();

    while !(status_received
        && attach_received
        && snapshot_received
        && startup_messages_received
        && startup_spawn_received.len() == startup_spawn_request_ids.len())
    {
        let frame = reader.read::<DaemonStreamFrame>().await?.ok_or_else(|| {
            AgentmuxError::IpcError("daemon closed before TUI attach completed".to_string())
        })?;
        let spawned_agent_id = spawned_agent_id_from_frame(&frame);
        if let Some(response_id) = apply_tui_stream_frame(state, frame)? {
            if response_id == status_request_id {
                status_received = true;
            }
            if Some(response_id.as_str()) == attach_request_id {
                attach_received = true;
            }
            if Some(response_id.as_str()) == snapshot_request_id {
                snapshot_received = true;
            }
            if Some(response_id.as_str()) == startup_message_list_request_id {
                startup_messages_received = true;
            }
            if startup_spawn_request_ids.contains(&response_id) {
                startup_spawn_received.insert(response_id);
                if let Some(agent_id) = spawned_agent_id {
                    startup_agent_ids.push(agent_id);
                }
            }
        }
    }

    Ok(startup_agent_ids)
}

fn apply_tui_stream_frame(
    state: &mut TuiSessionState,
    frame: DaemonStreamFrame,
) -> Result<Option<String>> {
    match frame {
        DaemonStreamFrame::Response(response) => {
            if !response.ok {
                return Err(response_error("tui", response));
            }
            if response.id == "req_tui_status" {
                state.apply_daemon_status(&response.payload.clone().unwrap_or_else(|| json!({})));
            }
            if response.id == "req_snapshot" {
                state.apply_snapshot(&response.payload.clone().unwrap_or_else(|| json!({})));
            }
            if response.id == "req_message_list" {
                state.apply_message_list_payload(
                    &response.payload.clone().unwrap_or_else(|| json!({})),
                );
            }
            if is_agent_spawn_response_id(&response.id) {
                if let Some(payload) = response.payload.as_ref() {
                    state.apply_daemon_status(&json!({ "agents": [payload] }));
                }
            }
            Ok(Some(response.id))
        }
        DaemonStreamFrame::Event(event) => {
            state.apply_event(&event);
            Ok(None)
        }
    }
}

fn spawned_agent_id_from_frame(frame: &DaemonStreamFrame) -> Option<String> {
    let DaemonStreamFrame::Response(response) = frame else {
        return None;
    };
    if !response.ok || !is_agent_spawn_response_id(&response.id) {
        return None;
    }
    response
        .payload
        .as_ref()
        .and_then(|payload| payload.get("agent_id"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn is_agent_spawn_response_id(response_id: &str) -> bool {
    response_id == "req_agent_spawn_provider"
        || response_id == "req_bare_agent_spawn"
        || response_id.starts_with("req_start_agent_spawn_")
}

/// Apply a stream frame during the interactive loop. Unlike bootstrap, a failed
/// per-request response (e.g. input to an agent with no live PTY) is non-fatal:
/// the error text is returned as a notice so the session can keep running.
fn apply_runtime_stream_frame(
    state: &mut TuiSessionState,
    frame: DaemonStreamFrame,
) -> Option<String> {
    match apply_tui_stream_frame(state, frame) {
        Ok(_) => None,
        Err(error) => Some(error.to_string()),
    }
}

fn draw_tui_frame<T: TerminalIo>(
    terminal: &mut TerminalSession<T>,
    renderer: &TuiSessionRenderer,
    state: &TuiSessionState,
) -> Result<()> {
    terminal
        .io_mut()
        .draw(|frame| renderer.render(frame.area(), state, frame.buffer_mut()))
        .map_err(|error| AgentmuxError::TerminalError(format!("failed to draw TUI: {error}")))
}

async fn sync_current_terminal_pane_sizes<W>(
    writer: &mut JsonlWriter<W>,
    state: &mut TuiSessionState,
    resize_sequence: &mut u64,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let (cols, rows) = current_terminal_size()?;
    for request in resize_panes_for_terminal(state, cols, rows, resize_sequence) {
        writer.write(&request).await?;
    }
    Ok(())
}

fn current_terminal_size() -> Result<(u16, u16)> {
    crossterm_terminal::size().map_err(|error| {
        AgentmuxError::TerminalError(format!("failed to read terminal size: {error}"))
    })
}

fn pending_spawn_pane_size(
    state: &TuiSessionState,
    terminal_cols: u16,
    terminal_rows: u16,
) -> Option<TuiTerminalSize> {
    let mut snapshot = state.layout().snapshot();
    let pending_pane = "__agentmux_pending_spawn__".to_string();
    snapshot.panes.push(pending_pane.clone());
    snapshot.focused = Some(pending_pane.clone());

    let area = Rect::new(0, 0, terminal_cols, terminal_rows);
    PaneLayout::restore(snapshot)
        .pane_rects(area)
        .into_iter()
        .find_map(|(agent_id, rect)| {
            if agent_id != pending_pane {
                return None;
            }
            let (rows, cols) = PaneLayout::pane_inner_size(rect);
            (rows > 0 && cols > 0).then_some(TuiTerminalSize { rows, cols })
        })
}

fn resize_panes_for_terminal(
    state: &mut TuiSessionState,
    terminal_cols: u16,
    terminal_rows: u16,
    resize_sequence: &mut u64,
) -> Vec<ClientRequest> {
    resize_pane_sizes(state, terminal_cols, terminal_rows)
        .into_iter()
        .map(|(agent_id, size)| {
            *resize_sequence = resize_sequence.saturating_add(1);
            state.resize_pane(&agent_id, size);
            agent_resize_request(format!("req_resize_{resize_sequence}"), agent_id, size)
        })
        .collect()
}

fn mouse_scroll_delta(kind: MouseEventKind) -> Option<isize> {
    match kind {
        MouseEventKind::ScrollUp => Some(3),
        MouseEventKind::ScrollDown => Some(-3),
        _ => None,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum CopyModeAction {
    Redraw,
    CopyAndExit(String),
}

fn copy_mode_key_exits(code: KeyCode, modifiers: KeyModifiers) -> bool {
    if modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
        return false;
    }
    matches!(code, KeyCode::Esc | KeyCode::Char('q'))
}

fn copy_mode_mouse_action(
    state: &mut TuiSessionState,
    terminal_cols: u16,
    terminal_rows: u16,
    kind: MouseEventKind,
    mouse_col: u16,
    mouse_row: u16,
    drag_start: &mut Option<CopyPoint>,
) -> Option<CopyModeAction> {
    if let Some(delta) = mouse_scroll_delta(kind) {
        return (!matches!(state.scroll_focused_pane(delta), StateChange::Ignored))
            .then_some(CopyModeAction::Redraw);
    }

    let agent_id = state.layout().focused()?.to_string();
    let inner = focused_pane_inner_rect(state, terminal_cols, terminal_rows)?;

    match kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if !rect_contains(inner, mouse_col, mouse_row) {
                return None;
            }
            let point = copy_point_from_mouse(inner, mouse_col, mouse_row);
            *drag_start = Some(point);
            state.set_copy_selection(CopySelection::new(agent_id, point, point));
            Some(CopyModeAction::Redraw)
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            let start = (*drag_start)?;
            let point = copy_point_from_mouse(inner, mouse_col, mouse_row);
            state.set_copy_selection(CopySelection::new(agent_id, start, point));
            Some(CopyModeAction::Redraw)
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let start = (*drag_start)?;
            let point = copy_point_from_mouse(inner, mouse_col, mouse_row);
            let selection = CopySelection::new(agent_id, start, point);
            let text = selected_text(state, &selection, inner.height);
            state.set_copy_selection(selection);
            *drag_start = None;
            Some(CopyModeAction::CopyAndExit(text))
        }
        _ => None,
    }
}

fn focused_pane_inner_rect(
    state: &TuiSessionState,
    terminal_cols: u16,
    terminal_rows: u16,
) -> Option<Rect> {
    let focused = state.layout().focused()?;
    let area = Rect::new(0, 0, terminal_cols, terminal_rows);
    state
        .layout()
        .pane_rects(area)
        .into_iter()
        .find_map(|(agent_id, rect)| (agent_id == focused).then_some(inner_rect(rect)))
        .filter(|rect| rect.width > 0 && rect.height > 0)
}

fn inner_rect(rect: Rect) -> Rect {
    Rect::new(
        rect.x.saturating_add(1),
        rect.y.saturating_add(1),
        rect.width.saturating_sub(2),
        rect.height.saturating_sub(2),
    )
}

fn copy_point_from_mouse(inner: Rect, mouse_col: u16, mouse_row: u16) -> CopyPoint {
    CopyPoint {
        row: mouse_row
            .saturating_sub(inner.y)
            .min(inner.height.saturating_sub(1)),
        col: mouse_col
            .saturating_sub(inner.x)
            .min(inner.width.saturating_sub(1)),
    }
}

fn selected_text(
    state: &TuiSessionState,
    selection: &CopySelection,
    viewport_height: u16,
) -> String {
    let Some(pane) = state.pane(&selection.agent_id) else {
        return String::new();
    };
    let grid = pane.grid();
    let total_rows = grid.scrollback().len() + usize::from(grid.rows());
    let visible_rows = usize::from(viewport_height).min(total_rows);
    if visible_rows == 0 {
        return String::new();
    }
    let start_history_row = total_rows.saturating_sub(visible_rows).saturating_sub(
        pane.scroll_offset()
            .min(total_rows.saturating_sub(visible_rows)),
    );

    let (start, end) = selection.normalized();
    let mut lines = Vec::new();
    for row in start.row..=end.row {
        let mut line = String::new();
        let first_col = if row == start.row { start.col } else { 0 };
        let last_col = if row == end.row {
            end.col
        } else {
            grid.cols().saturating_sub(1)
        };
        let history_row = start_history_row + usize::from(row);
        for col in first_col..=last_col.min(grid.cols().saturating_sub(1)) {
            if let Some(ch) = visible_cell_char(grid, history_row, col) {
                line.push(ch);
            }
        }
        lines.push(line.trim_end().to_string());
    }
    lines.join("\n")
}

fn visible_cell_char(
    grid: &agentmux_terminal::ScreenGrid,
    history_row: usize,
    col: u16,
) -> Option<char> {
    let scrollback_rows = grid.scrollback().len();
    if history_row < scrollback_rows {
        return grid
            .scrollback()
            .get(history_row)
            .and_then(|line| line.cells().get(usize::from(col)))
            .map(|cell| cell.ch);
    }
    let grid_row = history_row.checked_sub(scrollback_rows)?;
    let grid_row = u16::try_from(grid_row).ok()?;
    grid.cell(grid_row, col).map(|cell| cell.ch)
}

fn scroll_pane_at(
    state: &mut TuiSessionState,
    terminal_cols: u16,
    terminal_rows: u16,
    mouse_col: u16,
    mouse_row: u16,
    delta: isize,
) -> bool {
    let area = Rect::new(0, 0, terminal_cols, terminal_rows);
    let target = state
        .layout()
        .pane_rects(area)
        .into_iter()
        .find_map(|(agent_id, rect)| rect_contains(rect, mouse_col, mouse_row).then_some(agent_id))
        .or_else(|| state.layout().focused().map(ToOwned::to_owned));
    let Some(agent_id) = target else {
        return false;
    };
    !matches!(state.scroll_pane(&agent_id, delta), StateChange::Ignored)
}

fn rect_contains(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x
        && row >= rect.y
        && col < rect.x.saturating_add(rect.width)
        && row < rect.y.saturating_add(rect.height)
}

fn resize_pane_sizes(
    state: &TuiSessionState,
    terminal_cols: u16,
    terminal_rows: u16,
) -> Vec<(String, TuiTerminalSize)> {
    let area = Rect::new(0, 0, terminal_cols, terminal_rows);
    state
        .layout()
        .pane_rects(area)
        .into_iter()
        .filter_map(|(agent_id, rect)| {
            state.pane(&agent_id)?;
            let (rows, cols) = PaneLayout::pane_inner_size(rect);
            (rows > 0 && cols > 0).then_some((agent_id, TuiTerminalSize { rows, cols }))
        })
        .collect()
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

async fn send_message_and_maybe_inject(
    socket_path: &Path,
    label: &str,
    create_request: ClientRequest,
    inject: bool,
) -> Result<()> {
    let create_response = send_daemon_request(socket_path, create_request).await?;
    if !inject {
        return print_response(label, create_response);
    }
    if !create_response.ok {
        return Err(response_error(label, create_response));
    }

    let payload = create_response.payload.unwrap_or_else(|| json!({}));
    let message_id = payload
        .get("message_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AgentmuxError::IpcError("message.create response missing message_id".to_string())
        })?
        .to_string();
    let inject_response =
        send_daemon_request(socket_path, message_inject_request(message_id)).await?;
    print_response(label, inject_response)
}

fn response_error(label: &str, response: DaemonResponse) -> AgentmuxError {
    let error = response.error.unwrap_or_else(|| {
        agentmux_ipc::ErrorBody::new(
            "missing_error_body",
            "daemon returned an error without an error body",
        )
    });
    AgentmuxError::UserError(format!(
        "{label} request failed: {} ({}){}",
        error.message,
        error.code,
        error
            .hint
            .map(|hint| format!("; hint: {hint}"))
            .unwrap_or_default()
    ))
}

fn print_response(label: &str, response: DaemonResponse) -> Result<()> {
    if !response.ok {
        return Err(response_error(label, response));
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

fn print_sessions_response(response: DaemonResponse) -> Result<()> {
    if !response.ok {
        return Err(response_error("sessions", response));
    }

    let payload = response.payload.unwrap_or_else(|| json!({}));
    print!("{}", format_sessions_payload(&payload));
    Ok(())
}

#[derive(Debug, Clone, Default)]
struct MessageHistoryFilter {
    limit: usize,
    task: Option<String>,
    agent: Option<String>,
    kind: Option<String>,
    status: Option<String>,
}

fn print_message_history_response(
    response: DaemonResponse,
    filter: &MessageHistoryFilter,
) -> Result<()> {
    if !response.ok {
        return Err(response_error("message history", response));
    }

    let payload = response.payload.unwrap_or_else(|| json!({}));
    print!("{}", format_message_history_payload(&payload, filter));
    Ok(())
}

fn format_sessions_payload(payload: &Value) -> String {
    let sessions = payload
        .get("agents")
        .and_then(Value::as_array)
        .map(|agents| {
            agents
                .iter()
                .filter(|agent| {
                    agent
                        .get("has_process")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if sessions.is_empty() {
        return "no running sessions\n".to_string();
    }

    let mut output = String::from("ID NAME ROLE STATUS INPUT PID CLIENTS\n");
    for session in sessions {
        let id = session.get("id").and_then(Value::as_str).unwrap_or("-");
        let name = session.get("name").and_then(Value::as_str).unwrap_or("-");
        let role = session.get("role").and_then(Value::as_str).unwrap_or("-");
        let status = session.get("status").and_then(Value::as_str).unwrap_or("-");
        let input = if session
            .get("input_ready")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            "ready"
        } else {
            "-"
        };
        let pid = session
            .get("process_id")
            .and_then(Value::as_u64)
            .map(|pid| pid.to_string())
            .unwrap_or_else(|| "-".to_string());
        let clients = session
            .get("attached_clients")
            .and_then(Value::as_array)
            .map(|clients| clients.len().to_string())
            .unwrap_or_else(|| "0".to_string());
        output.push_str(&format!(
            "{id} {name} {role} {status} {input} {pid} {clients}\n"
        ));
    }
    output
}

fn format_message_history_payload(payload: &Value, filter: &MessageHistoryFilter) -> String {
    let mut messages = payload
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages
                .iter()
                .filter(|message| message_matches_history_filter(message, filter))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    messages.sort_by(|left, right| {
        message_string_field(right, "created_at").cmp(&message_string_field(left, "created_at"))
    });

    let limit = filter.limit.max(1);
    if messages.is_empty() {
        return "no messages\n".to_string();
    }

    let mut output = String::from(
        "CREATED              STATUS               KIND                 FROM                 TO                   ID                   BODY\n",
    );
    for message in messages.into_iter().take(limit) {
        let created = compact_timestamp(&message_string_field(message, "created_at"));
        let status = message_string_field(message, "delivery_status");
        let kind = message_string_field(message, "kind");
        let from = message_endpoint_label(message.get("from"));
        let to = message_endpoint_label(message.get("to"));
        let id = message_string_field(message, "message_id");
        let body = truncate_for_table(&message_string_field(message, "body"), 72);
        output.push_str(&format!(
            "{:<20} {:<20} {:<20} {:<20} {:<20} {:<20} {}\n",
            truncate_for_table(&created, 20),
            truncate_for_table(&status, 20),
            truncate_for_table(&kind, 20),
            truncate_for_table(&from, 20),
            truncate_for_table(&to, 20),
            truncate_for_table(&id, 20),
            body,
        ));
    }
    output
}

fn message_matches_history_filter(message: &Value, filter: &MessageHistoryFilter) -> bool {
    if let Some(task) = filter.task.as_deref() {
        if message_string_field(message, "task_id") != task {
            return false;
        }
    }

    if let Some(kind) = filter.kind.as_deref() {
        if !message_string_field(message, "kind").eq_ignore_ascii_case(kind) {
            return false;
        }
    }

    if let Some(status) = filter.status.as_deref() {
        if !message_string_field(message, "delivery_status").eq_ignore_ascii_case(status) {
            return false;
        }
    }

    if let Some(agent) = filter.agent.as_deref() {
        let from = message_endpoint_label(message.get("from"));
        let to = message_endpoint_label(message.get("to"));
        if from != agent && to != agent && !from.ends_with(agent) && !to.ends_with(agent) {
            return false;
        }
    }

    true
}

fn message_string_field(message: &Value, field: &str) -> String {
    message
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| "-".to_string())
}

fn message_endpoint_label(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return "-".to_string();
    };
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let id = value.get("id").and_then(Value::as_str).unwrap_or("-");
    if id == "-" {
        kind.to_string()
    } else {
        format!("{kind}:{id}")
    }
}

fn compact_timestamp(value: &str) -> String {
    value
        .strip_suffix("+00:00")
        .unwrap_or(value)
        .replace('T', " ")
}

fn truncate_for_table(value: &str, max_chars: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    let mut truncated = normalized
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    truncated.push('…');
    truncated
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
    fn tui_bootstrap_requests_status_attach_and_snapshot() {
        let status = tui_daemon_status_request();
        let attach = attach_request("agent_01HX".to_string());
        let snapshot = snapshot_request("agent_01HX".to_string());

        assert_eq!(status.id, "req_tui_status");
        assert_eq!(status.command, IpcCommand::DaemonStatus);
        assert_eq!(status.payload, json!({}));
        assert_eq!(attach.id, "req_attach");
        assert_eq!(attach.command, IpcCommand::ClientAttach);
        assert_eq!(attach.payload["agent_id"], "agent_01HX");
        assert_eq!(snapshot.id, "req_snapshot");
        assert_eq!(snapshot.command, IpcCommand::AgentSnapshot);
        assert_eq!(snapshot.payload["agent_id"], "agent_01HX");
    }

    #[test]
    fn bare_session_spawn_request_registers_default_coding_agent() {
        let request = bare_session_spawn_request();

        assert_eq!(request.id, "req_agent_spawn_provider");
        assert_eq!(request.command, IpcCommand::AgentSpawn);
        assert_eq!(request.payload["provider"], "codex");
        assert_eq!(request.payload["role"], "implementer");
        let name = request.payload["name"].as_str().unwrap();
        assert!(name.starts_with("codex-"));
        assert_eq!(name.len(), "codex-".len() + 6);
    }

    #[test]
    fn provider_spawn_request_registers_selected_coding_agent() {
        let request = agent_spawn_for_provider_request(AgentProviderChoice::Agy);

        assert_eq!(request.id, "req_agent_spawn_provider");
        assert_eq!(request.command, IpcCommand::AgentSpawn);
        assert_eq!(request.payload["provider"], "agy");
        assert_eq!(request.payload["role"], "implementer");
        let name = request.payload["name"].as_str().unwrap();
        assert!(name.starts_with("agy-"));
        assert_eq!(name.len(), "agy-".len() + 6);
    }

    #[test]
    fn provider_spawn_request_can_include_initial_pty_size() {
        let request = agent_spawn_for_provider_request_with_size(
            AgentProviderChoice::Codex,
            Some(TuiTerminalSize { rows: 28, cols: 88 }),
        );

        assert_eq!(request.id, "req_agent_spawn_provider");
        assert_eq!(request.command, IpcCommand::AgentSpawn);
        assert_eq!(request.payload["provider"], "codex");
        assert_eq!(request.payload["size"], json!({ "rows": 28, "cols": 88 }));
    }

    #[test]
    fn start_command_accepts_comma_separated_providers() {
        let cli = Cli::try_parse_from(["agentmux", "start", "agy,messages,codex"]).unwrap();
        let Some(Commands::Start(args)) = cli.command else {
            panic!("expected start command");
        };

        assert_eq!(
            parse_start_panes(args.providers.as_deref()).unwrap(),
            vec![
                StartupPaneChoice::Agent(AgentProviderChoice::Agy),
                StartupPaneChoice::Messages,
                StartupPaneChoice::Agent(AgentProviderChoice::Codex)
            ]
        );
    }

    #[test]
    fn startup_spawn_request_uses_trackable_response_id() {
        let request = agent_spawn_for_provider_request_with_id(
            "req_start_agent_spawn_0",
            AgentProviderChoice::Agy,
            None,
        );

        assert_eq!(request.id, "req_start_agent_spawn_0");
        assert!(is_agent_spawn_response_id(&request.id));
        assert_eq!(request.command, IpcCommand::AgentSpawn);
        assert_eq!(request.payload["provider"], "agy");

        let frame = DaemonStreamFrame::Response(DaemonResponse::ok(
            "req_start_agent_spawn_0",
            json!({ "agent_id": "agent_01KBQ4Y8T3BHQP4FPY6Y1VPD2K" }),
        ));
        assert_eq!(
            spawned_agent_id_from_frame(&frame).as_deref(),
            Some("agent_01KBQ4Y8T3BHQP4FPY6Y1VPD2K")
        );
    }

    #[test]
    fn bare_spawn_response_yields_agent_id_for_tui_attach() {
        let agent_id = agent_id_from_spawn_response(DaemonResponse::ok(
            "req_bare_agent_spawn",
            json!({ "agent_id": "agent_01KBQ4Y8T3BHQP4FPY6Y1VPD2K" }),
        ))
        .unwrap();

        assert_eq!(agent_id, "agent_01KBQ4Y8T3BHQP4FPY6Y1VPD2K");
    }

    #[test]
    fn bare_spawn_response_requires_agent_id() {
        let error =
            agent_id_from_spawn_response(DaemonResponse::ok("req_bare_agent_spawn", json!({})))
                .unwrap_err();

        assert!(error.to_string().contains("missing agent_id"));
    }

    #[test]
    fn detach_request_uses_client_detach_ipc_command() {
        let request = detach_request();

        assert_eq!(request.id, "req_detach");
        assert_eq!(request.command, IpcCommand::ClientDetach);
        assert_eq!(request.payload, json!({}));
    }

    #[test]
    fn sigint_requests_tui_detach_for_terminal_restoring_shutdown_path() {
        assert_eq!(tui_signal_effect(TuiSignal::Sigint), CommandEffect::Detach);
    }

    #[test]
    fn quit_closes_tui_client_without_stopping_agent_sessions() {
        let request = tui_close_request(CommandEffect::Quit).expect("close request");

        assert_eq!(request.command, IpcCommand::ClientDetach);
        assert_eq!(request.payload, json!({}));
    }

    #[test]
    fn agent_resize_request_uses_resize_ipc_command() {
        let request = agent_resize_request(
            "req_resize_1".to_string(),
            "agent_001".to_string(),
            TuiTerminalSize { rows: 22, cols: 78 },
        );

        assert_eq!(request.id, "req_resize_1");
        assert_eq!(request.command, IpcCommand::AgentResize);
        assert_eq!(
            request.payload,
            json!({ "agent_id": "agent_001", "rows": 22, "cols": 78 })
        );
    }

    #[test]
    fn resize_pane_sizes_use_inner_pane_dimensions() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "agents": [
                {"id": "agent_a", "name": "a"},
                {"id": "agent_b", "name": "b"}
            ]
        }));

        let sizes = resize_pane_sizes(&state, 100, 24);

        assert_eq!(
            sizes,
            vec![
                (
                    "agent_a".to_string(),
                    TuiTerminalSize { rows: 22, cols: 48 }
                ),
                (
                    "agent_b".to_string(),
                    TuiTerminalSize { rows: 22, cols: 48 }
                ),
            ]
        );
    }

    #[test]
    fn resize_pane_sizes_ignore_local_conversation_list_pane() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "agents": [
                {"id": "agent_a", "name": "a"}
            ]
        }));
        state.open_conversation_list_pane();

        let sizes = resize_pane_sizes(&state, 100, 24);

        assert_eq!(
            sizes,
            vec![(
                "agent_a".to_string(),
                TuiTerminalSize { rows: 22, cols: 48 }
            )]
        );
    }

    #[test]
    fn pending_spawn_pane_size_matches_hypothetical_new_pane_inner_dimensions() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "agents": [
                {"id": "agent_a", "name": "a"}
            ]
        }));

        let size = pending_spawn_pane_size(&state, 100, 24);

        assert_eq!(size, Some(TuiTerminalSize { rows: 22, cols: 48 }));
    }

    #[test]
    fn pending_spawn_pane_size_uses_full_inner_area_when_first_pane() {
        let state = TuiSessionState::default();

        let size = pending_spawn_pane_size(&state, 100, 24);

        assert_eq!(size, Some(TuiTerminalSize { rows: 22, cols: 98 }));
    }

    #[test]
    fn resize_panes_for_terminal_updates_state_and_returns_resize_requests() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "agents": [
                {"id": "agent_a", "name": "a"}
            ]
        }));
        let mut sequence = 7;

        let requests = resize_panes_for_terminal(&mut state, 90, 30, &mut sequence);

        assert_eq!(sequence, 8);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].id, "req_resize_8");
        assert_eq!(
            requests[0].payload,
            json!({ "agent_id": "agent_a", "rows": 28, "cols": 88 })
        );
        let pane = state.pane("agent_a").expect("pane");
        assert_eq!(pane.grid().rows(), 28);
        assert_eq!(pane.grid().cols(), 88);
    }

    #[test]
    fn mouse_scroll_helpers_target_hovered_pane() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "agents": [
                {"id": "agent_a", "name": "a"},
                {"id": "agent_b", "name": "b"}
            ]
        }));

        assert_eq!(mouse_scroll_delta(MouseEventKind::ScrollUp), Some(3));
        assert_eq!(mouse_scroll_delta(MouseEventKind::ScrollDown), Some(-3));
        assert!(scroll_pane_at(&mut state, 100, 24, 75, 2, 3));
        assert_eq!(state.pane("agent_a").expect("pane a").scroll_offset(), 0);
        assert_eq!(state.pane("agent_b").expect("pane b").scroll_offset(), 3);

        assert!(scroll_pane_at(&mut state, 100, 24, 75, 2, -1));
        assert_eq!(state.pane("agent_b").expect("pane b").scroll_offset(), 2);
    }

    #[test]
    fn copy_mode_drag_targets_only_focused_pane_inner_area() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "agents": [
                {"id": "agent_a", "name": "a"},
                {"id": "agent_b", "name": "b"}
            ]
        }));
        assert!(state.layout_mut().focus("agent_b"));
        state.resize_pane("agent_b", TuiTerminalSize { rows: 3, cols: 8 });
        state.apply_event(&agentmux_ipc::DaemonEvent::new(
            agentmux_ipc::protocol::IpcEventKind::PtyOutputChunk,
            json!({ "agent_id": "agent_b", "text": "alpha\nbeta\n" }),
        ));

        let inner = focused_pane_inner_rect(&state, 20, 5).expect("focused inner rect");
        assert_eq!(inner, Rect::new(11, 1, 8, 3));
        let mut drag_start = None;

        assert_eq!(
            copy_mode_mouse_action(
                &mut state,
                20,
                5,
                MouseEventKind::Down(MouseButton::Left),
                1,
                1,
                &mut drag_start,
            ),
            None
        );
        assert!(state.copy_selection().is_none());

        assert_eq!(
            copy_mode_mouse_action(
                &mut state,
                20,
                5,
                MouseEventKind::Down(MouseButton::Left),
                inner.x + 1,
                inner.y,
                &mut drag_start,
            ),
            Some(CopyModeAction::Redraw)
        );
        assert_eq!(
            copy_mode_mouse_action(
                &mut state,
                20,
                5,
                MouseEventKind::Up(MouseButton::Left),
                inner.x + 3,
                inner.y + 1,
                &mut drag_start,
            ),
            Some(CopyModeAction::CopyAndExit("lpha\nbeta".to_string()))
        );
    }

    #[test]
    fn tui_stream_frame_seeds_status_and_applies_events() {
        let mut state = TuiSessionState::default();
        let status = DaemonResponse::ok(
            "req_tui_status",
            json!({
                "agents": [
                    {
                        "id": "agent_01KBKX3F4SPGZ1A0JMQJEFAV7B",
                        "name": "impl",
                        "status": "ready"
                    }
                ]
            }),
        );

        let response_id =
            apply_tui_stream_frame(&mut state, DaemonStreamFrame::Response(status)).unwrap();

        assert_eq!(response_id.as_deref(), Some("req_tui_status"));
        assert_eq!(
            state.layout().focused(),
            Some("agent_01KBKX3F4SPGZ1A0JMQJEFAV7B")
        );
        assert_eq!(
            state
                .pane("agent_01KBKX3F4SPGZ1A0JMQJEFAV7B")
                .and_then(|pane| pane.status()),
            Some("ready")
        );

        apply_tui_stream_frame(
            &mut state,
            DaemonStreamFrame::Event(agentmux_ipc::DaemonEvent::new(
                agentmux_ipc::IpcEventKind::PtyOutputChunk,
                json!({
                    "agent_id": "agent_01KBKX3F4SPGZ1A0JMQJEFAV7B",
                    "text": "hi"
                }),
            )),
        )
        .unwrap();

        assert_eq!(
            state
                .pane("agent_01KBKX3F4SPGZ1A0JMQJEFAV7B")
                .unwrap()
                .grid()
                .line_text(0)
                .unwrap()
                .trim_end(),
            "hi"
        );
    }

    #[test]
    fn tui_stream_frame_updates_message_bus_from_message_list_response() {
        let mut state = TuiSessionState::default();
        let response = DaemonResponse::ok(
            "req_message_list",
            json!({
                "messages": [
                    {
                        "message_id": "msg_1",
                        "created_at": "2026-06-04T02:00:00+00:00",
                        "delivery_status": "delivered",
                        "kind": "handoff",
                        "from": { "kind": "agent", "id": "planner" },
                        "to": { "kind": "agent", "id": "impl" },
                        "body": "continue"
                    }
                ]
            }),
        );

        let response_id =
            apply_tui_stream_frame(&mut state, DaemonStreamFrame::Response(response)).unwrap();

        assert_eq!(response_id.as_deref(), Some("req_message_list"));
        assert_eq!(state.messages().len(), 1);
        assert_eq!(state.messages()[0].message_id, "msg_1");
    }

    #[test]
    fn tui_stream_frame_adds_spawned_provider_agent() {
        let mut state = TuiSessionState::default();
        let response = DaemonResponse::ok(
            "req_agent_spawn_provider",
            json!({
                "agent_id": "agent_01KBQ4Y8T3BHQP4FPY6Y1VPD2K",
                "name": "codex",
                "process_id": 42
            }),
        );

        let frame = DaemonStreamFrame::Response(response);
        assert_eq!(
            spawned_agent_id_from_frame(&frame).as_deref(),
            Some("agent_01KBQ4Y8T3BHQP4FPY6Y1VPD2K")
        );

        let response_id = apply_tui_stream_frame(&mut state, frame).unwrap();

        assert_eq!(response_id.as_deref(), Some("req_agent_spawn_provider"));
        assert_eq!(
            state.layout().focused(),
            Some("agent_01KBQ4Y8T3BHQP4FPY6Y1VPD2K")
        );
        assert_eq!(
            state
                .pane("agent_01KBQ4Y8T3BHQP4FPY6Y1VPD2K")
                .expect("spawned pane")
                .name(),
            "codex"
        );
    }

    #[test]
    fn tui_stream_frame_restores_snapshot_response() {
        let mut state = TuiSessionState::default();
        let snapshot = DaemonResponse::ok(
            "req_snapshot",
            json!({
                "agent_id": "agent_01KBKX3F4SPGZ1A0JMQJEFAV7B",
                "name": "impl",
                "rows": 2,
                "cols": 4,
                "lines": ["done", ">   "]
            }),
        );

        let response_id =
            apply_tui_stream_frame(&mut state, DaemonStreamFrame::Response(snapshot)).unwrap();

        assert_eq!(response_id.as_deref(), Some("req_snapshot"));
        assert_eq!(
            state
                .pane("agent_01KBKX3F4SPGZ1A0JMQJEFAV7B")
                .unwrap()
                .grid()
                .line_text(0)
                .unwrap(),
            "done"
        );
    }

    #[test]
    fn tui_stream_frame_returns_daemon_response_errors() {
        let mut state = TuiSessionState::default();
        let response = DaemonResponse::error(
            "req_attach",
            agentmux_ipc::ErrorBody::new("not_found", "agent missing"),
        );

        let error =
            apply_tui_stream_frame(&mut state, DaemonStreamFrame::Response(response)).unwrap_err();

        assert!(error.to_string().contains("agent missing"));
        assert!(error.to_string().contains("not_found"));
    }

    #[test]
    fn runtime_input_failure_is_non_fatal() {
        // A keystroke forwarded to an agent with no live PTY returns an error
        // response; during the interactive loop this must be a soft notice, not
        // a session-killing error.
        let mut state = TuiSessionState::default();
        let response = DaemonResponse::error(
            "req_input_1",
            agentmux_ipc::ErrorBody::new("INPUT_SCRIPT_FAILED", "agent has no live PTY"),
        );

        let notice = apply_runtime_stream_frame(&mut state, DaemonStreamFrame::Response(response));

        let notice = notice.expect("runtime failure is surfaced as a notice");
        assert!(notice.contains("no live PTY"));
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
    fn send_commands_accept_inject_flag() {
        let cli = Cli::try_parse_from([
            "agentmux",
            "agent",
            "send",
            "--inject",
            "agent_01HX",
            "hello",
        ])
        .unwrap();
        let Some(Commands::Agent(args)) = cli.command else {
            panic!("expected agent command");
        };
        let AgentAction::Send { inject, .. } = args.action else {
            panic!("expected agent send action");
        };
        assert!(inject);

        let cli = Cli::try_parse_from([
            "agentmux",
            "message",
            "send",
            "--inject",
            "--to",
            "agent:agent_01HX",
            "hello",
        ])
        .unwrap();
        let Some(Commands::Message(args)) = cli.command else {
            panic!("expected message command");
        };
        let MessageAction::Send { inject, .. } = args.action else {
            panic!("expected message send action");
        };
        assert!(inject);
    }

    #[test]
    fn format_message_history_payload_lists_messages_newest_first() {
        let payload = json!({
            "messages": [
                {
                    "message_id": "msg_old",
                    "task_id": "task_a",
                    "from": { "kind": "agent", "id": "planner" },
                    "to": { "kind": "role", "id": "tester" },
                    "kind": "handoff",
                    "body": "older handoff",
                    "delivery_status": "queued",
                    "created_at": "2026-06-04T01:00:00+00:00"
                },
                {
                    "message_id": "msg_new",
                    "task_id": "task_a",
                    "from": { "kind": "orchestrator" },
                    "to": { "kind": "agent", "id": "impl-codex" },
                    "kind": "test_result",
                    "body": "newer test result",
                    "delivery_status": "delivered",
                    "created_at": "2026-06-04T02:00:00+00:00"
                }
            ]
        });

        let output = format_message_history_payload(
            &payload,
            &MessageHistoryFilter {
                limit: 50,
                ..MessageHistoryFilter::default()
            },
        );

        assert!(output.starts_with("CREATED"));
        assert!(output.contains("msg_new"));
        assert!(output.contains("agent:impl-codex"));
        assert!(output.find("msg_new").unwrap() < output.find("msg_old").unwrap());
    }

    #[test]
    fn format_message_history_payload_filters_and_limits_messages() {
        let payload = json!({
            "messages": [
                {
                    "message_id": "msg_a",
                    "task_id": "task_a",
                    "from": { "kind": "agent", "id": "planner" },
                    "to": { "kind": "agent", "id": "impl-codex" },
                    "kind": "handoff",
                    "body": "first",
                    "delivery_status": "queued",
                    "created_at": "2026-06-04T01:00:00+00:00"
                },
                {
                    "message_id": "msg_b",
                    "task_id": "task_b",
                    "from": { "kind": "agent", "id": "tester" },
                    "to": { "kind": "agent", "id": "reviewer" },
                    "kind": "test_result",
                    "body": "second",
                    "delivery_status": "delivered",
                    "created_at": "2026-06-04T02:00:00+00:00"
                }
            ]
        });

        let output = format_message_history_payload(
            &payload,
            &MessageHistoryFilter {
                limit: 1,
                task: None,
                agent: Some("impl-codex".to_string()),
                kind: Some("handoff".to_string()),
                status: Some("queued".to_string()),
            },
        );

        assert!(output.contains("msg_a"));
        assert!(!output.contains("msg_b"));
    }

    #[test]
    fn format_message_history_payload_reports_empty_history() {
        assert_eq!(
            format_message_history_payload(
                &json!({ "messages": [] }),
                &MessageHistoryFilter {
                    limit: 50,
                    ..MessageHistoryFilter::default()
                },
            ),
            "no messages\n"
        );
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
        let spawn_name = spawn.payload["name"].as_str().unwrap();
        assert!(spawn_name.starts_with("implementer-"));
        assert_eq!(spawn_name.len(), "implementer-".len() + 6);

        let second_spawn =
            agent_spawn_request("codex".to_string(), "implementer".to_string()).unwrap();
        assert_ne!(spawn.payload["name"], second_spawn.payload["name"]);

        let stop = agent_stop_request("agent_01HX".to_string());
        assert_eq!(stop.command, IpcCommand::AgentStop);
        assert_eq!(stop.payload["agent_id"], "agent_01HX");

        let send = agent_send_request("agent_01HX".to_string(), "hello".to_string()).unwrap();
        assert_eq!(send.command, IpcCommand::MessageCreate);
        assert_eq!(send.payload["to"], "agent:agent_01HX");
        assert_eq!(send.payload["body"], "hello");

        let send_by_name =
            agent_send_request("codex-a1b2c3".to_string(), "hello".to_string()).unwrap();
        assert_eq!(send_by_name.payload["to"], "agent:codex-a1b2c3");

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
    fn sessions_list_request_targets_daemon_status() {
        let request = sessions_list_request();

        assert_eq!(request.id, "req_sessions_list");
        assert_eq!(request.command, IpcCommand::DaemonStatus);
        assert_eq!(request.payload, json!({}));
    }

    #[test]
    fn format_sessions_payload_lists_only_running_sessions() {
        let payload = json!({
            "agents": [
                {
                    "id": "agent_live",
                    "name": "shell",
                    "role": "tester",
                    "status": "awaiting_input",
                    "input_ready": true,
                    "process_id": 1234,
                    "has_process": true,
                    "attached_clients": ["csess_1", "csess_2"]
                },
                {
                    "id": "agent_restored",
                    "name": "restored",
                    "process_id": null,
                    "has_process": false,
                    "attached_clients": []
                }
            ]
        });

        assert_eq!(
            format_sessions_payload(&payload),
            "ID NAME ROLE STATUS INPUT PID CLIENTS\nagent_live shell tester awaiting_input ready 1234 2\n"
        );
    }

    #[test]
    fn format_sessions_payload_reports_empty_running_sessions() {
        assert_eq!(
            format_sessions_payload(&json!({ "agents": [] })),
            "no running sessions\n"
        );
        assert_eq!(format_sessions_payload(&json!({})), "no running sessions\n");
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
    fn result_protocol_install_updates_existing_local_instruction_files_once() {
        let root = std::env::temp_dir().join(format!(
            "agentmux-result-protocol-local-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let agents_path = root.join("AGENTS.md");
        let claude_path = root.join("CLAUDE.md");
        let gemini_path = root.join("GEMINI.md");
        std::fs::write(&agents_path, "# Agents\n").unwrap();
        std::fs::write(&claude_path, "# Claude\n").unwrap();

        let first = install_result_protocol(&root, false).unwrap();
        assert_eq!(first.len(), 3);
        assert_eq!(first[0].status, ResultProtocolInstallStatus::Added);
        assert_eq!(first[1].status, ResultProtocolInstallStatus::Added);
        assert_eq!(first[2].status, ResultProtocolInstallStatus::Missing);
        assert!(!gemini_path.exists());

        let second = install_result_protocol(&root, false).unwrap();
        assert_eq!(
            second[0].status,
            ResultProtocolInstallStatus::AlreadyPresent
        );
        assert_eq!(
            second[1].status,
            ResultProtocolInstallStatus::AlreadyPresent
        );
        assert_eq!(second[2].status, ResultProtocolInstallStatus::Missing);

        let contents = std::fs::read_to_string(&agents_path).unwrap();
        assert_eq!(contents.matches(RESULT_PROTOCOL_MARKER_START).count(), 1);
        assert!(contents.contains("AGENTMUX_RESULT:"));
        assert!(contents.contains("messages[]"));
        assert!(contents.contains("Allowed `messages[].kind` values"));
        assert!(contents.contains("AGENTMUX_AGENT_NAME"));
        assert!(contents.contains("agentmux message inject <message_id>"));
        assert!(contents.contains("agentmux agent inject <message_id> <agent_id>"));
        assert!(contents.contains("agentmux start \"agy,messages,codex\""));
        assert!(contents.contains("Conversation List"));
        assert!(contents.contains("Injection is asynchronous"));
        assert!(contents.contains("Two-session exchange example"));
        assert!(contents.contains("agentmux message list"));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn result_protocol_install_refreshes_stale_managed_block() {
        let root = std::env::temp_dir().join(format!(
            "agentmux-result-protocol-refresh-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let agents_path = root.join("AGENTS.md");
        std::fs::write(
            &agents_path,
            "# Agents\n\n<!-- agentmux-result-protocol:start -->\nold instructions\n<!-- agentmux-result-protocol:end -->\n",
        )
        .unwrap();

        let report = install_result_protocol(&root, false).unwrap();

        assert_eq!(report[0].status, ResultProtocolInstallStatus::Updated);
        let contents = std::fs::read_to_string(&agents_path).unwrap();
        assert_eq!(contents.matches(RESULT_PROTOCOL_MARKER_START).count(), 1);
        assert!(!contents.contains("old instructions"));
        assert!(contents.contains("Two-session exchange example"));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn result_protocol_install_can_create_global_style_instruction_file() {
        let root = std::env::temp_dir().join(format!(
            "agentmux-result-protocol-global-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let target = root.join(".codex/AGENTS.md");

        let first = install_result_protocol_to_file(&target, true).unwrap();
        assert_eq!(first.status, ResultProtocolInstallStatus::Added);
        assert!(target.exists());

        let second = install_result_protocol_to_file(&target, true).unwrap();
        assert_eq!(second.status, ResultProtocolInstallStatus::AlreadyPresent);
        let contents = std::fs::read_to_string(&target).unwrap();
        assert_eq!(contents.matches(RESULT_PROTOCOL_MARKER_START).count(), 1);

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
