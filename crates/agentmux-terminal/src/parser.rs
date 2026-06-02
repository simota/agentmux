//! VTE-backed ANSI/VT parser — stub.
//!
//! Wraps `vte::Parser` and implements `vte::Perform` to update a `ScreenGrid`.
//!
//! #TODO(agent): implement vte::Perform — handle print, execute, csi_dispatch,
//!               esc_dispatch, osc_dispatch for full VT-220 coverage.

/// Drives incremental parsing of raw PTY output bytes into `ScreenGrid` mutations.
pub struct TerminalParser {
    inner: vte::Parser,
}

impl TerminalParser {
    pub fn new() -> Self {
        Self {
            inner: vte::Parser::new(),
        }
    }

    /// Feed a chunk of raw PTY bytes into the parser.
    ///
    /// `performer` receives all decoded VT actions.
    /// #TODO(agent): accept &mut dyn vte::Perform and wire to ScreenGrid
    pub fn advance(&mut self, bytes: &[u8], performer: &mut impl vte::Perform) {
        for &b in bytes {
            self.inner.advance(performer, b);
        }
    }
}

impl Default for TerminalParser {
    fn default() -> Self {
        Self::new()
    }
}
