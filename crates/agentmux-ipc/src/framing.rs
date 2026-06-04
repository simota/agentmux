//! JSON Lines framing for IPC streams.

use agentmux_core::{AgentmuxError, error::Result};
use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

/// Maximum accepted JSONL frame size. Large screen/context payloads should use
/// files or chunking instead of unbounded single-line messages.
pub const MAX_JSONL_FRAME_BYTES: usize = 1024 * 1024;

pub struct JsonlReader<R> {
    inner: R,
}

impl<R> JsonlReader<R> {
    pub fn new(inner: R) -> Self {
        Self { inner }
    }

    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R> JsonlReader<R>
where
    R: AsyncBufRead + Unpin,
{
    pub async fn read<T>(&mut self) -> Result<Option<T>>
    where
        T: DeserializeOwned,
    {
        read_jsonl(&mut self.inner).await
    }
}

pub struct JsonlWriter<W> {
    inner: W,
}

impl<W> JsonlWriter<W> {
    pub fn new(inner: W) -> Self {
        Self { inner }
    }

    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W> JsonlWriter<W>
where
    W: AsyncWrite + Unpin,
{
    pub async fn write<T>(&mut self, value: &T) -> Result<()>
    where
        T: Serialize,
    {
        write_jsonl(&mut self.inner, value).await
    }
}

pub async fn read_jsonl<R, T>(reader: &mut R) -> Result<Option<T>>
where
    R: AsyncBufRead + Unpin,
    T: DeserializeOwned,
{
    let mut line = String::new();
    let bytes_read = reader
        .read_line(&mut line)
        .await
        .map_err(|error| AgentmuxError::IpcError(format!("failed to read JSONL frame: {error}")))?;

    if bytes_read == 0 {
        return Ok(None);
    }

    if bytes_read > MAX_JSONL_FRAME_BYTES {
        return Err(AgentmuxError::IpcError(format!(
            "JSONL frame exceeds {MAX_JSONL_FRAME_BYTES} bytes"
        )));
    }

    let trimmed = line.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() {
        return Err(AgentmuxError::IpcError(
            "empty JSONL frame is not valid IPC".to_string(),
        ));
    }

    serde_json::from_str(trimmed)
        .map(Some)
        .map_err(|error| AgentmuxError::IpcError(format!("invalid JSONL frame: {error}")))
}

pub async fn write_jsonl<W, T>(writer: &mut W, value: &T) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let mut encoded = serde_json::to_vec(value).map_err(|error| {
        AgentmuxError::IpcError(format!("failed to encode JSONL frame: {error}"))
    })?;
    if encoded.len() > MAX_JSONL_FRAME_BYTES {
        return Err(AgentmuxError::IpcError(format!(
            "JSONL frame exceeds {MAX_JSONL_FRAME_BYTES} bytes"
        )));
    }

    encoded.push(b'\n');
    writer.write_all(&encoded).await.map_err(|error| {
        AgentmuxError::IpcError(format!("failed to write JSONL frame: {error}"))
    })?;
    writer
        .flush()
        .await
        .map_err(|error| AgentmuxError::IpcError(format!("failed to flush JSONL frame: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        ClientRequest, DaemonEvent, DaemonResponse, DaemonStreamFrame, IpcCommand, IpcEventKind,
    };
    use serde_json::json;
    use tokio::io::{AsyncWriteExt, BufReader, duplex};

    #[tokio::test]
    async fn writes_and_reads_typed_jsonl_frame() {
        let (client, server) = duplex(4096);
        let mut writer = JsonlWriter::new(client);
        let mut reader = JsonlReader::new(BufReader::new(server));
        let request = ClientRequest::new(
            "req_001",
            IpcCommand::DaemonStatus,
            json!({ "verbose": true }),
        );

        writer.write(&request).await.unwrap();

        let decoded: ClientRequest = reader.read().await.unwrap().unwrap();
        assert_eq!(decoded.id, "req_001");
        assert_eq!(decoded.command, IpcCommand::DaemonStatus);
        assert_eq!(decoded.payload["verbose"], true);
    }

    #[tokio::test]
    async fn read_returns_none_on_clean_eof() {
        let (client, server) = duplex(128);
        drop(client);
        let mut reader = JsonlReader::new(BufReader::new(server));

        let decoded: Option<ClientRequest> = reader.read().await.unwrap();
        assert!(decoded.is_none());
    }

    #[tokio::test]
    async fn read_rejects_invalid_json_line() {
        let (mut client, server) = duplex(128);
        client.write_all(b"{not-json}\n").await.unwrap();
        let mut reader = JsonlReader::new(BufReader::new(server));

        let error = reader.read::<ClientRequest>().await.unwrap_err();
        assert!(error.to_string().contains("invalid JSONL frame"));
    }

    #[tokio::test]
    async fn write_rejects_oversized_frame() {
        let (client, _server) = duplex(128);
        let mut writer = JsonlWriter::new(client);
        let request = ClientRequest::new(
            "req_big",
            IpcCommand::MessageCreate,
            json!({ "body": "x".repeat(MAX_JSONL_FRAME_BYTES) }),
        );

        let error = writer.write(&request).await.unwrap_err();
        assert!(error.to_string().contains("JSONL frame exceeds"));
    }

    #[tokio::test]
    async fn reads_mixed_daemon_stream_frames() {
        let (client, server) = duplex(4096);
        let mut writer = JsonlWriter::new(client);
        let mut reader = JsonlReader::new(BufReader::new(server));

        writer
            .write(&DaemonStreamFrame::from(DaemonResponse::ok(
                "req_attach",
                json!({ "client_id": "client_001" }),
            )))
            .await
            .unwrap();
        writer
            .write(&DaemonStreamFrame::from(DaemonEvent::new(
                IpcEventKind::AgentSpawned,
                json!({ "agent_id": "agent_001" }),
            )))
            .await
            .unwrap();

        let first: DaemonStreamFrame = reader.read().await.unwrap().unwrap();
        let second: DaemonStreamFrame = reader.read().await.unwrap().unwrap();

        assert!(matches!(first, DaemonStreamFrame::Response(_)));
        assert!(matches!(second, DaemonStreamFrame::Event(_)));
    }
}
