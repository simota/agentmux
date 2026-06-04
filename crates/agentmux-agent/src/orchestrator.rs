//! Pure orchestration decisions for the v0.1 task workflow.
//!
//! The daemon owns sockets, PTYs, persistence, and actual delivery. This module
//! keeps team template resolution, planner bootstrap, result-driven routing,
//! and stalled detection deterministic and unit-testable.

use std::str::FromStr;
use std::time::Duration;

use agentmux_core::{
    AgentRole, AgentStatus, AgentmuxError, ArtifactId, ContextItemId, DateTimeUtc, DeliveryMode,
    Priority, TaskId, error::Result,
};
use agentmux_message::{
    MessageKind, MessageSource, MessageTarget, NewAgentMessage, message::AgentMessage,
};

use crate::result::{
    AgentResult, AgentResultParse, AgentResultStatus, OutgoingMessage, OutgoingMessageKind,
    OutgoingPriority, ParsedAgentResult, ResultRecommendation, ResultRisk,
};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRunPlan {
    pub task_id: TaskId,
    pub team: TeamTemplate,
    pub bootstrap: OrchestratorMessage,
}

pub fn plan_task_run(
    task_id: TaskId,
    task_body: impl AsRef<str>,
    team: TeamTemplate,
) -> Result<TaskRunPlan> {
    let body = task_body.as_ref();
    if body.trim().is_empty() {
        return Err(AgentmuxError::UserError(
            "task body must not be empty".to_string(),
        ));
    }

    let planner = team.planner()?;
    Ok(TaskRunPlan {
        task_id: task_id.clone(),
        team: team.clone(),
        bootstrap: OrchestratorMessage {
            task_id: Some(task_id),
            from: MessageSource::Orchestrator,
            to: MessageTarget::Role(planner.role.clone()),
            kind: MessageKind::TaskAssignment,
            priority: Priority::High,
            body: render_planner_bootstrap(body, &team),
            delivery_mode: DeliveryMode::InjectWhenIdle,
            requires_response: true,
            context_refs: Vec::new(),
            artifact_refs: Vec::new(),
        },
    })
}

fn render_planner_bootstrap(task_body: &str, team: &TeamTemplate) -> String {
    let mut prompt = String::from(
        "[agentmux task]\n\
あなたはplannerです。\n\
以下のタスクを分解し、implementer agentへ送る作業指示を作成してください。\n\n\
Task:\n",
    );
    prompt.push_str(task_body.trim());
    prompt.push_str("\n\n利用可能agent:\n");
    for agent in &team.agents {
        prompt.push_str(&format!(
            "- {}: {} {}\n",
            agent.name,
            provider_label(&agent.provider),
            role_label(&agent.role)
        ));
    }
    prompt.push_str(
        "\n制約:\n\
- 実装agentはそれぞれ専用worktreeで作業します\n\
- public APIの破壊的変更は禁止\n\
- 最小変更を優先してください\n\n\
最後に必ず AGENTMUX_RESULT JSON を出力してください。\n",
    );
    prompt
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRouteIdentity {
    pub name: String,
    pub role: AgentRole,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OrchestratorResult {
    Routed(ResultRouting),
    NeedsStatusProbe(OrchestratorMessage),
    WaitingForResult,
}

pub fn route_agent_result_parse(
    agent: &AgentRouteIdentity,
    task_id: TaskId,
    team: &TeamTemplate,
    parsed: AgentResultParse,
) -> Result<OrchestratorResult> {
    match parsed {
        AgentResultParse::Found(ParsedAgentResult { result, .. }) => {
            route_agent_result(agent, task_id, team, result).map(OrchestratorResult::Routed)
        }
        AgentResultParse::NotFound => Ok(OrchestratorResult::WaitingForResult),
        AgentResultParse::NeedsStatusProbe(probe) => {
            Ok(OrchestratorResult::NeedsStatusProbe(status_probe_message(
                task_id,
                MessageTarget::Role(agent.role.clone()),
                format!(
                    "AGENTMUX_RESULT を正しい JSON で再出力してください。reason: {}",
                    probe.reason
                ),
            )))
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResultRouting {
    pub status: AgentResultStatus,
    pub summary: String,
    pub outgoing: Vec<OrchestratorMessage>,
    pub needs_human: bool,
}

pub fn route_agent_result(
    agent: &AgentRouteIdentity,
    task_id: TaskId,
    team: &TeamTemplate,
    result: AgentResult,
) -> Result<ResultRouting> {
    let needs_human = matches!(
        result.status,
        AgentResultStatus::Blocked | AgentResultStatus::NeedsInput | AgentResultStatus::Failed
    );
    let mut outgoing = Vec::new();

    for message in &result.messages {
        outgoing.push(convert_outgoing_message(
            agent,
            task_id.clone(),
            team,
            message,
        )?);
    }

    if outgoing.is_empty() {
        if let Some(next) = result.next.as_deref() {
            if !next.eq_ignore_ascii_case("none") {
                outgoing.push(summary_handoff(
                    task_id.clone(),
                    MessageSource::TeamAgent(agent.name.clone()),
                    resolve_result_target(team, next)?,
                    &agent.name,
                    &result,
                ));
            }
        }
    }

    Ok(ResultRouting {
        status: result.status,
        summary: result.summary,
        outgoing,
        needs_human,
    })
}

fn convert_outgoing_message(
    agent: &AgentRouteIdentity,
    task_id: TaskId,
    team: &TeamTemplate,
    message: &OutgoingMessage,
) -> Result<OrchestratorMessage> {
    Ok(OrchestratorMessage {
        task_id: Some(task_id),
        from: MessageSource::TeamAgent(agent.name.clone()),
        to: resolve_result_target(team, &message.to)?,
        kind: map_message_kind(message.kind),
        priority: map_priority(message.priority),
        body: message.body.clone(),
        delivery_mode: DeliveryMode::InjectWhenIdle,
        requires_response: true,
        context_refs: parse_context_refs(&message.context_refs)?,
        artifact_refs: parse_artifact_refs(&message.artifact_refs)?,
    })
}

fn summary_handoff(
    task_id: TaskId,
    from: MessageSource,
    to: MessageTarget,
    from_agent_name: &str,
    result: &AgentResult,
) -> OrchestratorMessage {
    OrchestratorMessage {
        task_id: Some(task_id),
        from,
        to,
        kind: MessageKind::Handoff,
        priority: Priority::Normal,
        body: format!(
            "[agentmux handoff]\nfrom: {from_agent_name}\nkind: Handoff\n\n{}\n\nAGENTMUX_RESULT JSON で結果を返してください。\n",
            result.summary
        ),
        delivery_mode: DeliveryMode::InjectWhenIdle,
        requires_response: true,
        context_refs: Vec::new(),
        artifact_refs: Vec::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandardWorkflowStage {
    Planning,
    Implementing,
    Testing,
    Reviewing,
    Completed,
    NeedsHuman,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowHandoffContext {
    pub task_title: String,
    pub worktree_path: String,
    pub test_command: String,
    pub diff_path: Option<String>,
    pub test_log_path: Option<String>,
    pub task_brief_path: Option<String>,
    pub candidate_worktrees: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowTurnRecord {
    pub agent_name: String,
    pub role: AgentRole,
    pub result: AgentResult,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StandardWorkflowState {
    pub task_id: TaskId,
    pub stage: StandardWorkflowStage,
    pub turns: Vec<WorkflowTurnRecord>,
}

impl StandardWorkflowState {
    pub fn new(task_id: TaskId) -> Self {
        Self {
            task_id,
            stage: StandardWorkflowStage::Planning,
            turns: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowAdvance {
    pub state: StandardWorkflowState,
    pub outgoing: Vec<OrchestratorMessage>,
    pub final_summary: Option<FinalSummary>,
    pub needs_human: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalSummary {
    pub task_title: String,
    pub candidate_worktrees: Vec<String>,
    pub changed_files: Vec<String>,
    pub test_results: Vec<String>,
    pub reviewer_recommendation: Option<ResultRecommendation>,
    pub risks: Vec<String>,
    pub open_questions: Vec<String>,
    pub recommended_next_action: String,
    pub promote_command: Option<String>,
}

impl FinalSummary {
    pub fn render_markdown(&self) -> String {
        let mut body = format!("# Final summary: {}\n\n", self.task_title);
        push_list(&mut body, "candidate worktrees", &self.candidate_worktrees);
        push_list(&mut body, "changed files", &self.changed_files);
        push_list(&mut body, "test results", &self.test_results);
        body.push_str(&format!(
            "- reviewer recommendation: {}\n",
            self.reviewer_recommendation
                .map(recommendation_label)
                .unwrap_or("none")
        ));
        push_list(&mut body, "risks", &self.risks);
        push_list(&mut body, "open questions", &self.open_questions);
        body.push_str(&format!(
            "- recommended next action: {}\n",
            self.recommended_next_action
        ));
        if let Some(command) = &self.promote_command {
            body.push_str(&format!("- promote command: `{command}`\n"));
        }
        body
    }
}

pub fn advance_standard_workflow(
    mut state: StandardWorkflowState,
    agent: &AgentRouteIdentity,
    team: &TeamTemplate,
    result: AgentResult,
    context: &WorkflowHandoffContext,
) -> Result<WorkflowAdvance> {
    let expected_stage = expected_stage_for_role(&agent.role);
    if state.stage != expected_stage {
        return Err(AgentmuxError::OrchestratorError(format!(
            "workflow stage {:?} cannot accept result from {:?}",
            state.stage, agent.role
        )));
    }

    let needs_human = !matches!(result.status, AgentResultStatus::Completed);
    state.turns.push(WorkflowTurnRecord {
        agent_name: agent.name.clone(),
        role: agent.role.clone(),
        result,
    });

    if needs_human {
        state.stage = StandardWorkflowStage::NeedsHuman;
        return Ok(WorkflowAdvance {
            state,
            outgoing: Vec::new(),
            final_summary: None,
            needs_human: true,
        });
    }

    let outgoing = match agent.role {
        AgentRole::Planner => {
            state.stage = StandardWorkflowStage::Implementing;
            vec![implementer_handoff(
                state.task_id.clone(),
                team,
                context,
                state
                    .turns
                    .last()
                    .expect("turn was just pushed")
                    .result
                    .next
                    .as_deref(),
                latest_summary(&state),
            )?]
        }
        AgentRole::Implementer => {
            state.stage = StandardWorkflowStage::Testing;
            vec![tester_handoff(
                state.task_id.clone(),
                context,
                latest_summary(&state),
            )]
        }
        AgentRole::Tester => {
            state.stage = StandardWorkflowStage::Reviewing;
            vec![reviewer_handoff(
                state.task_id.clone(),
                context,
                latest_summary(&state),
            )]
        }
        AgentRole::Reviewer => {
            state.stage = StandardWorkflowStage::Completed;
            let summary = build_final_summary(&state, context);
            return Ok(WorkflowAdvance {
                state,
                outgoing: Vec::new(),
                final_summary: Some(summary),
                needs_human: false,
            });
        }
        _ => {
            return Err(AgentmuxError::OrchestratorError(format!(
                "role {:?} is not part of the standard v0.1 workflow",
                agent.role
            )));
        }
    };

    Ok(WorkflowAdvance {
        state,
        outgoing,
        final_summary: None,
        needs_human: false,
    })
}

fn expected_stage_for_role(role: &AgentRole) -> StandardWorkflowStage {
    match role {
        AgentRole::Planner => StandardWorkflowStage::Planning,
        AgentRole::Implementer => StandardWorkflowStage::Implementing,
        AgentRole::Tester => StandardWorkflowStage::Testing,
        AgentRole::Reviewer => StandardWorkflowStage::Reviewing,
        _ => StandardWorkflowStage::NeedsHuman,
    }
}

fn latest_summary(state: &StandardWorkflowState) -> &str {
    state
        .turns
        .last()
        .map(|turn| turn.result.summary.as_str())
        .unwrap_or("")
}

fn implementer_handoff(
    task_id: TaskId,
    team: &TeamTemplate,
    context: &WorkflowHandoffContext,
    requested_next: Option<&str>,
    assignment: &str,
) -> Result<OrchestratorMessage> {
    let to = match requested_next {
        Some(next) if !next.eq_ignore_ascii_case("none") => resolve_result_target(team, next)?,
        _ => MessageTarget::Role(AgentRole::Implementer),
    };
    Ok(OrchestratorMessage {
        task_id: Some(task_id),
        from: MessageSource::Orchestrator,
        to,
        kind: MessageKind::TaskAssignment,
        priority: Priority::High,
        body: format!(
            "[agentmux handoff]\nfrom: planner\nkind: TaskAssignment\n\n\
あなたはimplementerです。\n\
専用worktree内で次の修正案を実装してください。\n\n\
{}\n\n\
対象worktree:\n{}\n\n\
完了時:\n\
- 変更ファイル\n\
- 実装方針\n\
- テスト状況\n\
- reviewer/testerへの次action\n\
を AGENTMUX_RESULT JSON で返してください。\n",
            assignment.trim(),
            context.worktree_path
        ),
        delivery_mode: DeliveryMode::InjectWhenIdle,
        requires_response: true,
        context_refs: Vec::new(),
        artifact_refs: Vec::new(),
    })
}

fn tester_handoff(
    task_id: TaskId,
    context: &WorkflowHandoffContext,
    implementer_summary: &str,
) -> OrchestratorMessage {
    OrchestratorMessage {
        task_id: Some(task_id.clone()),
        from: MessageSource::Orchestrator,
        to: MessageTarget::Role(AgentRole::Tester),
        kind: MessageKind::Handoff,
        priority: Priority::High,
        body: format!(
            "[agentmux handoff]\nfrom: orchestrator\nkind: TestRequest\n\n\
対象worktree:\n{}\n\n\
実行してください:\n{}\n\n\
実装要約:\n{}\n\n\
結果を .agentmux/artifacts/{}/ に保存し、要約を AGENTMUX_RESULT で返してください。\n",
            context.worktree_path,
            context.test_command,
            implementer_summary.trim(),
            task_id
        ),
        delivery_mode: DeliveryMode::InjectWhenIdle,
        requires_response: true,
        context_refs: Vec::new(),
        artifact_refs: Vec::new(),
    }
}

fn reviewer_handoff(
    task_id: TaskId,
    context: &WorkflowHandoffContext,
    test_summary: &str,
) -> OrchestratorMessage {
    OrchestratorMessage {
        task_id: Some(task_id),
        from: MessageSource::Orchestrator,
        to: MessageTarget::Role(AgentRole::Reviewer),
        kind: MessageKind::Handoff,
        priority: Priority::High,
        body: format!(
            "[agentmux handoff]\nfrom: orchestrator\nkind: ReviewRequest\n\n\
以下をレビューしてください。\n\n\
- diff: {}\n\
- test log: {}\n\
- task brief: {}\n\n\
テスト要約:\n{}\n\n\
観点:\n\
- バグが修正されているか\n\
- 変更が最小か\n\
- テストが十分か\n\
- リスクは何か\n\n\
AGENTMUX_RESULT JSONで approve/request_changes/needs_tests を返してください。\n",
            context.diff_path.as_deref().unwrap_or("(not captured)"),
            context.test_log_path.as_deref().unwrap_or("(not captured)"),
            context
                .task_brief_path
                .as_deref()
                .unwrap_or("(not captured)"),
            test_summary.trim()
        ),
        delivery_mode: DeliveryMode::InjectWhenIdle,
        requires_response: true,
        context_refs: Vec::new(),
        artifact_refs: Vec::new(),
    }
}

fn build_final_summary(
    state: &StandardWorkflowState,
    context: &WorkflowHandoffContext,
) -> FinalSummary {
    let changed_files = state
        .turns
        .iter()
        .flat_map(|turn| turn.result.changed_files.iter().cloned())
        .fold(Vec::new(), |mut files, file| {
            if !files.contains(&file) {
                files.push(file);
            }
            files
        });
    let test_results = state
        .turns
        .iter()
        .filter(|turn| turn.role == AgentRole::Tester)
        .map(|turn| turn.result.summary.clone())
        .collect();
    let reviewer = state
        .turns
        .iter()
        .rev()
        .find(|turn| turn.role == AgentRole::Reviewer)
        .map(|turn| &turn.result);
    let reviewer_recommendation = reviewer.and_then(|result| result.recommendation);
    let risks = state
        .turns
        .iter()
        .filter_map(|turn| {
            turn.result
                .risk
                .map(|risk| format!("{}: {}", turn.agent_name, risk_label(risk)))
        })
        .collect();
    let open_questions = state
        .turns
        .iter()
        .flat_map(|turn| turn.result.needs.iter().cloned())
        .collect();
    let promote_command = context
        .candidate_worktrees
        .first()
        .map(|worktree| format!("agentmux worktree promote {worktree}"));
    let recommended_next_action = match reviewer_recommendation {
        Some(ResultRecommendation::Approve) => "promote approved candidate worktree".to_string(),
        Some(ResultRecommendation::RequestChanges) => {
            "send reviewer feedback to implementer".to_string()
        }
        Some(ResultRecommendation::NeedsTests) => "run or attach additional tests".to_string(),
        Some(ResultRecommendation::Continue) => "continue standard workflow".to_string(),
        Some(ResultRecommendation::None) | None => {
            "inspect final summary and decide next action".to_string()
        }
    };

    FinalSummary {
        task_title: context.task_title.clone(),
        candidate_worktrees: context.candidate_worktrees.clone(),
        changed_files,
        test_results,
        reviewer_recommendation,
        risks,
        open_questions,
        recommended_next_action,
        promote_command,
    }
}

fn push_list(body: &mut String, label: &str, items: &[String]) {
    body.push_str(&format!("- {label}:\n"));
    if items.is_empty() {
        body.push_str("  - none\n");
    } else {
        for item in items {
            body.push_str(&format!("  - {item}\n"));
        }
    }
}

fn recommendation_label(recommendation: ResultRecommendation) -> &'static str {
    match recommendation {
        ResultRecommendation::Approve => "approve",
        ResultRecommendation::RequestChanges => "request_changes",
        ResultRecommendation::NeedsTests => "needs_tests",
        ResultRecommendation::Continue => "continue",
        ResultRecommendation::None => "none",
    }
}

fn risk_label(risk: ResultRisk) -> &'static str {
    match risk {
        ResultRisk::Low => "low",
        ResultRisk::Medium => "medium",
        ResultRisk::High => "high",
        ResultRisk::Unknown => "unknown",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StallDecision {
    NoAction,
    SendStatusProbe(OrchestratorMessage),
    NeedsHuman {
        agent: AgentRouteIdentity,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StalledDetector {
    pub quiet_threshold: Duration,
    pub max_status_probe: u8,
}

impl StalledDetector {
    pub fn detect(
        &self,
        agent: AgentRouteIdentity,
        task_id: TaskId,
        status: AgentStatus,
        last_activity_at: DateTimeUtc,
        now: DateTimeUtc,
        status_probe_count: u8,
    ) -> StallDecision {
        if !is_stall_candidate(&status) || !quiet_for(last_activity_at, now, self.quiet_threshold) {
            return StallDecision::NoAction;
        }

        if status_probe_count < self.max_status_probe {
            return StallDecision::SendStatusProbe(status_probe_message(
                task_id,
                MessageTarget::Role(agent.role.clone()),
                "一定時間出力がないため、現在の状態を AGENTMUX_RESULT JSON で報告してください。"
                    .to_string(),
            ));
        }

        StallDecision::NeedsHuman {
            agent,
            reason: "status probe retry limit exceeded".to_string(),
        }
    }
}

fn is_stall_candidate(status: &AgentStatus) -> bool {
    matches!(
        status,
        AgentStatus::RunningTurn
            | AgentStatus::RunningCommand
            | AgentStatus::Stalled
            | AgentStatus::AwaitingInput
    )
}

fn quiet_for(earlier: DateTimeUtc, later: DateTimeUtc, threshold: Duration) -> bool {
    let nanos = later.unix_timestamp_nanos() - earlier.unix_timestamp_nanos();
    if nanos < 0 {
        return false;
    }
    u64::try_from(nanos)
        .map(Duration::from_nanos)
        .is_ok_and(|elapsed| elapsed >= threshold)
}

fn status_probe_message(task_id: TaskId, to: MessageTarget, body: String) -> OrchestratorMessage {
    OrchestratorMessage {
        task_id: Some(task_id),
        from: MessageSource::Orchestrator,
        to,
        kind: MessageKind::StatusProbe,
        priority: Priority::Urgent,
        body,
        delivery_mode: DeliveryMode::InjectWhenIdle,
        requires_response: true,
        context_refs: Vec::new(),
        artifact_refs: Vec::new(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrchestratorMessage {
    pub task_id: Option<TaskId>,
    pub from: MessageSource,
    pub to: MessageTarget,
    pub kind: MessageKind,
    pub priority: Priority,
    pub body: String,
    pub delivery_mode: DeliveryMode,
    pub requires_response: bool,
    pub context_refs: Vec<ContextItemId>,
    pub artifact_refs: Vec<ArtifactId>,
}

impl OrchestratorMessage {
    pub fn into_new_agent_message(self) -> NewAgentMessage {
        NewAgentMessage {
            task_id: self.task_id,
            from: self.from,
            to: self.to,
            kind: self.kind,
            priority: self.priority,
            body: self.body,
            context_refs: self.context_refs,
            artifact_refs: self.artifact_refs,
            delivery_mode: self.delivery_mode,
            requires_response: self.requires_response,
        }
    }

    pub fn into_agent_message(self) -> AgentMessage {
        AgentMessage::new(self.into_new_agent_message())
    }
}

fn resolve_result_target(team: &TeamTemplate, raw: &str) -> Result<MessageTarget> {
    let target = raw.trim();
    if target.is_empty() {
        return Err(AgentmuxError::OrchestratorError(
            "empty result target".to_string(),
        ));
    }

    if let Some(role) = target.strip_prefix("role:") {
        return parse_role_target(role);
    }
    if let Some(team_name) = target.strip_prefix("team:") {
        return Ok(MessageTarget::Team(team_name.to_string()));
    }
    if target.eq_ignore_ascii_case("all") || target.eq_ignore_ascii_case("broadcast") {
        return Ok(MessageTarget::Broadcast);
    }

    if let Some(agent) = team.agent_named(target) {
        return Ok(MessageTarget::Role(agent.role.clone()));
    }

    parse_role_target(target)
}

fn parse_role_target(raw: &str) -> Result<MessageTarget> {
    let normalized = raw.trim().to_ascii_lowercase().replace('-', "_");
    let role = match normalized.as_str() {
        "planner" => AgentRole::Planner,
        "implementer" | "impl" => AgentRole::Implementer,
        "reviewer" => AgentRole::Reviewer,
        "tester" => AgentRole::Tester,
        "debugger" => AgentRole::Debugger,
        "refactorer" => AgentRole::Refactorer,
        "security_reviewer" => AgentRole::SecurityReviewer,
        "docs_writer" => AgentRole::DocsWriter,
        "integrator" => AgentRole::Integrator,
        "context_manager" => AgentRole::ContextManager,
        _ => {
            return Err(AgentmuxError::OrchestratorError(format!(
                "unknown result target '{raw}'"
            )));
        }
    };
    Ok(MessageTarget::Role(role))
}

fn parse_context_refs(refs: &[String]) -> Result<Vec<ContextItemId>> {
    refs.iter()
        .map(|value| {
            ContextItemId::from_str(value).map_err(|error| {
                AgentmuxError::OrchestratorError(format!("invalid context ref '{value}': {error}"))
            })
        })
        .collect()
}

fn parse_artifact_refs(refs: &[String]) -> Result<Vec<ArtifactId>> {
    refs.iter()
        .map(|value| {
            ArtifactId::from_str(value).map_err(|error| {
                AgentmuxError::OrchestratorError(format!("invalid artifact ref '{value}': {error}"))
            })
        })
        .collect()
}

fn map_message_kind(kind: OutgoingMessageKind) -> MessageKind {
    match kind {
        OutgoingMessageKind::TaskAssignment => MessageKind::TaskAssignment,
        OutgoingMessageKind::Question => MessageKind::Question,
        OutgoingMessageKind::Finding => MessageKind::Finding,
        OutgoingMessageKind::PatchProposal => MessageKind::PatchProposal,
        OutgoingMessageKind::ReviewComment => MessageKind::ReviewComment,
        OutgoingMessageKind::TestResult => MessageKind::TestResult,
        OutgoingMessageKind::FailureReport => MessageKind::FailureReport,
        OutgoingMessageKind::Decision => MessageKind::Decision,
        OutgoingMessageKind::Handoff => MessageKind::Handoff,
        OutgoingMessageKind::ApprovalRequest => MessageKind::ApprovalRequest,
        OutgoingMessageKind::ContextUpdate => MessageKind::ContextUpdate,
        OutgoingMessageKind::StatusProbe => MessageKind::StatusProbe,
    }
}

fn map_priority(priority: OutgoingPriority) -> Priority {
    match priority {
        OutgoingPriority::Low => Priority::Low,
        OutgoingPriority::Normal => Priority::Normal,
        OutgoingPriority::High => Priority::High,
        OutgoingPriority::Urgent => Priority::Urgent,
    }
}

fn role_label(role: &AgentRole) -> &'static str {
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

fn provider_label(provider: &TeamAgentProvider) -> &str {
    match provider {
        TeamAgentProvider::Claude => "Claude",
        TeamAgentProvider::Codex => "Codex",
        TeamAgentProvider::Shell => "shell",
        TeamAgentProvider::Custom(name) => name.as_str(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::{AgentResultStatus, OutgoingMessageKind, StatusProbeRequest};

    fn task_id() -> TaskId {
        TaskId::new()
    }

    fn planner_identity() -> AgentRouteIdentity {
        AgentRouteIdentity {
            name: "planner".to_string(),
            role: AgentRole::Planner,
        }
    }

    fn identity(name: &str, role: AgentRole) -> AgentRouteIdentity {
        AgentRouteIdentity {
            name: name.to_string(),
            role,
        }
    }

    fn completed_result(summary: &str) -> AgentResult {
        AgentResult {
            status: AgentResultStatus::Completed,
            summary: summary.to_string(),
            changed_files: Vec::new(),
            messages: Vec::new(),
            context_updates: Vec::new(),
            needs: Vec::new(),
            next: None,
            recommendation: None,
            risk: None,
        }
    }

    fn workflow_context() -> WorkflowHandoffContext {
        WorkflowHandoffContext {
            task_title: "Fix auth refresh".to_string(),
            worktree_path: "/repo/.worktrees/task-123-codex".to_string(),
            test_command: "cargo test -p auth".to_string(),
            diff_path: Some(".agentmux/artifacts/task-123/diff-impl-codex-001.patch".to_string()),
            test_log_path: Some(".agentmux/artifacts/task-123/test-tester-001.log".to_string()),
            task_brief_path: Some(".agentmux/inbox/planner/task-brief.md".to_string()),
            candidate_worktrees: vec!["task-123-codex".to_string()],
        }
    }

    #[test]
    fn default_team_template_matches_spec_roles_and_worktrees() {
        let team = default_claude_codex_team();

        assert_eq!(team.name, "claude-codex");
        assert_eq!(team.agents.len(), 5);
        assert_eq!(team.agents[0].name, "planner");
        assert_eq!(team.agents[0].role, AgentRole::Planner);
        assert_eq!(team.agents[1].name, "impl-codex");
        assert_eq!(team.agents[1].worktree, WorktreePolicy::Dedicated);
        assert_eq!(
            team.agents
                .iter()
                .filter(|agent| agent.role == AgentRole::Implementer)
                .count(),
            2
        );
        assert!(
            team.agents
                .iter()
                .any(|agent| agent.role == AgentRole::Tester)
        );
        assert!(
            team.agents
                .iter()
                .any(|agent| agent.role == AgentRole::Reviewer)
        );
    }

    #[test]
    fn task_run_plans_planner_bootstrap_prompt() {
        let plan = plan_task_run(
            task_id(),
            "Fix the failing auth refresh test.",
            default_claude_codex_team(),
        )
        .expect("task run plan");

        assert_eq!(plan.bootstrap.to, MessageTarget::Role(AgentRole::Planner));
        assert_eq!(plan.bootstrap.kind, MessageKind::TaskAssignment);
        assert_eq!(plan.bootstrap.delivery_mode, DeliveryMode::InjectWhenIdle);
        assert!(plan.bootstrap.body.contains("あなたはplannerです"));
        assert!(
            plan.bootstrap
                .body
                .contains("Fix the failing auth refresh test.")
        );
        assert!(plan.bootstrap.body.contains("impl-codex"));
        assert!(plan.bootstrap.body.contains("AGENTMUX_RESULT JSON"));
    }

    #[test]
    fn result_messages_route_to_role_targets() {
        let result = AgentResult {
            status: AgentResultStatus::Completed,
            summary: "Planning done.".to_string(),
            changed_files: Vec::new(),
            messages: vec![OutgoingMessage {
                to: "role:tester".to_string(),
                kind: OutgoingMessageKind::TestResult,
                body: "Run cargo test.".to_string(),
                priority: OutgoingPriority::High,
                context_refs: Vec::new(),
                artifact_refs: Vec::new(),
            }],
            context_updates: Vec::new(),
            needs: Vec::new(),
            next: None,
            recommendation: None,
            risk: None,
        };

        let routed = route_agent_result(
            &planner_identity(),
            task_id(),
            &default_claude_codex_team(),
            result,
        )
        .expect("route result");

        assert_eq!(routed.status, AgentResultStatus::Completed);
        assert!(!routed.needs_human);
        assert_eq!(routed.outgoing.len(), 1);
        assert_eq!(
            routed.outgoing[0].from,
            MessageSource::TeamAgent("planner".to_string())
        );
        assert_eq!(
            routed.outgoing[0].to,
            MessageTarget::Role(AgentRole::Tester)
        );
        assert_eq!(routed.outgoing[0].kind, MessageKind::TestResult);
        assert_eq!(routed.outgoing[0].priority, Priority::High);
    }

    #[test]
    fn result_next_creates_summary_handoff_when_no_explicit_messages() {
        let result = AgentResult {
            status: AgentResultStatus::Completed,
            summary: "Implement auth fix in the dedicated worktree.".to_string(),
            changed_files: Vec::new(),
            messages: Vec::new(),
            context_updates: Vec::new(),
            needs: Vec::new(),
            next: Some("impl-codex".to_string()),
            recommendation: None,
            risk: None,
        };

        let routed = route_agent_result(
            &planner_identity(),
            task_id(),
            &default_claude_codex_team(),
            result,
        )
        .expect("route next");

        assert_eq!(
            routed.outgoing[0].from,
            MessageSource::TeamAgent("planner".to_string())
        );
        assert_eq!(
            routed.outgoing[0].to,
            MessageTarget::Role(AgentRole::Implementer)
        );
        assert_eq!(routed.outgoing[0].kind, MessageKind::Handoff);
        assert!(routed.outgoing[0].body.contains("from: planner"));
    }

    #[test]
    fn malformed_result_parse_routes_status_probe() {
        let routed = route_agent_result_parse(
            &planner_identity(),
            task_id(),
            &default_claude_codex_team(),
            AgentResultParse::NeedsStatusProbe(StatusProbeRequest {
                marker_offset: 10,
                reason: "bad json".to_string(),
            }),
        )
        .expect("route parse");

        let OrchestratorResult::NeedsStatusProbe(message) = routed else {
            panic!("expected status probe");
        };

        assert_eq!(message.kind, MessageKind::StatusProbe);
        assert_eq!(message.priority, Priority::Urgent);
        assert!(message.body.contains("bad json"));
    }

    #[test]
    fn stalled_detector_sends_probe_then_escalates() {
        let detector = StalledDetector {
            quiet_threshold: Duration::from_secs(60),
            max_status_probe: 2,
        };
        let now = DateTimeUtc::UNIX_EPOCH + time::Duration::seconds(120);
        let last = DateTimeUtc::UNIX_EPOCH;

        let first = detector.detect(
            planner_identity(),
            task_id(),
            AgentStatus::RunningTurn,
            last,
            now,
            1,
        );
        assert!(matches!(first, StallDecision::SendStatusProbe(_)));

        let second = detector.detect(
            planner_identity(),
            task_id(),
            AgentStatus::RunningTurn,
            last,
            now,
            2,
        );
        assert!(matches!(second, StallDecision::NeedsHuman { .. }));
    }

    #[test]
    fn stalled_detector_ignores_recent_activity() {
        let detector = StalledDetector {
            quiet_threshold: Duration::from_secs(60),
            max_status_probe: 2,
        };
        let now = DateTimeUtc::UNIX_EPOCH + time::Duration::seconds(30);

        let decision = detector.detect(
            planner_identity(),
            task_id(),
            AgentStatus::RunningTurn,
            DateTimeUtc::UNIX_EPOCH,
            now,
            0,
        );

        assert_eq!(decision, StallDecision::NoAction);
    }

    #[test]
    fn standard_workflow_advances_handoffs_and_builds_final_summary() {
        let task_id = task_id();
        let team = default_claude_codex_team();
        let context = workflow_context();
        let mut state = StandardWorkflowState::new(task_id.clone());

        let mut planner_result = completed_result("Implement the auth refresh expiry fix.");
        planner_result.next = Some("impl-codex".to_string());
        let advanced = advance_standard_workflow(
            state,
            &identity("planner", AgentRole::Planner),
            &team,
            planner_result,
            &context,
        )
        .expect("planner handoff");
        state = advanced.state;
        assert_eq!(state.stage, StandardWorkflowStage::Implementing);
        assert_eq!(advanced.outgoing.len(), 1);
        assert_eq!(
            advanced.outgoing[0].to,
            MessageTarget::Role(AgentRole::Implementer)
        );
        assert_eq!(advanced.outgoing[0].kind, MessageKind::TaskAssignment);
        assert!(advanced.outgoing[0].body.contains("kind: TaskAssignment"));
        assert!(advanced.outgoing[0].body.contains(&context.worktree_path));

        let mut implementer_result = completed_result("Changed expiry validation and added tests.");
        implementer_result.changed_files = vec![
            "src/auth/refresh.rs".to_string(),
            "tests/auth_refresh.rs".to_string(),
        ];
        let advanced = advance_standard_workflow(
            state,
            &identity("impl-codex", AgentRole::Implementer),
            &team,
            implementer_result,
            &context,
        )
        .expect("tester handoff");
        state = advanced.state;
        assert_eq!(state.stage, StandardWorkflowStage::Testing);
        assert_eq!(
            advanced.outgoing[0].to,
            MessageTarget::Role(AgentRole::Tester)
        );
        assert!(advanced.outgoing[0].body.contains("kind: TestRequest"));
        assert!(advanced.outgoing[0].body.contains(&context.test_command));

        let tester_result = completed_result("cargo test -p auth passed.");
        let advanced = advance_standard_workflow(
            state,
            &identity("tester", AgentRole::Tester),
            &team,
            tester_result,
            &context,
        )
        .expect("reviewer handoff");
        state = advanced.state;
        assert_eq!(state.stage, StandardWorkflowStage::Reviewing);
        assert_eq!(
            advanced.outgoing[0].to,
            MessageTarget::Role(AgentRole::Reviewer)
        );
        assert!(advanced.outgoing[0].body.contains("kind: ReviewRequest"));
        assert!(
            advanced.outgoing[0]
                .body
                .contains(context.diff_path.as_deref().expect("diff path"))
        );

        let mut reviewer_result = completed_result("Reviewed and approved.");
        reviewer_result.recommendation = Some(ResultRecommendation::Approve);
        reviewer_result.risk = Some(ResultRisk::Low);
        let advanced = advance_standard_workflow(
            state,
            &identity("reviewer", AgentRole::Reviewer),
            &team,
            reviewer_result,
            &context,
        )
        .expect("final summary");

        let summary = advanced.final_summary.expect("final summary");
        assert_eq!(advanced.state.stage, StandardWorkflowStage::Completed);
        assert!(advanced.outgoing.is_empty());
        assert_eq!(
            summary.changed_files,
            vec!["src/auth/refresh.rs", "tests/auth_refresh.rs"]
        );
        assert_eq!(
            summary.reviewer_recommendation,
            Some(ResultRecommendation::Approve)
        );
        assert_eq!(
            summary.promote_command.as_deref(),
            Some("agentmux worktree promote task-123-codex")
        );
        let rendered = summary.render_markdown();
        assert!(rendered.contains("Fix auth refresh"));
        assert!(rendered.contains("reviewer recommendation: approve"));
        assert!(rendered.contains("cargo test -p auth passed."));
    }

    #[test]
    fn standard_workflow_stops_for_non_completed_result() {
        let task_id = task_id();
        let mut result = completed_result("Need credentials.");
        result.status = AgentResultStatus::Blocked;
        result.needs = vec!["refresh token fixture secret".to_string()];

        let advanced = advance_standard_workflow(
            StandardWorkflowState::new(task_id),
            &identity("planner", AgentRole::Planner),
            &default_claude_codex_team(),
            result,
            &workflow_context(),
        )
        .expect("blocked planner result");

        assert_eq!(advanced.state.stage, StandardWorkflowStage::NeedsHuman);
        assert!(advanced.needs_human);
        assert!(advanced.outgoing.is_empty());
        assert!(advanced.final_summary.is_none());
    }
}
