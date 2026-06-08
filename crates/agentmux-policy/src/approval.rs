//! In-memory approval queue and typed approval audit events.

use std::collections::BTreeMap;

use agentmux_core::{
    ApprovalId, ApprovalKind, ApprovalStatus, ContextItemId, RiskLevel, WorktreeId,
};
use serde::{Deserialize, Serialize};

use crate::policy::{PolicyDecision, PolicyEngine};

/// An approval request shown to the human before an unsafe action proceeds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub id: ApprovalId,
    pub kind: ApprovalKind,
    pub risk: RiskLevel,
    pub title: String,
    pub description: String,
    pub proposed_input: Option<String>,
    pub command: Option<String>,
    #[serde(default)]
    pub worktree_id: Option<WorktreeId>,
    pub context_refs: Vec<ContextItemId>,
    pub status: ApprovalStatus,
}

impl ApprovalRequest {
    pub fn command(
        kind: ApprovalKind,
        command: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        let command = command.into();
        let reason = reason.into();
        Self {
            id: ApprovalId::new(),
            kind,
            risk: RiskLevel::High,
            title: format!("Approval required: {command}"),
            description: reason,
            proposed_input: None,
            command: Some(command),
            worktree_id: None,
            context_refs: Vec::new(),
            status: ApprovalStatus::Pending,
        }
    }

    pub fn worktree_adopt(worktree_id: WorktreeId) -> Self {
        Self {
            id: ApprovalId::new(),
            kind: ApprovalKind::GitCommit,
            risk: RiskLevel::High,
            title: format!("Approval required: adopt worktree {worktree_id}"),
            description: "Merge the selected arena worktree into the integration branch."
                .to_string(),
            proposed_input: None,
            command: Some("git merge --no-commit --no-ff".to_string()),
            worktree_id: Some(worktree_id),
            context_refs: Vec::new(),
            status: ApprovalStatus::Pending,
        }
    }
}

/// Events emitted by queue operations for later JSONL event-log persistence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApprovalEvent {
    ApprovalCreated {
        approval_id: ApprovalId,
        kind: ApprovalKind,
        risk: RiskLevel,
        title: String,
    },
    ApprovalDecided {
        approval_id: ApprovalId,
        status: ApprovalStatus,
    },
    PolicyDenied {
        kind: ApprovalKind,
        risk: RiskLevel,
        title: String,
        description: String,
        command: Option<String>,
    },
}

/// Gate result for a proposed automated action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalGate {
    Allowed,
    Queued(ApprovalRequest, ApprovalEvent),
    Denied(ApprovalEvent),
}

/// Queue error for manual approval commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalQueueError {
    UnknownApproval(ApprovalId),
    AlreadyDecided {
        id: ApprovalId,
        status: ApprovalStatus,
    },
}

#[derive(Debug, Default)]
pub struct ApprovalQueue {
    requests: BTreeMap<ApprovalId, ApprovalRequest>,
}

impl ApprovalQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn pending(&self) -> Vec<&ApprovalRequest> {
        self.requests
            .values()
            .filter(|request| request.status == ApprovalStatus::Pending)
            .collect()
    }

    pub fn get(&self, id: &ApprovalId) -> Option<&ApprovalRequest> {
        self.requests.get(id)
    }

    pub fn gate_command(&mut self, engine: &PolicyEngine, command: &str) -> ApprovalGate {
        let classification = engine.classify_command(command);
        let kind = approval_kind_for_command(command);
        let request = ApprovalRequest {
            id: ApprovalId::new(),
            kind,
            risk: classification.risk,
            title: format!("Approval required: {command}"),
            description: classification.reason,
            proposed_input: None,
            command: Some(command.to_string()),
            worktree_id: None,
            context_refs: Vec::new(),
            status: ApprovalStatus::Pending,
        };

        self.submit(engine.evaluate_command(command), request)
    }

    pub fn submit(&mut self, decision: PolicyDecision, request: ApprovalRequest) -> ApprovalGate {
        match decision {
            PolicyDecision::Allow => ApprovalGate::Allowed,
            PolicyDecision::AllowIfMatchesRules | PolicyDecision::Ask => self.enqueue(request),
            PolicyDecision::Deny => ApprovalGate::Denied(policy_denied_event(request)),
        }
    }

    pub fn approve(&mut self, id: &ApprovalId) -> Result<ApprovalEvent, ApprovalQueueError> {
        self.decide(id, ApprovalStatus::Approved)
    }

    pub fn reject(&mut self, id: &ApprovalId) -> Result<ApprovalEvent, ApprovalQueueError> {
        self.decide(id, ApprovalStatus::Rejected)
    }

    fn enqueue(&mut self, request: ApprovalRequest) -> ApprovalGate {
        let event = ApprovalEvent::ApprovalCreated {
            approval_id: request.id.clone(),
            kind: request.kind.clone(),
            risk: request.risk.clone(),
            title: request.title.clone(),
        };
        self.requests.insert(request.id.clone(), request.clone());
        ApprovalGate::Queued(request, event)
    }

    fn decide(
        &mut self,
        id: &ApprovalId,
        status: ApprovalStatus,
    ) -> Result<ApprovalEvent, ApprovalQueueError> {
        let request = self
            .requests
            .get_mut(id)
            .ok_or_else(|| ApprovalQueueError::UnknownApproval(id.clone()))?;

        if request.status != ApprovalStatus::Pending {
            return Err(ApprovalQueueError::AlreadyDecided {
                id: id.clone(),
                status: request.status.clone(),
            });
        }

        request.status = status.clone();
        Ok(ApprovalEvent::ApprovalDecided {
            approval_id: id.clone(),
            status,
        })
    }
}

fn policy_denied_event(request: ApprovalRequest) -> ApprovalEvent {
    ApprovalEvent::PolicyDenied {
        kind: request.kind,
        risk: request.risk,
        title: request.title,
        description: request.description,
        command: request.command,
    }
}

fn approval_kind_for_command(command: &str) -> ApprovalKind {
    let lower = command.to_ascii_lowercase();
    // Detect git push/commit with the same `contains` logic the policy engine
    // uses (`evaluate_dangerous_command`), so chained or prefixed forms
    // (`cd repo && git push`) are labeled correctly in the audit log / approval
    // UI instead of collapsing to a generic `ShellCommand`. Push/commit are
    // checked before curl/secret to keep ordering consistent with the engine.
    if lower.contains("git push") {
        ApprovalKind::GitPush
    } else if lower.contains("git commit") {
        ApprovalKind::GitCommit
    } else if lower.contains("curl ") || lower.contains("wget ") {
        ApprovalKind::NetworkAccess
    } else if contains_secret_indicator(&lower) {
        ApprovalKind::SecretAccess
    } else {
        ApprovalKind::ShellCommand
    }
}

fn contains_secret_indicator(command: &str) -> bool {
    [
        ".env",
        "id_rsa",
        "id_ed25519",
        "private_key",
        "credentials",
        "secrets",
        "begin private key",
    ]
    .iter()
    .any(|needle| command.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentmux_core::AutomationLevel;

    #[test]
    fn dangerous_ask_command_is_queued_not_allowed() {
        let engine = PolicyEngine::new(AutomationLevel::AutoPrompt);
        let mut queue = ApprovalQueue::new();

        let gate = queue.gate_command(&engine, "rm -rf target");

        let ApprovalGate::Queued(request, event) = gate else {
            panic!("expected dangerous command to be queued");
        };
        assert_eq!(request.kind, ApprovalKind::ShellCommand);
        assert_eq!(request.status, ApprovalStatus::Pending);
        assert_eq!(request.command.as_deref(), Some("rm -rf target"));
        assert_eq!(queue.pending().len(), 1);
        assert_eq!(
            event,
            ApprovalEvent::ApprovalCreated {
                approval_id: request.id.clone(),
                kind: ApprovalKind::ShellCommand,
                risk: RiskLevel::High,
                title: "Approval required: rm -rf target".to_string()
            }
        );
    }

    #[test]
    fn denied_policy_emits_policy_denied_without_queueing() {
        let engine = PolicyEngine::new(AutomationLevel::AutoPrompt);
        let mut queue = ApprovalQueue::new();

        let gate = queue.gate_command(&engine, "git push origin main");

        let ApprovalGate::Denied(event) = gate else {
            panic!("expected git push to be denied by default policy");
        };
        assert_eq!(queue.pending().len(), 0);
        assert_eq!(
            event,
            ApprovalEvent::PolicyDenied {
                kind: ApprovalKind::GitPush,
                risk: RiskLevel::High,
                title: "Approval required: git push origin main".to_string(),
                description: "remote repository modification".to_string(),
                command: Some("git push origin main".to_string())
            }
        );
    }

    #[test]
    fn allowed_policy_does_not_queue() {
        let policy = crate::ApprovalPolicy {
            allow_read_only_commands: PolicyDecision::Allow,
            ..crate::ApprovalPolicy::default()
        };
        let engine = PolicyEngine::with_policy(AutomationLevel::AutoPromptAndApproveSafe, policy);
        let mut queue = ApprovalQueue::new();

        assert_eq!(
            queue.gate_command(&engine, "git status --short"),
            ApprovalGate::Allowed
        );
        assert!(queue.pending().is_empty());
    }

    #[test]
    fn approval_kind_for_chained_git_push_commit_is_correct() {
        // Chained / env-prefixed forms must be labeled GitPush / GitCommit (not a
        // generic ShellCommand) so the audit log and approval UI match the policy
        // engine's classification.
        assert_eq!(
            approval_kind_for_command("cd repo && git push"),
            ApprovalKind::GitPush
        );
        assert_eq!(
            approval_kind_for_command("GIT_SSH=x git push origin main"),
            ApprovalKind::GitPush
        );
        assert_eq!(
            approval_kind_for_command("make build && git commit -m x"),
            ApprovalKind::GitCommit
        );
        // Plain forms still resolve correctly.
        assert_eq!(
            approval_kind_for_command("git push origin main"),
            ApprovalKind::GitPush
        );
    }

    #[test]
    fn chained_git_push_is_denied_and_labeled_git_push() {
        let engine = PolicyEngine::new(AutomationLevel::AutoPrompt);
        let mut queue = ApprovalQueue::new();

        let gate = queue.gate_command(&engine, "cd repo && git push");

        let ApprovalGate::Denied(ApprovalEvent::PolicyDenied { kind, .. }) = gate else {
            panic!("expected chained git push to be denied as GitPush");
        };
        assert_eq!(kind, ApprovalKind::GitPush);
        assert_eq!(queue.pending().len(), 0);
    }

    #[test]
    fn manual_approve_and_reject_transition_only_pending_requests() {
        let engine = PolicyEngine::new(AutomationLevel::AutoPrompt);
        let mut queue = ApprovalQueue::new();

        let ApprovalGate::Queued(request, _) = queue.gate_command(&engine, "cargo test") else {
            panic!("expected command to require approval");
        };
        let event = queue.approve(&request.id).expect("approval should succeed");
        assert_eq!(
            event,
            ApprovalEvent::ApprovalDecided {
                approval_id: request.id.clone(),
                status: ApprovalStatus::Approved
            }
        );
        assert_eq!(
            queue.get(&request.id).map(|request| &request.status),
            Some(&ApprovalStatus::Approved)
        );
        assert_eq!(
            queue.reject(&request.id),
            Err(ApprovalQueueError::AlreadyDecided {
                id: request.id.clone(),
                status: ApprovalStatus::Approved
            })
        );
    }
}
