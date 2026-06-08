use crate::*;

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub socket_path: PathBuf,
}

impl DaemonConfig {
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
        }
    }
}

/// Build a [`PolicyEngine`] from project config (`[automation]` + `[policy]`).
///
/// The config stores the automation level and each `allow_*` decision as the
/// PascalCase variant name (`"AutoPrompt"`, `"Allow"`, `"Ask"`, `"Deny"`), so we
/// parse them at this boundary into the typed enums and feed `protected_paths`
/// straight into [`PolicyEngine::with_protected_paths`]. An unrecognized value is
/// a config error rather than a silent fallback.
pub fn policy_engine_from_config(
    automation: &AutomationConfig,
    policy: &agentmux_core::config::PolicyConfig,
) -> Result<PolicyEngine> {
    let approval_policy = agentmux_policy::ApprovalPolicy {
        allow_prompt_injection: PolicyDecision::Allow,
        allow_read_only_commands: parse_policy_decision(&policy.allow_read_only_commands)?,
        allow_workspace_write: parse_policy_decision(&policy.allow_workspace_write)?,
        allow_test_commands: parse_policy_decision(&policy.allow_test_commands)?,
        allow_network: parse_policy_decision(&policy.allow_network)?,
        allow_git_commit: parse_policy_decision(&policy.allow_git_commit)?,
        allow_git_push: parse_policy_decision(&policy.allow_git_push)?,
        allow_delete_files: parse_policy_decision(&policy.allow_delete_files)?,
        allow_secret_access: parse_policy_decision(&policy.allow_secret_access)?,
        allow_full_access: parse_policy_decision(&policy.allow_full_access)?,
    };
    Ok(
        PolicyEngine::with_policy(parse_automation_level(&automation.level)?, approval_policy)
            .with_protected_paths(policy.protected_paths.paths.iter().cloned()),
    )
}

fn parse_automation_level(raw: &str) -> Result<AutomationLevel> {
    match raw.trim() {
        "ObserveOnly" => Ok(AutomationLevel::ObserveOnly),
        "AutoPrompt" => Ok(AutomationLevel::AutoPrompt),
        "AutoPromptAndApproveSafe" => Ok(AutomationLevel::AutoPromptAndApproveSafe),
        "AutoWorkspaceWrite" => Ok(AutomationLevel::AutoWorkspaceWrite),
        "AutoFullAccess" => Ok(AutomationLevel::AutoFullAccess),
        other => Err(AgentmuxError::UserError(format!(
            "invalid automation.level '{other}'"
        ))),
    }
}

fn parse_policy_decision(raw: &str) -> Result<PolicyDecision> {
    match raw.trim() {
        "Allow" => Ok(PolicyDecision::Allow),
        "AllowIfMatchesRules" => Ok(PolicyDecision::AllowIfMatchesRules),
        "Ask" => Ok(PolicyDecision::Ask),
        "Deny" => Ok(PolicyDecision::Deny),
        other => Err(AgentmuxError::UserError(format!(
            "invalid policy decision '{other}'"
        ))),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredAgentSession {
    pub id: AgentSessionId,
    pub name: String,
    pub role: AgentRole,
    pub status: Option<AgentStatus>,
    pub process_id: Option<u32>,
    pub attached_clients: BTreeSet<ClientSessionId>,
}

impl RegisteredAgentSession {
    pub(crate) fn with_role(name: String, role: AgentRole, process_id: Option<u32>) -> Self {
        Self {
            id: AgentSessionId::new(),
            name,
            role,
            status: None,
            process_id,
            attached_clients: BTreeSet::new(),
        }
    }

    pub(crate) fn restored_with_role(id: AgentSessionId, name: String, role: AgentRole) -> Self {
        Self {
            id,
            name,
            role,
            status: None,
            process_id: None,
            attached_clients: BTreeSet::new(),
        }
    }
}
