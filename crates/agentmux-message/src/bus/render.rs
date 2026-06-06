use agentmux_core::{AgentProvider, Priority};

use super::types::PromptContext;
use crate::message::{AgentMessage, MessageKind, MessageSource, MessageTarget};
use crate::thread::MessageThread;

pub fn render_prompt(
    message: &AgentMessage,
    provider: AgentProvider,
    context: &PromptContext,
    thread: Option<&MessageThread>,
) -> String {
    let mut rendered = format!(
        "[agentmux handoff]\nfrom: {}\nkind: {}\npriority: {}\nmessage_id: {}\n",
        source_label(&message.from),
        kind_label(&message.kind),
        priority_label(&message.priority),
        message.id,
    );
    if let Some(thread) = thread {
        rendered.push_str(&format!("thread: {}\ntopic: {}\n", thread.id, thread.topic));
    }
    rendered.push_str(&format!(
        "\nmessage:\n{}\n\nattached context:\n",
        message.body
    ));

    if context.inline_items.is_empty() && context.mailbox_paths.is_empty() {
        rendered.push_str("- none\n");
    } else {
        for item in &context.inline_items {
            rendered.push_str(&format!("- {}: {}\n", item.title, item.body));
        }
        for path in &context.mailbox_paths {
            rendered.push_str(&format!("- {}\n", path.display()));
        }
    }

    rendered.push_str("\nrequired:\n");
    if !context.mailbox_paths.is_empty() {
        rendered.push_str("- attached context の path を必要に応じて読んでください\n");
    }
    if let Some(thread) = thread {
        rendered.push_str(&format!(
            "- この会議スレッドへの返信は `agentmux message send --thread {} --kind <Kind> \"<body>\"` を使ってください(自分以外の参加者全員に届きます)\n",
            thread.id
        ));
        rendered.push_str(&format!(
            "- 発言上限は 1 参加者あたり {} 通です。上限に達したら結論を要約し、スレッド外で人間に判断を仰いでください\n",
            thread.max_messages_per_participant
        ));
    }
    rendered.push_str("- 内容を読んで必要なら作業してください\n");
    rendered.push_str("- 通常の返信や進捗共有では送信前に人間確認を求めないでください\n");
    rendered.push_str("- 完了時は必ず AGENTMUX_RESULT JSON を出力してください\n");

    match provider {
        AgentProvider::Codex => {
            rendered.push_str("\nprovider note: workspace 内の path はそのまま参照してください\n");
        }
        AgentProvider::ClaudeCode => {
            rendered.push_str("\nprovider note: mailbox file は作業ディレクトリから読めます\n");
        }
        AgentProvider::Shell | AgentProvider::Custom(_) => {}
    }

    rendered
}

fn source_label(source: &MessageSource) -> String {
    match source {
        MessageSource::User(id) => format!("user:{id}"),
        MessageSource::Agent(id) => format!("agent:{id}"),
        MessageSource::TeamAgent(name) => format!("team_agent:{name}"),
        MessageSource::Role(role) => format!("role:{role:?}"),
        MessageSource::System => "system".to_string(),
        MessageSource::Orchestrator => "orchestrator".to_string(),
    }
}

pub(crate) fn target_label(target: &MessageTarget) -> String {
    match target {
        MessageTarget::Agent(id) => format!("agent:{id}"),
        MessageTarget::AgentName(name) => format!("agent:{name}"),
        MessageTarget::Role(role) => format!("role:{role:?}"),
        MessageTarget::Task(id) => format!("task:{id}"),
        MessageTarget::Team(team) => format!("team:{team}"),
        MessageTarget::Thread(id) => format!("thread:{id}"),
        MessageTarget::Broadcast => "broadcast".to_string(),
    }
}

fn kind_label(kind: &MessageKind) -> &'static str {
    match kind {
        MessageKind::TaskAssignment => "TaskAssignment",
        MessageKind::Question => "Question",
        MessageKind::Finding => "Finding",
        MessageKind::PatchProposal => "PatchProposal",
        MessageKind::ReviewComment => "ReviewComment",
        MessageKind::TestResult => "TestResult",
        MessageKind::FailureReport => "FailureReport",
        MessageKind::Decision => "Decision",
        MessageKind::Handoff => "Handoff",
        MessageKind::ApprovalRequest => "ApprovalRequest",
        MessageKind::ContextUpdate => "ContextUpdate",
        MessageKind::StatusProbe => "StatusProbe",
    }
}

fn priority_label(priority: &Priority) -> &'static str {
    match priority {
        Priority::Low => "Low",
        Priority::Normal => "Normal",
        Priority::High => "High",
        Priority::Urgent => "Urgent",
    }
}
