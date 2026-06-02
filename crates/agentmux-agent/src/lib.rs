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
pub mod session;
pub mod signal;

pub use adapter::InteractiveAgentAdapter;
pub use capabilities::AgentCapabilities;
