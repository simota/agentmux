use crate::*;

impl DaemonRuntime {
    // NOTE: worktree-adoption approvals always enqueue (PolicyDecision::Ask) —
    // promotion is a human-gated merge, never auto-approved. Command gating for
    // WorktreeTest goes through `command_is_denied` (PolicyEngine) in dispatch;
    // file-write gating is staged in `file_write_is_denied` (see #2 there).
    pub async fn submit_approval_request(&self, request: ApprovalRequest) -> ApprovalRequest {
        let mut state = self.state.write().await;
        let gate = state
            .approvals
            .submit(agentmux_policy::PolicyDecision::Ask, request);
        let agentmux_policy::ApprovalGate::Queued(request, event) = gate else {
            unreachable!("PolicyDecision::Ask always queues an approval request");
        };
        drop(state);

        let _ = self.append_approval_event(&event);
        self.publish(approval_daemon_event(&event));
        request
    }

    pub(crate) async fn request_worktree_adoption(
        &self,
        worktree_id: WorktreeId,
    ) -> Result<ApprovalRequest> {
        let _ = self.worktree_by_id(&worktree_id).await?;
        self.ensure_arena_candidate_ready(&worktree_id).await?;
        {
            let state = self.state.read().await;
            if let Some(existing) = state
                .approvals
                .pending()
                .into_iter()
                .find(|request| request.worktree_id.is_some())
            {
                return Err(AgentmuxError::UserError(format!(
                    "worktree adoption approval '{}' is already pending",
                    existing.id
                )));
            }
        }
        let request = self
            .submit_approval_request(ApprovalRequest::worktree_adopt(worktree_id.clone()))
            .await;
        self.publish(DaemonEvent::new(
            IpcEventKind::WorktreeAdoptRequested,
            json!({
                "worktree_id": worktree_id.to_string(),
                "approval_id": request.id.to_string(),
            }),
        ));
        Ok(request)
    }

    /// Evaluate a shell command against the configured policy engine. Returns
    /// `true` when the decision is `Deny` (the only outcome that aborts before
    /// the command can run); `Allow`/`Ask`/`AllowIfMatchesRules` all return
    /// `false` so the existing run/approval flow is preserved.
    pub(crate) async fn command_is_denied(&self, command: &str) -> bool {
        let state = self.state.read().await;
        state.policy.evaluate_command(command) == PolicyDecision::Deny
    }

    /// Evaluate a proposed workspace file write against the configured policy
    /// engine. Returns `true` when the decision is `Deny` (protected path or a
    /// `Deny` workspace-write policy).
    ///
    /// NOTE: there is no caller yet — the daemon has no FileWrite dispatch path
    /// (no IPC command or approval flow writes workspace files through the
    /// daemon today; agents write directly inside their own PTYs/worktrees).
    /// This connects `PolicyEngine::evaluate_file_write` so #2 is wired the
    /// moment such a path is added; until then it intentionally has no call
    /// site. The `#[allow(dead_code)]` keeps the build clean without a TODO.
    #[allow(dead_code)]
    pub(crate) async fn file_write_is_denied(&self, path: &str) -> bool {
        let state = self.state.read().await;
        state.policy.evaluate_file_write(path) == PolicyDecision::Deny
    }

    pub async fn list_approvals(&self) -> Vec<ApprovalRequest> {
        let state = self.state.read().await;
        state.approvals.pending().into_iter().cloned().collect()
    }

    pub async fn approve_approval(
        &self,
        approval_id: &ApprovalId,
    ) -> std::result::Result<ApprovalRequest, ApprovalQueueError> {
        self.decide_approval(approval_id, true).await
    }

    pub async fn reject_approval(
        &self,
        approval_id: &ApprovalId,
    ) -> std::result::Result<ApprovalRequest, ApprovalQueueError> {
        self.decide_approval(approval_id, false).await
    }

    pub(crate) async fn decide_approval(
        &self,
        approval_id: &ApprovalId,
        approved: bool,
    ) -> std::result::Result<ApprovalRequest, ApprovalQueueError> {
        let mut state = self.state.write().await;
        let event = if approved {
            state.approvals.approve(approval_id)?
        } else {
            state.approvals.reject(approval_id)?
        };
        let request = state
            .approvals
            .get(approval_id)
            .expect("decided approval remains in queue")
            .clone();
        drop(state);

        let _ = self.append_approval_event(&event);
        self.publish(approval_daemon_event(&event));
        if let Some(worktree_id) = request.worktree_id.clone() {
            let runtime = self.clone();
            tokio::spawn(async move {
                if approved {
                    if let Err(error) = runtime.promote_worktree(&worktree_id).await {
                        runtime.publish(DaemonEvent::new(
                            IpcEventKind::Error,
                            json!({
                                "worktree_id": worktree_id.to_string(),
                                "signal": "worktree_promote_failed",
                                "error": error.to_string(),
                            }),
                        ));
                    }
                } else if let Err(error) = runtime.archive_worktree(&worktree_id).await {
                    runtime.publish(DaemonEvent::new(
                        IpcEventKind::Error,
                        json!({
                            "worktree_id": worktree_id.to_string(),
                            "signal": "worktree_archive_failed",
                            "error": error.to_string(),
                        }),
                    ));
                }
            });
        }
        Ok(request)
    }
}

pub(crate) fn approval_payload(approval: &ApprovalRequest) -> serde_json::Value {
    json!({
        "approval_id": approval.id.to_string(),
        "kind": approval.kind,
        "risk": approval.risk,
        "title": approval.title,
        "description": approval.description,
        "proposed_input": approval.proposed_input,
        "command": approval.command,
        "worktree_id": approval.worktree_id.as_ref().map(ToString::to_string),
        "context_refs": approval.context_refs.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "status": approval.status,
    })
}

pub(crate) fn approval_queue_error(error: ApprovalQueueError) -> String {
    match error {
        ApprovalQueueError::UnknownApproval(id) => format!("unknown approval '{id}'"),
        ApprovalQueueError::AlreadyDecided { id, status } => {
            format!("approval '{id}' is already {status:?}")
        }
    }
}
