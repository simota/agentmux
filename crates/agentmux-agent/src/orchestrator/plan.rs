use agentmux_core::{AgentmuxError, DeliveryMode, Priority, TaskId, error::Result};
use agentmux_message::{MessageKind, MessageSource, MessageTarget};

use super::message::OrchestratorMessage;
use super::team::{TeamTemplate, provider_label, role_label};

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
