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

    // Background redelivery for messages parked in `WaitingForAgent` (e.g.
    // deferred by the human-typing quiet window). Aborted with the serve loop.
    let redelivery = runtime.spawn_waiting_message_redelivery_loop();

    // Run the serve loop, then always run the shutdown tail — even when the
    // loop exits with an error — so the socket file is removed and the
    // `daemon.stopped` recovery event is recorded.
    let served = run_serve_loop(listener, &runtime, &socket_path, shutdown).await;
    redelivery.abort();
    let finished = finish_shutdown(&runtime, &socket_path).await;
    served.and(finished)
}

async fn run_serve_loop(
    listener: UnixListener,
    runtime: &DaemonRuntime,
    socket_path: &Path,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    let restored_agent_count = runtime.recover_state_from_event_log().await?;
    let started_payload =
        json!({ "socket_path": socket_path, "restored_agent_count": restored_agent_count });
    runtime.append_daemon_lifecycle_event("daemon.started", started_payload.clone())?;
    runtime.publish(DaemonEvent::new(
        IpcEventKind::DaemonStarted,
        started_payload,
    ));

    let mut clients = JoinSet::new();
    let mut accept_backoff = ACCEPT_ERROR_BACKOFF_MIN;
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            biased;
            () = &mut shutdown => break,
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _addr)) => {
                        accept_backoff = ACCEPT_ERROR_BACKOFF_MIN;
                        let runtime = runtime.clone();
                        clients.spawn(async move {
                            if let Err(error) = handle_client(stream, runtime).await {
                                eprintln!("agentmux-daemon client error: {error}");
                            }
                        });
                    }
                    // Accept errors are usually transient (EMFILE/ENFILE when
                    // the fd table is exhausted, ECONNABORTED, ...). Killing
                    // the daemon — and every live agent session with it — over
                    // one failed accept is wrong: log, back off briefly so a
                    // persistent error cannot spin the loop, and keep serving.
                    Err(error) => {
                        eprintln!("agentmux-daemon failed to accept client: {error}");
                        let payload = json!({
                            "signal": "client_accept_failed",
                            "error": error.to_string(),
                            "backoff_ms": accept_backoff.as_millis() as u64,
                        });
                        let _ = runtime
                            .append_daemon_lifecycle_event("daemon.accept_failed", payload.clone());
                        runtime.publish(DaemonEvent::new(IpcEventKind::Error, payload));
                        tokio::time::sleep(accept_backoff).await;
                        accept_backoff = next_accept_backoff(accept_backoff);
                    }
                }
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
    Ok(())
}

/// Initial/maximum backoff applied between `accept()` retries after an accept
/// error, so a persistent error (e.g. EMFILE) cannot busy-spin the serve loop.
pub(crate) const ACCEPT_ERROR_BACKOFF_MIN: Duration = Duration::from_millis(100);
pub(crate) const ACCEPT_ERROR_BACKOFF_MAX: Duration = Duration::from_secs(1);

/// Exponential accept-error backoff, capped at [`ACCEPT_ERROR_BACKOFF_MAX`].
pub(crate) fn next_accept_backoff(current: Duration) -> Duration {
    (current * 2).min(ACCEPT_ERROR_BACKOFF_MAX)
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
