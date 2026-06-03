//! `InteractiveAgentAdapter` trait and associated supporting types.
//!
//! See `docs/spec/05_agent_adapter_design.md §3`.
//!
//! All provider adapters (ClaudeCode, Codex, Shell, Custom) implement this
//! trait. The orchestrator only calls methods on this trait — never on the
//! concrete adapter type.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

pub use agentmux_context::ContextPack;
use agentmux_core::{AgentProvider, AgentRole, ProjectId, TaskId, error::Result};
use agentmux_pty::{PtyHandle, PtyReadEvent, PtyReadLoop, PtySpawnSpec, TerminalSize};
use agentmux_terminal::{ScreenGrid, TerminalParser};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::capabilities::AgentCapabilities;
use crate::signal::StateSignal;

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

/// Opaque handle returned by `InteractiveAgentAdapter::spawn`.
///
/// The orchestrator passes this back for all subsequent operations on a
/// live agent session.
pub struct AgentHandle {
    /// Human-readable identifier used in log messages.
    pub name: String,
    /// OS PID of the spawned agent process, once known.
    pub process_id: Option<u32>,
    /// Capabilities declared by the adapter at spawn time.
    pub capabilities: AgentCapabilities,
    pty: Arc<Mutex<PtyHandle>>,
    output: Arc<Mutex<PtyReadLoop>>,
    screen: Arc<Mutex<TerminalParser>>,
}

impl AgentHandle {
    /// Spawn a PTY-backed process and keep both master-side control and output channel state.
    pub fn spawn_pty(
        name: impl Into<String>,
        capabilities: AgentCapabilities,
        spec: PtySpawnSpec,
        output_channel_capacity: usize,
    ) -> Result<Self> {
        let size = spec.size;
        let pty = PtyHandle::spawn(spec)?;
        let output = pty.spawn_read_loop(output_channel_capacity)?;

        Ok(Self::from_pty_with_size(
            name,
            capabilities,
            pty,
            output,
            size,
        ))
    }

    /// Wrap an already-spawned PTY and read loop in an adapter handle.
    pub fn from_pty(
        name: impl Into<String>,
        capabilities: AgentCapabilities,
        pty: PtyHandle,
        output: PtyReadLoop,
    ) -> Self {
        Self::from_pty_with_size(name, capabilities, pty, output, TerminalSize::default())
    }

    /// Wrap an already-spawned PTY and read loop with an explicit screen size.
    pub fn from_pty_with_size(
        name: impl Into<String>,
        capabilities: AgentCapabilities,
        pty: PtyHandle,
        output: PtyReadLoop,
        size: TerminalSize,
    ) -> Self {
        let process_id = pty.process_id();

        Self {
            name: name.into(),
            process_id,
            capabilities,
            pty: Arc::new(Mutex::new(pty)),
            output: Arc::new(Mutex::new(output)),
            screen: Arc::new(Mutex::new(TerminalParser::new(size.rows, size.cols))),
        }
    }

    /// Write raw bytes to the PTY master.
    pub async fn write_pty_bytes(&self, bytes: &[u8]) -> Result<()> {
        self.pty.lock().await.write_bytes(bytes)
    }

    /// Resize the PTY visible cell dimensions.
    pub async fn resize_pty(&self, size: TerminalSize) -> Result<()> {
        self.pty.lock().await.resize(size)?;
        self.screen.lock().await.resize(size.rows, size.cols);
        Ok(())
    }

    /// Receive the next event from the async PTY output channel.
    pub async fn recv_pty_event(&self) -> Option<PtyReadEvent> {
        let event = self.output.lock().await.recv().await;
        if let Some(PtyReadEvent::Output(bytes)) = &event {
            self.screen.lock().await.advance(bytes);
        }
        event
    }

    /// Poll whether the child process has exited.
    pub async fn try_wait(&self) -> Result<Option<agentmux_pty::PtyExitStatus>> {
        self.pty.lock().await.try_wait()
    }

    /// Capture the current terminal grid after applying any buffered PTY output.
    pub async fn snapshot_screen(&self) -> ScreenSnapshot {
        let mut output = self.output.lock().await;
        let mut screen = self.screen.lock().await;

        while let Some(event) = output.try_recv() {
            if let PtyReadEvent::Output(bytes) = event {
                screen.advance(&bytes);
            }
        }

        ScreenSnapshot {
            grid: screen.grid().clone(),
        }
    }
}

/// All parameters needed to spawn a new agent session.
///
/// See `docs/spec/05_agent_adapter_design.md §4`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSpawnSpec {
    pub project_id: ProjectId,
    pub task_id: Option<TaskId>,
    pub name: String,
    pub provider: AgentProvider,
    pub role: AgentRole,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub initial_size: TerminalSize,
    pub permission_profile: PermissionProfile,
    pub startup_prompt: Option<String>,
}

/// Permission boundary applied when spawning an agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionProfile {
    /// Provider-neutral compatibility profile: read-only access to the worktree.
    Readonly,
    /// Provider-neutral compatibility profile: writes confined to the dedicated worktree.
    WorkspaceWrite,
    /// Provider-neutral compatibility profile: full access, requiring a manual gate before spawn.
    FullAccess,
    /// Codex CLI sandbox and approval settings.
    Codex(CodexPermissionProfile),
    /// Claude Code permission mode.
    ClaudeCode(ClaudePermissionProfile),
}

impl PermissionProfile {
    /// Recommended Codex startup profile from the adapter spec.
    pub fn codex_workspace_write_on_request() -> Self {
        Self::Codex(CodexPermissionProfile {
            sandbox: CodexSandboxMode::WorkspaceWrite,
            approval_policy: CodexApprovalPolicy::OnRequest,
            network_access: false,
        })
    }

    /// Conservative Claude Code startup profile.
    pub fn claude_default() -> Self {
        Self::ClaudeCode(ClaudePermissionProfile {
            permission_mode: ClaudePermissionMode::Default,
        })
    }

    /// Whether this profile crosses the v0.1 full-access boundary.
    pub fn requires_manual_spawn_approval(&self) -> bool {
        matches!(
            self,
            Self::FullAccess
                | Self::Codex(CodexPermissionProfile {
                    sandbox: CodexSandboxMode::DangerFullAccess,
                    ..
                })
                | Self::ClaudeCode(ClaudePermissionProfile {
                    permission_mode: ClaudePermissionMode::BypassPermissions,
                })
        )
    }
}

/// Codex CLI permission settings.
///
/// Field values intentionally mirror Codex's native config vocabulary so
/// provider adapters can render them directly as flags or TOML.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexPermissionProfile {
    pub sandbox: CodexSandboxMode,
    pub approval_policy: CodexApprovalPolicy,
    pub network_access: bool,
}

/// Codex sandbox modes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodexSandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

/// Codex approval policies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodexApprovalPolicy {
    Untrusted,
    OnFailure,
    OnRequest,
    Never,
}

/// Claude Code permission settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudePermissionProfile {
    pub permission_mode: ClaudePermissionMode,
}

/// Claude Code permission modes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClaudePermissionMode {
    #[serde(rename = "default")]
    Default,
    #[serde(rename = "acceptEdits")]
    AcceptEdits,
    #[serde(rename = "bypassPermissions")]
    BypassPermissions,
    #[serde(rename = "plan")]
    Plan,
}

/// A frozen screenshot of the agent's current terminal screen.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScreenSnapshot {
    pub grid: ScreenGrid,
}

impl ScreenSnapshot {
    pub fn rows(&self) -> u16 {
        self.grid.rows()
    }

    pub fn cols(&self) -> u16 {
        self.grid.cols()
    }
}

/// An ordered sequence of input actions to send to a running agent.
///
/// See `docs/spec/03_domain_model.md §12`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputScript {
    pub id: agentmux_core::InputScriptId,
    pub target_agent_id: agentmux_core::AgentSessionId,
    pub reason: String,
    pub preconditions: Vec<InputPrecondition>,
    pub actions: Vec<InputAction>,
    pub safety: InputSafety,
    pub created_at: agentmux_core::DateTimeUtc,
}

/// A precondition that must hold before automated input is sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputPrecondition {
    AgentIdle,
    InputLockAvailable,
    QuietFor(std::time::Duration),
}

/// Coarse safety classification for automated input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputSafety {
    Safe,
    NeedsApproval,
    Dangerous,
}

/// Atomic input action the adapter can send to the PTY.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputAction {
    TypeText(String),
    PasteText(String),
    PressEnter,
    PressEsc,
    PressTab,
    PressBackspace,
    PressCtrl(char),
    PressAlt(char),
    SendRaw(Vec<u8>),
    Wait(std::time::Duration),
}

// ---------------------------------------------------------------------------
// Core trait
// ---------------------------------------------------------------------------

/// The single interface through which the orchestrator controls any agent.
///
/// Each provider (ClaudeCode, Codex, Shell, Custom) implements this trait.
/// Provider-specific details MUST NOT leak outside the implementing type.
///
/// All methods return `agentmux_core::error::Result<T>` so callers can
/// handle errors through the unified `AgentmuxError` enum.
#[async_trait]
pub trait InteractiveAgentAdapter: Send + Sync {
    /// Spawn a new agent process according to `spec` and return a live handle.
    async fn spawn(&self, spec: AgentSpawnSpec) -> Result<AgentHandle>;

    /// Send an `InputScript` (sequence of key actions) to the agent's PTY.
    ///
    /// Callers must hold the input lock before calling this method.
    async fn send_input_script(&self, handle: &AgentHandle, script: InputScript) -> Result<()>;

    /// Send a SIGINT (Ctrl-C) to interrupt a running command within the agent.
    async fn interrupt(&self, handle: &AgentHandle) -> Result<()>;

    /// Notify the agent's PTY of a terminal resize.
    async fn resize(&self, handle: &AgentHandle, size: TerminalSize) -> Result<()>;

    /// Capture a point-in-time screenshot of the agent's terminal screen.
    async fn snapshot_screen(&self, handle: &AgentHandle) -> Result<ScreenSnapshot>;

    /// Analyse a screen snapshot and produce `StateSignal`s.
    ///
    /// The caller merges signals from all sources and resolves the final
    /// `AgentStatus` by priority order.
    async fn detect_state(
        &self,
        handle: &AgentHandle,
        snapshot: &ScreenSnapshot,
    ) -> Result<Vec<StateSignal>>;

    /// Render a handoff prompt string to inject into the agent's input.
    ///
    /// Inline if `context_pack` is small; path-based if large (ADR-0005).
    async fn render_handoff_prompt(
        &self,
        message: &agentmux_core::AgentSessionId, // placeholder — will be AgentMessage
        context_pack: &ContextPack,
    ) -> Result<String>;
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

    fn shell_spec_with_size(script: &str, size: TerminalSize) -> PtySpawnSpec {
        let mut spec = shell_spec(script);
        spec.size = size;
        spec
    }

    #[test]
    fn context_pack_comes_from_context_crate() {
        use agentmux_context::{ContextBroker, ContextPackRequest, NewContextItem};
        use agentmux_core::{
            ContextKind, ContextScope, ContextSource, ProjectId, TaskId, Visibility,
        };

        fn accepts_adapter_context_pack(pack: &ContextPack) -> usize {
            pack.inline_items.len()
                + pack.mailbox_files.len()
                + pack.artifact_refs.len()
                + pack.omitted_items.len()
        }

        let project_id = ProjectId::new();
        let task_id = TaskId::new();
        let mut broker = ContextBroker::new();
        let item = broker
            .create_item(NewContextItem {
                project_id: project_id.clone(),
                task_id: Some(task_id.clone()),
                scope: ContextScope::Task,
                kind: ContextKind::TaskBrief,
                title: "Brief".to_string(),
                body: "Use the shared context broker pack.".to_string(),
                source: ContextSource::Human,
                visibility: Visibility::Internal,
                confidence: 1.0,
                tags: Vec::new(),
                related_files: Vec::new(),
                artifact_refs: Vec::new(),
            })
            .expect("context item should be valid");

        let pack = broker
            .select_pack(ContextPackRequest {
                project_id,
                task_id: Some(task_id),
                attached_context_ids: vec![item.id.clone()],
                max_inline_chars: 1024,
            })
            .expect("context pack selection should succeed");

        assert_eq!(accepts_adapter_context_pack(&pack), 1);
        assert_eq!(pack.inline_items[0].id, item.id);
    }

    #[test]
    fn permission_profile_keeps_provider_neutral_legacy_names() {
        let encoded = serde_json::to_string(&PermissionProfile::WorkspaceWrite)
            .expect("serialize legacy profile");
        assert_eq!(encoded, "\"workspace_write\"");

        let decoded: PermissionProfile =
            serde_json::from_str("\"readonly\"").expect("deserialize legacy profile");
        assert_eq!(decoded, PermissionProfile::Readonly);
    }

    #[test]
    fn codex_permission_profile_matches_native_sandbox_and_approval_names() {
        let profile = PermissionProfile::codex_workspace_write_on_request();

        let encoded = serde_json::to_value(&profile).expect("serialize codex profile");
        assert_eq!(
            encoded,
            serde_json::json!({
                "codex": {
                    "sandbox": "workspace-write",
                    "approval_policy": "on-request",
                    "network_access": false
                }
            })
        );

        assert!(!profile.requires_manual_spawn_approval());
        assert!(
            PermissionProfile::Codex(CodexPermissionProfile {
                sandbox: CodexSandboxMode::DangerFullAccess,
                approval_policy: CodexApprovalPolicy::OnRequest,
                network_access: true,
            })
            .requires_manual_spawn_approval()
        );
    }

    #[test]
    fn claude_permission_profile_matches_native_permission_mode_names() {
        let accept_edits = PermissionProfile::ClaudeCode(ClaudePermissionProfile {
            permission_mode: ClaudePermissionMode::AcceptEdits,
        });
        let bypass = PermissionProfile::ClaudeCode(ClaudePermissionProfile {
            permission_mode: ClaudePermissionMode::BypassPermissions,
        });

        assert_eq!(
            serde_json::to_value(&accept_edits).expect("serialize claude profile"),
            serde_json::json!({
                "claude_code": {
                    "permission_mode": "acceptEdits"
                }
            })
        );
        assert!(!accept_edits.requires_manual_spawn_approval());
        assert!(bypass.requires_manual_spawn_approval());
        assert!(!PermissionProfile::claude_default().requires_manual_spawn_approval());
    }

    #[tokio::test]
    async fn agent_handle_keeps_pty_process_id_and_output_channel() {
        let handle = AgentHandle::spawn_pty(
            "shell-test",
            AgentCapabilities::minimal(),
            shell_spec("read line; printf 'agentmux:%s' \"$line\""),
            4,
        )
        .expect("spawn PTY-backed handle");

        assert_eq!(handle.name, "shell-test");
        assert!(handle.process_id.is_some());

        handle
            .write_pty_bytes(b"adapter-output\n")
            .await
            .expect("write through PTY master");

        let mut output = Vec::new();
        while let Some(event) = handle.recv_pty_event().await {
            match event {
                PtyReadEvent::Output(bytes) => {
                    output.extend(bytes);
                    if output
                        .windows(b"agentmux:adapter-output".len())
                        .any(|window| window == b"agentmux:adapter-output")
                    {
                        break;
                    }
                }
                PtyReadEvent::Eof => break,
                PtyReadEvent::Error(error) => panic!("PTY read loop error: {error}"),
            }
        }

        let status = loop {
            if let Some(status) = handle.try_wait().await.expect("poll child status") {
                break status;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };

        assert!(status.success, "unexpected status: {}", status.display);
        assert!(
            String::from_utf8_lossy(&output).contains("agentmux:adapter-output"),
            "output was {:?}",
            String::from_utf8_lossy(&output)
        );
    }

    #[tokio::test]
    async fn agent_handle_resizes_held_pty() {
        let handle = AgentHandle::spawn_pty(
            "resize-test",
            AgentCapabilities::minimal(),
            shell_spec("printf ready"),
            1,
        )
        .expect("spawn PTY-backed handle");

        let error = handle
            .resize_pty(TerminalSize { rows: 0, cols: 80 })
            .await
            .expect_err("zero rows should be rejected");

        assert!(matches!(error, agentmux_core::AgentmuxError::PtyError(_)));
    }

    #[tokio::test]
    async fn agent_handle_snapshots_screen_grid_from_pty_output() {
        let handle = AgentHandle::spawn_pty(
            "snapshot-test",
            AgentCapabilities::minimal(),
            shell_spec_with_size(
                "printf '\\033[31;1mOK\\033[0m\\nnext'",
                TerminalSize { rows: 2, cols: 8 },
            ),
            8,
        )
        .expect("spawn PTY-backed handle");

        let mut captured = None;
        for _ in 0..100 {
            let snapshot = handle.snapshot_screen().await;
            if snapshot
                .grid
                .line_text(0)
                .is_some_and(|line| line.contains("OK"))
            {
                captured = Some(snapshot);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let snapshot = captured.expect("screen should contain PTY output");

        assert_eq!(snapshot.rows(), 2);
        assert_eq!(snapshot.cols(), 8);
        assert_eq!(snapshot.grid.line_text(0).as_deref(), Some("OK      "));
        assert_eq!(
            snapshot.grid.cell(0, 0).map(|cell| cell.style.fg),
            Some(Some(agentmux_terminal::TerminalColor::Indexed(1)))
        );
    }
}
