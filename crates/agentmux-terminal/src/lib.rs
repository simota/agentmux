//! `agentmux-terminal` — Virtual terminal emulation.
//!
//! Responsibilities (see `docs/spec/02_system_architecture.md §4.4`):
//! - integrate `vte` for ANSI / VT-100/220/340 escape-sequence parsing
//! - maintain a `ScreenGrid` (rows × cols of styled cells)
//! - handle alternate screen (smcup / rmcup)
//! - track cursor position and style attributes (bold, fg/bg colour, …)
//! - scrollback buffer
//! - dirty-region tracking for efficient screen-diff delivery to clients
//!
//! #TODO(agent): implement ScreenGrid with Cell type (char + style)
//! #TODO(agent): implement vte::Perform on TerminalParser
//! #TODO(agent): implement scrollback ring buffer
//! #TODO(agent): implement dirty-region bit mask for pane diff delivery

pub mod grid;
pub mod parser;

pub use grid::ScreenGrid;
pub use parser::TerminalParser;
