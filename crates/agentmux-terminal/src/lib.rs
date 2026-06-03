//! `agentmux-terminal` — Virtual terminal emulation.
//!
//! Responsibilities (see `docs/spec/02_system_architecture.md §4.4`):
//! - integrate `vte` for ANSI / VT-100/220/340 escape-sequence parsing
//! - maintain a `ScreenGrid` (rows × cols of styled cells)
//! - handle alternate screen (smcup / rmcup)
//! - track cursor position and style attributes (bold, fg/bg colour, …)
//! - scrollback buffer
//! - dirty-region tracking for efficient screen-diff delivery to clients

pub mod grid;
pub mod parser;

pub use grid::{
    Cell, CellStyle, CellWidth, CursorState, DirtyRegion, Line, ScreenGrid, TerminalColor,
};
pub use parser::{ActiveScreen, TerminalParser};
