//! Pure orchestration decisions for the v0.1 task workflow.
//!
//! The daemon owns sockets, PTYs, persistence, and actual delivery. This module
//! keeps team template resolution, planner bootstrap, result-driven routing,
//! and stalled detection deterministic and unit-testable.

mod message;
mod plan;
mod routing;
mod stall;
mod team;
mod workflow;

pub use message::OrchestratorMessage;
pub use plan::{TaskRunPlan, plan_task_run};
pub use routing::{
    AgentRouteIdentity, OrchestratorResult, ResultRouting, route_agent_result,
    route_agent_result_parse,
};
pub use stall::{StallDecision, StalledDetector};
pub use team::{
    TeamAgentProvider, TeamAgentSpec, TeamTemplate, WorktreePolicy, default_claude_codex_team,
};
pub use workflow::{
    FinalSummary, StandardWorkflowStage, StandardWorkflowState, WorkflowAdvance,
    WorkflowHandoffContext, WorkflowTurnRecord, advance_standard_workflow,
};

#[cfg(test)]
mod tests;
