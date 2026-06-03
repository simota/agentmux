//! `agentmux-core` — Domain IDs, common error type, enums, and time helpers.
//!
//! Every other crate in the workspace depends on this crate. Keep it lean:
//! no async, no I/O, no heavy deps. Only data types and error definitions.

pub mod config;
pub mod enums;
pub mod error;
pub mod ids;

pub use config::AgentmuxConfig;
pub use enums::*;
pub use error::AgentmuxError;
pub use ids::*;

/// Re-export `time::OffsetDateTime` as the canonical wall-clock timestamp type.
pub type DateTimeUtc = time::OffsetDateTime;

#[cfg(test)]
mod spec_tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::path::{Path, PathBuf};

    const OPEN_QUESTIONS: &str = include_str!("../../../docs/spec/15_open_questions.md");

    #[test]
    fn open_questions_are_resolved_for_v0_1() {
        let question_headings = OPEN_QUESTIONS
            .lines()
            .filter(|line| line.starts_with("## "))
            .count();
        let decisions = OPEN_QUESTIONS.matches("**v0.1 decision:**").count();

        assert_eq!(question_headings, 13);
        assert_eq!(decisions, question_headings);
    }

    #[test]
    fn shipped_sources_have_no_agent_todo_or_placeholder_macros() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("core crate lives under workspace/crates/agentmux-core");
        let scan_roots = [
            workspace.join("Cargo.toml"),
            workspace.join("rust-toolchain.toml"),
            workspace.join("crates"),
        ];

        let mut violations = Vec::new();
        for root in scan_roots {
            collect_placeholder_violations(&root, &mut violations);
        }

        assert!(
            violations.is_empty(),
            "unresolved implementation placeholders remain:\n{}",
            violations.join("\n")
        );
    }

    fn collect_placeholder_violations(path: &Path, violations: &mut Vec<String>) {
        if path.is_dir() {
            let mut entries = fs::read_dir(path)
                .unwrap_or_else(|error| panic!("failed to read '{}': {error}", path.display()))
                .map(|entry| {
                    entry
                        .unwrap_or_else(|error| {
                            panic!("failed to read entry under '{}': {error}", path.display())
                        })
                        .path()
                })
                .collect::<Vec<PathBuf>>();
            entries.sort();

            for entry in entries {
                collect_placeholder_violations(&entry, violations);
            }
            return;
        }

        if !matches!(
            path.extension().and_then(OsStr::to_str),
            Some("rs" | "toml")
        ) {
            return;
        }

        let contents = fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("failed to read '{}': {error}", path.display()));
        let markers = [
            ["#TODO", "(agent)"].concat(),
            ["todo", "!("].concat(),
            ["unimplemented", "!("].concat(),
        ];
        for (line_number, line) in contents.lines().enumerate() {
            if markers.iter().any(|marker| line.contains(marker)) {
                violations.push(format!("{}:{}", path.display(), line_number + 1));
            }
        }
    }
}
