//! `agentmux-policy` — Approval policy and safety guardrails.
//!
//! Responsibilities (see `docs/spec/02_system_architecture.md §4.10` and
//! `docs/spec/09_security_policy_approval.md`):
//! - automation level evaluation (Manual / Ask / Auto)
//! - approval policy: classify whether an action requires human sign-off
//! - command classification (safe / dangerous / destructive)
//! - safety guardrails: protected paths, network deny-list, push block
//!
//! Default policy (from spec §9):
//! - network access, git push, secret access, full access → `Deny`
//! - all other actions → `Ask`
//!
//! The policy engine MUST be synchronous and side-effect-free so it can
//! be called in the hot path of every input injection.

pub mod approval;
pub mod policy;

pub use approval::{
    ApprovalEvent, ApprovalGate, ApprovalQueue, ApprovalQueueError, ApprovalRequest,
};
pub use policy::{
    ApprovalPolicy, CommandClassification, CommandSafety, InputSafety, PolicyDecision, PolicyEngine,
};
