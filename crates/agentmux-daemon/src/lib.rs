//! Runtime pieces for the local agentmux daemon.
//!
//! The daemon keeps live agent/session state in memory and exposes it over
//! JSONL IPC on a Unix domain socket. Persistence and actual provider process
//! spawning are layered on later Phase 2 tasks.

pub(crate) use std::collections::{BTreeMap, BTreeSet};
pub(crate) use std::future::Future;
pub(crate) use std::path::{Path, PathBuf};
pub(crate) use std::process::Command;
pub(crate) use std::sync::{Arc, Mutex};
pub(crate) use std::time::Duration;

pub(crate) use agentmux_agent::adapter::InputSafety;
pub(crate) use agentmux_agent::{
    AgentResult, AgentResultParse, AgentResultStatus, AgentRouteIdentity, EncodedInputStep,
    InputAction, InputScript, StandardWorkflowState, WorkflowHandoffContext,
    advance_standard_workflow, default_claude_codex_team, encode_input_action,
    parse_agent_result_marker, plan_task_run, route_agent_result,
};
pub(crate) use agentmux_context::{
    ContextBroker, ContextItem, ContextPackRequest, MailboxConfig, NewContextItem,
};
pub(crate) use agentmux_core::config::{
    AutomationConfig, ContextConfig, DEFAULT_MESSAGE_INJECT_SEND_DELAY_MS,
    DEFAULT_MESSAGE_PASTE_ENTER_DELAY_MS, DEFAULT_RESULT_DETECTION_TAIL_BYTES,
};
pub(crate) use agentmux_core::{
    AgentProvider, AgentRole, AgentSessionId, AgentStatus, AgentmuxError, ApprovalId,
    AutomationLevel, ClientId, ClientSessionId, ContextItemId, ContextKind, ContextScope,
    ContextSource, DateTimeUtc, DeliveryMode, DeliveryStatus, InputScriptId, MessageId, Priority,
    ProjectId, TaskId, ThreadId, Visibility, WorktreeId, WorktreeStatus, error::Result,
};
pub(crate) use agentmux_ipc::{
    ClientHello, ClientRequest, DaemonEvent, DaemonResponse, ErrorBody, EventSubscribeFilter,
    IpcCommand, IpcEventKind, JsonlReader, JsonlWriter, ProtocolCompatibility,
};
pub(crate) use agentmux_message::{
    AgentDescriptor, AgentMessage, IdleDelivery, MessageBus, MessageKind, MessageSource,
    MessageTarget, MessageThread, NewAgentMessage, NewMessageThread, PreparedInjection,
    PromptContext, PromptContextItem,
};
pub(crate) use agentmux_policy::{
    ApprovalEvent, ApprovalQueue, ApprovalQueueError, ApprovalRequest, PolicyDecision, PolicyEngine,
};
pub(crate) use agentmux_pty::{CTRL_C, PtyHandle, PtyReadEvent, PtySpawnSpec};
pub(crate) use agentmux_store::{EventLog, EventLogEntry};
pub(crate) use agentmux_terminal::TerminalParser;
pub(crate) use agentmux_worktree::{
    CaptureDiff, CreateWorktree, MergeOutcome, TestCommand, TestRunStatus, Worktree,
    WorktreeManager,
};
pub(crate) use serde_json::json;
pub(crate) use tokio::io::BufReader;
pub(crate) use tokio::net::{UnixListener, UnixStream};
pub(crate) use tokio::sync::{RwLock, broadcast, mpsc, watch};
pub(crate) use tokio::task::JoinSet;

mod config;
mod state;
mod runtime;
mod agent;
mod message;
mod context;
mod worktree;
mod approval;
mod meeting;
mod task;
mod result;
mod events;
mod server;

#[cfg(test)]
mod tests;

// Internal cross-module visibility: every submodule does `use crate::*` and
// relies on these globs to reach sibling items (all moved items are
// `pub(crate)`), while the `pub use` lines below preserve the crate's public API.
pub(crate) use approval::*;
pub(crate) use context::*;
pub(crate) use events::*;
pub(crate) use meeting::*;
pub(crate) use message::*;
pub(crate) use result::*;
pub(crate) use server::*;
pub(crate) use state::*;
pub(crate) use worktree::*;

pub use config::{DaemonConfig, RegisteredAgentSession, policy_engine_from_config};
pub use runtime::DaemonRuntime;
pub use server::{handle_client, serve, serve_until_shutdown};
pub use state::OpenMeetingInput;
