use std::{io::ErrorKind, time::Duration};

use agentmux_agent::{
    InputAction, InputScript,
    adapter::{InputPrecondition, InputSafety},
};
use agentmux_core::{DateTimeUtc, InputScriptId};
use agentmux_daemon::{DaemonRuntime, handle_client};
use agentmux_ipc::{
    ClientHello, ClientRequest, DaemonEvent, DaemonResponse, IpcCommand, IpcEventKind, JsonlReader,
    JsonlWriter,
};
use agentmux_store::EventLog;
use serde_json::json;
use tokio::io::BufReader;
use tokio::net::{UnixListener, UnixStream};

#[tokio::test]
async fn task_run_shell_stub_handoffs_planner_to_implementer_tester_and_reviewer() {
    let root = std::env::temp_dir().join(format!("agentmux-task-run-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&root).expect("temporary project root is created");
    let event_log_path = root.join(".agentmux").join("events.jsonl");
    let runtime = DaemonRuntime::new(16).with_event_log(EventLog::new(&event_log_path));
    let (client_stream, server_stream) = UnixStream::pair().expect("test IPC pair is created");
    let server = tokio::spawn(handle_client(server_stream, runtime));

    let (reader, writer) = client_stream.into_split();
    let mut reader = JsonlReader::new(BufReader::new(reader));
    let mut writer = JsonlWriter::new(writer);

    writer.write(&ClientHello::new("0.1.0")).await.unwrap();
    writer
        .write(&ClientRequest::new(
            "req_task_run",
            IpcCommand::TaskRun,
            json!({
                "body": "small deterministic task",
                "team": "shell-stub",
                "project_path": root,
                "runner": "shell-stub",
            }),
        ))
        .await
        .unwrap();

    let response = read_response(&mut reader).await;
    assert!(response.ok, "task.run response was {response:?}");
    let payload = response.payload.expect("task.run payload");
    assert_eq!(payload["status"], "completed");
    assert_eq!(payload["stage"], "Completed");
    assert_eq!(payload["shell_processes"].as_array().unwrap().len(), 4);
    assert!(
        payload["final_summary"]
            .as_str()
            .unwrap()
            .contains("approve")
    );

    let handoffs = payload["handoffs"].as_array().expect("handoffs");
    assert_eq!(handoffs.len(), 4);
    assert!(
        handoffs[0]["body"]
            .as_str()
            .unwrap()
            .contains("あなたはplannerです")
    );
    assert!(
        handoffs[1]["body"]
            .as_str()
            .unwrap()
            .contains("kind: TaskAssignment")
    );
    assert!(
        handoffs[2]["body"]
            .as_str()
            .unwrap()
            .contains("kind: TestRequest")
    );
    assert!(
        handoffs[3]["body"]
            .as_str()
            .unwrap()
            .contains("kind: ReviewRequest")
    );

    let event_log = std::fs::read_to_string(&event_log_path).expect("task event is written");
    let events = event_log
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["type"], "task.completed");
    assert_eq!(events[0]["payload"]["status"], "completed");

    server.abort();
    std::fs::remove_dir_all(root).expect("temporary project root is removed");
}

#[tokio::test]
async fn daemon_ipc_spawn_attach_inject_detach_and_reattach_reaches_live_process() {
    let root = std::env::temp_dir().join(format!("agentmux-attach-e2e-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&root).expect("temporary project root is created");
    let output_path = root.join("input.txt");
    let runtime = DaemonRuntime::new(32);

    let (client_stream, server_stream) = UnixStream::pair().expect("test IPC pair is created");
    let first_server = tokio::spawn(handle_client(server_stream, runtime.clone()));
    let (reader, writer) = client_stream.into_split();
    let mut reader = JsonlReader::new(BufReader::new(reader));
    let mut writer = JsonlWriter::new(writer);

    writer.write(&ClientHello::new("0.1.0")).await.unwrap();
    writer
        .write(&ClientRequest::new(
            "req_spawn",
            IpcCommand::AgentSpawn,
            json!({
                "name": "attach-e2e-shell",
                "command": "/bin/sh",
                "args": [
                    "-c",
                    format!(
                        "IFS= read -r line; printf '%s\\n' \"$line\" > {}; while :; do sleep 1; done",
                        output_path.display()
                    )
                ],
                "cwd": root,
                "env": { "TERM": "xterm-256color" },
                "size": { "rows": 24, "cols": 80 },
            }),
        ))
        .await
        .unwrap();
    let spawn_response = read_response(&mut reader).await;
    assert!(spawn_response.ok, "spawn response was {spawn_response:?}");
    let agent_id = spawn_response.payload.unwrap()["agent_id"]
        .as_str()
        .unwrap()
        .to_string();

    writer
        .write(&ClientRequest::new(
            "req_attach",
            IpcCommand::ClientAttach,
            json!({ "agent_id": agent_id }),
        ))
        .await
        .unwrap();
    let (attach_response, attach_events) =
        read_response_and_events(&mut reader, "req_attach", &[IpcEventKind::ClientAttached]).await;
    assert!(
        attach_response.ok,
        "attach response was {attach_response:?}"
    );
    assert_eq!(attach_events[0].payload["agent_id"], agent_id);

    let script = InputScript {
        id: InputScriptId::new(),
        target_agent_id: agent_id.parse().expect("spawned agent id parses"),
        reason: "integration input injection".to_string(),
        preconditions: vec![InputPrecondition::InputLockAvailable],
        actions: vec![
            InputAction::TypeText("ipc input reached the pty".to_string()),
            InputAction::PressEnter,
        ],
        safety: InputSafety::Safe,
        created_at: DateTimeUtc::UNIX_EPOCH,
    };
    writer
        .write(&ClientRequest::new(
            "req_inject",
            IpcCommand::AgentSendInputScript,
            serde_json::to_value(&script).expect("input script serializes"),
        ))
        .await
        .unwrap();
    let (inject_response, inject_events) =
        read_response_and_events(&mut reader, "req_inject", &[IpcEventKind::InputInjected]).await;
    assert!(
        inject_response.ok,
        "inject response was {inject_response:?}"
    );
    assert_eq!(inject_events[0].payload["agent_id"], agent_id);
    assert_file_contains(&output_path, "ipc input reached the pty").await;

    writer
        .write(&ClientRequest::new(
            "req_detach",
            IpcCommand::ClientDetach,
            json!({}),
        ))
        .await
        .unwrap();
    let detach_response = read_response(&mut reader).await;
    assert!(
        detach_response.ok,
        "detach response was {detach_response:?}"
    );

    writer
        .write(&ClientRequest::new(
            "req_status_detached",
            IpcCommand::DaemonStatus,
            json!({}),
        ))
        .await
        .unwrap();
    let detached_status = read_response(&mut reader).await;
    assert!(
        detached_status.ok,
        "status response was {detached_status:?}"
    );
    assert_eq!(
        detached_status.payload.unwrap()["agents"][0]["attached_clients"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    drop(writer);
    drop(reader);
    let _ = first_server.await;

    let (client_stream, server_stream) = UnixStream::pair().expect("test IPC pair is created");
    let second_server = tokio::spawn(handle_client(server_stream, runtime.clone()));
    let (reader, writer) = client_stream.into_split();
    let mut reader = JsonlReader::new(BufReader::new(reader));
    let mut writer = JsonlWriter::new(writer);

    writer.write(&ClientHello::new("0.1.0")).await.unwrap();
    writer
        .write(&ClientRequest::new(
            "req_reattach",
            IpcCommand::ClientAttach,
            json!({ "agent_id": agent_id }),
        ))
        .await
        .unwrap();
    let (reattach_response, reattach_events) =
        read_response_and_events(&mut reader, "req_reattach", &[IpcEventKind::ClientAttached])
            .await;
    assert!(
        reattach_response.ok,
        "reattach response was {reattach_response:?}"
    );
    assert_eq!(reattach_events[0].payload["agent_id"], agent_id);

    writer
        .write(&ClientRequest::new(
            "req_status_reattached",
            IpcCommand::DaemonStatus,
            json!({}),
        ))
        .await
        .unwrap();
    let reattached_status = read_response(&mut reader).await;
    let reattached_payload = reattached_status.payload.unwrap();
    assert_eq!(reattached_payload["agent_count"], 1);
    assert_eq!(reattached_payload["agents"][0]["id"], agent_id);
    assert_eq!(reattached_payload["agents"][0]["has_process"], true);
    assert_eq!(
        reattached_payload["agents"][0]["attached_clients"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    runtime
        .stop_agent(&agent_id.parse().expect("spawned agent id parses"))
        .await
        .expect("live agent is stopped");
    second_server.abort();
    std::fs::remove_dir_all(root).expect("temporary project root is removed");
}

#[tokio::test]
async fn daemon_socket_attach_stream_receives_spawn_event_and_detaches() {
    let socket_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join("t");
    std::fs::create_dir_all(&socket_dir).expect("daemon test socket dir is created");
    let socket_name = ulid::Ulid::new().to_string();
    let socket_path = socket_dir.join(format!("{}.sock", &socket_name[..8]));
    let runtime = DaemonRuntime::new(16);
    let anchor_agent = runtime.register_agent("anchor".to_string()).await;

    let (server, client_stream) = connect_daemon_test_socket(runtime.clone(), &socket_path).await;
    let (reader, writer) = client_stream.into_split();
    let mut reader = JsonlReader::new(BufReader::new(reader));
    let mut writer = JsonlWriter::new(writer);

    writer.write(&ClientHello::new("0.1.0")).await.unwrap();
    writer
        .write(&ClientRequest::new(
            "req_attach",
            IpcCommand::ClientAttach,
            json!({ "agent_id": anchor_agent.id.to_string() }),
        ))
        .await
        .unwrap();
    let (attach_response, attach_events) =
        read_response_and_events(&mut reader, "req_attach", &[IpcEventKind::ClientAttached]).await;
    assert!(
        attach_response.ok,
        "attach response was {attach_response:?}"
    );
    assert_eq!(
        attach_events[0].payload["agent_id"],
        anchor_agent.id.to_string()
    );

    writer
        .write(&ClientRequest::new(
            "req_spawn",
            IpcCommand::AgentSpawn,
            json!({ "name": "socket-spawned" }),
        ))
        .await
        .unwrap();
    let (spawn_response, spawn_events) =
        read_response_and_events(&mut reader, "req_spawn", &[IpcEventKind::AgentSpawned]).await;
    assert!(spawn_response.ok, "spawn response was {spawn_response:?}");
    let spawned_agent_id = spawn_response.payload.unwrap()["agent_id"]
        .as_str()
        .expect("spawn response includes agent id")
        .to_string();
    assert_eq!(spawn_events[0].payload["agent_id"], spawned_agent_id);
    assert_eq!(spawn_events[0].payload["name"], "socket-spawned");

    writer
        .write(&ClientRequest::new(
            "req_detach",
            IpcCommand::ClientDetach,
            json!({}),
        ))
        .await
        .unwrap();
    let detach_response = read_response(&mut reader).await;
    assert!(
        detach_response.ok,
        "detach response was {detach_response:?}"
    );

    runtime.register_agent("after-detach".to_string()).await;
    assert_no_frame(&mut reader).await;

    drop(writer);
    drop(reader);
    let server_result = server.await.expect("server task joins");
    assert!(server_result.is_ok(), "server result was {server_result:?}");
    if socket_path.exists() {
        std::fs::remove_file(&socket_path).expect("daemon test socket is removed");
    }
}

async fn connect_daemon_test_socket(
    runtime: DaemonRuntime,
    socket_path: &std::path::Path,
) -> (
    tokio::task::JoinHandle<agentmux_core::error::Result<()>>,
    UnixStream,
) {
    match UnixListener::bind(socket_path) {
        Ok(listener) => {
            let server_runtime = runtime.clone();
            let server = tokio::spawn(async move {
                let (server_stream, _) = listener.accept().await.expect("client connects");
                handle_client(server_stream, server_runtime).await
            });
            let client_stream = UnixStream::connect(socket_path)
                .await
                .expect("client connects through socket path");
            (server, client_stream)
        }
        Err(error) if error.kind() == ErrorKind::PermissionDenied => {
            let (client_stream, server_stream) =
                UnixStream::pair().expect("sandbox fallback IPC pair is created");
            let server = tokio::spawn(async move { handle_client(server_stream, runtime).await });
            (server, client_stream)
        }
        Err(error) => panic!("daemon test socket is bound: {error}"),
    }
}

async fn read_response<R>(reader: &mut JsonlReader<R>) -> DaemonResponse
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    for _ in 0..8 {
        let frame =
            tokio::time::timeout(Duration::from_secs(2), reader.read::<serde_json::Value>())
                .await
                .expect("daemon response is not timed out")
                .expect("daemon response is readable")
                .expect("daemon response frame exists");
        if frame.get("ok").is_some() {
            return serde_json::from_value(frame).expect("response frame is valid");
        }
    }
    panic!("daemon did not send a response frame");
}

async fn read_response_and_events<R>(
    reader: &mut JsonlReader<R>,
    request_id: &str,
    expected_events: &[IpcEventKind],
) -> (DaemonResponse, Vec<DaemonEvent>)
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let mut response = None;
    let mut events = Vec::new();
    for _ in 0..16 {
        let frame =
            tokio::time::timeout(Duration::from_secs(2), reader.read::<serde_json::Value>())
                .await
                .expect("daemon frame is not timed out")
                .expect("daemon frame is readable")
                .expect("daemon frame exists");

        if frame.get("ok").is_some() {
            let daemon_response: DaemonResponse =
                serde_json::from_value(frame).expect("response frame is valid");
            if daemon_response.id == request_id {
                response = Some(daemon_response);
            }
        } else {
            let event: DaemonEvent = serde_json::from_value(frame).expect("event frame is valid");
            if expected_events.contains(&event.kind) {
                events.push(event);
            }
        }

        if events.len() == expected_events.len() {
            if let Some(response) = response.take() {
                return (response, events);
            }
        }
    }
    panic!("daemon did not send response {request_id:?} with expected events {expected_events:?}");
}

async fn assert_no_frame<R>(reader: &mut JsonlReader<R>)
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    if let Ok(Ok(Some(frame))) = tokio::time::timeout(
        Duration::from_millis(100),
        reader.read::<serde_json::Value>(),
    )
    .await
    {
        panic!("unexpected daemon frame after detach: {frame:?}");
    }
}

async fn assert_file_contains(path: &std::path::Path, needle: &str) {
    for _ in 0..100 {
        if let Ok(content) = std::fs::read_to_string(path)
            && content.contains(needle)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("{} did not contain {needle:?}", path.display());
}
