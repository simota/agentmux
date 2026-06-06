use agentmux_core::{AgentRole, AgentmuxError, error::Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamTemplate {
    pub name: String,
    pub agents: Vec<TeamAgentSpec>,
}

impl TeamTemplate {
    pub fn planner(&self) -> Result<&TeamAgentSpec> {
        self.agents
            .iter()
            .find(|agent| agent.role == AgentRole::Planner)
            .ok_or_else(|| {
                AgentmuxError::OrchestratorError(format!(
                    "team template '{}' has no planner",
                    self.name
                ))
            })
    }

    pub fn agent_named(&self, name: &str) -> Option<&TeamAgentSpec> {
        self.agents.iter().find(|agent| agent.name == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamAgentSpec {
    pub name: String,
    pub provider: TeamAgentProvider,
    pub role: AgentRole,
    pub worktree: WorktreePolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeamAgentProvider {
    Claude,
    Codex,
    Shell,
    Custom(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreePolicy {
    Main,
    Dedicated,
    Target,
    Readonly,
}

pub fn default_claude_codex_team() -> TeamTemplate {
    TeamTemplate {
        name: "claude-codex".to_string(),
        agents: vec![
            TeamAgentSpec {
                name: "planner".to_string(),
                provider: TeamAgentProvider::Claude,
                role: AgentRole::Planner,
                worktree: WorktreePolicy::Main,
            },
            TeamAgentSpec {
                name: "impl-codex".to_string(),
                provider: TeamAgentProvider::Codex,
                role: AgentRole::Implementer,
                worktree: WorktreePolicy::Dedicated,
            },
            TeamAgentSpec {
                name: "impl-claude".to_string(),
                provider: TeamAgentProvider::Claude,
                role: AgentRole::Implementer,
                worktree: WorktreePolicy::Dedicated,
            },
            TeamAgentSpec {
                name: "tester".to_string(),
                provider: TeamAgentProvider::Shell,
                role: AgentRole::Tester,
                worktree: WorktreePolicy::Target,
            },
            TeamAgentSpec {
                name: "reviewer".to_string(),
                provider: TeamAgentProvider::Codex,
                role: AgentRole::Reviewer,
                worktree: WorktreePolicy::Readonly,
            },
        ],
    }
}

pub(crate) fn role_label(role: &AgentRole) -> &'static str {
    match role {
        AgentRole::Planner => "planner",
        AgentRole::Implementer => "implementer",
        AgentRole::Reviewer => "reviewer",
        AgentRole::Tester => "tester",
        AgentRole::Debugger => "debugger",
        AgentRole::Refactorer => "refactorer",
        AgentRole::SecurityReviewer => "security reviewer",
        AgentRole::DocsWriter => "docs writer",
        AgentRole::Integrator => "integrator",
        AgentRole::ContextManager => "context manager",
        AgentRole::Custom(_) => "custom",
    }
}

pub(crate) fn provider_label(provider: &TeamAgentProvider) -> &str {
    match provider {
        TeamAgentProvider::Claude => "Claude",
        TeamAgentProvider::Codex => "Codex",
        TeamAgentProvider::Shell => "shell",
        TeamAgentProvider::Custom(name) => name.as_str(),
    }
}
