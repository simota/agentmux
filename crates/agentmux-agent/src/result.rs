//! `AGENTMUX_RESULT` marker parsing.
//!
//! The parser intentionally does not repair *malformed* JSON. Invalid marker
//! payloads are surfaced as a StatusProbe requirement so the orchestrator can
//! ask the agent for a clean result.
//!
//! # Transport normalization vs. semantic repair
//!
//! The marker payload is read from a raw PTY stream, so it carries
//! transport-layer artifacts produced by the agent's interactive TUI rather
//! than by the agent's intent:
//!
//! - **ANSI/VT escape sequences** (CSI / OSC / lone ESC) injected for colors,
//!   cursor movement, etc.
//! - **Terminal line wrapping**: a TUI that re-renders its output wraps long
//!   lines at the pane width, inserting a hard newline (and often continuation
//!   indentation) into the byte stream — *including inside JSON string
//!   literals* (e.g. a long Japanese `body` split mid-string).
//!
//! Removing these artifacts before parsing is **not** semantic repair: a
//! conforming JSON document never contains a literal (unescaped) newline inside
//! a string — newlines must be encoded as `\n`. Therefore any raw newline found
//! *inside* a string literal is provably a transport artifact, and stripping it
//! recovers the exact bytes the agent emitted. We never alter escaped content,
//! never balance braces, never guess missing fields: if the unwrapped text is
//! still not valid JSON, we still return [`AgentResultParse::NeedsStatusProbe`].

use serde::{Deserialize, Serialize};

pub const RESULT_MARKER: &str = "AGENTMUX_RESULT";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentResult {
    pub status: AgentResultStatus,
    pub summary: String,
    #[serde(default)]
    pub changed_files: Vec<String>,
    #[serde(default)]
    pub messages: Vec<OutgoingMessage>,
    #[serde(default)]
    pub context_updates: Vec<ContextUpdate>,
    #[serde(default)]
    pub needs: Vec<String>,
    pub next: Option<String>,
    pub recommendation: Option<ResultRecommendation>,
    pub risk: Option<ResultRisk>,
}

impl AgentResult {
    fn validate(self) -> Result<Self, String> {
        if self.summary.trim().is_empty() {
            return Err("summary must not be empty".to_string());
        }

        for message in &self.messages {
            if message.to.trim().is_empty() {
                return Err("message.to must not be empty".to_string());
            }
            if message.body.trim().is_empty() {
                return Err("message.body must not be empty".to_string());
            }
        }

        for update in &self.context_updates {
            if update.title.trim().is_empty() {
                return Err("context_update.title must not be empty".to_string());
            }
            if update.body.trim().is_empty() {
                return Err("context_update.body must not be empty".to_string());
            }
            if !(0.0..=1.0).contains(&update.confidence) {
                return Err("context_update.confidence must be between 0 and 1".to_string());
            }
        }

        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentResultStatus {
    Completed,
    Blocked,
    NeedsInput,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutgoingMessage {
    pub to: String,
    pub kind: OutgoingMessageKind,
    pub body: String,
    #[serde(default = "default_priority")]
    pub priority: OutgoingPriority,
    #[serde(default)]
    pub context_refs: Vec<String>,
    #[serde(default)]
    pub artifact_refs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutgoingMessageKind {
    TaskAssignment,
    Question,
    Finding,
    PatchProposal,
    ReviewComment,
    TestResult,
    FailureReport,
    Decision,
    Handoff,
    ApprovalRequest,
    ContextUpdate,
    StatusProbe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutgoingPriority {
    Low,
    Normal,
    High,
    Urgent,
}

fn default_priority() -> OutgoingPriority {
    OutgoingPriority::Normal
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextUpdate {
    pub kind: ContextUpdateKind,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_confidence")]
    pub confidence: f32,
}

fn default_confidence() -> f32 {
    0.8
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextUpdateKind {
    ProjectSummary,
    ArchitectureNote,
    CodingRule,
    TaskBrief,
    FileReference,
    DiffSummary,
    TestResult,
    ErrorLog,
    AgentFinding,
    Decision,
    Risk,
    OpenQuestion,
    HandoffSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultRecommendation {
    Approve,
    RequestChanges,
    NeedsTests,
    Continue,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultRisk {
    Low,
    Medium,
    High,
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AgentResultParse {
    Found(ParsedAgentResult),
    NotFound,
    NeedsStatusProbe(StatusProbeRequest),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedAgentResult {
    pub result: AgentResult,
    pub marker_offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusProbeRequest {
    pub marker_offset: usize,
    pub reason: String,
}

pub fn parse_agent_result_marker(buffer_tail: &str) -> AgentResultParse {
    // Strip transport-layer ANSI/VT escape sequences before locating the marker
    // so that color/cursor codes interleaved with the marker or JSON do not
    // defeat detection or parsing. `marker_offset` is reported as an offset into
    // this sanitized text; no production caller indexes the raw buffer with it.
    let sanitized = strip_ansi_escapes(buffer_tail);

    let Some(marker_offset) = sanitized.rfind(RESULT_MARKER) else {
        return AgentResultParse::NotFound;
    };

    let after_marker = &sanitized[marker_offset + RESULT_MARKER.len()..];
    let Some(json_slice) = marker_payload(after_marker) else {
        return AgentResultParse::NeedsStatusProbe(StatusProbeRequest {
            marker_offset,
            reason: "AGENTMUX_RESULT marker was found without a JSON object".to_string(),
        });
    };

    // Extract a single brace-balanced object, unwrapping terminal line-wrap
    // artifacts (raw newlines + continuation indent inside string literals).
    let Some(json_tail) = extract_wrapped_json_object(json_slice) else {
        return AgentResultParse::NeedsStatusProbe(StatusProbeRequest {
            marker_offset,
            reason: "AGENTMUX_RESULT marker was found without a complete JSON object".to_string(),
        });
    };

    match serde_json::Deserializer::from_str(&json_tail)
        .into_iter::<AgentResult>()
        .next()
    {
        Some(Ok(result)) => match result.validate() {
            Ok(result) => AgentResultParse::Found(ParsedAgentResult {
                result,
                marker_offset,
            }),
            Err(reason) => AgentResultParse::NeedsStatusProbe(StatusProbeRequest {
                marker_offset,
                reason,
            }),
        },
        Some(Err(error)) => AgentResultParse::NeedsStatusProbe(StatusProbeRequest {
            marker_offset,
            reason: format!("AGENTMUX_RESULT JSON is invalid: {error}"),
        }),
        None => AgentResultParse::NeedsStatusProbe(StatusProbeRequest {
            marker_offset,
            reason: "AGENTMUX_RESULT marker was found without JSON".to_string(),
        }),
    }
}

fn marker_payload(after_marker: &str) -> Option<&str> {
    let trimmed = after_marker.trim_start();
    let trimmed = trimmed.strip_prefix(':').unwrap_or(trimmed).trim_start();
    trimmed.starts_with('{').then_some(trimmed)
}

/// Remove ANSI/VT escape sequences and stray C0 control characters from raw
/// terminal text, while honoring the one VT control whose effect changes the
/// emitted glyphs (REP).
///
/// Handles the families that a coding-agent TUI emits around its output:
/// - CSI: `ESC [` ... final byte in `0x40..=0x7e`. Most CSI sequences are pure
///   presentation (color, cursor moves, erase) and are dropped. The single
///   exception is **REP** (`ESC [ Pn b`), which means "repeat the previously
///   emitted glyph Pn times"; we expand it so padding/indent runs survive as
///   real characters (terminal-faithful). REP with no preceding glyph is a
///   no-op.
/// - OSC: `ESC ]` ... terminated by BEL (`0x07`) or ST (`ESC \`).
/// - other two-byte escapes: `ESC` followed by a single byte (e.g. `ESC =`),
///   plus the standalone `ESC \` (ST) terminator.
/// - **C0 control characters** other than `\n`, `\r`, `\t` (i.e. `0x00-0x08`,
///   `0x0b`, `0x0c`, `0x0e-0x1f`, `0x7f`). A conforming JSON document never
///   contains a raw control character even inside a string literal (they must
///   be `\u`/`\n`/etc. escaped), so any such byte is provably a transport
///   artifact (e.g. a leftover `\x08` backspace after a cursor move) and is
///   safe to drop. Tab/newline/carriage-return are kept here because they are
///   legal JSON inter-token whitespace and/or are needed by the string-wrap
///   rejoin in `extract_wrapped_json_object`.
///
/// Anything that is not part of an escape or a stray control char is preserved
/// verbatim, including the bytes inside JSON string literals.
fn strip_ansi_escapes(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.peek().copied() {
                // CSI: ESC [ params... final
                Some('[') => {
                    chars.next();
                    let mut params = String::new();
                    let mut final_byte = None;
                    for c in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            final_byte = Some(c);
                            break;
                        }
                        params.push(c);
                    }
                    // REP (final byte 'b'): repeat the last emitted glyph Pn
                    // times. Default Pn is 1 when omitted.
                    if final_byte == Some('b')
                        && let Some(last) = out.chars().last()
                    {
                        let count: usize = params.trim().parse().unwrap_or(1);
                        for _ in 0..count {
                            out.push(last);
                        }
                    }
                }
                // OSC: ESC ] ... (BEL | ST)
                Some(']') => {
                    chars.next();
                    while let Some(c) = chars.next() {
                        if c == '\u{07}' {
                            break;
                        }
                        if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                // Any other escape: drop ESC and the single following byte.
                Some(_) => {
                    chars.next();
                }
                // Trailing lone ESC at end of buffer: drop it.
                None => {}
            }
            continue;
        }

        // Drop stray C0 control characters (and DEL) that JSON can never carry
        // raw; keep tab/newline/carriage-return.
        if (ch.is_control() && ch != '\n' && ch != '\r' && ch != '\t') || ch == '\u{7f}' {
            continue;
        }

        out.push(ch);
    }

    out
}

/// Walk a single JSON object starting at the leading `{`, tracking string and
/// escape state, and return its text with terminal line-wrap artifacts removed.
///
/// While inside a string literal, a raw (unescaped) newline cannot occur in
/// valid JSON, so any `\n` (optionally `\r\n`) found there is treated as a wrap
/// boundary: the run of pane-padding whitespace immediately *before* the
/// newline, the newline itself, and the run of continuation indentation
/// immediately *after* it are all dropped, rejoining the split string. Outside
/// of strings, whitespace (including newlines) is preserved — it is legal JSON
/// inter-token whitespace and serde handles it.
///
/// The padding/indent at a wrap boundary is display padding that is inseparable
/// from the wrap itself, so it is removed. (Known limitation: if a renderer
/// elides the original significant space at the wrap point, that space cannot be
/// recovered.)
///
/// Returns `None` if no balanced object terminates within the slice.
fn extract_wrapped_json_object(slice: &str) -> Option<String> {
    let mut chars = slice.chars().peekable();
    if chars.peek() != Some(&'{') {
        return None;
    }

    let mut out = String::with_capacity(slice.len());
    let mut depth: usize = 0;
    let mut in_string = false;
    let mut escaped = false;

    while let Some(ch) = chars.next() {
        if in_string {
            if escaped {
                out.push(ch);
                escaped = false;
                continue;
            }
            match ch {
                '\\' => {
                    out.push('\\');
                    escaped = true;
                }
                '"' => {
                    out.push('"');
                    in_string = false;
                }
                '\r' | '\n' => {
                    // Terminal wrap inside a string literal: drop any pane
                    // padding already emitted just before the newline, the
                    // newline (consuming a following `\n` for CRLF), and the
                    // continuation indentation after it, so the split string
                    // rejoins without stray whitespace.
                    while matches!(out.chars().last(), Some(' ') | Some('\t')) {
                        out.pop();
                    }
                    if ch == '\r' && chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    while matches!(chars.peek(), Some(' ') | Some('\t')) {
                        chars.next();
                    }
                }
                _ => out.push(ch),
            }
            continue;
        }

        // Outside a string literal.
        match ch {
            '"' => {
                out.push('"');
                in_string = true;
            }
            '{' => {
                depth += 1;
                out.push('{');
            }
            '}' => {
                out.push('}');
                depth -= 1;
                if depth == 0 {
                    return Some(out);
                }
            }
            _ => out.push(ch),
        }
    }

    // Reached end of slice without closing the object.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_latest_multiline_result_marker() {
        let transcript = r#"
old AGENTMUX_RESULT: {"status":"failed","summary":"old"}
done
AGENTMUX_RESULT:
{
  "status": "completed",
  "summary": "Implemented parser.",
  "changed_files": ["crates/agentmux-agent/src/result.rs"],
  "messages": [
    {
      "to": "role:tester",
      "kind": "TestResult",
      "body": "Run parser tests",
      "context_refs": ["ctx_1"],
      "artifact_refs": ["art_1"]
    }
  ],
  "context_updates": [
    {
      "kind": "Decision",
      "title": "Parser behavior",
      "body": "Do not repair invalid JSON."
    }
  ],
  "needs": [],
  "next": "tester",
  "recommendation": "continue",
  "risk": "low"
}
trailing shell prompt
"#;

        let AgentResultParse::Found(parsed) = parse_agent_result_marker(transcript) else {
            panic!("expected parsed result");
        };

        assert_eq!(parsed.result.status, AgentResultStatus::Completed);
        assert_eq!(parsed.result.summary, "Implemented parser.");
        assert_eq!(
            parsed.result.changed_files,
            ["crates/agentmux-agent/src/result.rs"]
        );
        assert_eq!(
            parsed.result.messages[0].kind,
            OutgoingMessageKind::TestResult
        );
        assert_eq!(parsed.result.messages[0].priority, OutgoingPriority::Normal);
        assert_eq!(
            parsed.result.context_updates[0].confidence,
            default_confidence()
        );
        assert_eq!(parsed.result.next.as_deref(), Some("tester"));
    }

    #[test]
    fn reports_not_found_when_marker_is_absent() {
        assert_eq!(
            parse_agent_result_marker("still working"),
            AgentResultParse::NotFound
        );
    }

    #[test]
    fn malformed_json_requires_status_probe_without_repair() {
        let parsed = parse_agent_result_marker(
            r#"AGENTMUX_RESULT: {"status":"completed","summary":"missing close""#,
        );

        let AgentResultParse::NeedsStatusProbe(probe) = parsed else {
            panic!("expected status probe request");
        };

        assert_eq!(probe.marker_offset, 0);
        // An unterminated object is reported as an incomplete object by the
        // brace-balanced extractor; either phrasing signals "malformed, no
        // repair attempted".
        assert!(
            probe.reason.contains("JSON is invalid")
                || probe.reason.contains("without a complete JSON object"),
            "unexpected reason: {}",
            probe.reason
        );
    }

    #[test]
    fn schema_shape_errors_require_status_probe() {
        let parsed = parse_agent_result_marker(
            r#"AGENTMUX_RESULT: {"status":"done","summary":"bad status"}"#,
        );

        let AgentResultParse::NeedsStatusProbe(probe) = parsed else {
            panic!("expected status probe request");
        };

        assert!(probe.reason.contains("unknown variant"));
    }

    #[test]
    fn empty_summary_requires_status_probe() {
        let parsed =
            parse_agent_result_marker(r#"AGENTMUX_RESULT: {"status":"completed","summary":"   "}"#);

        let AgentResultParse::NeedsStatusProbe(probe) = parsed else {
            panic!("expected status probe request");
        };

        assert_eq!(probe.reason, "summary must not be empty");
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let parsed = parse_agent_result_marker(
            r#"AGENTMUX_RESULT: {"status":"completed","summary":"ok","extra":true}"#,
        );

        assert!(matches!(parsed, AgentResultParse::NeedsStatusProbe(_)));
    }

    /// Screenshot reproduction: a long Japanese `body` is wrapped by the agy TUI
    /// at the pane width, inserting a raw newline + continuation indent *inside*
    /// the JSON string literal. The parser must unwrap it and recover the exact
    /// string the agent emitted, with no stray whitespace at the wrap point.
    #[test]
    fn wrapped_string_literal_with_raw_newline_is_rejoined() {
        // The body, as the agent intended it (single logical line).
        let expected_body =
            "実装が完了しました。テスターはフォーカスされたペインで回帰テストを実行してください。";

        // The same text as it appears on the PTY stream: the terminal wrapped
        // the string mid-character-run, inserting "\n" + 6 spaces of indent.
        let wrapped = "AGENTMUX_RESULT:\n{\n  \"status\": \"completed\",\n  \"summary\": \"handoff\",\n  \"messages\": [\n    {\n      \"to\": \"role:tester\",\n      \"kind\": \"TestResult\",\n      \"body\": \"実装が完了しました。テスターはフォーカ\n      スされたペインで回帰テストを実行してください。\",\n      \"priority\": \"normal\"\n    }\n  ],\n  \"next\": null\n}\n";

        let AgentResultParse::Found(parsed) = parse_agent_result_marker(wrapped) else {
            panic!("wrapped result must parse");
        };
        assert_eq!(parsed.result.messages.len(), 1);
        assert_eq!(parsed.result.messages[0].body, expected_body);
    }

    /// ANSI color/escape sequences interleaved with the marker line and the JSON
    /// (as a TUI emits them) must be stripped before parsing.
    #[test]
    fn ansi_escapes_around_marker_and_json_are_stripped() {
        let with_ansi = concat!(
            "\x1b[1m\x1b[32mAGENTMUX_RESULT:\x1b[0m\n",
            "\x1b[2m{\x1b[0m\n",
            "  \"status\": \"completed\",\n",
            "  \"summary\": \"\x1b[33mcolored summary\x1b[0m\",\n",
            "  \"next\": null\n",
            "}\n",
            "\x1b]0;some title\x07",
        );

        let AgentResultParse::Found(parsed) = parse_agent_result_marker(with_ansi) else {
            panic!("ANSI-laden result must parse");
        };
        assert_eq!(parsed.result.status, AgentResultStatus::Completed);
        assert_eq!(parsed.result.summary, "colored summary");
    }

    /// Wrapping that lands *outside* a string literal produces raw newlines that
    /// are already legal JSON inter-token whitespace; parsing must still succeed.
    #[test]
    fn wrap_outside_string_is_legal_whitespace() {
        let wrapped = "AGENTMUX_RESULT:\n{\n  \"status\":\n  \"completed\",\n  \"summary\":\n  \"ok\",\n  \"next\": null\n}\n";

        let AgentResultParse::Found(parsed) = parse_agent_result_marker(wrapped) else {
            panic!("whitespace-wrapped result must parse");
        };
        assert_eq!(parsed.result.summary, "ok");
    }

    /// A genuinely malformed object (syntax error, not a transport artifact)
    /// must still surface a StatusProbe — we do not repair semantics.
    #[test]
    fn genuinely_broken_json_still_requires_status_probe() {
        // Missing comma between fields is a structural error, not wrapping.
        let broken =
            "AGENTMUX_RESULT:\n{\n  \"status\": \"completed\"\n  \"summary\": \"oops\"\n}\n";

        let AgentResultParse::NeedsStatusProbe(probe) = parse_agent_result_marker(broken) else {
            panic!("broken JSON must require a status probe");
        };
        assert!(probe.reason.contains("JSON is invalid"));
    }

    #[test]
    fn strip_ansi_escapes_preserves_plain_text_and_multibyte() {
        assert_eq!(strip_ansi_escapes("plain"), "plain");
        assert_eq!(
            strip_ansi_escapes("\x1b[31mあか\x1b[0m"),
            "あか",
            "CSI sequences removed, multibyte preserved"
        );
        assert_eq!(
            strip_ansi_escapes("a\x1b]0;title\x07b"),
            "ab",
            "OSC (BEL terminated) removed"
        );
        assert_eq!(
            strip_ansi_escapes("a\x1b]0;title\x1b\\b"),
            "ab",
            "OSC (ST terminated) removed"
        );
    }

    #[test]
    fn strip_ansi_escapes_drops_c0_controls_but_keeps_whitespace() {
        // Backspace (\x08) and other C0 controls are dropped; \n \r \t kept.
        assert_eq!(strip_ansi_escapes("a\x08b"), "ab");
        assert_eq!(strip_ansi_escapes("a\x00\x0b\x0c\x1f\x7fb"), "ab");
        assert_eq!(strip_ansi_escapes("a\nb\tc\rd"), "a\nb\tc\rd");
    }

    #[test]
    fn strip_ansi_expands_rep_sequence() {
        // REP: repeat the previous glyph Pn times. " \x1b[5b" -> 6 spaces total.
        assert_eq!(strip_ansi_escapes(" \x1b[5b"), "      ");
        // Default count (no param) repeats once.
        assert_eq!(strip_ansi_escapes("x\x1b[b"), "xx");
        // REP with no preceding glyph is a no-op.
        assert_eq!(strip_ansi_escapes("\x1b[3b"), "");
        // REP after a multibyte glyph repeats that glyph.
        assert_eq!(strip_ansi_escapes("あ\x1b[2b"), "あああ");
    }

    #[test]
    fn wrapped_string_with_trailing_padding_is_rejoined_cleanly() {
        // "agent:agy-  \n  l4h9sw." -> trailing padding + newline + indent
        // all removed -> "agent:agy-l4h9sw."
        let wrapped = "AGENTMUX_RESULT:\n{\n  \"status\": \"completed\",\n  \"summary\": \"to agent:agy-  \n  l4h9sw.\",\n  \"next\": null\n}\n";

        let AgentResultParse::Found(parsed) = parse_agent_result_marker(wrapped) else {
            panic!("padded-wrap result must parse");
        };
        assert_eq!(parsed.result.summary, "to agent:agy-l4h9sw.");
    }

    #[test]
    fn backspace_after_open_brace_does_not_break_parse() {
        // Reproduces the captured "{ ... \n\x08 ... \"status\"" shape that
        // produced `key must be a string at line 2 column 1`.
        let raw = "AGENTMUX_RESULT:\x1b[27X\x1b[m\n\x1b[16D\x1b[38;5;252m{\x1b[42X\x1b[m\n\x08\x1b[38;5;252m  \"status\": \"completed\",\x1b[m\n  \"summary\": \"ok\",\n  \"next\": null\n}\n";

        let AgentResultParse::Found(parsed) = parse_agent_result_marker(raw) else {
            panic!("control-char-laden result must parse");
        };
        assert_eq!(parsed.result.status, AgentResultStatus::Completed);
        assert_eq!(parsed.result.summary, "ok");
    }

    /// Real captured agy drip render (final repaint frame). The parser must
    /// recover the message cleanly: correct target/kind and a body free of
    /// control characters and stray padding whitespace.
    #[test]
    fn parses_real_agy_drip_fixture() {
        const FIXTURE: &str = include_str!("../fixtures/agy_drip_render.txt");

        let AgentResultParse::Found(parsed) = parse_agent_result_marker(FIXTURE) else {
            panic!("real agy drip fixture must parse to Found");
        };
        assert_eq!(parsed.result.messages.len(), 1);
        let msg = &parsed.result.messages[0];
        assert_eq!(msg.to, "agent:agy-l4h9sw");
        assert_eq!(msg.kind, OutgoingMessageKind::Question);
        // Body must contain no raw control characters.
        assert!(
            !msg.body.chars().any(|c| c.is_control()),
            "body must not contain control characters: {:?}",
            msg.body
        );
        // No double spaces from padding artifacts.
        assert!(
            !msg.body.contains("  "),
            "body must not contain stray padding runs: {:?}",
            msg.body
        );
        assert!(msg.body.contains("お疲れ様です"));
        assert!(msg.body.contains("初期コミットを予定しています"));
    }
}
