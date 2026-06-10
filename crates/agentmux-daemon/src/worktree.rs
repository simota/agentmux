use crate::*;

impl DaemonRuntime {
    pub async fn register_worktree(&self, worktree: Worktree) {
        let mut state = self.state.write().await;
        state.worktrees.insert(worktree.id.clone(), worktree);
    }

    pub(crate) async fn register_worktree_with_repo_root(
        &self,
        worktree: Worktree,
        repo_root: PathBuf,
    ) {
        let mut state = self.state.write().await;
        state
            .worktree_repo_roots
            .insert(worktree.id.clone(), repo_root);
        state.worktrees.insert(worktree.id.clone(), worktree);
    }

    pub async fn list_worktrees(&self) -> Vec<Worktree> {
        let state = self.state.read().await;
        state.worktrees.values().cloned().collect()
    }

    pub async fn capture_worktree_diff(
        &self,
        worktree_id: &WorktreeId,
    ) -> Result<serde_json::Value> {
        let worktree = self.worktree_by_id(worktree_id).await?;
        let captured = {
            let worktree = worktree.clone();
            run_blocking("worktree.diff", move || {
                agentmux_worktree::WorktreeManager::new(
                    worktree.project_id.clone(),
                    worktree.path.clone(),
                    worktree.path.join(".agentmux/worktrees"),
                )?
                .capture_diff_artifact(
                    CaptureDiff {
                        task_id: worktree.task_id.clone(),
                        agent_name: worktree.branch_name.clone(),
                        worktree_path: worktree.path.clone(),
                        base_branch: worktree.base_branch.clone(),
                    },
                    worktree.path.join(".agentmux/artifacts"),
                )
            })
            .await?
        };

        self.publish(DaemonEvent::new(
            IpcEventKind::WorktreeDiffCaptured,
            json!({
                "worktree_id": worktree.id.to_string(),
                "artifact_id": captured.patch.id.to_string(),
                "stat": captured.stat,
            }),
        ));
        self.record_arena_diff(&worktree.id, captured.stat.clone())
            .await;

        Ok(json!({
            "worktree": worktree_payload(&worktree),
            "artifact": artifact_payload(
                captured.patch.id.to_string(),
                captured.patch.path.display().to_string(),
                captured.patch.title,
            ),
            "stat": captured.stat,
        }))
    }

    pub async fn run_worktree_test(
        &self,
        worktree_id: &WorktreeId,
        command: TestCommand,
    ) -> Result<serde_json::Value> {
        let mut worktree = self.worktree_by_id(worktree_id).await?;
        self.set_worktree_status(worktree_id, WorktreeStatus::Testing)
            .await?;
        // Test commands run for minutes and are synchronous `std::process`
        // calls: run them on the blocking pool so an async worker is never
        // pinned (agentmux-worktree enforces the kill-on-timeout bound).
        let result = {
            let worktree = worktree.clone();
            run_blocking("worktree.test", move || {
                agentmux_worktree::WorktreeManager::new(
                    worktree.project_id.clone(),
                    worktree.path.clone(),
                    worktree.path.join(".agentmux/worktrees"),
                )?
                .run_test_command_artifact(
                    worktree.task_id.clone(),
                    &worktree.branch_name,
                    &worktree.path,
                    command,
                    worktree.path.join(".agentmux/artifacts"),
                )
            })
            .await
        };

        match result {
            Ok(test_run) => {
                worktree.status = if test_run.status == TestRunStatus::Passed {
                    WorktreeStatus::ReviewReady
                } else {
                    WorktreeStatus::Failed
                };
                self.set_worktree_status(worktree_id, worktree.status.clone())
                    .await?;
                self.publish(DaemonEvent::new(
                    IpcEventKind::ArtifactCreated,
                    json!({
                        "worktree_id": worktree.id.to_string(),
                        "artifact_id": test_run.artifact.id.to_string(),
                    }),
                ));
                self.record_arena_test(&worktree.id, test_run.status).await;
                self.publish(DaemonEvent::new(
                    IpcEventKind::WorktreeTestCompleted,
                    json!({
                        "worktree_id": worktree.id.to_string(),
                        "status": test_run.status,
                        "command": test_run.command,
                        "exit_code": test_run.exit_code,
                    }),
                ));
                Ok(json!({
                    "worktree": worktree_payload(&worktree),
                    "test": {
                        "status": test_run.status,
                        "command": test_run.command,
                        "exit_code": test_run.exit_code,
                        "artifact": artifact_payload(
                            test_run.artifact.id.to_string(),
                            test_run.artifact.path.display().to_string(),
                            test_run.artifact.title,
                        ),
                    },
                }))
            }
            Err(error) => {
                let _ = self
                    .set_worktree_status(worktree_id, WorktreeStatus::Failed)
                    .await;
                Err(error)
            }
        }
    }

    pub(crate) async fn promote_worktree(&self, worktree_id: &WorktreeId) -> Result<Worktree> {
        self.ensure_arena_candidate_ready(worktree_id).await?;
        let worktree = self.worktree_by_id(worktree_id).await?;
        let repo_root = self.repo_root_for_worktree(worktree_id, &worktree).await;
        let manager = WorktreeManager::new(
            worktree.project_id.clone(),
            repo_root.clone(),
            repo_root.join(".agentmux/worktrees"),
        )?;
        let merge_outcome = {
            let manager = manager.clone();
            let worktree = worktree.clone();
            run_blocking("worktree.promote", move || {
                manager.merge_to_integration_branch(&worktree, "agentmux/integration")
            })
            .await?
        };
        match merge_outcome {
            MergeOutcome::Conflict => {
                self.set_worktree_status(worktree_id, WorktreeStatus::Conflicted)
                    .await?;
                Err(AgentmuxError::UserError(format!(
                    "worktree '{worktree_id}' merge conflicted and was aborted"
                )))
            }
            MergeOutcome::Clean | MergeOutcome::Dirty => {
                self.set_worktree_status(worktree_id, WorktreeStatus::Promoted)
                    .await
            }
        }
    }

    pub async fn archive_worktree(&self, worktree_id: &WorktreeId) -> Result<Worktree> {
        self.set_worktree_status(worktree_id, WorktreeStatus::Archived)
            .await
    }

    pub(crate) async fn ensure_arena_candidate_ready(
        &self,
        worktree_id: &WorktreeId,
    ) -> Result<()> {
        let state = self.state.read().await;
        let Some(candidate) = state.arena_candidates.get(worktree_id) else {
            return Err(AgentmuxError::UserError(format!(
                "worktree '{worktree_id}' is not a registered arena candidate"
            )));
        };
        if candidate.diff_stat.is_none() {
            return Err(AgentmuxError::UserError(format!(
                "worktree '{worktree_id}' adoption requires captured diff"
            )));
        }
        if candidate.test_status != Some(TestRunStatus::Passed) {
            return Err(AgentmuxError::UserError(format!(
                "worktree '{worktree_id}' adoption requires passed tests"
            )));
        }
        Ok(())
    }

    pub(crate) async fn worktree_by_id(&self, worktree_id: &WorktreeId) -> Result<Worktree> {
        let state = self.state.read().await;
        state
            .worktrees
            .get(worktree_id)
            .cloned()
            .ok_or_else(|| AgentmuxError::UserError(format!("unknown worktree '{worktree_id}'")))
    }

    pub(crate) async fn repo_root_for_worktree(
        &self,
        worktree_id: &WorktreeId,
        worktree: &Worktree,
    ) -> PathBuf {
        let state = self.state.read().await;
        state
            .worktree_repo_roots
            .get(worktree_id)
            .cloned()
            .unwrap_or_else(|| worktree.path.clone())
    }

    pub(crate) async fn record_arena_diff(&self, worktree_id: &WorktreeId, stat: String) {
        let mut state = self.state.write().await;
        if let Some(candidate) = state.arena_candidates.get_mut(worktree_id) {
            candidate.diff_stat = Some(stat);
        }
    }

    pub(crate) async fn record_arena_test(&self, worktree_id: &WorktreeId, status: TestRunStatus) {
        let mut state = self.state.write().await;
        if let Some(candidate) = state.arena_candidates.get_mut(worktree_id) {
            candidate.test_status = Some(status);
        }
    }

    pub(crate) async fn set_worktree_status(
        &self,
        worktree_id: &WorktreeId,
        status: WorktreeStatus,
    ) -> Result<Worktree> {
        let mut state = self.state.write().await;
        let Some(worktree) = state.worktrees.get_mut(worktree_id) else {
            return Err(AgentmuxError::UserError(format!(
                "unknown worktree '{worktree_id}'"
            )));
        };
        worktree.status = status;
        Ok(worktree.clone())
    }

    pub(crate) async fn resolve_agent_worktree(
        &self,
        agent_id: Option<&AgentSessionId>,
        agent_name: &str,
    ) -> Option<WorktreeId> {
        let state = self.state.read().await;
        if let Some(agent_id) = agent_id
            && let Some(agent) = state.agents.get(agent_id)
        {
            return agent.worktree_id.clone();
        }
        state
            .agents
            .values()
            .find(|agent| agent.metadata.name == agent_name)
            .and_then(|agent| agent.worktree_id.clone())
    }

    pub(crate) async fn is_arena_candidate(&self, worktree_id: &WorktreeId) -> bool {
        let state = self.state.read().await;
        state.arena_candidates.contains_key(worktree_id)
    }

    pub(crate) fn capture_and_test_arena_candidate(&self, worktree_id: WorktreeId) {
        let runtime = self.clone();
        tokio::spawn(async move {
            if let Err(error) = runtime.capture_worktree_diff(&worktree_id).await {
                runtime.publish(DaemonEvent::new(
                    IpcEventKind::Error,
                    json!({
                        "worktree_id": worktree_id.to_string(),
                        "signal": "worktree_diff_capture_failed",
                        "error": error.to_string(),
                    }),
                ));
                return;
            }
            if let Err(error) = runtime
                .run_worktree_test(
                    &worktree_id,
                    TestCommand {
                        name: "default".to_string(),
                        command: "cargo test".to_string(),
                    },
                )
                .await
            {
                runtime.publish(DaemonEvent::new(
                    IpcEventKind::Error,
                    json!({
                        "worktree_id": worktree_id.to_string(),
                        "signal": "worktree_test_failed",
                        "error": error.to_string(),
                    }),
                ));
            }
        });
    }
}

/// Run a synchronous, process-spawning closure on the blocking pool.
///
/// Worktree git/test operations are blocking `std::process` calls; executing
/// them inline on a Tokio async worker pins that worker for the full command
/// duration (and on a current-thread runtime, the whole daemon). A panicking
/// or cancelled blocking task is mapped to an internal error.
pub(crate) async fn run_blocking<T, F>(label: &'static str, task: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(task)
        .await
        .map_err(|error| AgentmuxError::Internal(format!("{label} task failed: {error}")))?
}

pub(crate) fn worktree_payload(worktree: &Worktree) -> serde_json::Value {
    json!({
        "worktree_id": worktree.id.to_string(),
        "project_id": worktree.project_id.to_string(),
        "task_id": worktree.task_id.to_string(),
        "owner_agent_id": worktree.owner_agent_id.as_ref().map(ToString::to_string),
        "path": worktree.path.display().to_string(),
        "branch_name": worktree.branch_name,
        "base_branch": worktree.base_branch,
        "status": worktree.status,
        "created_at": worktree.created_at.to_string(),
    })
}
