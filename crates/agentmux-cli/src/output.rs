//! Daemon request transport and human-readable response/output formatting.

use std::path::Path;

use agentmux_core::{AgentmuxError, error::Result};
use agentmux_ipc::{ClientHello, ClientRequest, DaemonResponse, JsonlReader, JsonlWriter};
use serde_json::{Value, json};
use tokio::io::BufReader;
use tokio::net::UnixStream;

use crate::daemon::ensure_daemon;
use crate::requests::message_inject_request;

pub(crate) async fn send_daemon_request(
    socket_path: &Path,
    request: ClientRequest,
) -> Result<DaemonResponse> {
    ensure_daemon(socket_path).await?;
    let stream = UnixStream::connect(socket_path).await.map_err(|error| {
        AgentmuxError::IpcError(format!(
            "failed to connect daemon socket '{}': {error}",
            socket_path.display()
        ))
    })?;
    let request_id = request.id.clone();
    let (reader, writer) = stream.into_split();
    let mut reader = JsonlReader::new(BufReader::new(reader));
    let mut writer = JsonlWriter::new(writer);

    writer
        .write(&ClientHello::new(env!("CARGO_PKG_VERSION")))
        .await?;
    writer.write(&request).await?;

    while let Some(frame) = reader.read::<Value>().await? {
        if frame.get("id").and_then(Value::as_str) == Some(request_id.as_str()) {
            return serde_json::from_value(frame).map_err(|error| {
                AgentmuxError::IpcError(format!("invalid daemon response: {error}"))
            });
        }
    }

    Err(AgentmuxError::IpcError(format!(
        "daemon closed before responding to {request_id}"
    )))
}

pub(crate) async fn send_message_and_maybe_inject(
    socket_path: &Path,
    label: &str,
    create_request: ClientRequest,
    inject: bool,
) -> Result<()> {
    let create_response = send_daemon_request(socket_path, create_request).await?;
    if !inject {
        return print_response(label, create_response);
    }
    if !create_response.ok {
        return Err(response_error(label, create_response));
    }

    let payload = create_response.payload.unwrap_or_else(|| json!({}));
    let message_id = payload
        .get("message_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AgentmuxError::IpcError("message.create response missing message_id".to_string())
        })?
        .to_string();
    let inject_response =
        send_daemon_request(socket_path, message_inject_request(message_id)).await?;
    print_response(label, inject_response)
}

pub(crate) fn response_error(label: &str, response: DaemonResponse) -> AgentmuxError {
    let error = response.error.unwrap_or_else(|| {
        agentmux_ipc::ErrorBody::new(
            "missing_error_body",
            "daemon returned an error without an error body",
        )
    });
    AgentmuxError::UserError(format!(
        "{label} request failed: {} ({}){}",
        error.message,
        error.code,
        error
            .hint
            .map(|hint| format!("; hint: {hint}"))
            .unwrap_or_default()
    ))
}

pub(crate) fn print_response(label: &str, response: DaemonResponse) -> Result<()> {
    if !response.ok {
        return Err(response_error(label, response));
    }

    let payload = response.payload.unwrap_or_else(|| json!({}));
    println!(
        "{}",
        serde_json::to_string_pretty(&payload).map_err(|error| AgentmuxError::IpcError(
            format!("invalid response payload: {error}")
        ))?
    );
    Ok(())
}

pub(crate) fn print_sessions_response(response: DaemonResponse) -> Result<()> {
    if !response.ok {
        return Err(response_error("sessions", response));
    }

    let payload = response.payload.unwrap_or_else(|| json!({}));
    print!("{}", format_sessions_payload(&payload));
    Ok(())
}

#[derive(Debug, Clone, Default)]
pub(crate) struct MessageHistoryFilter {
    pub(crate) limit: usize,
    pub(crate) task: Option<String>,
    pub(crate) thread: Option<String>,
    pub(crate) agent: Option<String>,
    pub(crate) kind: Option<String>,
    pub(crate) status: Option<String>,
}

pub(crate) fn print_message_history_response(
    response: DaemonResponse,
    filter: &MessageHistoryFilter,
) -> Result<()> {
    if !response.ok {
        return Err(response_error("message history", response));
    }

    let payload = response.payload.unwrap_or_else(|| json!({}));
    print!("{}", format_message_history_payload(&payload, filter));
    Ok(())
}

pub(crate) fn format_sessions_payload(payload: &Value) -> String {
    let sessions = payload
        .get("agents")
        .and_then(Value::as_array)
        .map(|agents| {
            agents
                .iter()
                .filter(|agent| {
                    agent
                        .get("has_process")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if sessions.is_empty() {
        return "no running sessions\n".to_string();
    }

    let mut output = String::from("ID NAME ROLE STATUS INPUT PID CLIENTS\n");
    for session in sessions {
        let id = session.get("id").and_then(Value::as_str).unwrap_or("-");
        let name = session.get("name").and_then(Value::as_str).unwrap_or("-");
        let role = session.get("role").and_then(Value::as_str).unwrap_or("-");
        let status = session.get("status").and_then(Value::as_str).unwrap_or("-");
        let input = if session
            .get("input_ready")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            "ready"
        } else {
            "-"
        };
        let pid = session
            .get("process_id")
            .and_then(Value::as_u64)
            .map(|pid| pid.to_string())
            .unwrap_or_else(|| "-".to_string());
        let clients = session
            .get("attached_clients")
            .and_then(Value::as_array)
            .map(|clients| clients.len().to_string())
            .unwrap_or_else(|| "0".to_string());
        output.push_str(&format!(
            "{id} {name} {role} {status} {input} {pid} {clients}\n"
        ));
    }
    output
}

pub(crate) fn format_message_history_payload(payload: &Value, filter: &MessageHistoryFilter) -> String {
    let mut messages = payload
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages
                .iter()
                .filter(|message| message_matches_history_filter(message, filter))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    messages.sort_by(|left, right| {
        message_string_field(right, "created_at").cmp(&message_string_field(left, "created_at"))
    });

    let limit = filter.limit.max(1);
    if messages.is_empty() {
        return "no messages\n".to_string();
    }

    let mut output = String::from(
        "CREATED              STATUS               KIND                 FROM                 TO                   ID                   BODY\n",
    );
    for message in messages.into_iter().take(limit) {
        let created = compact_timestamp(&message_string_field(message, "created_at"));
        let status = message_string_field(message, "delivery_status");
        let kind = message_string_field(message, "kind");
        let from = message_endpoint_label(message.get("from"));
        let to = message_endpoint_label(message.get("to"));
        let id = message_string_field(message, "message_id");
        let body = truncate_for_table(&message_string_field(message, "body"), 72);
        output.push_str(&format!(
            "{:<20} {:<20} {:<20} {:<20} {:<20} {:<20} {}\n",
            truncate_for_table(&created, 20),
            truncate_for_table(&status, 20),
            truncate_for_table(&kind, 20),
            truncate_for_table(&from, 20),
            truncate_for_table(&to, 20),
            truncate_for_table(&id, 20),
            body,
        ));
    }
    output
}

pub(crate) fn message_matches_history_filter(message: &Value, filter: &MessageHistoryFilter) -> bool {
    if let Some(task) = filter.task.as_deref() {
        if message_string_field(message, "task_id") != task {
            return false;
        }
    }

    if let Some(thread) = filter.thread.as_deref() {
        if message_string_field(message, "thread_id") != thread {
            return false;
        }
    }

    if let Some(kind) = filter.kind.as_deref() {
        if !message_string_field(message, "kind").eq_ignore_ascii_case(kind) {
            return false;
        }
    }

    if let Some(status) = filter.status.as_deref() {
        if !message_string_field(message, "delivery_status").eq_ignore_ascii_case(status) {
            return false;
        }
    }

    if let Some(agent) = filter.agent.as_deref() {
        let from = message_endpoint_label(message.get("from"));
        let to = message_endpoint_label(message.get("to"));
        if from != agent && to != agent && !from.ends_with(agent) && !to.ends_with(agent) {
            return false;
        }
    }

    true
}

pub(crate) fn message_string_field(message: &Value, field: &str) -> String {
    message
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| "-".to_string())
}

pub(crate) fn message_endpoint_label(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return "-".to_string();
    };
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let id = value.get("id").and_then(Value::as_str).unwrap_or("-");
    if id == "-" {
        kind.to_string()
    } else {
        format!("{kind}:{id}")
    }
}

pub(crate) fn compact_timestamp(value: &str) -> String {
    value
        .strip_suffix("+00:00")
        .unwrap_or(value)
        .replace('T', " ")
}

pub(crate) fn truncate_for_table(value: &str, max_chars: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    let mut truncated = normalized
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    truncated.push('…');
    truncated
}
