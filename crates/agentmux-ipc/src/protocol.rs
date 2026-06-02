//! IPC protocol envelope types (stub).
//!
//! #TODO(agent): define ClientRequest, DaemonResponse, DaemonEvent enums
//!               with full variant coverage from the spec.

use serde::{Deserialize, Serialize};

/// Placeholder envelope sent from client → daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientRequest {
    /// Protocol version the client was built with.
    pub version: u32,
    /// Opaque JSON payload — will become a tagged enum.
    /// #TODO(agent): replace with typed enum once all variants are defined.
    pub payload: serde_json::Value,
}

/// Placeholder envelope sent from daemon → client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonResponse {
    /// Mirrors the request's version field.
    pub version: u32,
    /// `true` if the request was handled successfully.
    pub ok: bool,
    /// Opaque JSON payload — will become a tagged enum.
    /// #TODO(agent): replace with typed enum.
    pub payload: serde_json::Value,
}
