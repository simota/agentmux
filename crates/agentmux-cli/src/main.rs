//! `agentmux` — CLI entry point.
//!
//! Top-level subcommands mirror `docs/spec/11_cli_tui_user_spec.md §2`.
//! The CLI is a thin JSONL/Unix-socket client for the daemon. Interactive
//! control remains in the TUI.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use agentmux_core::{AgentmuxError, error::Result};
use agentmux_ipc::{
    ClientHello, ClientRequest, DaemonResponse, IpcCommand, JsonlReader, JsonlWriter,
};
use agentmux_pty::{PtyHandle, PtySpawnSpec, TerminalSize};
use agentmux_store::Store;
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use tokio::io::BufReader;
use tokio::net::UnixStream;

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

    match cli.command {
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
            DaemonAction::Start => println!("daemon start — not yet implemented"),
            DaemonAction::Stop => println!("daemon stop — not yet implemented"),
            DaemonAction::Status => {
                let response = send_daemon_request(&socket_path, daemon_status_request()).await?;
                print_response("daemon", response)?;
            }
        },
        Commands::Project(args) => match args.action {
            ProjectAction::Init { path } => {
                let project_dir = init_project(Path::new(&path))?;
                let response = send_daemon_request(
                    &socket_path,
                    ClientRequest::new(
                        "req_project_init",
                        IpcCommand::DaemonStatus,
                        json!({ "project_path": project_dir }),
                    ),
                )
                .await?;
                print_response("project", response)?;
            }
            ProjectAction::Open { path } => {
                println!("project open {path} — not yet implemented");
            }
            ProjectAction::Status => {
                let response = send_daemon_request(&socket_path, daemon_status_request()).await?;
                print_response("project", response)?;
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
            AgentAction::Ls => println!("agent ls — not yet implemented"),
            AgentAction::Spawn { provider, role } => {
                println!("agent spawn provider={provider} role={role} — not yet implemented");
            }
            AgentAction::Stop { agent_id } => {
                println!("agent stop {agent_id} — not yet implemented")
            }
            AgentAction::Send { agent_id, body } => {
                println!("agent send {agent_id} {body:?} — not yet implemented");
            }
            AgentAction::Inject {
                message_id,
                agent_id,
            } => {
                println!("agent inject {message_id} → {agent_id} — not yet implemented");
            }
            AgentAction::Focus { agent_id } => {
                println!("agent focus {agent_id} — not yet implemented")
            }
            AgentAction::Interrupt { agent_id } => {
                println!("agent interrupt {agent_id} — not yet implemented");
            }
        },
        Commands::Message(args) => match args.action {
            MessageAction::List => println!("message list — not yet implemented"),
            MessageAction::Show { message_id } => {
                println!("message show {message_id} — not yet implemented")
            }
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
            ContextAction::Show { context_id } => {
                println!("context show {context_id} — not yet implemented")
            }
            ContextAction::Search { query } => {
                println!("context search {query:?} — not yet implemented")
            }
            ContextAction::Attach {
                context_id,
                message_id,
            } => {
                println!("context attach {context_id} → {message_id} — not yet implemented");
            }
            ContextAction::Inject {
                context_id,
                agent_id,
            } => {
                println!("context inject {context_id} → {agent_id} — not yet implemented");
            }
            ContextAction::Export { output } => {
                println!("context export {output} — not yet implemented")
            }
        },
        Commands::Worktree(args) => match args.action {
            WorktreeAction::List => println!("worktree list — not yet implemented"),
            WorktreeAction::Diff { worktree_id } => {
                println!("worktree diff {worktree_id} — not yet implemented")
            }
            WorktreeAction::Test { worktree_id } => {
                println!("worktree test {worktree_id} — not yet implemented")
            }
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
            let response = send_daemon_request(&socket_path, attach_request(args.target)).await?;
            print_response("attach", response)?;
        }
        Commands::Layout(args) => match args.action {
            LayoutAction::Save { name } => println!("layout save {name} — not yet implemented"),
            LayoutAction::Load { name } => println!("layout load {name} — not yet implemented"),
            LayoutAction::List => println!("layout list — not yet implemented"),
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
        std::fs::write(&config_path, "version = 1\n").map_err(|error| {
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
        Ok(contents) => match parse_minimal_config_version(&contents) {
            Some(version) => DoctorCheck::ok("config parse", format!("version={version}")),
            None => DoctorCheck::fail(
                "config parse",
                format!("missing numeric version in {}", config_path.display()),
            ),
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

fn parse_minimal_config_version(contents: &str) -> Option<u64> {
    contents.lines().find_map(|line| {
        let line = line.split_once('#').map_or(line, |(head, _)| head).trim();
        let (key, value) = line.split_once('=')?;
        (key.trim() == "version")
            .then(|| value.trim().parse::<u64>().ok())
            .flatten()
    })
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

async fn send_daemon_request(socket_path: &Path, request: ClientRequest) -> Result<DaemonResponse> {
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
    fn project_init_creates_agentmux_config_without_overwriting_existing_file() {
        let root = std::env::temp_dir().join(format!("agentmux-cli-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let project_dir = init_project(&root).unwrap();
        let config_path = project_dir.join(".agentmux/config.toml");
        assert_eq!(
            std::fs::read_to_string(&config_path).unwrap(),
            "version = 1\n"
        );

        std::fs::write(&config_path, "version = 1\ncustom = true\n").unwrap();
        init_project(&root).unwrap();
        assert_eq!(
            std::fs::read_to_string(&config_path).unwrap(),
            "version = 1\ncustom = true\n"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn minimal_config_parser_accepts_version_key_and_ignores_comments() {
        assert_eq!(
            parse_minimal_config_version("# project\nversion = 1 # v0.1\n"),
            Some(1)
        );
        assert_eq!(
            parse_minimal_config_version("team = 'claude-codex'\n"),
            None
        );
        assert_eq!(parse_minimal_config_version("version = invalid\n"), None);
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
        std::fs::write(root.join(".agentmux/config.toml"), "version = 1\n").unwrap();

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
                && check.detail == "version=1"
        }));

        std::fs::remove_dir_all(&root).unwrap();
    }
}
