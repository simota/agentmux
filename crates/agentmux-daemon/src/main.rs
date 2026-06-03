//! `agentmux-daemon` — The agentmux background daemon.

use std::path::PathBuf;

use agentmux_core::error::Result;
use agentmux_daemon::{DaemonConfig, DaemonRuntime, serve};

#[tokio::main]
async fn main() -> Result<()> {
    let socket_path = std::env::var_os("AGENTMUX_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(default_socket_path);

    println!("agentmux-daemon v{} starting", env!("CARGO_PKG_VERSION"));
    println!("Socket path: {}", socket_path.display());

    serve(DaemonConfig::new(socket_path), DaemonRuntime::new(1024)).await
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
