//! Prefix-tagged ID newtypes for all domain entities.
//!
//! IDs are based on ULID (Universally Unique Lexicographically Sortable
//! Identifiers), which gives us time-ordering and human-readable prefixes
//! for log readability (e.g. `proj_01J...`, `task_01J...`).
//!
//! Each newtype wraps a `ulid::Ulid` and serialises as `"<prefix><ulid>"`.
//!
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use ulid::Ulid;

macro_rules! define_id {
    ($name:ident, $prefix:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub struct $name(pub Ulid);

        impl $name {
            /// Generate a new random ID.
            pub fn new() -> Self {
                Self(Ulid::new())
            }

            /// Return the human-readable prefix string (e.g. `"proj_"`).
            pub const fn prefix() -> &'static str {
                $prefix
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}{}", $prefix, self.0)
            }
        }

        impl FromStr for $name {
            type Err = String;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                let raw = value.strip_prefix($prefix).ok_or_else(|| {
                    format!(
                        "invalid {} prefix: expected '{}'",
                        stringify!($name),
                        $prefix
                    )
                })?;
                Ulid::from_string(raw)
                    .map(Self)
                    .map_err(|error| format!("invalid {} ULID: {error}", stringify!($name)))
            }
        }
    };
}

define_id!(ProjectId, "proj_");
define_id!(TaskId, "task_");
define_id!(AgentSessionId, "agent_");
define_id!(PaneId, "pane_");
define_id!(MessageId, "msg_");
define_id!(ContextItemId, "ctx_");
define_id!(ArtifactId, "art_");
define_id!(ApprovalId, "appr_");
define_id!(WorktreeId, "wt_");
define_id!(PtyId, "pty_");
define_id!(TerminalBufferId, "tbuf_");
define_id!(ClientSessionId, "csess_");
define_id!(InboxId, "inbox_");
define_id!(ContextScopeId, "cscope_");
define_id!(InputScriptId, "iscript_");
define_id!(JobId, "job_");
define_id!(ClientId, "client_");
// Generic actor reference used in audit fields (human or orchestrator).
define_id!(ActorId, "actor_");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_from_str_round_trip_prefixed_id() {
        let id = TaskId::new();
        let encoded = id.to_string();

        assert!(encoded.starts_with(TaskId::prefix()));
        assert_eq!(encoded.parse::<TaskId>().expect("valid task id"), id);
    }

    #[test]
    fn from_str_rejects_wrong_prefix() {
        let id = TaskId::new();
        let encoded = id
            .to_string()
            .replace(TaskId::prefix(), ProjectId::prefix());

        let error = encoded.parse::<TaskId>().expect_err("wrong prefix");

        assert!(error.contains("invalid TaskId prefix"));
        assert!(error.contains(TaskId::prefix()));
    }

    #[test]
    fn from_str_rejects_invalid_ulid() {
        let error = "task_not-a-ulid"
            .parse::<TaskId>()
            .expect_err("invalid ulid");

        assert!(error.contains("invalid TaskId ULID"));
    }
}
