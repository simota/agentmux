use std::time::Duration;

use agentmux_core::{AgentRole, AgentStatus};
use agentmux_daemon::{DaemonRuntime, handle_client};
use agentmux_ipc::{
    ClientHello, ClientRequest, DaemonEvent, DaemonResponse, DaemonStreamFrame,
    EventSubscribeFilter, IpcCommand, IpcEventKind, JsonlReader, JsonlWriter,
};
use serde_json::json;
use tokio::io::BufReader;
use tokio::net::UnixStream;

type TestReader = JsonlReader<BufReader<tokio::net::unix::OwnedReadHalf>>;
type TestWriter = JsonlWriter<tokio::net::unix::OwnedWriteHalf>;

async fn connected_client(runtime: DaemonRuntime) -> (TestReader, TestWriter) {
    let (client, server) = UnixStream::pair().unwrap();
    tokio::spawn(async move {
        handle_client(server, runtime).await.unwrap();
    });
    let (reader, writer) = client.into_split();
    let reader = JsonlReader::new(BufReader::new(reader));
    let mut writer = JsonlWriter::new(writer);
    writer.write(&ClientHello::new("0.1.0")).await.unwrap();
    (reader, writer)
}

async fn read_until_response(reader: &mut TestReader, response_id: &str) -> Vec<DaemonEvent> {
    let mut events = Vec::new();
    loop {
        match reader.read::<DaemonStreamFrame>().await.unwrap().unwrap() {
            DaemonStreamFrame::Response(response) => {
                assert_response(response, response_id);
                return events;
            }
            DaemonStreamFrame::Event(event) => events.push(event),
        }
    }
}

async fn drain_events(reader: &mut TestReader) -> Vec<DaemonEvent> {
    let mut events = Vec::new();
    while let Ok(Ok(Some(frame))) = tokio::time::timeout(
        Duration::from_millis(25),
        reader.read::<DaemonStreamFrame>(),
    )
    .await
    {
        match frame {
            DaemonStreamFrame::Response(response) => {
                panic!("unexpected response while draining events: {}", response.id);
            }
            DaemonStreamFrame::Event(event) => events.push(event),
        }
    }
    events
}

fn assert_response(response: DaemonResponse, response_id: &str) {
    assert_eq!(response.id, response_id);
    assert!(response.ok, "response failed: {:?}", response.error);
}

async fn spawn_and_attach(
    reader: &mut TestReader,
    writer: &mut TestWriter,
    agent_name: &str,
) -> String {
    writer
        .write(&ClientRequest::new(
            "req_spawn",
            IpcCommand::AgentSpawn,
            json!({ "name": agent_name, "role": "implementer" }),
        ))
        .await
        .unwrap();
    let agent_id = loop {
        match reader.read::<DaemonStreamFrame>().await.unwrap().unwrap() {
            DaemonStreamFrame::Response(response) => {
                assert_response(response.clone(), "req_spawn");
                break response
                    .payload
                    .as_ref()
                    .and_then(|payload| payload["agent_id"].as_str())
                    .map(ToOwned::to_owned);
            }
            DaemonStreamFrame::Event(_) => {}
        }
    }
    .expect("agent id");
    writer
        .write(&ClientRequest::new(
            "req_attach",
            IpcCommand::ClientAttach,
            json!({ "agent_id": agent_id }),
        ))
        .await
        .unwrap();
    read_until_response(reader, "req_attach").await;
    drain_events(reader).await;
    agent_id
}

async fn create_message(
    reader: &mut TestReader,
    writer: &mut TestWriter,
    agent_id: &str,
    request_id: &str,
) -> Vec<DaemonEvent> {
    writer
        .write(&ClientRequest::new(
            request_id,
            IpcCommand::MessageCreate,
            json!({
                "to": agent_id,
                "body": "please review",
                "kind": "handoff",
                "priority": "normal",
                "delivery_mode": "inject_when_idle",
            }),
        ))
        .await
        .unwrap();
    let mut events = read_until_response(reader, request_id).await;
    events.extend(drain_events(reader).await);
    events
}

async fn subscribe(
    reader: &mut TestReader,
    writer: &mut TestWriter,
    request_id: &str,
    filter: EventSubscribeFilter,
) {
    writer
        .write(&ClientRequest::new(
            request_id,
            IpcCommand::EventSubscribe,
            serde_json::to_value(filter).unwrap(),
        ))
        .await
        .unwrap();
    read_until_response(reader, request_id).await;
    drain_events(reader).await;
}

#[tokio::test]
async fn subscribed_client_filters_unmatched_event_kinds() {
    let runtime = DaemonRuntime::new(32);
    let (mut reader, mut writer) = connected_client(runtime).await;
    let agent_id = spawn_and_attach(&mut reader, &mut writer, "impl-codex").await;

    writer
        .write(&ClientRequest::new(
            "req_subscribe",
            IpcCommand::EventSubscribe,
            serde_json::to_value(EventSubscribeFilter {
                task_id: None,
                roles: Vec::new(),
                kinds: vec!["agent.status_changed".to_string()],
            })
            .unwrap(),
        ))
        .await
        .unwrap();
    read_until_response(&mut reader, "req_subscribe").await;
    drain_events(&mut reader).await;

    let events = create_message(&mut reader, &mut writer, &agent_id, "req_message").await;

    assert!(
        !events
            .iter()
            .any(|event| event.kind == IpcEventKind::MessageCreated)
    );
}

#[tokio::test]
async fn second_event_subscribe_replaces_previous_filter() {
    let runtime = DaemonRuntime::new(32);
    let (mut reader, mut writer) = connected_client(runtime).await;
    let agent_id = spawn_and_attach(&mut reader, &mut writer, "impl-codex").await;

    subscribe(
        &mut reader,
        &mut writer,
        "req_subscribe_status",
        EventSubscribeFilter {
            task_id: None,
            roles: Vec::new(),
            kinds: vec!["agent.status_changed".to_string()],
        },
    )
    .await;
    subscribe(
        &mut reader,
        &mut writer,
        "req_subscribe_all",
        EventSubscribeFilter {
            task_id: None,
            roles: Vec::new(),
            kinds: Vec::new(),
        },
    )
    .await;

    let events = create_message(&mut reader, &mut writer, &agent_id, "req_message").await;

    assert!(
        events
            .iter()
            .any(|event| event.kind == IpcEventKind::MessageCreated)
    );
}

#[tokio::test]
async fn clients_keep_independent_event_filters() {
    let runtime = DaemonRuntime::new(32);
    let (mut spawn_reader, mut spawn_writer) = connected_client(runtime.clone()).await;
    let agent_id = spawn_and_attach(&mut spawn_reader, &mut spawn_writer, "impl-codex").await;
    let (mut spawn_events_reader, mut spawn_events_writer) =
        connected_client(runtime.clone()).await;
    let (mut message_events_reader, mut message_events_writer) = connected_client(runtime).await;

    spawn_events_writer
        .write(&ClientRequest::new(
            "req_attach_spawn_events",
            IpcCommand::ClientAttach,
            json!({ "agent_id": agent_id }),
        ))
        .await
        .unwrap();
    read_until_response(&mut spawn_events_reader, "req_attach_spawn_events").await;
    drain_events(&mut spawn_events_reader).await;
    message_events_writer
        .write(&ClientRequest::new(
            "req_attach_message_events",
            IpcCommand::ClientAttach,
            json!({ "agent_id": agent_id }),
        ))
        .await
        .unwrap();
    read_until_response(&mut message_events_reader, "req_attach_message_events").await;
    drain_events(&mut message_events_reader).await;

    subscribe(
        &mut spawn_events_reader,
        &mut spawn_events_writer,
        "req_subscribe_spawn",
        EventSubscribeFilter {
            task_id: None,
            roles: Vec::new(),
            kinds: vec!["agent.spawned".to_string()],
        },
    )
    .await;
    subscribe(
        &mut message_events_reader,
        &mut message_events_writer,
        "req_subscribe_message",
        EventSubscribeFilter {
            task_id: None,
            roles: Vec::new(),
            kinds: vec!["message.created".to_string()],
        },
    )
    .await;

    spawn_writer
        .write(&ClientRequest::new(
            "req_spawn_second",
            IpcCommand::AgentSpawn,
            json!({ "name": "reviewer", "role": "reviewer" }),
        ))
        .await
        .unwrap();
    read_until_response(&mut spawn_reader, "req_spawn_second").await;
    let spawn_client_events = drain_events(&mut spawn_events_reader).await;
    let message_client_events = drain_events(&mut message_events_reader).await;

    assert!(
        spawn_client_events
            .iter()
            .any(|event| event.kind == IpcEventKind::AgentSpawned)
    );
    assert!(
        !message_client_events
            .iter()
            .any(|event| event.kind == IpcEventKind::AgentSpawned)
    );

    let _ = create_message(
        &mut spawn_reader,
        &mut spawn_writer,
        &agent_id,
        "req_message_for_clients",
    )
    .await;
    let spawn_client_events = drain_events(&mut spawn_events_reader).await;
    let message_client_events = drain_events(&mut message_events_reader).await;

    assert!(
        !spawn_client_events
            .iter()
            .any(|event| event.kind == IpcEventKind::MessageCreated)
    );
    assert!(
        message_client_events
            .iter()
            .any(|event| event.kind == IpcEventKind::MessageCreated)
    );
}

#[tokio::test]
async fn unsubscribed_client_still_receives_all_events() {
    let runtime = DaemonRuntime::new(32);
    let (mut reader, mut writer) = connected_client(runtime).await;
    let agent_id = spawn_and_attach(&mut reader, &mut writer, "impl-codex").await;

    let events = create_message(&mut reader, &mut writer, &agent_id, "req_message").await;

    assert!(
        events
            .iter()
            .any(|event| event.kind == IpcEventKind::MessageCreated)
    );
}

#[tokio::test]
async fn role_filter_forwards_agent_status_changed_without_payload_role() {
    let runtime = DaemonRuntime::new(32);
    let agent = runtime
        .register_agent_with_role("tester-a1b2c3".to_string(), AgentRole::Tester)
        .await;
    let (mut reader, mut writer) = connected_client(runtime.clone()).await;

    writer
        .write(&ClientRequest::new(
            "req_attach",
            IpcCommand::ClientAttach,
            json!({ "agent_id": agent.id.to_string() }),
        ))
        .await
        .unwrap();
    read_until_response(&mut reader, "req_attach").await;
    drain_events(&mut reader).await;
    subscribe(
        &mut reader,
        &mut writer,
        "req_subscribe_tester_status",
        EventSubscribeFilter {
            task_id: None,
            roles: vec!["tester".to_string()],
            kinds: vec!["agent.status_changed".to_string()],
        },
    )
    .await;

    runtime
        .apply_agent_status_signal(&agent.id, AgentStatus::AwaitingInput, "needs input")
        .await
        .unwrap();
    let events = drain_events(&mut reader).await;

    assert!(events.iter().any(|event| {
        event.kind == IpcEventKind::AgentStatusChanged
            && event.payload["agent_id"] == agent.id.to_string()
            && event.payload.get("role").is_none()
    }));
}
