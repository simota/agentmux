use std::time::Duration;

use agentmux_core::{
    AgentRole, AgentStatus, DateTimeUtc, DeliveryMode, Priority, TaskId,
};
use agentmux_message::{MessageKind, MessageSource, MessageTarget};

use super::*;
use crate::result::{
    AgentResult, AgentResultParse, AgentResultStatus, OutgoingMessage, OutgoingMessageKind,
    OutgoingPriority, ResultRecommendation, ResultRisk, StatusProbeRequest,
};

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
