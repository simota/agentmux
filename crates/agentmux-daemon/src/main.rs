//! `agentmux-daemon` — The agentmux background daemon.

use std::path::PathBuf;

use agentmux_core::AgentmuxError;
use agentmux_core::error::Result;
use agentmux_daemon::{DaemonConfig, DaemonRuntime, serve};

#[tokio::main]
async fn main() -> Result<()> {
    let socket_path = std::env::var_os("AGENTMUX_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(default_socket_path);

    println!("agentmux-daemon v{} starting", env!("CARGO_PKG_VERSION"));
    println!("Socket path: {}", socket_path.display());

    // Wire `.agentmux/` (event log, policy engine, injection timing) from the
    // working directory's project config; pre-init directories get a plain
    // runtime. An invalid config.toml aborts startup instead of silently
    // running with spec defaults.
    let project_root = std::env::current_dir().map_err(|error| {
        AgentmuxError::Internal(format!("failed to resolve working directory: {error}"))
    })?;
    let runtime = DaemonRuntime::for_project(1024, &project_root).await?;
    println!(
        "Project config: {}",
        if project_root.join(".agentmux").is_dir() {
            "wired from .agentmux/"
        } else {
            "not initialized (plain runtime)"
        }
    );

    let pid_path = write_pidfile(&socket_path);
    let result = serve(DaemonConfig::new(socket_path), runtime).await;
    remove_pidfile(&pid_path);

    result
}

fn write_pidfile(socket_path: &std::path::Path) -> Option<PathBuf> {
    let pid_path = socket_path.parent()?.join("agentmux.pid");
    if let Some(parent) = pid_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&pid_path, std::process::id().to_string());
    Some(pid_path)
}

fn remove_pidfile(pid_path: &Option<PathBuf>) {
    if let Some(pid_path) = pid_path {
        let _ = std::fs::remove_file(pid_path);
    }
}

fn default_socket_path() -> PathBuf {
    if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime_dir).join("agentmux/agentmux.sock");
    }

    let uid = std::env::var_os("USER")
        .and_then(|value| value.into_string().ok())
        .unwrap_or_else(|| "current".to_string());
    std::env::temp_dir().join(format!("agentmux-{uid}/agentmux.sock"))
}
