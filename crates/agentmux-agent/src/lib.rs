//! `agentmux-agent` — Agent session management and provider adapters.
//!
//! Responsibilities (see `docs/spec/02_system_architecture.md §4.6` and
//! `docs/spec/05_agent_adapter_design.md`):
//! - `AgentSession` lifecycle management (spawn → monitor → teardown)
//! - Provider-specific adapters (`ClaudeCodeTuiAdapter`, `CodexTuiAdapter`, …)
//!   each implementing `InteractiveAgentAdapter`
//! - Multi-source `StateSignal` detection and priority resolution
//! - `AGENTMUX_RESULT` marker parser (do NOT attempt JSON repair on bad output)
//!
//! Provider-specific details MUST stay inside adapter implementations.
//! The orchestration layer only touches `AgentSession`, `InputScript`,
//! `StateSignal`, and `AgentMessage`.

pub mod adapter;
pub mod capabilities;
pub mod input;
pub mod orchestrator;
pub mod result;
pub mod session;
pub mod signal;

pub use adapter::{InputAction, InputScript, InteractiveAgentAdapter};
pub use capabilities::AgentCapabilities;
pub use input::{
    EncodedInputStep, InputPreconditionState, ReadyInputWrite, check_input_preconditions,
    encode_input_action, write_input_script_to_pty, write_input_script_to_pty_when_ready,
};
pub use orchestrator::{
    AgentRouteIdentity, FinalSummary, OrchestratorMessage, OrchestratorResult, ResultRouting,
    StallDecision, StalledDetector, StandardWorkflowStage, StandardWorkflowState, TaskRunPlan,
    TeamAgentSpec, TeamTemplate, WorkflowAdvance, WorkflowHandoffContext, WorkflowTurnRecord,
    WorktreePolicy, advance_standard_workflow, default_claude_codex_team, plan_task_run,
    route_agent_result, route_agent_result_parse,
};
pub use result::{
    AgentResult, AgentResultParse, AgentResultStatus, ContextUpdate, ContextUpdateKind,
    OutgoingMessage, OutgoingMessageKind, OutgoingPriority, ParsedAgentResult, RESULT_MARKER,
    ResultRecommendation, ResultRisk, StatusProbeRequest, parse_agent_result_marker,
};
pub use session::{InputActivity, InputLock, InputOwner};
