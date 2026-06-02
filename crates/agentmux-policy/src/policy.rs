//! Policy engine — stub.
//!
//! #TODO(agent): implement full rule evaluation over ApprovalKind + context
//! #TODO(agent): implement protected-path glob matching
//! #TODO(agent): implement network denylist

use agentmux_core::{ApprovalKind, AutomationLevel};
use serde::{Deserialize, Serialize};

/// The outcome of a policy evaluation for a proposed action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyDecision {
    /// Action is safe to proceed automatically.
    Allow,
    /// Action requires explicit human approval before proceeding.
    Ask,
    /// Action is permanently denied regardless of automation level.
    Deny,
}

/// Stateless policy evaluator.
///
/// Instantiate once per daemon; methods are `&self` (no mutation needed).
pub struct PolicyEngine {
    /// Global automation level — overrides per-action rules when set to Manual.
    pub automation_level: AutomationLevel,
}

impl PolicyEngine {
    pub fn new(automation_level: AutomationLevel) -> Self {
        Self { automation_level }
    }

    /// Evaluate whether `kind` of approval action is permitted.
    ///
    /// Default policy (from spec §9):
    /// - `NetworkAccess`, `GitPush`, `SecretAccess`, `FullAccess` → `Deny`
    /// - everything else → `Ask` (escalated to `Allow` if AutomationLevel::Auto)
    pub fn evaluate(&self, kind: &ApprovalKind) -> PolicyDecision {
        use ApprovalKind::*;
        match kind {
            NetworkAccess | GitPush | SecretAccess | FullAccess => PolicyDecision::Deny,
            _ => match self.automation_level {
                AutomationLevel::Manual => PolicyDecision::Ask,
                AutomationLevel::Ask => PolicyDecision::Ask,
                AutomationLevel::Auto => PolicyDecision::Allow,
            },
        }
    }
}
