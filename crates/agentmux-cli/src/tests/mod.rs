//! Unit tests for the `agentmux` CLI (moved verbatim from `main.rs`).

use std::path::PathBuf;

use agentmux_core::{AgentmuxConfig, AgentmuxError, error::Result};
use agentmux_ipc::{ClientRequest, DaemonResponse, DaemonStreamFrame, IpcCommand, PROTOCOL_VERSION};
use agentmux_tui::layout::Rect;
use agentmux_tui::state::{
    AgentProviderChoice, CommandEffect, TerminalSize as TuiTerminalSize, TuiSessionState,
};
use crossterm::event::{MouseButton, MouseEventKind};
use serde_json::{Value, json};

use crate::*;

fn bare_session_spawn_request() -> ClientRequest {
    agent_spawn_for_provider_request(AgentProviderChoice::Codex)
}

fn agent_spawn_for_provider_request(provider: AgentProviderChoice) -> ClientRequest {
    agent_spawn_for_provider_request_with_size(provider, None)
}

fn agent_id_from_spawn_response(response: DaemonResponse) -> Result<String> {
    if !response.ok {
        return Err(response_error("agent.spawn", response));
    }

    response
        .payload
        .and_then(|payload| {
            payload
                .get("agent_id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .ok_or_else(|| AgentmuxError::IpcError("agent.spawn response missing agent_id".to_string()))
}

mod messages;
mod parse;
mod protocol;
mod requests;
mod tui;
