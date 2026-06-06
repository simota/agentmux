use crate::*;

pub(crate) enum ServerFrame {
    Response(DaemonResponse),
    Event(DaemonEvent),
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
