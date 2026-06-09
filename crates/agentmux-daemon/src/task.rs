use crate::*;

impl DaemonRuntime {
    pub async fn run_task_with_shell_stubs(
        &self,
        body: String,
        team_name: String,
        project_path: PathBuf,
    ) -> Result<serde_json::Value> {
        if body.trim().is_empty() {
            return Err(AgentmuxError::UserError(
                "task.run requires non-empty body".to_string(),
            ));
        }

        let task_id = TaskId::new();
        let team = default_claude_codex_team();
        let plan = plan_task_run(task_id.clone(), &body, team.clone())?;
        self.register_task_team_message_agents(&task_id, &team)
            .await;
        let bootstrap_message = self.persist_orchestrator_message(&plan.bootstrap).await?;
        let context = WorkflowHandoffContext {
            task_title: body.trim().to_string(),
            worktree_path: project_path.display().to_string(),
            test_command: "cargo test --workspace".to_string(),
            diff_path: Some(
                project_path
                    .join(".agentmux/artifacts/diff.patch")
                    .display()
                    .to_string(),
            ),
            test_log_path: Some(
                project_path
                    .join(".agentmux/artifacts/test.log")
                    .display()
                    .to_string(),
            ),
            task_brief_path: Some(
                project_path
                    .join(".agentmux/inbox/planner/task-brief.md")
                    .display()
                    .to_string(),
            ),
            candidate_worktrees: vec![project_path.display().to_string()],
        };
        let mut state = StandardWorkflowState::new(task_id.clone());
        let mut handoffs = vec![message_payload(&bootstrap_message)];
        let mut shell_processes = Vec::new();

        for (name, role) in [
            ("planner", AgentRole::Planner),
            ("impl-codex", AgentRole::Implementer),
            ("tester", AgentRole::Tester),
            ("reviewer", AgentRole::Reviewer),
        ] {
            let output = run_shell_stub_agent(name)?;
            let result = parse_shell_stub_result(name, &output.stdout)?;
            shell_processes.push(json!({
                "agent": name,
                "exit_code": output.exit_code,
                "stdout": output.stdout,
            }));
            let advanced = advance_standard_workflow(
                state,
                &AgentRouteIdentity {
                    name: name.to_string(),
                    role,
                },
                &team,
                result,
                &context,
            )?;
            state = advanced.state;
            for outgoing in &advanced.outgoing {
                let message = self.persist_orchestrator_message(outgoing).await?;
                handoffs.push(message_payload(&message));
            }
            if let Some(summary) = advanced.final_summary {
                let payload = json!({
                    "task_id": task_id.to_string(),
                    "team": team_name,
                    "runner": "shell-stub",
                    "project_path": project_path.display().to_string(),
                    "status": "completed",
                    "stage": format!("{:?}", state.stage),
                    "handoffs": handoffs,
                    "shell_processes": shell_processes,
                    "final_summary": summary.render_markdown(),
                    "recommended_next_action": summary.recommended_next_action,
                });
                self.append_daemon_lifecycle_event("task.completed", payload.clone())?;
                return Ok(payload);
            }
        }

        Err(AgentmuxError::OrchestratorError(
            "shell stub task workflow ended without final summary".to_string(),
        ))
    }

    pub async fn run_task_with_arena(
        &self,
        body: String,
        providers: Vec<String>,
        project_path: PathBuf,
        base_branch: String,
    ) -> Result<serde_json::Value> {
        if body.trim().is_empty() {
            return Err(AgentmuxError::UserError(
                "task.run requires non-empty body".to_string(),
            ));
        }
        if providers.is_empty() {
            return Err(AgentmuxError::UserError(
                "arena task.run requires at least one provider".to_string(),
            ));
        }
        let mut provider_labels = BTreeSet::new();
        for provider in &providers {
            let label = slug_label(provider);
            if !provider_labels.insert(label.clone()) {
                return Err(AgentmuxError::UserError(format!(
                    "arena provider label '{label}' is duplicated"
                )));
            }
        }

        let task_id = TaskId::new();
        let project_id = {
            let state = self.state.read().await;
            state.default_project_id.clone()
        };
        let manager = WorktreeManager::new(
            project_id,
            project_path.clone(),
            project_path.join(".agentmux/worktrees"),
        )?;
        let task_slug = body.trim().to_string();
        let mut candidates = Vec::new();

        for provider in providers {
            let agent_name = format!("impl-{}", slug_label(&provider));
            let worktree = manager.create_worktree(CreateWorktree {
                task_id: task_id.clone(),
                task_slug: task_slug.clone(),
                agent_name: agent_name.clone(),
                owner_agent_id: None,
                base_branch: base_branch.clone(),
            })?;
            self.register_worktree_with_repo_root(worktree.clone(), project_path.clone())
                .await;

            let mut env: BTreeMap<String, String> = std::env::vars().collect();
            env.insert("TERM".to_string(), "xterm-256color".to_string());
            let spec = PtySpawnSpec {
                command: provider_command(&provider),
                args: default_provider_args(Some(provider.as_str())),
                cwd: worktree.path.clone(),
                env,
                size: Default::default(),
            };
            let agent = self
                .spawn_agent_with_role_and_worktree(
                    agent_name,
                    AgentRole::Implementer,
                    spec,
                    Some(worktree.id.clone()),
                )
                .await?;
            {
                let mut state = self.state.write().await;
                if let Some(stored) = state.worktrees.get_mut(&worktree.id) {
                    stored.owner_agent_id = Some(agent.id.clone());
                }
                state.arena_candidates.insert(
                    worktree.id.clone(),
                    ArenaCandidate {
                        worktree_id: worktree.id.clone(),
                        agent_id: agent.id.clone(),
                        provider: provider.clone(),
                        diff_stat: None,
                        test_status: None,
                    },
                );
            }
            self.publish(DaemonEvent::new(
                IpcEventKind::WorktreeCreated,
                json!({
                    "worktree": worktree_payload(&worktree),
                    "agent_id": agent.id.to_string(),
                    "provider": provider,
                }),
            ));
            candidates.push(json!({
                "worktree": worktree_payload(&worktree),
                "agent_id": agent.id.to_string(),
                "name": agent.name,
            }));
        }

        Ok(json!({
            "task_id": task_id.to_string(),
            "runner": "arena",
            "project_path": project_path.display().to_string(),
            "base_branch": base_branch,
            "candidates": candidates,
        }))
    }

    pub(crate) async fn register_task_team_message_agents(
        &self,
        task_id: &TaskId,
        team: &agentmux_agent::TeamTemplate,
    ) {
        let mut state = self.state.write().await;
        for agent in &team.agents {
            if agent.name == "impl-claude" {
                continue;
            }
            let agent_id = AgentSessionId::new();
            state.messages.register_agent(
                AgentDescriptor::new(agent_id, agent.role.clone())
                    .with_name(agent.name.clone())
                    .with_task_id(task_id.clone())
                    .with_team(team.name.clone()),
            );
        }
    }

    pub(crate) async fn persist_orchestrator_message(
        &self,
        message: &agentmux_agent::OrchestratorMessage,
    ) -> Result<AgentMessage> {
        self.create_message(NewAgentMessage {
            task_id: message.task_id.clone(),
            thread_id: None,
            from: message.from.clone(),
            to: message.to.clone(),
            kind: message.kind.clone(),
            priority: message.priority.clone(),
            body: message.body.clone(),
            context_refs: message.context_refs.clone(),
            artifact_refs: message.artifact_refs.clone(),
            delivery_mode: message.delivery_mode.clone(),
            requires_response: message.requires_response,
        })
        .await
    }

    /// Like `persist_orchestrator_message` but stores the message even when no
    /// agents are currently registered for the target (used from
    /// `persist_agent_result_messages` so that messages produced by a
    /// just-completed turn are kept even when the target has not yet spawned).
    pub(crate) async fn persist_orchestrator_message_allow_no_recipients(
        &self,
        message: &agentmux_agent::OrchestratorMessage,
    ) -> Result<AgentMessage> {
        self.create_message_allow_no_recipients(NewAgentMessage {
            task_id: message.task_id.clone(),
            thread_id: None,
            from: message.from.clone(),
            to: message.to.clone(),
            kind: message.kind.clone(),
            priority: message.priority.clone(),
            body: message.body.clone(),
            context_refs: message.context_refs.clone(),
            artifact_refs: message.artifact_refs.clone(),
            delivery_mode: message.delivery_mode.clone(),
            requires_response: message.requires_response,
        })
        .await
    }

    pub async fn persist_agent_result_messages(
        &self,
        agent: &AgentRouteIdentity,
        task_id: TaskId,
        team: &agentmux_agent::TeamTemplate,
        result: AgentResult,
    ) -> Result<Vec<AgentMessage>> {
        let routed = route_agent_result(agent, task_id, team, result)?;
        let mut messages = Vec::with_capacity(routed.outgoing.len());
        for outgoing in &routed.outgoing {
            messages.push(
                self.persist_orchestrator_message_allow_no_recipients(outgoing)
                    .await?,
            );
        }
        Ok(messages)
    }

    pub(crate) async fn persist_live_agent_result(
        &self,
        agent_id: Option<&AgentSessionId>,
        agent_name: &str,
        output_tail: &str,
        seen_hashes: &mut SeenResultHashes,
    ) -> Result<LiveResultOutcome> {
        let result = match parse_agent_result_marker(output_tail) {
            AgentResultParse::Found(parsed) => parsed.result,
            AgentResultParse::NotFound => return Ok(LiveResultOutcome::NotFound),
            AgentResultParse::NeedsStatusProbe(probe) => {
                return Ok(LiveResultOutcome::NeedsProbe {
                    reason: probe.reason,
                });
            }
        };

        // Content-hash dedup: drip/repaint re-emits the same AGENTMUX_RESULT
        // block many times; persist each distinct result exactly once while
        // still allowing genuinely new results (multi-turn exchanges) through.
        let hash = result_content_hash(agent_name, &result);
        if seen_hashes.contains(hash) {
            return Ok(LiveResultOutcome::Duplicate);
        }
        seen_hashes.record(hash);

        let completed = result.status == AgentResultStatus::Completed;
        let task_id = TaskId::new();
        let team = default_claude_codex_team();
        // Resolve the emitting session's role from live state rather than
        // guessing it from the name. Sessions start at the default role and
        // are reassigned at runtime via `agent.set_role`; the routing identity
        // must reflect that current role, not a name heuristic.
        let role = match agent_id {
            Some(agent_id) => self
                .state
                .read()
                .await
                .agents
                .get(agent_id)
                .map(|session| session.metadata.role.clone())
                .unwrap_or_else(default_agent_role),
            None => default_agent_role(),
        };
        let agent = AgentRouteIdentity {
            name: agent_name.to_string(),
            role,
        };
        let messages = self
            .persist_agent_result_messages(&agent, task_id, &team, result)
            .await?;
        self.trigger_idle_delivery_for_messages(&messages).await;
        if completed
            && let Some(worktree_id) = self.resolve_agent_worktree(agent_id, agent_name).await
            && self.is_arena_candidate(&worktree_id).await
        {
            self.capture_and_test_arena_candidate(worktree_id);
        }
        Ok(LiveResultOutcome::Persisted)
    }
}

#[derive(Debug)]
pub(crate) struct ShellStubOutput {
    stdout: String,
    exit_code: Option<i32>,
}

pub(crate) fn run_shell_stub_agent(agent_name: &str) -> Result<ShellStubOutput> {
    let result = match agent_name {
        "planner" => json!({
            "status": "completed",
            "summary": "Assign the deterministic shell-stub implementation.",
            "changed_files": [],
            "messages": [],
            "context_updates": [],
            "needs": [],
            "next": "impl-codex",
            "recommendation": "continue",
            "risk": "low",
        }),
        "impl-codex" => json!({
            "status": "completed",
            "summary": "Shell stub implemented the requested change.",
            "changed_files": ["src/lib.rs"],
            "messages": [],
            "context_updates": [],
            "needs": [],
            "next": "tester",
            "recommendation": "continue",
            "risk": "low",
        }),
        "tester" => json!({
            "status": "completed",
            "summary": "cargo test --workspace passed in shell stub.",
            "changed_files": [],
            "messages": [],
            "context_updates": [],
            "needs": [],
            "next": "reviewer",
            "recommendation": "continue",
            "risk": "low",
        }),
        "reviewer" => json!({
            "status": "completed",
            "summary": "Shell stub reviewer approved the candidate.",
            "changed_files": [],
            "messages": [],
            "context_updates": [],
            "needs": [],
            "next": "none",
            "recommendation": "approve",
            "risk": "low",
        }),
        other => {
            return Err(AgentmuxError::OrchestratorError(format!(
                "unknown shell stub agent '{other}'"
            )));
        }
    };
    let marker = format!("AGENTMUX_RESULT: {result}");
    let script = format!("cat <<'AGENTMUX_EOF'\n{marker}\nAGENTMUX_EOF\n");
    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .output()
        .map_err(|error| {
            AgentmuxError::OrchestratorError(format!(
                "failed to run shell stub agent '{agent_name}': {error}"
            ))
        })?;
    if !output.status.success() {
        return Err(AgentmuxError::OrchestratorError(format!(
            "shell stub agent '{agent_name}' exited with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(ShellStubOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        exit_code: output.status.code(),
    })
}

pub(crate) fn parse_shell_stub_result(agent_name: &str, stdout: &str) -> Result<AgentResult> {
    match parse_agent_result_marker(stdout) {
        AgentResultParse::Found(parsed) => Ok(parsed.result),
        AgentResultParse::NotFound => Err(AgentmuxError::OrchestratorError(format!(
            "shell stub agent '{agent_name}' did not emit AGENTMUX_RESULT"
        ))),
        AgentResultParse::NeedsStatusProbe(probe) => {
            Err(AgentmuxError::OrchestratorError(format!(
                "shell stub agent '{agent_name}' emitted invalid AGENTMUX_RESULT: {}",
                probe.reason
            )))
        }
    }
}

pub(crate) fn slug_label(value: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !slug.is_empty() {
            slug.push('-');
            last_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "agent".to_string()
    } else {
        slug
    }
}
