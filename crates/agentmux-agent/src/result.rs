//! `AGENTMUX_RESULT` marker parsing.
//!
//! The parser intentionally does not repair malformed JSON. Invalid marker
//! payloads are surfaced as a StatusProbe requirement so the orchestrator can
//! ask the agent for a clean result.

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
    let Some(marker_offset) = buffer_tail.rfind(RESULT_MARKER) else {
        return AgentResultParse::NotFound;
    };

    let after_marker = &buffer_tail[marker_offset + RESULT_MARKER.len()..];
    let Some(json_tail) = marker_payload(after_marker) else {
        return AgentResultParse::NeedsStatusProbe(StatusProbeRequest {
            marker_offset,
            reason: "AGENTMUX_RESULT marker was found without a JSON object".to_string(),
        });
    };

    match serde_json::Deserializer::from_str(json_tail)
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
        assert!(probe.reason.contains("JSON is invalid"));
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
}
