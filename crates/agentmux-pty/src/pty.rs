//! PTY handle and terminal size types.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;

use agentmux_core::{AgentmuxError, error::Result};
use portable_pty::{Child, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Terminal dimensions sent on spawn and on resize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSize {
    pub rows: u16,
    pub cols: u16,
}

impl Default for TerminalSize {
    fn default() -> Self {
        // Standard 80×24 terminal.
        Self { rows: 24, cols: 80 }
    }
}

impl TerminalSize {
    fn to_pty_size(self) -> Result<PtySize> {
        if self.rows == 0 || self.cols == 0 {
            return Err(AgentmuxError::PtyError(format!(
                "terminal size must be non-zero, got {}x{}",
                self.cols, self.rows
            )));
        }

        Ok(PtySize {
            rows: self.rows,
            cols: self.cols,
            pixel_width: 0,
            pixel_height: 0,
        })
    }
}

/// Parameters needed to spawn a process inside a PTY.
///
/// Mirrors `docs/spec/04_tui_pty_terminal_design.md §4.2` without tying this
/// crate to higher-level agent provider types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PtySpawnSpec {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub size: TerminalSize,
}

/// Bytes for a terminal bracketed paste sequence.
pub fn bracketed_paste_bytes(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(b"\x1b[200~".len() + text.len() + b"\x1b[201~".len());
    out.extend_from_slice(b"\x1b[200~");
    out.extend_from_slice(text.as_bytes());
    out.extend_from_slice(b"\x1b[201~");
    out
}

/// Terminal byte for Ctrl+C/SIGINT forwarding in raw mode.
pub const CTRL_C: &[u8] = b"\x03";

/// Exit status for a child process spawned in a PTY.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtyExitStatus {
    pub success: bool,
    pub exit_code: u32,
    pub display: String,
}

impl From<ExitStatus> for PtyExitStatus {
    fn from(status: ExitStatus) -> Self {
        Self {
            success: status.success(),
            exit_code: status.exit_code(),
            display: status.to_string(),
        }
    }
}

/// Event emitted by the async PTY read loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtyReadEvent {
    Output(Vec<u8>),
    Eof,
    Error(String),
}

/// Async-facing handle for PTY output produced by a blocking reader task.
pub struct PtyReadLoop {
    receiver: mpsc::Receiver<PtyReadEvent>,
    task: JoinHandle<()>,
}

impl PtyReadLoop {
    /// Receive the next PTY output event.
    pub async fn recv(&mut self) -> Option<PtyReadEvent> {
        self.receiver.recv().await
    }

    /// Whether the underlying blocking read task has finished.
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
}

/// Live PTY process handle.
pub struct PtyHandle {
    master: Option<Box<dyn MasterPty + Send>>,
    child: Box<dyn Child + Send + Sync>,
    writer: Option<Box<dyn Write + Send>>,
}

impl PtyHandle {
    /// Spawn `spec.command` in a new PTY and keep the master-side writer.
    pub fn spawn(spec: PtySpawnSpec) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(spec.size.to_pty_size()?)
            .map_err(|error| AgentmuxError::PtyError(error.to_string()))?;

        let mut command = CommandBuilder::new(&spec.command);
        command.args(&spec.args);
        command.cwd(spec.cwd);
        for (key, value) in spec.env {
            command.env(key, value);
        }

        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| AgentmuxError::PtyError(error.to_string()))?;
        drop(pair.slave);

        let writer = pair
            .master
            .take_writer()
            .map_err(|error| AgentmuxError::PtyError(error.to_string()))?;

        Ok(Self {
            master: Some(pair.master),
            child,
            writer: Some(writer),
        })
    }

    /// Return the child process ID when the platform exposes it.
    pub fn process_id(&self) -> Option<u32> {
        self.child.process_id()
    }

    /// Clone a blocking reader for PTY output.
    pub fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>> {
        self.master_ref()?
            .try_clone_reader()
            .map_err(|error| AgentmuxError::PtyError(error.to_string()))
    }

    /// Start an async read loop for PTY output.
    ///
    /// `portable-pty` exposes a blocking reader, so the actual reads happen on
    /// Tokio's blocking pool and are forwarded over an async channel.
    pub fn spawn_read_loop(&self, channel_capacity: usize) -> Result<PtyReadLoop> {
        let mut reader = self.try_clone_reader()?;
        let capacity = channel_capacity.max(1);
        let (sender, receiver) = mpsc::channel(capacity);

        let task = tokio::task::spawn_blocking(move || {
            let mut buffer = [0_u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => {
                        let _ = sender.blocking_send(PtyReadEvent::Eof);
                        break;
                    }
                    Ok(bytes_read) => {
                        let chunk = buffer[..bytes_read].to_vec();
                        if sender.blocking_send(PtyReadEvent::Output(chunk)).is_err() {
                            break;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) => {
                        let _ = sender.blocking_send(PtyReadEvent::Error(error.to_string()));
                        break;
                    }
                }
            }
        });

        Ok(PtyReadLoop { receiver, task })
    }

    /// Write raw input bytes to the PTY master.
    pub fn write_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer_mut()?
            .write_all(bytes)
            .map_err(|error| AgentmuxError::PtyError(error.to_string()))?;
        self.writer_mut()?
            .flush()
            .map_err(|error| AgentmuxError::PtyError(error.to_string()))
    }

    /// Resize the PTY visible cell dimensions.
    pub fn resize(&self, size: TerminalSize) -> Result<()> {
        self.master_ref()?
            .resize(size.to_pty_size()?)
            .map_err(|error| AgentmuxError::PtyError(error.to_string()))
    }

    /// Poll whether the child process has exited.
    pub fn try_wait(&mut self) -> Result<Option<PtyExitStatus>> {
        self.child
            .try_wait()
            .map(|status| status.map(Into::into))
            .map_err(|error| AgentmuxError::PtyError(error.to_string()))
    }

    /// Block until the child process exits.
    pub fn wait(&mut self) -> Result<PtyExitStatus> {
        self.child
            .wait()
            .map(Into::into)
            .map_err(|error| AgentmuxError::PtyError(error.to_string()))
    }

    /// Terminate the child process.
    pub fn terminate(&mut self) -> Result<()> {
        self.signal_process_group();
        self.close_master_handles();

        self.child
            .kill()
            .map_err(|error| AgentmuxError::PtyError(error.to_string()))
    }

    fn master_ref(&self) -> Result<&(dyn MasterPty + Send)> {
        self.master
            .as_deref()
            .ok_or_else(|| AgentmuxError::PtyError("PTY master is closed".to_string()))
    }

    fn writer_mut(&mut self) -> Result<&mut Box<dyn Write + Send>> {
        self.writer
            .as_mut()
            .ok_or_else(|| AgentmuxError::PtyError("PTY writer is closed".to_string()))
    }

    fn close_master_handles(&mut self) {
        drop(self.writer.take());
        drop(self.master.take());
    }

    #[cfg(unix)]
    fn signal_process_group(&self) {
        let Some(pgid) = self
            .master
            .as_deref()
            .and_then(MasterPty::process_group_leader)
        else {
            return;
        };

        if pgid > 0 {
            // Best effort: closing the master is the portable shutdown path;
            // SIGHUP nudges PTY foreground children that would otherwise keep
            // the slave side open after the direct child is killed.
            let _ = unsafe { libc::kill(-pgid, libc::SIGHUP) };
        }
    }

    #[cfg(not(unix))]
    fn signal_process_group(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell_spec(script: &str) -> PtySpawnSpec {
        let mut env = BTreeMap::new();
        env.insert("TERM".to_string(), "xterm-256color".to_string());

        PtySpawnSpec {
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), script.to_string()],
            cwd: std::env::current_dir().expect("current dir should be available"),
            env,
            size: TerminalSize::default(),
        }
    }

    #[test]
    fn bracketed_paste_wraps_prompt_bytes() {
        let bytes = bracketed_paste_bytes("line 1\nline 2");

        assert_eq!(bytes, b"\x1b[200~line 1\nline 2\x1b[201~");
    }

    #[test]
    fn ctrl_c_is_interrupt_byte() {
        assert_eq!(CTRL_C, b"\x03");
    }

    #[test]
    fn spawn_reads_output_and_waits_for_exit() {
        let handle = PtyHandle::spawn(shell_spec("printf agentmux-pty")).expect("spawn shell");
        let mut reader = handle.try_clone_reader().expect("clone pty reader");
        let mut output = String::new();
        reader.read_to_string(&mut output).expect("read output");

        let mut handle = handle;
        let status = handle.wait().expect("wait for shell");

        assert!(status.success, "unexpected status: {}", status.display);
        assert!(output.contains("agentmux-pty"), "output was {output:?}");
    }

    #[test]
    fn writes_input_to_interactive_process() {
        let mut handle = PtyHandle::spawn(shell_spec("cat")).expect("spawn cat");
        let mut reader = handle.try_clone_reader().expect("clone pty reader");

        handle
            .write_bytes(b"round trip through pty\n")
            .expect("write input");
        handle.terminate().expect("terminate cat");

        let mut output = String::new();
        reader.read_to_string(&mut output).expect("read output");
        let _ = handle.wait();

        assert!(
            output.contains("round trip through pty"),
            "output was {output:?}"
        );
    }

    #[tokio::test]
    async fn async_read_loop_relays_echo_output() {
        let mut handle =
            PtyHandle::spawn(shell_spec("printf async-pty-read")).expect("spawn shell");
        let mut read_loop = handle.spawn_read_loop(4).expect("spawn read loop");

        let mut output = Vec::new();
        while let Some(event) = read_loop.recv().await {
            match event {
                PtyReadEvent::Output(bytes) => {
                    output.extend(bytes);
                    if output
                        .windows(b"async-pty-read".len())
                        .any(|window| window == b"async-pty-read")
                    {
                        break;
                    }
                }
                PtyReadEvent::Eof => break,
                PtyReadEvent::Error(error) => panic!("read loop error: {error}"),
            }
        }

        let status = handle.wait().expect("wait for shell");

        assert!(status.success, "unexpected status: {}", status.display);
        assert!(
            String::from_utf8_lossy(&output).contains("async-pty-read"),
            "output was {:?}",
            String::from_utf8_lossy(&output)
        );
    }

    #[test]
    fn resize_rejects_zero_dimensions() {
        let handle = PtyHandle::spawn(shell_spec("printf ready")).expect("spawn shell");
        let error = handle
            .resize(TerminalSize { rows: 0, cols: 80 })
            .expect_err("zero rows should be invalid");

        assert!(matches!(error, AgentmuxError::PtyError(_)));
    }

    #[test]
    fn terminate_closes_pty_and_reaps_process_with_child_holding_slave() {
        let mut handle =
            PtyHandle::spawn(shell_spec("cat >/dev/null & wait")).expect("spawn shell");

        handle.terminate().expect("terminate process group");

        let mut status = None;
        for _ in 0..200 {
            if let Some(exit) = handle.try_wait().expect("poll child status") {
                status = Some(exit);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        assert!(
            status.is_some(),
            "terminated PTY child should exit within the bounded wait"
        );
        assert!(
            handle.write_bytes(b"after terminate").is_err(),
            "terminated PTY should close its writer"
        );
    }
}
