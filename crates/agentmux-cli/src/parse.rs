//! Parsing and normalization helpers for CLI arguments and wire values.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use agentmux_core::{AgentmuxError, error::Result};
use agentmux_tui::layout::SplitDirection;
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

/// Result of parsing the `agentmux start "<spec>"` layout argument.
///
/// `direction` follows the engine naming in [`agentmux_tui::layout::SplitDirection`],
/// NOT the spec notation. The spec notation is intentionally inverted relative to
/// the engine: spec `|` (left-right) maps to engine `Vertical`, and spec `―`
/// (top-bottom) maps to engine `Horizontal`. See [`parse_start_layout`] for the
/// concrete mapping at each conversion site.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StartupLayout {
    pub panes: Vec<StartupPaneChoice>,
    pub direction: SplitDirection,
}

/// Top-bottom split character `―` (U+2015, HORIZONTAL BAR).
const TOP_BOTTOM_BAR: char = '\u{2015}';

/// Parse the `agentmux start "<spec>"` layout argument (Phase 1: flat direction only).
///
/// Supported separators (one direction per start, no nesting):
/// - `|` (U+007C) and ASCII alias `/`  -> panes placed left-right -> engine `Vertical`
/// - `―` (U+2015) and a standalone ` - ` (space-padded ASCII dash) -> panes placed
///   top-bottom -> engine `Horizontal`
/// - `,` (legacy) -> equivalent to `|` (left-right -> engine `Vertical`)
/// - no separator / empty -> default `Vertical` (preserves the picker behavior)
///
/// Phase 2 features (`()` nesting and `:N` size specs) are rejected with an explicit
/// `UserError` rather than silently ignored.
pub(crate) fn parse_start_layout(raw: Option<&str>) -> Result<StartupLayout> {
    let Some(raw) = raw else {
        return Ok(StartupLayout {
            panes: Vec::new(),
            direction: SplitDirection::Vertical,
        });
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(StartupLayout {
            panes: Vec::new(),
            direction: SplitDirection::Vertical,
        });
    }

    // Phase 2: nested layouts via `()` are not supported yet.
    if raw.contains('(') || raw.contains(')') {
        return Err(AgentmuxError::UserError(
            "nested layouts with '()' are not supported yet; use a flat list like \
             \"agy | codex\" for now"
                .to_string(),
        ));
    }
    // Phase 2: explicit pane size specs via `:N` are not supported yet.
    if raw.contains(':') {
        return Err(AgentmuxError::UserError(
            "pane size specs like 'agy:60' are not supported yet; sizes arrive in a later phase"
                .to_string(),
        ));
    }

    let has_comma = raw.contains(',');
    let has_lr = raw.contains('|') || raw.contains('/');
    let has_tb_bar = raw.contains(TOP_BOTTOM_BAR);
    // A standalone space-padded `-` means top-bottom. Word-internal hyphens
    // (e.g. `claude-code`) carry no surrounding whitespace and are not matched.
    let has_tb_dash = raw.split_whitespace().any(|token| token == "-");

    if has_comma && (has_lr || has_tb_bar || has_tb_dash) {
        return Err(AgentmuxError::UserError(
            "legacy comma list cannot be mixed with '|'/'―' splitters; use one style \
             ('|' is the modern equivalent of ',')"
                .to_string(),
        ));
    }
    if has_lr && (has_tb_bar || has_tb_dash) {
        return Err(AgentmuxError::UserError(
            "cannot mix left-right '|' and top-bottom '―' at the same level; Phase 1 supports \
             only one direction per start (nesting with '()' arrives later)"
                .to_string(),
        ));
    }

    // Direction mapping: spec top-bottom -> engine `Horizontal`; everything else
    // (comma / left-right / single pane) -> engine `Vertical`.
    let direction = if has_tb_bar || has_tb_dash {
        SplitDirection::Horizontal
    } else {
        SplitDirection::Vertical
    };

    // Tokenize according to the detected separator style.
    let tokens: Vec<&str> = if has_comma {
        raw.split(',').collect()
    } else if has_lr {
        raw.split(|ch| ch == '|' || ch == '/').collect()
    } else if has_tb_bar {
        raw.split(TOP_BOTTOM_BAR).collect()
    } else if has_tb_dash {
        // Whitespace tokens; standalone `-` is the separator, the rest are pane names.
        raw.split_whitespace().filter(|token| *token != "-").collect()
    } else {
        // No separator: the whole input is a single pane name.
        vec![raw]
    };

    let panes = tokens
        .into_iter()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(parse_start_pane_choice)
        .collect::<Result<Vec<_>>>()?;

    if panes.is_empty() {
        return Err(AgentmuxError::UserError(format!(
            "expected a pane name; got only separators in '{raw}'"
        )));
    }

    Ok(StartupLayout { panes, direction })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(provider: AgentProviderChoice) -> StartupPaneChoice {
        StartupPaneChoice::Agent(provider)
    }

    fn ok(raw: &str) -> StartupLayout {
        parse_start_layout(Some(raw)).expect("expected a valid layout")
    }

    fn err(raw: &str) -> String {
        match parse_start_layout(Some(raw)) {
            Err(AgentmuxError::UserError(message)) => message,
            other => panic!("expected UserError, got {other:?}"),
        }
    }

    #[test]
    fn none_input_yields_empty_panes_and_vertical() {
        let layout = parse_start_layout(None).expect("None must parse");
        assert!(layout.panes.is_empty());
        assert_eq!(layout.direction, SplitDirection::Vertical);
    }

    #[test]
    fn blank_input_yields_empty_panes_and_vertical() {
        let layout = ok("   ");
        assert!(layout.panes.is_empty());
        assert_eq!(layout.direction, SplitDirection::Vertical);
    }

    #[test]
    fn single_pane_is_vertical() {
        let layout = ok("agy");
        assert_eq!(layout.panes, vec![agent(AgentProviderChoice::Agy)]);
        assert_eq!(layout.direction, SplitDirection::Vertical);
    }

    #[test]
    fn legacy_comma_list_stays_vertical_with_order_preserved() {
        let layout = ok("agy,codex,messages");
        assert_eq!(
            layout.panes,
            vec![
                agent(AgentProviderChoice::Agy),
                agent(AgentProviderChoice::Codex),
                StartupPaneChoice::Messages,
            ]
        );
        assert_eq!(layout.direction, SplitDirection::Vertical);
    }

    #[test]
    fn left_right_bar_is_vertical_with_and_without_spaces() {
        let expected = vec![agent(AgentProviderChoice::Agy), agent(AgentProviderChoice::Codex)];
        let spaced = ok("agy | codex");
        assert_eq!(spaced.panes, expected);
        assert_eq!(spaced.direction, SplitDirection::Vertical);
        let tight = ok("agy|codex");
        assert_eq!(tight.panes, expected);
        assert_eq!(tight.direction, SplitDirection::Vertical);
    }

    #[test]
    fn left_right_slash_alias_is_vertical_with_and_without_spaces() {
        let expected = vec![agent(AgentProviderChoice::Agy), agent(AgentProviderChoice::Codex)];
        let spaced = ok("agy / codex");
        assert_eq!(spaced.panes, expected);
        assert_eq!(spaced.direction, SplitDirection::Vertical);
        let tight = ok("agy/codex");
        assert_eq!(tight.panes, expected);
        assert_eq!(tight.direction, SplitDirection::Vertical);
    }

    #[test]
    fn top_bottom_bar_u2015_is_horizontal() {
        let layout = ok("agy ― codex");
        assert_eq!(
            layout.panes,
            vec![agent(AgentProviderChoice::Agy), agent(AgentProviderChoice::Codex)]
        );
        assert_eq!(layout.direction, SplitDirection::Horizontal);
    }

    #[test]
    fn top_bottom_spaced_dash_is_horizontal() {
        let layout = ok("agy - codex");
        assert_eq!(
            layout.panes,
            vec![agent(AgentProviderChoice::Agy), agent(AgentProviderChoice::Codex)]
        );
        assert_eq!(layout.direction, SplitDirection::Horizontal);
    }

    #[test]
    fn word_internal_hyphen_is_not_a_separator() {
        let layout = ok("claude-code | codex");
        assert_eq!(
            layout.panes,
            vec![agent(AgentProviderChoice::Claude), agent(AgentProviderChoice::Codex)]
        );
        assert_eq!(layout.direction, SplitDirection::Vertical);
    }

    #[test]
    fn hyphen_without_spaces_is_a_single_unknown_token() {
        let message = err("agy-codex");
        assert!(
            message.contains("unknown start pane 'agy-codex'"),
            "unexpected message: {message}"
        );
    }

    #[test]
    fn mixing_left_right_and_top_bottom_is_rejected() {
        let message = err("agy | codex ― messages");
        assert!(
            message.contains("cannot mix left-right '|' and top-bottom"),
            "unexpected message: {message}"
        );
    }

    #[test]
    fn mixing_comma_and_splitter_is_rejected() {
        let message = err("agy, codex | messages");
        assert!(
            message.contains("legacy comma list cannot be mixed"),
            "unexpected message: {message}"
        );
    }

    #[test]
    fn nested_layout_with_parens_is_rejected() {
        let message = err("(agy ― codex) | messages");
        assert!(
            message.contains("nested layouts with '()' are not supported yet"),
            "unexpected message: {message}"
        );
    }

    #[test]
    fn pane_size_spec_is_rejected() {
        let message = err("agy:60 | codex:40");
        assert!(
            message.contains("pane size specs like 'agy:60' are not supported yet"),
            "unexpected message: {message}"
        );
    }

    #[test]
    fn separator_only_input_is_rejected() {
        let message = err("|");
        assert!(
            message.contains("expected a pane name; got only separators"),
            "unexpected message: {message}"
        );
    }

    #[test]
    fn pane_names_are_case_insensitive() {
        let layout = ok("AGY | CODEX");
        assert_eq!(
            layout.panes,
            vec![agent(AgentProviderChoice::Agy), agent(AgentProviderChoice::Codex)]
        );
        assert_eq!(layout.direction, SplitDirection::Vertical);
    }
}
