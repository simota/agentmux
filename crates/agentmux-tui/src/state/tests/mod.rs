//! Unit tests for the TUI session state module.

use super::*;
use crate::keymap::{FocusDirection, TuiCommand};
use agentmux_ipc::protocol::{DaemonEvent, IpcEventKind};
use serde_json::json;

fn event(kind: IpcEventKind, payload: serde_json::Value) -> DaemonEvent {
    DaemonEvent::new(kind, payload)
}

#[cfg(feature = "arena")]
mod arena;
mod commands;
#[cfg(feature = "activity-feed")]
mod feed;
mod lifecycle;
mod messages;
mod session_list;
mod terminal;
