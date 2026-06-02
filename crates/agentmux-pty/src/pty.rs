//! PTY handle and terminal size types (stub).
//!
//! #TODO(agent): implement PtyHandle wrapping portable_pty master/slave pair
//! #TODO(agent): implement async read loop using spawn_blocking

use serde::{Deserialize, Serialize};

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
