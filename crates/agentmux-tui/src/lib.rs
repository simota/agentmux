//! `agentmux-tui` — Terminal UI rendering layer.
//!
//! Responsibilities (see `docs/spec/02_system_architecture.md §4.5`):
//! - ratatui-based pane layout (split/resize/zoom)
//! - per-pane rendering: agent TUI mirror, internal views, shell panes
//! - keymap handling (prefix key `Ctrl-g` is NOT forwarded to agents)
//! - command palette overlay
//! - panic hook that restores the terminal to cooked mode on crash
//!
//! This crate is a **library** consumed by `agentmux-cli`.
//! It holds no daemon state — all data comes from IPC events.
//!
//! #TODO(agent): implement PaneLayout (split/resize) using ratatui Layout
//! #TODO(agent): implement terminal restore panic hook
//! #TODO(agent): implement keymap dispatcher (prefix key interception)

pub mod layout;
pub mod render;
