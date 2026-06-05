//! Daemon lifecycle helpers: socket/pid resolution, auto-start, shutdown, and
//! protocol-capability probes.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use agentmux_core::{AgentmuxError, error::Result};
use tokio::net::UnixStream;

#[cfg(feature = "arena")]
use agentmux_ipc::ARENA_PROTOCOL_VERSION;
#[cfg(feature = "activity-feed")]
use agentmux_ipc::EVENT_SUBSCRIBE_PROTOCOL_VERSION;
#[cfg(any(feature = "arena", feature = "activity-feed"))]
use agentmux_tui::state::TuiSessionState;

#[cfg(feature = "arena")]
use crate::requests::daemon_status_request;
#[cfg(feature = "arena")]
use crate::send_daemon_request;
#[cfg(feature = "arena")]
use serde_json::Value;

pub(crate) fn default_socket_path() -> PathBuf {
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

#[cfg(feature = "arena")]
pub(crate) async fn daemon_supports_arena(socket_path: &Path) -> Result<bool> {
    let response = send_daemon_request(socket_path, daemon_status_request()).await?;
    if !response.ok {
        return Ok(false);
    }
    Ok(response
        .payload
        .as_ref()
        .and_then(|payload| payload.get("protocol_version"))
        .and_then(Value::as_u64)
        .and_then(|version| u32::try_from(version).ok())
        .is_some_and(|version| version >= ARENA_PROTOCOL_VERSION))
}

#[cfg(feature = "arena")]
pub(crate) fn daemon_supports_arena_state(state: &TuiSessionState) -> bool {
    state
        .daemon_protocol_version()
        .is_some_and(|version| version >= ARENA_PROTOCOL_VERSION)
}

#[cfg(feature = "activity-feed")]
pub(crate) fn daemon_supports_event_subscribe(state: &TuiSessionState) -> bool {
    state
        .daemon_protocol_version()
        .is_some_and(|version| version >= EVENT_SUBSCRIBE_PROTOCOL_VERSION)
}

/// Pidfile written by the daemon next to its socket; used by `daemon stop`.
pub(crate) fn daemon_pid_path(socket_path: &Path) -> Option<PathBuf> {
    socket_path
        .parent()
        .map(|parent| parent.join("agentmux.pid"))
}

/// Resolve the `agentmux-daemon` binary: prefer a sibling of the running CLI
/// (so dev `target/debug` and installed `bin/` both work), else fall back to PATH.
pub(crate) fn resolve_daemon_binary() -> std::ffi::OsString {
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
pub(crate) async fn daemon_running(socket_path: &Path) -> bool {
    UnixStream::connect(socket_path).await.is_ok()
}

/// Ensure the daemon is reachable, auto-starting it in the background if not.
pub(crate) async fn ensure_daemon(socket_path: &Path) -> Result<()> {
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
pub(crate) fn stop_daemon(socket_path: &Path) -> Result<()> {
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
