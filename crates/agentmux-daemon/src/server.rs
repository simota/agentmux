use crate::*;

pub(crate) enum ServerFrame {
    Response(DaemonResponse),
    Event(DaemonEvent),
}

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

pub async fn handle_client(stream: UnixStream, runtime: DaemonRuntime) -> Result<()> {
    let client_id = ClientSessionId::new();
    let (reader, writer) = stream.into_split();
    let mut reader = JsonlReader::new(BufReader::new(reader));
    let mut events = runtime.subscribe();
    let (frames, mut frame_receiver) = mpsc::channel::<ServerFrame>(32);
    let (attached, mut attached_events) = watch::channel(false);
    let event_filter = Arc::new(Mutex::new(None::<EventSubscribeFilter>));

    let writer_task = tokio::spawn(async move {
        let mut writer = JsonlWriter::new(writer);
        while let Some(frame) = frame_receiver.recv().await {
            match frame {
                ServerFrame::Response(response) => writer.write(&response).await?,
                ServerFrame::Event(event) => writer.write(&event).await?,
            }
        }
        Result::<()>::Ok(())
    });

    let event_frames = frames.clone();
    let event_task_filter = Arc::clone(&event_filter);
    let event_task_runtime = runtime.clone();
    let event_task = tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    if !*attached_events.borrow_and_update() {
                        continue;
                    }
                    let filter = event_task_filter
                        .lock()
                        .map(|filter| filter.clone())
                        .unwrap_or(None);
                    let should_forward =
                        should_forward_event(&event_task_runtime, filter.as_ref(), &event).await;
                    if !should_forward {
                        continue;
                    }
                    if event_frames.send(ServerFrame::Event(event)).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    let hello = match reader.read::<ClientHello>().await? {
        Some(hello) => hello,
        None => {
            event_task.abort();
            drop(frames);
            return writer_task
                .await
                .map_err(|error| AgentmuxError::IpcError(error.to_string()))?;
        }
    };
    if let Some(error) = protocol_error(hello.protocol_compatibility()) {
        send_frame(
            &frames,
            ServerFrame::Response(DaemonResponse::error("hello", error)),
        )
        .await?;
        event_task.abort();
        drop(frames);
        return writer_task
            .await
            .map_err(|error| AgentmuxError::IpcError(error.to_string()))?;
    }

    runtime
        .state
        .write()
        .await
        .clients
        .insert(client_id.clone(), None);

    while let Some(request) = reader.read::<ClientRequest>().await? {
        let command = request.command.clone();
        if matches!(command, IpcCommand::ClientAttach | IpcCommand::AgentFocus) {
            let _ = attached.send(true);
        } else if command == IpcCommand::ClientDetach {
            let _ = attached.send(false);
        }
        let response = handle_request(&runtime, &client_id, &event_filter, request).await;
        let attach_failed =
            matches!(command, IpcCommand::ClientAttach | IpcCommand::AgentFocus) && !response.ok;
        send_frame(&frames, ServerFrame::Response(response)).await?;
        if attach_failed {
            let _ = attached.send(false);
        }
    }

    runtime.detach_client(&client_id).await;
    event_task.abort();
    drop(frames);
    writer_task
        .await
        .map_err(|error| AgentmuxError::IpcError(error.to_string()))?
}

pub(crate) async fn send_frame(frames: &mpsc::Sender<ServerFrame>, frame: ServerFrame) -> Result<()> {
    frames
        .send(frame)
        .await
        .map_err(|_| AgentmuxError::IpcError("client writer task stopped".to_string()))
}

pub(crate) async fn should_forward_event(
    runtime: &DaemonRuntime,
    filter: Option<&EventSubscribeFilter>,
    event: &DaemonEvent,
) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    if !filter.kinds.is_empty()
        && !filter
            .kinds
            .iter()
            .any(|kind| kind == event_kind_label(&event.kind))
    {
        return false;
    }
    if let Some(task_id) = filter.task_id.as_deref()
        && payload_string_field(&event.payload, "task_id").as_deref() != Some(task_id)
    {
        return false;
    }
    if !filter.roles.is_empty()
        && event_role(runtime, event)
            .await
            .as_deref()
            .is_none_or(|role| !filter.roles.iter().any(|expected| expected == role))
    {
        return false;
    }
    true
}

pub(crate) async fn event_role(runtime: &DaemonRuntime, event: &DaemonEvent) -> Option<String> {
    if let Some(role) = payload_string_field(&event.payload, "role") {
        return Some(role);
    }
    let agent_id = payload_string_field(&event.payload, "agent_id")?
        .parse::<AgentSessionId>()
        .ok()?;
    let state = runtime.state.read().await;
    state
        .agents
        .get(&agent_id)
        .map(|agent| agent_role_label(&agent.metadata.role))
}

pub(crate) fn payload_string_field(payload: &serde_json::Value, field: &str) -> Option<String> {
    payload
        .get(field)
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned)
}

pub(crate) async fn handle_request(
    runtime: &DaemonRuntime,
    client_id: &ClientSessionId,
    event_filter: &Arc<Mutex<Option<EventSubscribeFilter>>>,
    request: ClientRequest,
) -> DaemonResponse {
    if let Some(error) = protocol_error(request.protocol_compatibility()) {
        return DaemonResponse::error(request.id, error);
    }

    match request.command {
        IpcCommand::DaemonStatus => DaemonResponse::ok(request.id, runtime.status_payload().await),
        IpcCommand::EventSubscribe => {
            let filter =
                match serde_json::from_value::<EventSubscribeFilter>(request.payload.clone()) {
                    Ok(filter) => filter,
                    Err(error) => {
                        return DaemonResponse::error(
                            request.id,
                            ErrorBody::new(
                                "INVALID_EVENT_SUBSCRIBE_FILTER",
                                format!("event.subscribe filter is invalid: {error}"),
                            ),
                        );
                    }
                };
            match event_filter.lock() {
                Ok(mut current) => {
                    *current = Some(filter);
                    DaemonResponse::ok(request.id, json!({ "subscribed": true }))
                }
                Err(_) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new(
                        "EVENT_SUBSCRIBE_FAILED",
                        "event.subscribe filter lock is poisoned",
                    ),
                ),
            }
        }
        IpcCommand::TaskRun => {
            let (body, team, project_path, runner) = match task_run_payload(&request.payload) {
                Ok(payload) => payload,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_TASK_RUN", error.to_string()),
                    );
                }
            };
            if runner == "arena" {
                let providers = match arena_providers_payload(&request.payload) {
                    Ok(providers) => providers,
                    Err(error) => {
                        return DaemonResponse::error(
                            request.id,
                            ErrorBody::new("INVALID_ARENA_PROVIDERS", error.to_string()),
                        );
                    }
                };
                let base_branch = request
                    .payload
                    .get("base_branch")
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or("main")
                    .to_string();
                return match runtime
                    .run_task_with_arena(body, providers, project_path, base_branch)
                    .await
                {
                    Ok(payload) => DaemonResponse::ok(request.id, payload),
                    Err(error) => DaemonResponse::error(
                        request.id,
                        ErrorBody::new("TASK_RUN_FAILED", error.to_string()),
                    ),
                };
            }
            if runner != "shell-stub" {
                return DaemonResponse::error(
                    request.id,
                    ErrorBody::new(
                        "TASK_RUNNER_UNAVAILABLE",
                        format!("unsupported task.run runner '{runner}'"),
                    )
                    .with_hint("use runner=shell-stub for deterministic test execution"),
                );
            }

            match runtime
                .run_task_with_shell_stubs(body, team, project_path)
                .await
            {
                Ok(payload) => DaemonResponse::ok(request.id, payload),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("TASK_RUN_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::AgentSpawn => {
            let name = request
                .payload
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("agent")
                .to_string();
            let role = match request
                .payload
                .get("role")
                .and_then(|value| value.as_str())
                .map(parse_agent_role)
                .transpose()
            {
                Ok(Some(role)) => role,
                Ok(None) => inferred_agent_role(&name),
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("AGENT_SPAWN_FAILED", error.to_string()),
                    );
                }
            };
            let agent = match pty_spawn_spec_from_payload(&request.payload) {
                Ok(Some(spec)) => runtime.spawn_agent_with_role(name, role, spec).await,
                Ok(None) => Ok(runtime.register_agent_with_role(name, role).await),
                Err(error) => Err(error),
            };
            match agent {
                Ok(agent) => DaemonResponse::ok(
                    request.id,
                    json!({
                        "agent_id": agent.id.to_string(),
                        "name": agent.name,
                        "role": agent_role_label(&agent.role),
                        "process_id": agent.process_id,
                    }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("AGENT_SPAWN_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::ClientAttach => {
            let Some(agent_id) = request
                .payload
                .get("agent_id")
                .and_then(|value| value.as_str())
                .and_then(parse_agent_session_id)
            else {
                return DaemonResponse::error(
                    request.id,
                    ErrorBody::new("INVALID_ATTACH_TARGET", "client.attach requires agent_id"),
                );
            };

            match runtime
                .attach_client(client_id.clone(), agent_id.clone())
                .await
            {
                Ok(()) => DaemonResponse::ok(
                    request.id,
                    json!({
                        "client_id": client_id.to_string(),
                        "agent_id": agent_id.to_string(),
                    }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("AGENT_NOT_FOUND", error.to_string())
                        .with_hint("call agent.spawn or choose an agent from daemon.status"),
                ),
            }
        }
        IpcCommand::ClientDetach => {
            let agent_id = runtime.detach_client(client_id).await;
            DaemonResponse::ok(
                request.id,
                json!({
                    "client_id": client_id.to_string(),
                    "agent_id": agent_id.map(|id| id.to_string()),
                }),
            )
        }
        IpcCommand::AgentStop => {
            let agent_id = match agent_id_payload(&request.payload, "agent.stop") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_AGENT_ID", error.to_string()),
                    );
                }
            };
            match runtime.stop_agent(&agent_id).await {
                Ok(agent) => DaemonResponse::ok(
                    request.id,
                    json!({
                        "agent_id": agent.id.to_string(),
                        "name": agent.name,
                        "stopped": true,
                    }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("AGENT_STOP_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::AgentFocus => {
            let agent_id = match agent_id_payload(&request.payload, "agent.focus") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_AGENT_ID", error.to_string()),
                    );
                }
            };
            match runtime
                .attach_client(client_id.clone(), agent_id.clone())
                .await
            {
                Ok(()) => DaemonResponse::ok(
                    request.id,
                    json!({
                        "client_id": client_id.to_string(),
                        "agent_id": agent_id.to_string(),
                        "focused": true,
                    }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("AGENT_FOCUS_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::AgentInterrupt => {
            let agent_id = match agent_id_payload(&request.payload, "agent.interrupt") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_AGENT_ID", error.to_string()),
                    );
                }
            };
            match runtime.interrupt_agent(&agent_id).await {
                Ok(()) => DaemonResponse::ok(
                    request.id,
                    json!({ "agent_id": agent_id.to_string(), "interrupted": true }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("AGENT_INTERRUPT_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::AgentResize => {
            let agent_id = match agent_id_payload(&request.payload, "agent.resize") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_AGENT_ID", error.to_string()),
                    );
                }
            };
            let size = match terminal_size_payload(&request.payload, "agent.resize") {
                Ok(size) => size,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_TERMINAL_SIZE", error.to_string()),
                    );
                }
            };
            match runtime.resize_agent(&agent_id, size).await {
                Ok(()) => DaemonResponse::ok(
                    request.id,
                    json!({
                        "agent_id": agent_id.to_string(),
                        "rows": size.rows,
                        "cols": size.cols,
                        "resized": true,
                    }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("AGENT_RESIZE_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::AgentSnapshot => {
            let agent_id = match agent_id_payload(&request.payload, "agent.snapshot") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_AGENT_ID", error.to_string()),
                    );
                }
            };
            match runtime.snapshot_agent(&agent_id).await {
                Ok(payload) => DaemonResponse::ok(request.id, payload),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("AGENT_SNAPSHOT_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::AgentSendInputScript => {
            let script = match serde_json::from_value::<InputScript>(request.payload) {
                Ok(script) => script,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new(
                            "INVALID_INPUT_SCRIPT",
                            format!("agent.send_input_script payload is invalid: {error}"),
                        ),
                    );
                }
            };
            match runtime.send_input_script(&script).await {
                Ok(()) => DaemonResponse::ok(
                    request.id,
                    json!({
                        "input_script_id": script.id.to_string(),
                        "agent_id": script.target_agent_id.to_string(),
                    }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("INPUT_SCRIPT_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::MeetingOpen => {
            let Some(topic) = payload_string_field(&request.payload, "topic")
                .filter(|topic| !topic.trim().is_empty())
            else {
                return DaemonResponse::error(
                    request.id,
                    ErrorBody::new("INVALID_MEETING_TOPIC", "meeting.open requires topic"),
                );
            };
            let participants: Vec<String> = request
                .payload
                .get("participants")
                .and_then(|value| value.as_array())
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str())
                        .map(ToOwned::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            let kind = match request
                .payload
                .get("kind")
                .and_then(|value| value.as_str())
                .map(parse_message_kind)
                .transpose()
            {
                Ok(kind) => kind.unwrap_or(MessageKind::Question),
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_MESSAGE_KIND", error.to_string()),
                    );
                }
            };
            let priority = match request
                .payload
                .get("priority")
                .and_then(|value| value.as_str())
                .map(parse_priority)
                .transpose()
            {
                Ok(priority) => priority.unwrap_or(Priority::Normal),
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_PRIORITY", error.to_string()),
                    );
                }
            };
            let opened_by = request
                .payload
                .get("from_agent_id")
                .and_then(|value| value.as_str())
                .and_then(|raw| raw.trim().parse::<AgentSessionId>().ok())
                .map(MessageSource::Agent)
                .unwrap_or_else(|| MessageSource::User(ClientId::new()));
            let input = OpenMeetingInput {
                topic,
                participants,
                opened_by,
                max_messages_per_participant: request
                    .payload
                    .get("max_messages_per_participant")
                    .and_then(|value| value.as_u64())
                    .map(|value| value as u32),
                kind,
                priority,
                body: payload_string_field(&request.payload, "body"),
            };
            match runtime.open_meeting(input).await {
                Ok((thread, kickoff)) => DaemonResponse::ok(
                    request.id,
                    json!({
                        "thread": thread_payload(&thread, 1),
                        "kickoff_message": message_payload(&kickoff),
                    }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("MEETING_OPEN_FAILED", error.to_string())
                        .with_hint("check participants with `agentmux sessions`"),
                ),
            }
        }
        IpcCommand::MeetingClose => {
            let Some(thread_id) = request
                .payload
                .get("thread_id")
                .and_then(|value| value.as_str())
                .and_then(|raw| raw.trim().parse::<ThreadId>().ok())
            else {
                return DaemonResponse::error(
                    request.id,
                    ErrorBody::new("INVALID_THREAD_ID", "meeting.close requires thread_id"),
                );
            };
            match runtime.close_meeting(&thread_id).await {
                Ok(thread) => {
                    let count = runtime
                        .list_meetings()
                        .await
                        .iter()
                        .find(|(t, _)| t.id == thread.id)
                        .map(|(_, count)| *count)
                        .unwrap_or(0);
                    DaemonResponse::ok(request.id, thread_payload(&thread, count))
                }
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("MEETING_CLOSE_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::MeetingList => {
            let threads = runtime.list_meetings().await;
            DaemonResponse::ok(
                request.id,
                json!({
                    "threads": threads
                        .iter()
                        .map(|(thread, count)| thread_payload(thread, *count))
                        .collect::<Vec<_>>(),
                }),
            )
        }
        IpcCommand::MessageCreate => {
            let input = match message_create_payload(&request.payload) {
                Ok(input) => input,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_MESSAGE_CREATE", error.to_string()),
                    );
                }
            };
            match runtime.create_message(input).await {
                Ok(message) => {
                    // Deliver to an idle target PTY immediately so `message
                    // send` no longer requires a separate manual `inject`.
                    // Eligibility is delegated to the existing idle-delivery
                    // machinery (delivery_mode + agent status).
                    runtime
                        .trigger_idle_delivery_for_messages(std::slice::from_ref(&message))
                        .await;
                    DaemonResponse::ok(request.id, message_payload(&message))
                }
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("MESSAGE_CREATE_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::MessageList => {
            let messages = runtime.list_messages().await;
            DaemonResponse::ok(
                request.id,
                json!({
                    "messages": messages.iter().map(message_payload).collect::<Vec<_>>(),
                }),
            )
        }
        IpcCommand::MessageShow => {
            let Some(message_id) = request
                .payload
                .get("message_id")
                .and_then(|value| value.as_str())
                .and_then(parse_message_id)
            else {
                return DaemonResponse::error(
                    request.id,
                    ErrorBody::new("INVALID_MESSAGE_ID", "message.show requires message_id"),
                );
            };
            match runtime.get_message(&message_id).await {
                Some(message) => DaemonResponse::ok(request.id, message_payload(&message)),
                None => DaemonResponse::error(
                    request.id,
                    ErrorBody::new(
                        "MESSAGE_NOT_FOUND",
                        format!("unknown message '{message_id}'"),
                    ),
                ),
            }
        }
        IpcCommand::MessageInject => {
            let Some(message_id) = request
                .payload
                .get("message_id")
                .and_then(|value| value.as_str())
                .and_then(parse_message_id)
            else {
                return DaemonResponse::error(
                    request.id,
                    ErrorBody::new("INVALID_MESSAGE_ID", "message.inject requires message_id"),
                );
            };
            let agent_id = request
                .payload
                .get("agent_id")
                .and_then(|value| value.as_str())
                .map(str::parse::<AgentSessionId>)
                .transpose();
            let agent_id = match agent_id {
                Ok(agent_id) => agent_id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_AGENT_ID", error.to_string()),
                    );
                }
            };
            let result = match agent_id {
                Some(agent_id) => {
                    runtime
                        .inject_message_to_agent(&message_id, &agent_id)
                        .await
                }
                None => runtime.inject_message(&message_id).await,
            };
            match result {
                Ok(message) => DaemonResponse::ok(request.id, message_payload(&message)),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("MESSAGE_INJECT_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::ContextCreate => {
            let input = match context_create_payload(&request.payload, runtime).await {
                Ok(input) => input,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_CONTEXT_CREATE", error.to_string()),
                    );
                }
            };
            match runtime.create_context(input).await {
                Ok(item) => DaemonResponse::ok(request.id, context_payload(&item)),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("CONTEXT_CREATE_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::ContextSearch => match context_search_payload(&request.payload) {
            Ok(ContextLookup::List) => {
                let contexts = runtime.list_contexts().await;
                DaemonResponse::ok(
                    request.id,
                    json!({
                        "contexts": contexts.iter().map(context_payload).collect::<Vec<_>>(),
                    }),
                )
            }
            Ok(ContextLookup::Show(context_id)) => match runtime.get_context(&context_id).await {
                Some(item) => DaemonResponse::ok(request.id, context_payload(&item)),
                None => DaemonResponse::error(
                    request.id,
                    ErrorBody::new(
                        "CONTEXT_NOT_FOUND",
                        format!("unknown context item '{context_id}'"),
                    ),
                ),
            },
            Ok(ContextLookup::Search(query)) => {
                let contexts = runtime.search_contexts(&query).await;
                DaemonResponse::ok(
                    request.id,
                    json!({
                        "contexts": contexts.iter().map(context_payload).collect::<Vec<_>>(),
                    }),
                )
            }
            Err(error) => DaemonResponse::error(
                request.id,
                ErrorBody::new("INVALID_CONTEXT_SEARCH", error.to_string()),
            ),
        },
        IpcCommand::ContextAttach => {
            let (context_id, message_id) = match context_attach_payload(&request.payload) {
                Ok(ids) => ids,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_CONTEXT_ATTACH", error.to_string()),
                    );
                }
            };
            match runtime
                .attach_context_to_message(&context_id, &message_id)
                .await
            {
                Ok(message) => DaemonResponse::ok(request.id, message_payload(&message)),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("CONTEXT_ATTACH_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::ContextInject => {
            let (context_id, agent_id) = match context_inject_payload(&request.payload) {
                Ok(ids) => ids,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_CONTEXT_INJECT", error.to_string()),
                    );
                }
            };
            match runtime.inject_context(&context_id, &agent_id).await {
                Ok(item) => DaemonResponse::ok(
                    request.id,
                    json!({
                        "context": context_payload(&item),
                        "agent_id": agent_id.to_string(),
                    }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("CONTEXT_INJECT_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::ContextExport => {
            let Some(output) = request
                .payload
                .get("output")
                .and_then(|value| value.as_str())
            else {
                return DaemonResponse::error(
                    request.id,
                    ErrorBody::new("INVALID_CONTEXT_EXPORT", "context.export requires output"),
                );
            };
            match runtime.export_contexts(Path::new(output)).await {
                Ok(count) => DaemonResponse::ok(
                    request.id,
                    json!({ "output": output, "context_count": count }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("CONTEXT_EXPORT_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::WorktreeList => {
            let worktrees = runtime.list_worktrees().await;
            DaemonResponse::ok(
                request.id,
                json!({
                    "worktrees": worktrees.iter().map(worktree_payload).collect::<Vec<_>>(),
                }),
            )
        }
        IpcCommand::WorktreeDiff => {
            let worktree_id = match worktree_id_payload(&request.payload, "worktree.diff") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_WORKTREE_ID", error.to_string()),
                    );
                }
            };
            match runtime.capture_worktree_diff(&worktree_id).await {
                Ok(payload) => DaemonResponse::ok(request.id, payload),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("WORKTREE_DIFF_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::WorktreeTest => {
            let worktree_id = match worktree_id_payload(&request.payload, "worktree.test") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_WORKTREE_ID", error.to_string()),
                    );
                }
            };
            let command = worktree_test_command_payload(&request.payload);
            match runtime.run_worktree_test(&worktree_id, command).await {
                Ok(payload) => DaemonResponse::ok(request.id, payload),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("WORKTREE_TEST_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::WorktreePromote => {
            let worktree_id = match worktree_id_payload(&request.payload, "worktree.promote") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_WORKTREE_ID", error.to_string()),
                    );
                }
            };
            match runtime.request_worktree_adoption(worktree_id).await {
                Ok(approval) => DaemonResponse::ok(request.id, approval_payload(&approval)),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("WORKTREE_PROMOTE_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::WorktreeArchive => {
            let worktree_id = match worktree_id_payload(&request.payload, "worktree.archive") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_WORKTREE_ID", error.to_string()),
                    );
                }
            };
            match runtime.archive_worktree(&worktree_id).await {
                Ok(worktree) => DaemonResponse::ok(request.id, worktree_payload(&worktree)),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("WORKTREE_ARCHIVE_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::WorktreeAdopt => {
            let worktree_id = match worktree_id_payload(&request.payload, "worktree.adopt") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_WORKTREE_ID", error.to_string()),
                    );
                }
            };
            match runtime.request_worktree_adoption(worktree_id).await {
                Ok(approval) => DaemonResponse::ok(request.id, approval_payload(&approval)),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("WORKTREE_ADOPT_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::ApprovalList => {
            let approvals = runtime.list_approvals().await;
            DaemonResponse::ok(
                request.id,
                json!({
                    "approvals": approvals.iter().map(approval_payload).collect::<Vec<_>>(),
                }),
            )
        }
        IpcCommand::ApprovalApprove => {
            let approval_id = match approval_id_payload(&request.payload, "approval.approve") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_APPROVAL_ID", error.to_string()),
                    );
                }
            };
            match runtime.approve_approval(&approval_id).await {
                Ok(approval) => DaemonResponse::ok(request.id, approval_payload(&approval)),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("APPROVAL_DECISION_FAILED", approval_queue_error(error)),
                ),
            }
        }
        IpcCommand::ApprovalReject => {
            let approval_id = match approval_id_payload(&request.payload, "approval.reject") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_APPROVAL_ID", error.to_string()),
                    );
                }
            };
            match runtime.reject_approval(&approval_id).await {
                Ok(approval) => DaemonResponse::ok(request.id, approval_payload(&approval)),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("APPROVAL_DECISION_FAILED", approval_queue_error(error)),
                ),
            }
        }
        IpcCommand::LayoutSet => {
            let name = match required_string(&request.payload, "name", "layout.set") {
                Ok(name) => name.to_string(),
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_LAYOUT_SET", error.to_string()),
                    );
                }
            };
            let layout = request
                .payload
                .get("layout")
                .cloned()
                .unwrap_or_else(|| json!({ "name": name }));
            match runtime.save_layout(name.clone(), layout.clone()).await {
                Ok(()) => DaemonResponse::ok(
                    request.id,
                    json!({ "name": name, "layout": layout, "saved": true }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("LAYOUT_SET_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::LayoutGet => {
            let Some(name) = request.payload.get("name").and_then(|value| value.as_str()) else {
                let layouts = runtime.list_layouts().await;
                return DaemonResponse::ok(request.id, json!({ "layouts": layouts }));
            };
            match runtime.get_layout(name).await {
                Some(layout) => {
                    DaemonResponse::ok(request.id, json!({ "name": name, "layout": layout }))
                }
                None => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("LAYOUT_NOT_FOUND", format!("unknown layout '{name}'")),
                ),
            }
        }
        _ => DaemonResponse::error(
            request.id,
            ErrorBody::new(
                "COMMAND_NOT_IMPLEMENTED",
                "command is not implemented by the Phase 2 daemon listener",
            ),
        ),
    }
}

pub(crate) fn task_run_payload(payload: &serde_json::Value) -> Result<(String, String, PathBuf, String)> {
    let body = required_string(payload, "body", "task.run")?.to_string();
    let team = payload
        .get("team")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("claude-codex")
        .to_string();
    let project_path = payload
        .get("project_path")
        .and_then(|value| value.as_str())
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir().map_err(|error| {
            AgentmuxError::Internal(format!("failed to resolve current directory: {error}"))
        })?);
    let runner = payload
        .get("runner")
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
        .or_else(|| std::env::var("AGENTMUX_TASK_RUNNER").ok())
        .unwrap_or_else(|| "shell-stub".to_string());
    Ok((body, team, project_path, runner))
}

pub(crate) fn arena_providers_payload(payload: &serde_json::Value) -> Result<Vec<String>> {
    let providers =
        if let Some(values) = payload.get("providers").and_then(|value| value.as_array()) {
            values
                .iter()
                .map(|value| {
                    value.as_str().map(str::to_string).ok_or_else(|| {
                        AgentmuxError::UserError("providers must be strings".to_string())
                    })
                })
                .collect::<Result<Vec<_>>>()?
        } else if let Some(value) = payload.get("arena").and_then(|value| value.as_str()) {
            value.split(',').map(str::to_string).collect()
        } else {
            Vec::new()
        };
    let providers = providers
        .into_iter()
        .map(|provider| provider.trim().to_string())
        .filter(|provider| !provider.is_empty())
        .collect::<Vec<_>>();
    if providers.is_empty() {
        return Err(AgentmuxError::UserError(
            "arena task.run requires providers or arena".to_string(),
        ));
    }
    Ok(providers)
}

pub(crate) fn protocol_error(compatibility: ProtocolCompatibility) -> Option<ErrorBody> {
    match compatibility {
        ProtocolCompatibility::Compatible => None,
        ProtocolCompatibility::VersionMismatch { expected, actual } => Some(ErrorBody::new(
            "PROTOCOL_VERSION_MISMATCH",
            format!("expected protocol {expected}, got {actual}"),
        )),
        ProtocolCompatibility::InvalidHandshake => {
            Some(ErrorBody::new("INVALID_HANDSHAKE", "expected hello frame"))
        }
    }
}

pub(crate) fn parse_agent_session_id(value: &str) -> Option<AgentSessionId> {
    value.parse::<AgentSessionId>().ok()
}

pub(crate) fn agent_id_payload(payload: &serde_json::Value, command: &str) -> Result<AgentSessionId> {
    required_string(payload, "agent_id", command)?
        .parse::<AgentSessionId>()
        .map_err(|error| AgentmuxError::UserError(format!("invalid agent_id: {error}")))
}

pub(crate) fn terminal_size_payload(
    payload: &serde_json::Value,
    command: &str,
) -> Result<agentmux_pty::TerminalSize> {
    let rows = required_u16(payload, "rows", command)?;
    let cols = required_u16(payload, "cols", command)?;
    if rows == 0 || cols == 0 {
        return Err(AgentmuxError::UserError(format!(
            "{command} requires non-zero rows and cols, got {rows}x{cols}"
        )));
    }
    Ok(agentmux_pty::TerminalSize { rows, cols })
}

pub(crate) fn parse_message_id(value: &str) -> Option<MessageId> {
    value.parse::<MessageId>().ok()
}

pub(crate) fn message_create_payload(payload: &serde_json::Value) -> Result<NewAgentMessage> {
    let to = payload
        .get("to")
        .and_then(|value| value.as_str())
        .ok_or_else(|| AgentmuxError::UserError("message.create requires to".to_string()))
        .and_then(parse_message_target)?;
    let body = payload
        .get("body")
        .and_then(|value| value.as_str())
        .ok_or_else(|| AgentmuxError::UserError("message.create requires body".to_string()))?
        .to_string();
    let kind = payload
        .get("kind")
        .and_then(|value| value.as_str())
        .map(parse_message_kind)
        .transpose()?
        .unwrap_or(MessageKind::Handoff);
    let priority = payload
        .get("priority")
        .and_then(|value| value.as_str())
        .map(parse_priority)
        .transpose()?
        .unwrap_or(Priority::Normal);
    let delivery_mode = payload
        .get("delivery_mode")
        .and_then(|value| value.as_str())
        .map(parse_delivery_mode)
        .transpose()?
        .unwrap_or(DeliveryMode::InjectWhenIdle);

    // Attribute the message to the sending agent session when the client passes
    // a valid `from_agent_id` (sourced from the `AGENTMUX_AGENT_ID` env inside a
    // live session). Fall back to an anonymous User source otherwise; a
    // malformed id is treated as absent rather than an error.
    let from = payload
        .get("from_agent_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| raw.trim().parse::<AgentSessionId>().ok())
        .map(MessageSource::Agent)
        .unwrap_or_else(|| MessageSource::User(ClientId::new()));

    let thread_id = payload
        .get("thread_id")
        .and_then(|value| value.as_str())
        .map(|raw| {
            raw.trim()
                .parse::<ThreadId>()
                .map_err(|error| AgentmuxError::UserError(format!("invalid thread_id: {error}")))
        })
        .transpose()?;

    Ok(NewAgentMessage {
        task_id: None,
        thread_id,
        from,
        to,
        kind,
        priority,
        body,
        context_refs: Vec::new(),
        artifact_refs: Vec::new(),
        delivery_mode,
        requires_response: false,
    })
}

pub(crate) fn parse_message_target(raw: &str) -> Result<MessageTarget> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(AgentmuxError::UserError(
            "message target must not be empty".to_string(),
        ));
    }
    if raw == "broadcast" {
        return Ok(MessageTarget::Broadcast);
    }
    if let Some(role) = raw.strip_prefix("role:") {
        return Ok(MessageTarget::Role(parse_agent_role(role)?));
    }
    if let Some(team) = raw.strip_prefix("team:") {
        let team = team.trim();
        if team.is_empty() {
            return Err(AgentmuxError::UserError(
                "team message target must not be empty".to_string(),
            ));
        }
        return Ok(MessageTarget::Team(team.to_string()));
    }
    if let Some(thread) = raw.strip_prefix("thread:") {
        let thread_id = thread.trim().parse::<ThreadId>().map_err(|error| {
            AgentmuxError::UserError(format!("invalid thread message target: {error}"))
        })?;
        return Ok(MessageTarget::Thread(thread_id));
    }
    if raw.starts_with(ThreadId::prefix()) {
        let thread_id = raw.parse::<ThreadId>().map_err(|error| {
            AgentmuxError::UserError(format!("invalid thread message target: {error}"))
        })?;
        return Ok(MessageTarget::Thread(thread_id));
    }
    if let Some(agent) = raw.strip_prefix("agent:") {
        let agent = agent.trim();
        if agent.is_empty() {
            return Err(AgentmuxError::UserError(
                "agent message target must not be empty".to_string(),
            ));
        }
        if let Ok(agent_id) = agent.parse::<AgentSessionId>() {
            return Ok(MessageTarget::Agent(agent_id));
        }
        return Ok(MessageTarget::AgentName(agent.to_string()));
    }
    if let Ok(agent_id) = raw.parse::<AgentSessionId>() {
        return Ok(MessageTarget::Agent(agent_id));
    }
    Ok(MessageTarget::AgentName(raw.to_string()))
}

pub(crate) fn parse_agent_role(raw: &str) -> Result<AgentRole> {
    let normalized = raw.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    if normalized.is_empty() {
        return Err(AgentmuxError::UserError(
            "agent role must not be empty".to_string(),
        ));
    }
    let role = match normalized.as_str() {
        "planner" => AgentRole::Planner,
        "implementer" | "impl" => AgentRole::Implementer,
        "reviewer" | "review" => AgentRole::Reviewer,
        "tester" | "qa" => AgentRole::Tester,
        "debugger" | "debug" => AgentRole::Debugger,
        "refactorer" | "refactor" => AgentRole::Refactorer,
        "security_reviewer" | "security" => AgentRole::SecurityReviewer,
        "docs_writer" | "docs" | "docswriter" => AgentRole::DocsWriter,
        "integrator" => AgentRole::Integrator,
        "context_manager" | "context" => AgentRole::ContextManager,
        _ => AgentRole::Custom(normalized),
    };
    Ok(role)
}

pub(crate) fn parse_message_kind(raw: &str) -> Result<MessageKind> {
    serde_json::from_value(json!(raw))
        .map_err(|error| AgentmuxError::UserError(format!("invalid message kind '{raw}': {error}")))
}

pub(crate) fn parse_priority(raw: &str) -> Result<Priority> {
    serde_json::from_value(json!(raw))
        .map_err(|error| AgentmuxError::UserError(format!("invalid priority '{raw}': {error}")))
}

pub(crate) fn parse_delivery_mode(raw: &str) -> Result<DeliveryMode> {
    serde_json::from_value(json!(raw)).map_err(|error| {
        AgentmuxError::UserError(format!("invalid delivery_mode '{raw}': {error}"))
    })
}


pub(crate) fn message_payload(message: &AgentMessage) -> serde_json::Value {
    json!({
        "message_id": message.id.to_string(),
        "task_id": message.task_id.as_ref().map(ToString::to_string),
        "thread_id": message.thread_id.as_ref().map(ToString::to_string),
        "from": message.from,
        "to": message.to,
        "kind": message.kind,
        "priority": message.priority,
        "body": message.body,
        "context_refs": message.context_refs.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "artifact_refs": message.artifact_refs.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "delivery_mode": message.delivery_mode,
        "delivery_status": message.delivery_status,
        "requires_response": message.requires_response,
        "created_at": message.created_at.to_string(),
        "delivered_at": message.delivered_at.map(|ts| ts.to_string()),
        "read_at": message.read_at.map(|ts| ts.to_string()),
    })
}


pub(crate) fn required_string<'a>(
    payload: &'a serde_json::Value,
    field: &str,
    command: &str,
) -> Result<&'a str> {
    payload
        .get(field)
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AgentmuxError::UserError(format!("{command} requires {field}")))
}

pub(crate) fn required_u16(payload: &serde_json::Value, field: &str, command: &str) -> Result<u16> {
    payload
        .get(field)
        .and_then(|value| value.as_u64())
        .and_then(|value| u16::try_from(value).ok())
        .ok_or_else(|| AgentmuxError::UserError(format!("{command} requires {field}")))
}

pub(crate) fn parse_visibility(raw: &str) -> Result<Visibility> {
    serde_json::from_value(json!(raw))
        .map_err(|error| AgentmuxError::UserError(format!("invalid visibility '{raw}': {error}")))
}

pub(crate) fn worktree_id_payload(payload: &serde_json::Value, command: &str) -> Result<WorktreeId> {
    required_string(payload, "worktree_id", command)?
        .parse::<WorktreeId>()
        .map_err(|error| AgentmuxError::UserError(format!("invalid worktree_id: {error}")))
}

pub(crate) fn approval_id_payload(payload: &serde_json::Value, command: &str) -> Result<ApprovalId> {
    required_string(payload, "approval_id", command)?
        .parse::<ApprovalId>()
        .map_err(|error| AgentmuxError::UserError(format!("invalid approval_id: {error}")))
}

pub(crate) fn worktree_test_command_payload(payload: &serde_json::Value) -> TestCommand {
    TestCommand {
        name: payload
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or("default")
            .to_string(),
        command: payload
            .get("command")
            .and_then(|value| value.as_str())
            .unwrap_or("cargo test")
            .to_string(),
    }
}

pub(crate) fn json_error(error: serde_json::Error) -> AgentmuxError {
    AgentmuxError::StoreError(format!("failed to encode event payload: {error}"))
}

pub(crate) fn pty_spawn_spec_from_payload(payload: &serde_json::Value) -> Result<Option<PtySpawnSpec>> {
    let provider = payload.get("provider").and_then(|value| value.as_str());
    let command = match payload.get("command").and_then(|value| value.as_str()) {
        Some(command) => command.to_string(),
        // No explicit command: derive the launch command from the provider so a
        // bare `shell` pane (or claude/codex) actually gets a live PTY instead of
        // a metadata-only session that nothing can be typed into (spec §05 adapters).
        None => match provider {
            Some("shell") => std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()),
            Some("claude") => "claude".to_string(),
            Some("codex") => "codex".to_string(),
            Some("agy") => "agy".to_string(),
            _ => return Ok(None),
        },
    };

    let args = if let Some(value) = payload.get("args") {
        serde_json::from_value(value.clone()).map_err(|error| {
            AgentmuxError::UserError(format!("agent.spawn args must be strings: {error}"))
        })?
    } else {
        default_provider_args(provider)
    };
    let cwd = payload
        .get("cwd")
        .and_then(|value| value.as_str())
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir().map_err(|error| {
            AgentmuxError::Internal(format!("failed to resolve current directory: {error}"))
        })?);
    let mut env: BTreeMap<String, String> = payload
        .get("env")
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .map_err(|error| {
            AgentmuxError::UserError(format!("agent.spawn env must be a string map: {error}"))
        })?
        .unwrap_or_default();
    // A spawned shell/agent needs a usable environment (PATH, HOME, ...). When the
    // caller does not pass env, inherit the daemon's; always ensure TERM is set.
    if env.is_empty() {
        env = std::env::vars().collect();
    }
    env.entry("TERM".to_string())
        .or_insert_with(|| "xterm-256color".to_string());
    let size = payload
        .get("size")
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .map_err(|error| {
            AgentmuxError::UserError(format!(
                "agent.spawn size must include rows and cols: {error}"
            ))
        })?
        .unwrap_or_default();

    Ok(Some(PtySpawnSpec {
        command,
        args,
        cwd,
        env,
        size,
    }))
}

pub(crate) fn default_provider_args(provider: Option<&str>) -> Vec<String> {
    match provider {
        Some("agy") => vec!["--dangerously-skip-permissions".to_string()],
        _ => Vec::new(),
    }
}

pub(crate) fn provider_command(provider: &str) -> String {
    match provider {
        "shell" => std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()),
        "claude" => "claude".to_string(),
        "codex" => "codex".to_string(),
        "agy" => "agy".to_string(),
        custom => custom.to_string(),
    }
}

