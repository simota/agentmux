use crate::*;

pub async fn serve(config: DaemonConfig, runtime: DaemonRuntime) -> Result<()> {
    serve_until_shutdown(config, runtime, shutdown_signal()).await
}

pub async fn serve_until_shutdown(
    config: DaemonConfig,
    runtime: DaemonRuntime,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    let listener = bind_unix_listener(&config.socket_path)?;
    let socket_path = config.socket_path.clone();
    let restored_agent_count = runtime.recover_state_from_event_log().await?;
    let started_payload =
        json!({ "socket_path": socket_path, "restored_agent_count": restored_agent_count });
    runtime.append_daemon_lifecycle_event("daemon.started", started_payload.clone())?;
    runtime.publish(DaemonEvent::new(
        IpcEventKind::DaemonStarted,
        started_payload,
    ));

    let mut clients = JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            biased;
            () = &mut shutdown => break,
            accepted = listener.accept() => {
                let (stream, _addr) = accepted.map_err(|error| {
                    AgentmuxError::IpcError(format!("failed to accept daemon client: {error}"))
                })?;
                let runtime = runtime.clone();
                clients.spawn(async move {
                    if let Err(error) = handle_client(stream, runtime).await {
                        eprintln!("agentmux-daemon client error: {error}");
                    }
                });
            }
        }
    }

    drop(listener);
    clients.abort_all();
    while let Some(joined) = clients.join_next().await {
        if let Err(error) = joined
            && !error.is_cancelled()
        {
            return Err(AgentmuxError::IpcError(format!(
                "daemon client task failed during shutdown: {error}"
            )));
        }
    }

    finish_shutdown(&runtime, &socket_path).await?;
    Ok(())
}

pub(crate) async fn finish_shutdown(runtime: &DaemonRuntime, socket_path: &Path) -> Result<()> {
    let status = runtime.status_payload().await;
    let stopped_payload = json!({ "socket_path": socket_path, "state": status });
    runtime.append_daemon_lifecycle_event("daemon.stopped", stopped_payload.clone())?;
    runtime.publish(DaemonEvent::new(
        IpcEventKind::DaemonStopped,
        stopped_payload,
    ));
    remove_socket_file(socket_path)
}

pub(crate) async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut interrupt = signal(SignalKind::interrupt()).expect("SIGINT handler installs");
        let mut terminate = signal(SignalKind::terminate()).expect("SIGTERM handler installs");
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

pub(crate) fn bind_unix_listener(socket_path: &Path) -> Result<UnixListener> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            AgentmuxError::IpcError(format!(
                "failed to create socket directory '{}': {error}",
                parent.display()
            ))
        })?;
    }
    if socket_path.exists() {
        std::fs::remove_file(socket_path).map_err(|error| {
            AgentmuxError::IpcError(format!(
                "failed to remove stale socket '{}': {error}",
                socket_path.display()
            ))
        })?;
    }

    let listener = UnixListener::bind(socket_path).map_err(|error| {
        AgentmuxError::IpcError(format!(
            "failed to bind daemon socket '{}': {error}",
            socket_path.display()
        ))
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600)).map_err(
            |error| {
                AgentmuxError::IpcError(format!(
                    "failed to set socket permissions '{}': {error}",
                    socket_path.display()
                ))
            },
        )?;
    }

    Ok(listener)
}

pub(crate) fn remove_socket_file(socket_path: &Path) -> Result<()> {
    if !socket_path.exists() {
        return Ok(());
    }
    std::fs::remove_file(socket_path).map_err(|error| {
        AgentmuxError::IpcError(format!(
            "failed to remove daemon socket '{}': {error}",
            socket_path.display()
        ))
    })
}
