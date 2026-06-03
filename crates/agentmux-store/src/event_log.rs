//! Append-only JSONL event log.

use std::path::{Path, PathBuf};

use agentmux_core::{
    AgentSessionId, AgentmuxError, ArtifactId, ContextItemId, DateTimeUtc, DeliveryStatus,
    InputScriptId, MessageId, ProjectId, TaskId, error::Result,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::format_description::well_known::Rfc3339;

pub const EVENT_INPUT_SCRIPT_CREATED: &str = "input_script.created";
pub const EVENT_INPUT_SCRIPT_INJECTED: &str = "input_script.injected";
pub const EVENT_MESSAGE_CREATED: &str = "message.created";
pub const EVENT_MESSAGE_DELIVERED: &str = "message.delivered";
pub const EVENT_MESSAGE_INJECTED: &str = "message.injected";
pub const EVENT_CONTEXT_CREATED: &str = "context.created";
pub const EVENT_MAILBOX_WRITTEN: &str = "mailbox.written";
pub const EVENT_AGENT_RESULT: &str = "agent.result";

/// One audit event persisted as one JSON line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventLogEntry {
    pub id: String,
    pub ts: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub payload: Value,
}

impl EventLogEntry {
    pub fn new(kind: impl Into<String>, ts: DateTimeUtc, payload: Value) -> Result<Self> {
        if !payload.is_object() {
            return Err(AgentmuxError::StoreError(
                "event log payload must be a JSON object".to_string(),
            ));
        }

        Ok(Self {
            id: format!("evt_{}", ulid::Ulid::new()),
            ts: ts.format(&Rfc3339).map_err(|error| {
                AgentmuxError::StoreError(format!("invalid timestamp: {error}"))
            })?,
            kind: kind.into(),
            project_id: None,
            task_id: None,
            agent_id: None,
            payload,
        })
    }

    pub fn input_script_created(
        ts: DateTimeUtc,
        input_script_id: InputScriptId,
        target_agent_id: AgentSessionId,
        reason: impl Into<String>,
        safety: impl Serialize,
        action_count: usize,
        actions: impl Serialize,
    ) -> Result<Self> {
        Self::input_script_event(
            EVENT_INPUT_SCRIPT_CREATED,
            ts,
            InputScriptEventPayload {
                input_script_id: input_script_id.to_string(),
                target_agent_id: target_agent_id.to_string(),
                reason: reason.into(),
                safety: serde_json::to_value(safety).map_err(json_error)?,
                action_count,
                actions: serde_json::to_value(actions).map_err(json_error)?,
            },
            target_agent_id,
        )
    }

    pub fn input_script_injected(
        ts: DateTimeUtc,
        input_script_id: InputScriptId,
        target_agent_id: AgentSessionId,
        reason: impl Into<String>,
        safety: impl Serialize,
        action_count: usize,
        actions: impl Serialize,
    ) -> Result<Self> {
        Self::input_script_event(
            EVENT_INPUT_SCRIPT_INJECTED,
            ts,
            InputScriptEventPayload {
                input_script_id: input_script_id.to_string(),
                target_agent_id: target_agent_id.to_string(),
                reason: reason.into(),
                safety: serde_json::to_value(safety).map_err(json_error)?,
                action_count,
                actions: serde_json::to_value(actions).map_err(json_error)?,
            },
            target_agent_id,
        )
    }

    pub fn message_created(
        ts: DateTimeUtc,
        message_id: MessageId,
        task_id: Option<TaskId>,
        payload: MessageEventPayload,
    ) -> Result<Self> {
        let mut entry = Self::new(
            EVENT_MESSAGE_CREATED,
            ts,
            json!({
                "message_id": message_id.to_string(),
                "from": payload.from,
                "to": payload.to,
                "kind": payload.kind,
                "delivery_mode": payload.delivery_mode,
                "delivery_status": payload.delivery_status,
                "context_refs": payload
                    .context_refs
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
                "artifact_refs": payload
                    .artifact_refs
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
            }),
        )?;
        if let Some(task_id) = task_id {
            entry = entry.with_task_id(task_id);
        }
        Ok(entry)
    }

    pub fn message_delivered(
        ts: DateTimeUtc,
        message_id: MessageId,
        task_id: Option<TaskId>,
        agent_id: AgentSessionId,
        status: DeliveryStatus,
    ) -> Result<Self> {
        Self::message_delivery_event(
            EVENT_MESSAGE_DELIVERED,
            ts,
            message_id,
            task_id,
            agent_id,
            status,
        )
    }

    pub fn message_injected(
        ts: DateTimeUtc,
        message_id: MessageId,
        task_id: Option<TaskId>,
        agent_id: AgentSessionId,
        status: DeliveryStatus,
    ) -> Result<Self> {
        Self::message_delivery_event(
            EVENT_MESSAGE_INJECTED,
            ts,
            message_id,
            task_id,
            agent_id,
            status,
        )
    }

    pub fn context_created(
        ts: DateTimeUtc,
        project_id: ProjectId,
        task_id: Option<TaskId>,
        context_item_id: ContextItemId,
        kind: impl Into<String>,
        source: impl Into<String>,
    ) -> Result<Self> {
        let mut entry = Self::new(
            EVENT_CONTEXT_CREATED,
            ts,
            json!({
                "context_item_id": context_item_id.to_string(),
                "kind": kind.into(),
                "source": source.into(),
            }),
        )?
        .with_project_id(project_id);
        if let Some(task_id) = task_id {
            entry = entry.with_task_id(task_id);
        }
        Ok(entry)
    }

    pub fn mailbox_written(
        ts: DateTimeUtc,
        project_id: ProjectId,
        task_id: Option<TaskId>,
        agent_id: Option<AgentSessionId>,
        context_item_id: ContextItemId,
        mailbox_path: impl Into<String>,
        redacted: bool,
    ) -> Result<Self> {
        let mut entry = Self::new(
            EVENT_MAILBOX_WRITTEN,
            ts,
            json!({
                "context_item_id": context_item_id.to_string(),
                "mailbox_path": mailbox_path.into(),
                "redacted": redacted,
            }),
        )?
        .with_project_id(project_id);
        if let Some(task_id) = task_id {
            entry = entry.with_task_id(task_id);
        }
        if let Some(agent_id) = agent_id {
            entry = entry.with_agent_id(agent_id);
        }
        Ok(entry)
    }

    pub fn agent_result(
        ts: DateTimeUtc,
        task_id: Option<TaskId>,
        agent_id: AgentSessionId,
        status: impl Into<String>,
        summary: impl Into<String>,
        changed_files: Vec<String>,
    ) -> Result<Self> {
        let mut entry = Self::new(
            EVENT_AGENT_RESULT,
            ts,
            json!({
                "status": status.into(),
                "summary": summary.into(),
                "changed_files": changed_files,
            }),
        )?
        .with_agent_id(agent_id);
        if let Some(task_id) = task_id {
            entry = entry.with_task_id(task_id);
        }
        Ok(entry)
    }

    pub fn with_agent_id(mut self, agent_id: AgentSessionId) -> Self {
        self.agent_id = Some(agent_id.to_string());
        self
    }

    pub fn with_project_id(mut self, project_id: ProjectId) -> Self {
        self.project_id = Some(project_id.to_string());
        self
    }

    pub fn with_task_id(mut self, task_id: TaskId) -> Self {
        self.task_id = Some(task_id.to_string());
        self
    }

    fn input_script_event(
        kind: &str,
        ts: DateTimeUtc,
        payload: InputScriptEventPayload,
        target_agent_id: AgentSessionId,
    ) -> Result<Self> {
        Ok(Self::new(kind, ts, json!(payload))?.with_agent_id(target_agent_id))
    }

    fn message_delivery_event(
        kind: &str,
        ts: DateTimeUtc,
        message_id: MessageId,
        task_id: Option<TaskId>,
        agent_id: AgentSessionId,
        status: DeliveryStatus,
    ) -> Result<Self> {
        let mut entry = Self::new(
            kind,
            ts,
            json!({
                "message_id": message_id.to_string(),
                "delivery_status": status,
            }),
        )?
        .with_agent_id(agent_id);
        if let Some(task_id) = task_id {
            entry = entry.with_task_id(task_id);
        }
        Ok(entry)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct InputScriptEventPayload {
    input_script_id: String,
    target_agent_id: String,
    reason: String,
    safety: Value,
    action_count: usize,
    actions: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageEventPayload {
    pub from: String,
    pub to: String,
    pub kind: String,
    pub delivery_mode: String,
    pub delivery_status: DeliveryStatus,
    pub context_refs: Vec<ContextItemId>,
    pub artifact_refs: Vec<ArtifactId>,
}

/// File-backed JSONL event log.
#[derive(Debug, Clone)]
pub struct EventLog {
    path: PathBuf,
    rotation: Option<EventLogRotation>,
}

/// Size-based JSONL rotation policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventLogRotation {
    pub max_bytes: u64,
    pub keep: usize,
}

impl EventLogRotation {
    pub fn new(max_bytes: u64, keep: usize) -> Result<Self> {
        if max_bytes == 0 {
            return Err(AgentmuxError::StoreError(
                "event log rotation max_bytes must be greater than zero".to_string(),
            ));
        }
        if keep == 0 {
            return Err(AgentmuxError::StoreError(
                "event log rotation keep must be greater than zero".to_string(),
            ));
        }
        Ok(Self { max_bytes, keep })
    }
}

impl EventLog {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            rotation: None,
        }
    }

    pub fn with_rotation(mut self, rotation: EventLogRotation) -> Self {
        self.rotation = Some(rotation);
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, entry: &EventLogEntry) -> Result<()> {
        let encoded = serde_json::to_vec(entry).map_err(json_error)?;
        self.rotate_if_needed((encoded.len() + 1) as u64)?;

        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|error| {
                AgentmuxError::StoreError(format!(
                    "failed to create event log directory '{}': {error}",
                    parent.display()
                ))
            })?;
        }

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|error| {
                AgentmuxError::StoreError(format!(
                    "failed to open event log '{}': {error}",
                    self.path.display()
                ))
            })?;
        use std::io::Write;
        file.write_all(&encoded).map_err(|error| {
            AgentmuxError::StoreError(format!(
                "failed to write event log '{}': {error}",
                self.path.display()
            ))
        })?;
        file.write_all(b"\n").map_err(|error| {
            AgentmuxError::StoreError(format!(
                "failed to write event log '{}': {error}",
                self.path.display()
            ))
        })?;
        Ok(())
    }

    pub fn append_many<'a>(
        &self,
        entries: impl IntoIterator<Item = &'a EventLogEntry>,
    ) -> Result<()> {
        for entry in entries {
            self.append(entry)?;
        }
        Ok(())
    }

    pub fn read_entries(&self) -> Result<Vec<EventLogEntry>> {
        let mut entries = Vec::new();
        for path in self.log_paths_oldest_first() {
            let content = match std::fs::read_to_string(&path) {
                Ok(content) => content,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(AgentmuxError::StoreError(format!(
                        "failed to read event log '{}': {error}",
                        path.display()
                    )));
                }
            };
            let last_line_number = content.lines().count();
            for (index, line) in content.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<EventLogEntry>(line) {
                    Ok(entry) => entries.push(entry),
                    Err(_) if index + 1 == last_line_number && !line.trim_end().ends_with('}') => {}
                    Err(error) => {
                        return Err(AgentmuxError::StoreError(format!(
                            "invalid event log JSON in '{}': {error}",
                            path.display()
                        )));
                    }
                }
            }
        }
        Ok(entries)
    }

    fn rotate_if_needed(&self, pending_bytes: u64) -> Result<()> {
        let Some(rotation) = self.rotation else {
            return Ok(());
        };
        let current_bytes = match std::fs::metadata(&self.path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(AgentmuxError::StoreError(format!(
                    "failed to inspect event log '{}': {error}",
                    self.path.display()
                )));
            }
        };
        if current_bytes == 0 || current_bytes + pending_bytes <= rotation.max_bytes {
            return Ok(());
        }

        let oldest = rotated_path(&self.path, rotation.keep);
        if oldest.exists() {
            std::fs::remove_file(&oldest).map_err(|error| {
                AgentmuxError::StoreError(format!(
                    "failed to remove rotated event log '{}': {error}",
                    oldest.display()
                ))
            })?;
        }
        for index in (1..rotation.keep).rev() {
            let from = rotated_path(&self.path, index);
            if from.exists() {
                let to = rotated_path(&self.path, index + 1);
                std::fs::rename(&from, &to).map_err(|error| {
                    AgentmuxError::StoreError(format!(
                        "failed to rotate event log '{}' to '{}': {error}",
                        from.display(),
                        to.display()
                    ))
                })?;
            }
        }
        std::fs::rename(&self.path, rotated_path(&self.path, 1)).map_err(|error| {
            AgentmuxError::StoreError(format!(
                "failed to rotate event log '{}': {error}",
                self.path.display()
            ))
        })
    }

    fn log_paths_oldest_first(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if let Some(rotation) = self.rotation {
            for index in (1..=rotation.keep).rev() {
                paths.push(rotated_path(&self.path, index));
            }
        }
        paths.push(self.path.clone());
        paths
    }
}

fn json_error(error: serde_json::Error) -> AgentmuxError {
    AgentmuxError::StoreError(format!("invalid event log JSON: {error}"))
}

fn rotated_path(path: &Path, index: usize) -> PathBuf {
    PathBuf::from(format!("{}.{index}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn appends_one_json_object_per_line() {
        let root = std::env::temp_dir().join(format!("agentmux-event-log-{}", ulid::Ulid::new()));
        let path = root.join(".agentmux").join("events.jsonl");
        let log = EventLog::new(&path);
        let agent_id = AgentSessionId::new();
        let entry = EventLogEntry::new(
            "input_script.injected",
            DateTimeUtc::UNIX_EPOCH,
            json!({"input_script_id": "iscript_test", "action_count": 1}),
        )
        .expect("entry is created")
        .with_agent_id(agent_id.clone());

        log.append(&entry).expect("entry is appended");

        let content = std::fs::read_to_string(log.path()).expect("event log is readable");
        let lines: Vec<_> = content.lines().collect();
        assert_eq!(lines.len(), 1);
        let stored: serde_json::Value = serde_json::from_str(lines[0]).expect("valid JSONL line");
        assert!(stored["id"].as_str().unwrap().starts_with("evt_"));
        assert_eq!(stored["type"], "input_script.injected");
        assert_eq!(stored["ts"], "1970-01-01T00:00:00Z");
        assert_eq!(stored["agent_id"], agent_id.to_string());
        assert_eq!(stored["payload"]["action_count"], 1);

        std::fs::remove_dir_all(root).expect("temporary event log directory is removed");
    }

    #[test]
    fn rejects_non_object_payloads_to_match_schema() {
        let error = EventLogEntry::new("error", DateTimeUtc::UNIX_EPOCH, json!("not an object"))
            .expect_err("payload must be object");

        assert!(error.to_string().contains("payload must be a JSON object"));
    }

    #[test]
    fn builds_cross_cutting_event_payloads_for_schema_required_events() {
        let root = std::env::temp_dir().join(format!("agentmux-event-log-{}", ulid::Ulid::new()));
        let path = root.join(".agentmux").join("events.jsonl");
        let log = EventLog::new(&path);
        let project_id = ProjectId::new();
        let task_id = TaskId::new();
        let agent_id = AgentSessionId::new();
        let message_id = MessageId::new();
        let context_id = ContextItemId::new();
        let artifact_id = ArtifactId::new();

        let entries = [
            EventLogEntry::input_script_injected(
                DateTimeUtc::UNIX_EPOCH,
                InputScriptId::new(),
                agent_id.clone(),
                "handoff",
                "safe",
                1,
                json!([{"paste_text": "hello"}]),
            )
            .expect("input event"),
            EventLogEntry::message_created(
                DateTimeUtc::UNIX_EPOCH,
                message_id.clone(),
                Some(task_id.clone()),
                MessageEventPayload {
                    from: "orchestrator".to_string(),
                    to: "agent:impl".to_string(),
                    kind: "handoff".to_string(),
                    delivery_mode: "inject_when_idle".to_string(),
                    delivery_status: DeliveryStatus::Queued,
                    context_refs: vec![context_id.clone()],
                    artifact_refs: vec![artifact_id.clone()],
                },
            )
            .expect("message created"),
            EventLogEntry::message_injected(
                DateTimeUtc::UNIX_EPOCH,
                message_id,
                Some(task_id.clone()),
                agent_id.clone(),
                DeliveryStatus::Delivered,
            )
            .expect("message injected"),
            EventLogEntry::context_created(
                DateTimeUtc::UNIX_EPOCH,
                project_id.clone(),
                Some(task_id.clone()),
                context_id.clone(),
                "decision",
                "agent",
            )
            .expect("context created"),
            EventLogEntry::mailbox_written(
                DateTimeUtc::UNIX_EPOCH,
                project_id,
                Some(task_id.clone()),
                Some(agent_id.clone()),
                context_id,
                ".agentmux/inbox/impl/ctx-test.md",
                true,
            )
            .expect("mailbox written"),
            EventLogEntry::agent_result(
                DateTimeUtc::UNIX_EPOCH,
                Some(task_id),
                agent_id,
                "completed",
                "done",
                vec!["src/lib.rs".to_string()],
            )
            .expect("agent result"),
        ];

        log.append_many(entries.iter())
            .expect("events are appended");
        let content = std::fs::read_to_string(log.path()).expect("event log is readable");
        let lines: Vec<_> = content.lines().collect();
        assert_eq!(lines.len(), entries.len());

        for line in lines {
            let stored: serde_json::Value = serde_json::from_str(line).expect("valid JSONL event");
            assert!(stored["id"].as_str().unwrap().starts_with("evt_"));
            assert_eq!(stored["ts"], "1970-01-01T00:00:00Z");
            assert!(stored["type"].is_string());
            assert!(stored["payload"].is_object());
            assert!(stored.as_object().unwrap().keys().all(|key| {
                [
                    "id",
                    "ts",
                    "type",
                    "project_id",
                    "task_id",
                    "agent_id",
                    "payload",
                ]
                .contains(&key.as_str())
            }));
        }

        std::fs::remove_dir_all(root).expect("temporary event log directory is removed");
    }

    #[test]
    fn rejects_invalid_rotation_policy() {
        assert!(EventLogRotation::new(0, 1).is_err());
        assert!(EventLogRotation::new(1, 0).is_err());
    }

    #[test]
    fn rotates_event_log_when_next_entry_would_exceed_limit() {
        let root = std::env::temp_dir().join(format!("agentmux-event-log-{}", ulid::Ulid::new()));
        let path = root.join(".agentmux").join("events.jsonl");
        let rotation = EventLogRotation::new(220, 2).expect("rotation policy is valid");
        let log = EventLog::new(&path).with_rotation(rotation);
        let first = EventLogEntry::new(
            "first",
            DateTimeUtc::UNIX_EPOCH,
            json!({"body": "a".repeat(80)}),
        )
        .expect("first event");
        let second = EventLogEntry::new(
            "second",
            DateTimeUtc::UNIX_EPOCH,
            json!({"body": "b".repeat(80)}),
        )
        .expect("second event");

        log.append(&first).expect("first entry is appended");
        log.append(&second).expect("second entry triggers rotation");

        let current = std::fs::read_to_string(&path).expect("current log exists");
        let rotated = std::fs::read_to_string(rotated_path(&path, 1)).expect("rotated log exists");
        assert!(current.contains("\"type\":\"second\""));
        assert!(rotated.contains("\"type\":\"first\""));

        std::fs::remove_dir_all(root).expect("temporary event log directory is removed");
    }

    #[test]
    fn reads_rotated_event_logs_oldest_first_and_ignores_truncated_tail() {
        let root = std::env::temp_dir().join(format!("agentmux-event-log-{}", ulid::Ulid::new()));
        let path = root.join(".agentmux").join("events.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).expect("event log directory is created");
        let rotation = EventLogRotation::new(1024, 2).expect("rotation policy is valid");
        let log = EventLog::new(&path).with_rotation(rotation);
        let older =
            EventLogEntry::new("older", DateTimeUtc::UNIX_EPOCH, json!({})).expect("older event");
        let newer =
            EventLogEntry::new("newer", DateTimeUtc::UNIX_EPOCH, json!({})).expect("newer event");

        std::fs::write(
            rotated_path(&path, 1),
            serde_json::to_string(&older).unwrap() + "\n",
        )
        .expect("rotated log is written");
        std::fs::write(
            &path,
            serde_json::to_string(&newer).unwrap() + "\n{\"id\":\"evt_truncated\"",
        )
        .expect("current log is written");

        let entries = log.read_entries().expect("entries are read");

        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.kind.as_str())
                .collect::<Vec<_>>(),
            ["older", "newer"]
        );

        std::fs::remove_dir_all(root).expect("temporary event log directory is removed");
    }
}
