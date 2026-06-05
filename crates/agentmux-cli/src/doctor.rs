//! `agentmux doctor` environment diagnostics.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use agentmux_core::AgentmuxConfig;
use agentmux_pty::{PtyHandle, PtySpawnSpec, TerminalSize};
use agentmux_store::Store;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DoctorStatus {
    Ok,
    Warn,
    Fail,
}

impl DoctorStatus {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DoctorCheck {
    pub(crate) name: &'static str,
    pub(crate) status: DoctorStatus,
    pub(crate) detail: String,
}

impl DoctorCheck {
    pub(crate) fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: DoctorStatus::Ok,
            detail: detail.into(),
        }
    }

    pub(crate) fn warn(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: DoctorStatus::Warn,
            detail: detail.into(),
        }
    }

    pub(crate) fn fail(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: DoctorStatus::Fail,
            detail: detail.into(),
        }
    }
}

pub(crate) fn doctor_report(socket_path: &Path, project_dir: &Path) -> Vec<DoctorCheck> {
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

pub(crate) fn find_command_in_path(command: &str, path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
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

pub(crate) fn print_doctor_report(report: &[DoctorCheck]) {
    for check in report {
        println!(
            "{:<14} {:<5} {}",
            check.name,
            check.status.label(),
            check.detail
        );
    }
}
