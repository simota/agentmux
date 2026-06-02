//! `Store` — SQLite connection wrapper (stub).
//!
//! #TODO(agent): open connection, run migrations, expose typed CRUD methods

use std::path::Path;

use agentmux_core::error::Result;

/// The main persistence handle for the agentmux daemon.
///
/// Wraps a `rusqlite::Connection` (single-writer model for v0.1).
/// For async usage the store writer runs on a dedicated blocking task.
pub struct Store {
    // #TODO(agent): replace with a real connection or connection pool
    _db_path: std::path::PathBuf,
}

impl Store {
    /// Open (or create) the SQLite database at `db_path`.
    ///
    /// Runs embedded DDL migrations on first open.
    ///
    /// # Errors
    /// Returns `AgentmuxError::StoreError` if the file cannot be opened or
    /// migrations fail.
    pub fn open(db_path: &Path) -> Result<Self> {
        // #TODO(agent): call rusqlite::Connection::open(db_path)
        // #TODO(agent): run CREATE TABLE IF NOT EXISTS migrations
        Ok(Self {
            _db_path: db_path.to_owned(),
        })
    }
}
