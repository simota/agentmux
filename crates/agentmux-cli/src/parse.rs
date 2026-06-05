//! Parsing and normalization helpers for CLI arguments and wire values.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use agentmux_core::{AgentmuxError, error::Result};
use agentmux_tui::state::AgentProviderChoice;

use crate::StartupPaneChoice;

static AGENT_NAME_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(crate) fn unique_agent_name(prefix: &str) -> String {
    let prefix = sanitize_agent_name_prefix(prefix);
    let sequence = AGENT_NAME_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let entropy = nanos ^ ((std::process::id() as u64) << 32) ^ sequence;
    format!("{prefix}-{}", base36_suffix(entropy, 6))
}

pub(crate) fn sanitize_agent_name_prefix(prefix: &str) -> String {
    let mut output = String::new();
    let mut last_was_separator = false;
    for ch in prefix.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            output.push(ch.to_ascii_lowercase());
            last_was_separator = false;
        } else if !last_was_separator && !output.is_empty() {
            output.push('-');
            last_was_separator = true;
        }
    }
    while output.ends_with('-') {
        output.pop();
    }
    if output.is_empty() {
        "agent".to_string()
    } else {
        output
    }
}

pub(crate) fn base36_suffix(mut value: u64, len: usize) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut chars = vec!['0'; len];
    for slot in chars.iter_mut().rev() {
        *slot = DIGITS[(value % 36) as usize] as char;
        value /= 36;
    }
    chars.into_iter().collect()
}

pub(crate) fn parse_start_panes(raw: Option<&str>) -> Result<Vec<StartupPaneChoice>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    raw.split(',')
        .map(str::trim)
        .filter(|pane| !pane.is_empty())
        .map(parse_start_pane_choice)
        .collect()
}

pub(crate) fn parse_start_pane_choice(raw: &str) -> Result<StartupPaneChoice> {
    match raw.to_ascii_lowercase().as_str() {
        "messages" | "message" | "message-bus" | "message_bus" | "conversation-list"
        | "conversation_list" => Ok(StartupPaneChoice::Messages),
        _ => parse_provider_choice(raw).map(StartupPaneChoice::Agent),
    }
}

pub(crate) fn parse_provider_choice(raw: &str) -> Result<AgentProviderChoice> {
    match raw.to_ascii_lowercase().as_str() {
        "claude" | "claude-code" | "claude_code" => Ok(AgentProviderChoice::Claude),
        "codex" => Ok(AgentProviderChoice::Codex),
        "agy" | "antigravity" => Ok(AgentProviderChoice::Agy),
        _ => Err(AgentmuxError::UserError(format!(
            "unknown start pane '{raw}' (expected claude, codex, agy, or messages)"
        ))),
    }
}

pub(crate) fn normalize_agent_target(raw: &str) -> String {
    let target = raw.trim();
    if target.starts_with("agent:") {
        target.to_string()
    } else {
        format!("agent:{target}")
    }
}

/// Map a user-supplied `--kind` value (the protocol's PascalCase names, accepted
/// case-insensitively) to the daemon's snake_case wire value. Returns a clear
/// error listing the allowed values for anything unrecognized.
pub(crate) fn normalize_message_kind(raw: &str) -> Result<String> {
    let wire = match raw.trim().to_ascii_lowercase().as_str() {
        "taskassignment" => "task_assignment",
        "question" => "question",
        "finding" => "finding",
        "patchproposal" => "patch_proposal",
        "reviewcomment" => "review_comment",
        "testresult" => "test_result",
        "failurereport" => "failure_report",
        "decision" => "decision",
        "handoff" => "handoff",
        "approvalrequest" => "approval_request",
        "contextupdate" => "context_update",
        "statusprobe" => "status_probe",
        _ => {
            return Err(AgentmuxError::UserError(format!(
                "invalid message kind '{raw}'. Allowed values: TaskAssignment, Question, \
                 Finding, PatchProposal, ReviewComment, TestResult, FailureReport, Decision, \
                 Handoff, ApprovalRequest, ContextUpdate, StatusProbe"
            )));
        }
    };
    Ok(wire.to_string())
}

/// Validate and normalize a `--priority` value to the wire form.
pub(crate) fn normalize_priority(raw: &str) -> Result<String> {
    let wire = match raw.trim().to_ascii_lowercase().as_str() {
        "low" => "low",
        "normal" => "normal",
        "high" => "high",
        "urgent" => "urgent",
        _ => {
            return Err(AgentmuxError::UserError(format!(
                "invalid priority '{raw}'. Allowed values: low, normal, high, urgent"
            )));
        }
    };
    Ok(wire.to_string())
}

pub(crate) fn should_inject_message(_inject: bool, no_inject: bool) -> bool {
    !no_inject
}
