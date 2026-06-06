use agentmux_core::{AgentRole, AgentmuxError, DeliveryMode, Priority, TaskId, error::Result};
use agentmux_message::{MessageKind, MessageSource, MessageTarget};

use super::message::{OrchestratorMessage, resolve_result_target};
use super::routing::AgentRouteIdentity;
use super::team::TeamTemplate;
use crate::result::{AgentResult, AgentResultStatus, ResultRecommendation, ResultRisk};

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
