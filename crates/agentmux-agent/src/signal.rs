//! `StateSignal` — evidence unit used to determine `AgentStatus`.
//!
//! See `docs/spec/03_domain_model.md §13`.
//!
//! Multiple signals from different sources are collected and merged.
//! Resolution follows the priority order encoded in `StateSignalSource` Ord:
//!   HumanOverride > ExplicitMarker > HookEvent > Process >
//!   FileSystemEvent > PtyActivity > ScreenPattern

use agentmux_core::{AgentSessionId, AgentStatus, DateTimeUtc, StateSignalSource};
use serde::{Deserialize, Serialize};

/// A single piece of evidence about the current state of an agent session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateSignal {
    pub agent_id: AgentSessionId,
    pub source: StateSignalSource,
    /// How confident the detector is that this signal is correct (0.0–1.0).
    pub confidence: f32,
    /// The inferred `AgentStatus` from this signal.
    pub value: AgentStatus,
    /// Human-readable excerpt from the screen / hook payload / process info.
    pub evidence: String,
    pub observed_at: DateTimeUtc,
}
