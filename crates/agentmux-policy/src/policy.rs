//! Policy engine.

use agentmux_core::{ApprovalKind, AutomationLevel, RiskLevel};
use serde::{Deserialize, Serialize};

/// The outcome of a policy evaluation for a proposed action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyDecision {
    /// Action is safe to proceed automatically.
    Allow,
    /// Action may proceed only when it matches configured allow rules.
    AllowIfMatchesRules,
    /// Action requires explicit human approval before proceeding.
    Ask,
    /// Action is permanently denied regardless of automation level.
    Deny,
}

/// Coarse command risk class from `docs/spec/09_security_policy_approval.md §6`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandSafety {
    Safeish,
    Caution,
    Dangerous,
}

/// Safety class for automated input from spec §8.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputSafety {
    SafePromptOnly,
    MayTriggerToolUse,
    MayModifyFiles,
    MayRunCommands,
    Dangerous,
}

/// Command classification with a human-readable reason suitable for approvals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandClassification {
    pub safety: CommandSafety,
    pub risk: RiskLevel,
    pub reason: String,
}

/// Per-action policy defaults from spec §5.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalPolicy {
    pub allow_prompt_injection: PolicyDecision,
    pub allow_read_only_commands: PolicyDecision,
    pub allow_workspace_write: PolicyDecision,
    pub allow_test_commands: PolicyDecision,
    pub allow_network: PolicyDecision,
    pub allow_git_commit: PolicyDecision,
    pub allow_git_push: PolicyDecision,
    pub allow_delete_files: PolicyDecision,
    pub allow_secret_access: PolicyDecision,
    pub allow_full_access: PolicyDecision,
}

impl Default for ApprovalPolicy {
    fn default() -> Self {
        Self {
            allow_prompt_injection: PolicyDecision::Allow,
            allow_read_only_commands: PolicyDecision::Ask,
            allow_workspace_write: PolicyDecision::Ask,
            allow_test_commands: PolicyDecision::Ask,
            allow_network: PolicyDecision::Deny,
            allow_git_commit: PolicyDecision::Ask,
            allow_git_push: PolicyDecision::Deny,
            allow_delete_files: PolicyDecision::Ask,
            allow_secret_access: PolicyDecision::Deny,
            allow_full_access: PolicyDecision::Deny,
        }
    }
}

/// Stateless policy evaluator.
///
/// Instantiate once per daemon; methods are `&self` (no mutation needed).
pub struct PolicyEngine {
    /// Global automation level.
    pub automation_level: AutomationLevel,
    pub policy: ApprovalPolicy,
    test_command_prefixes: Vec<String>,
    /// Glob patterns for paths that must never be written automatically
    /// (spec §9 `protected_paths`: `.git/**`, `.env`, `*secret*`, `.agentmux/state.db`).
    protected_paths: Vec<String>,
}

fn default_test_command_prefixes() -> Vec<String> {
    vec![
        "cargo test".to_string(),
        "npm test".to_string(),
        "pytest".to_string(),
    ]
}

impl PolicyEngine {
    pub fn new(automation_level: AutomationLevel) -> Self {
        Self {
            automation_level,
            policy: ApprovalPolicy::default(),
            test_command_prefixes: default_test_command_prefixes(),
            protected_paths: Vec::new(),
        }
    }

    pub fn with_policy(automation_level: AutomationLevel, policy: ApprovalPolicy) -> Self {
        Self {
            automation_level,
            policy,
            test_command_prefixes: default_test_command_prefixes(),
            protected_paths: Vec::new(),
        }
    }

    pub fn with_test_command_prefixes<I, S>(mut self, prefixes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.test_command_prefixes = prefixes.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_protected_paths<I, S>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.protected_paths = paths.into_iter().map(Into::into).collect();
        self
    }

    /// Returns `true` when `path` matches any configured protected glob pattern.
    ///
    /// Path separators are normalized to `/`, a leading `./` is stripped, and any
    /// `..` traversal component forces a match (treated as protected) so a write
    /// cannot escape the workspace to reach a protected file by relative climbing.
    pub fn is_protected_path(&self, path: &str) -> bool {
        let normalized = normalize_path(path);
        if normalized.split('/').any(|segment| segment == "..") {
            return true;
        }
        self.protected_paths
            .iter()
            .any(|pattern| glob_match(&normalize_path(pattern), &normalized))
    }

    /// Evaluate a proposed file write. Protected paths are denied outright;
    /// everything else delegates to the configured `FileWrite` policy.
    pub fn evaluate_file_write(&self, path: &str) -> PolicyDecision {
        if self.is_protected_path(path) {
            return PolicyDecision::Deny;
        }
        self.evaluate(&ApprovalKind::FileWrite)
    }

    /// Evaluate whether `kind` of approval action is permitted.
    ///
    /// Default policy (from spec §9):
    /// - `NetworkAccess`, `GitPush`, `SecretAccess`, `FullAccess` → `Deny`
    /// - everything else → `Ask`, except prompt injection at `AutoPrompt` or above.
    pub fn evaluate(&self, kind: &ApprovalKind) -> PolicyDecision {
        use ApprovalKind::*;

        if self.automation_level == AutomationLevel::ObserveOnly {
            return match kind {
                NetworkAccess | GitPush | SecretAccess | FullAccess => PolicyDecision::Deny,
                _ => PolicyDecision::Ask,
            };
        }

        let configured = match kind {
            AutoInput => &self.policy.allow_prompt_injection,
            FileWrite => &self.policy.allow_workspace_write,
            ShellCommand => &self.policy.allow_read_only_commands,
            GitCommit => &self.policy.allow_git_commit,
            GitPush => &self.policy.allow_git_push,
            NetworkAccess => &self.policy.allow_network,
            SecretAccess => &self.policy.allow_secret_access,
            FullAccess => &self.policy.allow_full_access,
            ExternalTool => &self.policy.allow_network,
        };

        match (configured, &self.automation_level, kind) {
            (PolicyDecision::Deny, _, _) => PolicyDecision::Deny,
            (_, _, NetworkAccess | GitPush | SecretAccess | FullAccess) => configured.clone(),
            (PolicyDecision::Allow, _, _) => PolicyDecision::Allow,
            (
                PolicyDecision::Ask | PolicyDecision::AllowIfMatchesRules,
                AutomationLevel::AutoFullAccess,
                _,
            ) => PolicyDecision::Allow,
            (
                _,
                AutomationLevel::AutoPrompt | AutomationLevel::AutoPromptAndApproveSafe,
                AutoInput,
            ) => PolicyDecision::Allow,
            (_, AutomationLevel::AutoWorkspaceWrite, AutoInput | FileWrite) => {
                PolicyDecision::Allow
            }
            (decision, _, _) => decision.clone(),
        }
    }

    pub fn evaluate_command(&self, command: &str) -> PolicyDecision {
        let classification = self.classify_command(command);
        match classification.safety {
            CommandSafety::Dangerous => self.evaluate_dangerous_command(command),
            CommandSafety::Safeish => match self.automation_level {
                AutomationLevel::AutoPromptAndApproveSafe
                | AutomationLevel::AutoWorkspaceWrite
                | AutomationLevel::AutoFullAccess => self.policy.allow_read_only_commands.clone(),
                _ => PolicyDecision::Ask,
            },
            CommandSafety::Caution => {
                if self.is_configured_test_command(command) {
                    match self.automation_level {
                        AutomationLevel::AutoPromptAndApproveSafe
                        | AutomationLevel::AutoWorkspaceWrite
                        | AutomationLevel::AutoFullAccess => {
                            self.policy.allow_test_commands.clone()
                        }
                        _ => PolicyDecision::Ask,
                    }
                } else {
                    PolicyDecision::Ask
                }
            }
        }
    }

    fn evaluate_dangerous_command(&self, command: &str) -> PolicyDecision {
        let lower = command.to_ascii_lowercase();

        // Use the same `contains`-based detection as `dangerous_reason` so that
        // chained or prefixed forms (`cd repo && git push`, `GIT_SSH=x git push`)
        // route to the correct `ApprovalKind` instead of falling through to `Ask`.
        if lower.contains("git push") {
            return self.evaluate(&ApprovalKind::GitPush);
        }

        if lower.contains("git commit") {
            return self.evaluate(&ApprovalKind::GitCommit);
        }

        if lower.contains("curl ") || lower.contains("wget ") {
            return self.evaluate(&ApprovalKind::NetworkAccess);
        }

        if contains_secret_path(&lower) || lower.contains("begin private key") {
            return self.evaluate(&ApprovalKind::SecretAccess);
        }

        PolicyDecision::Ask
    }

    pub fn evaluate_input(&self, safety: &InputSafety) -> PolicyDecision {
        match safety {
            InputSafety::SafePromptOnly => self.evaluate(&ApprovalKind::AutoInput),
            InputSafety::MayTriggerToolUse => PolicyDecision::Ask,
            InputSafety::MayModifyFiles | InputSafety::MayRunCommands => {
                match self.automation_level {
                    AutomationLevel::AutoWorkspaceWrite | AutomationLevel::AutoFullAccess => {
                        PolicyDecision::Ask
                    }
                    _ => PolicyDecision::Ask,
                }
            }
            InputSafety::Dangerous => PolicyDecision::Ask,
        }
    }

    pub fn classify_command(&self, command: &str) -> CommandClassification {
        let normalized = normalize_command(command);

        if normalized.is_empty() {
            return CommandClassification {
                safety: CommandSafety::Caution,
                risk: RiskLevel::Medium,
                reason: "empty command cannot be classified as safe".to_string(),
            };
        }

        if let Some(reason) = dangerous_reason(&normalized) {
            return CommandClassification {
                safety: CommandSafety::Dangerous,
                risk: RiskLevel::High,
                reason,
            };
        }

        if is_safeish_read_only(&normalized) {
            return CommandClassification {
                safety: CommandSafety::Safeish,
                risk: RiskLevel::Low,
                reason: "read-only command".to_string(),
            };
        }

        if self.is_configured_test_command(&normalized) || is_caution_command(&normalized) {
            return CommandClassification {
                safety: CommandSafety::Caution,
                risk: RiskLevel::Medium,
                reason: "command may execute tools or modify workspace state".to_string(),
            };
        }

        CommandClassification {
            safety: CommandSafety::Caution,
            risk: RiskLevel::Medium,
            reason: "unrecognized command requires approval".to_string(),
        }
    }

    fn is_configured_test_command(&self, command: &str) -> bool {
        let normalized = normalize_command(command);
        self.test_command_prefixes
            .iter()
            .any(|prefix| command_matches_prefix(&normalized, &normalize_command(prefix)))
    }
}

fn normalize_command(command: &str) -> String {
    command.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Normalize a path for matching: convert `\` separators to `/` and strip a
/// single leading `./`. `..` components are preserved so callers can detect
/// traversal attempts.
fn normalize_path(path: &str) -> String {
    let unified = path.replace('\\', "/");
    unified.strip_prefix("./").unwrap_or(&unified).to_string()
}

/// Minimal glob matcher over `/`-separated paths supporting:
/// - `**` — matches any number of segments (including zero),
/// - `*` — matches any run of characters within a single segment,
/// - exact characters otherwise.
///
/// Both `pattern` and `path` are expected to be `normalize_path`-ed already.
fn glob_match(pattern: &str, path: &str) -> bool {
    let pattern_segments: Vec<&str> = pattern.split('/').collect();
    let path_segments: Vec<&str> = path.split('/').collect();
    glob_match_segments(&pattern_segments, &path_segments)
}

fn glob_match_segments(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => {
            // `**` matches zero or more leading path segments.
            (0..=path.len()).any(|skip| glob_match_segments(rest, &path[skip..]))
        }
        Some((seg, rest)) => match path.split_first() {
            Some((&candidate, path_rest)) if segment_match(seg, candidate) => {
                glob_match_segments(rest, path_rest)
            }
            _ => false,
        },
    }
}

/// Match a single path segment against a pattern segment where `*` matches any
/// run of characters within that segment.
fn segment_match(pattern: &str, segment: &str) -> bool {
    let pattern_chars: Vec<char> = pattern.chars().collect();
    let segment_chars: Vec<char> = segment.chars().collect();
    segment_match_inner(&pattern_chars, &segment_chars)
}

fn segment_match_inner(pattern: &[char], segment: &[char]) -> bool {
    match pattern.split_first() {
        None => segment.is_empty(),
        Some(('*', rest)) => {
            (0..=segment.len()).any(|skip| segment_match_inner(rest, &segment[skip..]))
        }
        Some((&p, rest)) => match segment.split_first() {
            Some((&s, seg_rest)) if p == s => segment_match_inner(rest, seg_rest),
            _ => false,
        },
    }
}

fn command_matches_prefix(command: &str, prefix: &str) -> bool {
    command == prefix || command.starts_with(&format!("{prefix} "))
}

fn is_safeish_read_only(command: &str) -> bool {
    // A command can only be Safeish if it does not chain, redirect, or substitute
    // another command. `normalize_command` collapses whitespace but preserves these
    // tokens, so `ls && npm install` or `cat $(cat /etc/shadow)` must be rejected
    // before any read-only prefix is honored — otherwise the trailing command
    // bypasses its own Caution/Dangerous gate.
    if contains_command_chaining(command) {
        return false;
    }

    ["git status", "git diff", "ls", "pwd"]
        .iter()
        .any(|prefix| command_matches_prefix(command, prefix))
        || command_matches_prefix(command, "cat") && !contains_secret_path(command)
}

/// Detect shell metacharacters that chain, redirect, or substitute a second
/// command. Any of these means the command is not a single read-only invocation.
fn contains_command_chaining(command: &str) -> bool {
    command.contains("&&")
        || command.contains("||")
        || command.contains(';')
        || command.contains('|')
        || command.contains('`')
        || command.contains("$(")
        || command.contains('>')
        || command.contains('<')
}

fn is_caution_command(command: &str) -> bool {
    [
        "cargo test",
        "npm test",
        "pytest",
        "cargo fmt",
        "prettier --write",
        "git add",
        "npm install",
        "cargo add",
    ]
    .iter()
    .any(|prefix| command_matches_prefix(command, prefix))
}

/// Detect an `rm` invocation carrying both a recursive and a force flag,
/// regardless of flag order or spelling. The literal `"rm -rf"` substring
/// pattern misses `rm -fr`, `rm -f -r`, and `rm --force --recursive`, which
/// are equally destructive — a false negative here would let them through as
/// `Caution` instead of `Dangerous`.
fn is_recursive_forced_rm(command: &str) -> bool {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    if !tokens.contains(&"rm") {
        return false;
    }
    let mut recursive = false;
    let mut force = false;
    for token in tokens {
        match token {
            "--recursive" => recursive = true,
            "--force" => force = true,
            _ if token.starts_with('-') && !token.starts_with("--") => {
                for flag in token[1..].chars() {
                    match flag {
                        'r' | 'R' => recursive = true,
                        'f' => force = true,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    recursive && force
}

fn dangerous_reason(command: &str) -> Option<String> {
    let lower = command.to_ascii_lowercase();

    if is_recursive_forced_rm(&lower) {
        return Some("recursive forced deletion".to_string());
    }

    let patterns = [
        ("rm -rf", "recursive forced deletion"),
        ("git commit", "git commit requires approval"),
        ("chmod -r", "recursive permission change"),
        ("chown -r", "recursive ownership change"),
        ("curl | sh", "remote script execution"),
        ("curl ", "network command requires approval"),
        ("wget | sh", "remote script execution"),
        ("wget ", "network command requires approval"),
        ("git reset --hard", "destructive git reset"),
        ("git clean -fdx", "destructive git clean"),
        ("git push", "remote repository modification"),
        ("kubectl", "deployment or cluster command"),
        ("terraform apply", "infrastructure mutation"),
        ("psql ", "database command"),
        ("mysql ", "database command"),
    ];

    for (needle, reason) in patterns {
        if lower.contains(needle) {
            return Some(reason.to_string());
        }
    }

    if contains_secret_path(&lower) || lower.contains("begin private key") {
        return Some("secret access".to_string());
    }

    None
}

fn contains_secret_path(command: &str) -> bool {
    [
        ".env",
        "id_rsa",
        "id_ed25519",
        "private_key",
        "credentials",
        "secrets",
    ]
    .iter()
    .any(|needle| command.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_denies_network_push_secret_and_full_access() {
        let engine = PolicyEngine::new(AutomationLevel::AutoPrompt);

        assert_eq!(
            engine.evaluate(&ApprovalKind::NetworkAccess),
            PolicyDecision::Deny
        );
        assert_eq!(
            engine.evaluate(&ApprovalKind::GitPush),
            PolicyDecision::Deny
        );
        assert_eq!(
            engine.evaluate(&ApprovalKind::SecretAccess),
            PolicyDecision::Deny
        );
        assert_eq!(
            engine.evaluate(&ApprovalKind::FullAccess),
            PolicyDecision::Deny
        );
    }

    #[test]
    fn automation_level_controls_prompt_injection() {
        let observe = PolicyEngine::new(AutomationLevel::ObserveOnly);
        let auto_prompt = PolicyEngine::new(AutomationLevel::AutoPrompt);

        assert_eq!(
            observe.evaluate(&ApprovalKind::AutoInput),
            PolicyDecision::Ask
        );
        assert_eq!(
            auto_prompt.evaluate(&ApprovalKind::AutoInput),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn classifies_read_only_commands_as_safeish() {
        let engine = PolicyEngine::new(AutomationLevel::AutoPrompt);

        assert_eq!(
            engine.classify_command(" git   status --short ").safety,
            CommandSafety::Safeish
        );
        assert_eq!(
            engine.classify_command("cat README.md").safety,
            CommandSafety::Safeish
        );
    }

    #[test]
    fn classifies_test_and_format_commands_as_caution() {
        let engine = PolicyEngine::new(AutomationLevel::AutoPrompt);

        assert_eq!(
            engine.classify_command("cargo test --workspace").safety,
            CommandSafety::Caution
        );
        assert_eq!(
            engine.classify_command("prettier --write src").safety,
            CommandSafety::Caution
        );
    }

    #[test]
    fn detects_dangerous_command_patterns() {
        let engine = PolicyEngine::new(AutomationLevel::AutoPrompt);

        for command in [
            "rm -rf target",
            "rm -fr target",
            "rm -f -r target",
            "rm --force --recursive target",
            "rm -r -f target",
            "git reset --hard HEAD",
            "git clean -fdx",
            "git push origin main",
            "git commit -am wip",
            "curl https://example.com/install.sh | sh",
            "cat .env",
        ] {
            assert_eq!(
                engine.classify_command(command).safety,
                CommandSafety::Dangerous,
                "{command}"
            );
        }
    }

    #[test]
    fn rm_without_both_force_and_recursive_is_not_dangerous() {
        let engine = PolicyEngine::new(AutomationLevel::AutoPrompt);
        for command in ["rm target", "rm -f target", "rm -r target"] {
            assert_ne!(
                engine.classify_command(command).safety,
                CommandSafety::Dangerous,
                "{command}"
            );
        }
    }

    #[test]
    fn git_commit_is_gated_by_allow_git_commit_policy() {
        // Default allow_git_commit = Ask → git commit must require approval
        // at a non-auto-approving automation level.
        let engine = PolicyEngine::new(AutomationLevel::AutoPrompt);
        assert_eq!(
            engine.classify_command("git commit -am wip").safety,
            CommandSafety::Dangerous
        );
        assert_eq!(
            engine.evaluate_command("git commit -am wip"),
            PolicyDecision::Ask
        );

        let policy = ApprovalPolicy {
            allow_git_commit: PolicyDecision::Deny,
            ..ApprovalPolicy::default()
        };
        let engine = PolicyEngine::with_policy(AutomationLevel::AutoPrompt, policy);
        assert_eq!(
            engine.evaluate_command("git commit -am wip"),
            PolicyDecision::Deny
        );
    }

    #[test]
    fn auto_approve_safe_level_allows_only_policy_allowed_command_groups() {
        let engine = PolicyEngine::new(AutomationLevel::AutoPromptAndApproveSafe);

        assert_eq!(
            engine.evaluate_command("git diff --stat"),
            PolicyDecision::Ask
        );

        let policy = ApprovalPolicy {
            allow_read_only_commands: PolicyDecision::Allow,
            allow_test_commands: PolicyDecision::Allow,
            ..ApprovalPolicy::default()
        };
        let engine = PolicyEngine::with_policy(AutomationLevel::AutoPromptAndApproveSafe, policy);

        assert_eq!(
            engine.evaluate_command("git diff --stat"),
            PolicyDecision::Allow
        );
        assert_eq!(
            engine.evaluate_command("cargo test -p agentmux-policy"),
            PolicyDecision::Allow
        );
        assert_eq!(
            engine.evaluate_command("git push origin main"),
            PolicyDecision::Deny
        );
    }

    #[test]
    fn chained_or_prefixed_git_push_commit_route_to_correct_approval_kind() {
        // Default policy: git push = Deny, git commit = Ask. Chained/prefixed forms
        // are classified Dangerous and must route via the same `contains` logic as
        // `dangerous_reason`, not fall through to a generic `Ask`.
        let engine = PolicyEngine::new(AutomationLevel::AutoPrompt);

        assert_eq!(
            engine.evaluate_command("cd /tmp && git push"),
            PolicyDecision::Deny
        );
        assert_eq!(
            engine.evaluate_command("GIT_SSH=x git push"),
            PolicyDecision::Deny
        );
        assert_eq!(
            engine.evaluate_command("make build && git commit -m x"),
            PolicyDecision::Ask
        );
    }

    #[test]
    fn chained_safeish_prefix_is_downgraded_to_caution() {
        let engine = PolicyEngine::new(AutomationLevel::AutoPrompt);

        // A read-only prefix followed by a chained command must not be Safeish,
        // otherwise the second command bypasses its own gate.
        assert_eq!(
            engine.classify_command("ls && npm install").safety,
            CommandSafety::Caution
        );
        assert_eq!(
            engine
                .classify_command("git status && prettier --write .")
                .safety,
            CommandSafety::Caution
        );
    }

    #[test]
    fn plain_read_only_commands_remain_safeish() {
        let engine = PolicyEngine::new(AutomationLevel::AutoPrompt);

        for command in ["ls", "ls -la", "git diff", "git diff --stat", "pwd"] {
            assert_eq!(
                engine.classify_command(command).safety,
                CommandSafety::Safeish,
                "{command}"
            );
        }
    }

    #[test]
    fn cat_with_command_substitution_is_not_safeish() {
        let engine = PolicyEngine::new(AutomationLevel::AutoPrompt);

        // `$(...)` substitution can read a protected file even without a literal
        // secret keyword or redirect, so it must not be classified Safeish.
        assert_ne!(
            engine.classify_command("cat $(cat /etc/passwd)").safety,
            CommandSafety::Safeish
        );
    }

    #[test]
    fn protected_path_glob_matcher_covers_documented_patterns() {
        let engine = PolicyEngine::new(AutomationLevel::AutoPrompt).with_protected_paths([
            ".git/**",
            ".env",
            "*secret*",
            ".agentmux/state.db",
        ]);

        // Matches.
        assert!(engine.is_protected_path(".git/config"));
        assert!(engine.is_protected_path(".git/hooks/pre-commit"));
        assert!(engine.is_protected_path(".env"));
        assert!(engine.is_protected_path("./.env"));
        assert!(engine.is_protected_path("my-secret-file"));
        assert!(engine.is_protected_path("app.secrets.json"));
        assert!(engine.is_protected_path(".agentmux/state.db"));
        // Backslash separators are normalized.
        assert!(engine.is_protected_path(".git\\config"));
        // `..` traversal is always treated as protected.
        assert!(engine.is_protected_path("../outside/.config"));

        // Non-matches.
        assert!(!engine.is_protected_path("src/main.rs"));
        assert!(!engine.is_protected_path("README.md"));
        assert!(!engine.is_protected_path(".gitignore")); // `.git` segment, not `.git/**`
        assert!(!engine.is_protected_path(".agentmux/config.toml"));
    }

    #[test]
    fn empty_protected_paths_match_nothing() {
        let engine = PolicyEngine::new(AutomationLevel::AutoPrompt);
        assert!(!engine.is_protected_path(".env"));
        assert!(!engine.is_protected_path(".git/config"));
    }

    #[test]
    fn evaluate_file_write_denies_protected_and_delegates_otherwise() {
        let engine = PolicyEngine::new(AutomationLevel::AutoWorkspaceWrite)
            .with_protected_paths([".git/**", ".env", "*secret*", ".agentmux/state.db"]);

        assert_eq!(
            engine.evaluate_file_write(".git/config"),
            PolicyDecision::Deny
        );
        assert_eq!(engine.evaluate_file_write(".env"), PolicyDecision::Deny);

        // Non-protected path delegates to FileWrite policy; AutoWorkspaceWrite
        // auto-allows FileWrite.
        assert_eq!(
            engine.evaluate_file_write("src/main.rs"),
            PolicyDecision::Allow
        );

        // At a lower automation level, FileWrite defaults to Ask.
        let observe = PolicyEngine::new(AutomationLevel::AutoPrompt)
            .with_protected_paths([".env".to_string()]);
        assert_eq!(
            observe.evaluate_file_write("src/main.rs"),
            PolicyDecision::Ask
        );
        assert_eq!(observe.evaluate_file_write(".env"), PolicyDecision::Deny);
    }

    #[test]
    fn safe_prompt_only_follows_prompt_policy_and_risky_input_requires_approval() {
        let engine = PolicyEngine::new(AutomationLevel::AutoPrompt);

        assert_eq!(
            engine.evaluate_input(&InputSafety::SafePromptOnly),
            PolicyDecision::Allow
        );
        assert_eq!(
            engine.evaluate_input(&InputSafety::MayRunCommands),
            PolicyDecision::Ask
        );
        assert_eq!(
            engine.evaluate_input(&InputSafety::Dangerous),
            PolicyDecision::Ask
        );
    }
}
