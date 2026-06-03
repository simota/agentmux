//! Project configuration loading and validation.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::{AgentmuxError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentmuxConfig {
    pub project: ProjectConfig,
    pub daemon: DaemonConfig,
    pub tui: TuiConfig,
    pub terminal: TerminalConfig,
    pub automation: AutomationConfig,
    pub policy: PolicyConfig,
    pub context: ContextConfig,
    pub providers: ProvidersConfig,
    pub test: TestConfig,
    pub team: BTreeMap<String, TeamConfig>,
}

impl AgentmuxConfig {
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let contents = std::fs::read_to_string(path).map_err(|error| {
            AgentmuxError::UserError(format!(
                "failed to read config '{}': {error}",
                path.display()
            ))
        })?;
        Self::parse_str(&contents)
    }

    pub fn parse_str(contents: &str) -> Result<Self> {
        let parsed = ParsedConfig::parse(contents)?;
        let config = Self::from_parsed(&parsed)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        validate_non_empty("project.name", &self.project.name)?;
        validate_non_empty("project.root", &self.project.root)?;
        validate_non_empty("project.default_branch", &self.project.default_branch)?;
        validate_git_ref_like("project.default_branch", &self.project.default_branch)?;

        validate_non_empty("daemon.socket_path", &self.daemon.socket_path)?;
        validate_one_of(
            "daemon.log_level",
            &self.daemon.log_level,
            &["trace", "debug", "info", "warn", "error"],
        )?;

        validate_one_of("tui.prefix_key", &self.tui.prefix_key, &["Ctrl-g"])?;
        validate_positive("tui.scrollback_lines", self.tui.scrollback_lines as u64)?;

        validate_non_empty("terminal.term", &self.terminal.term)?;
        validate_positive("terminal.initial_cols", self.terminal.initial_cols as u64)?;
        validate_positive("terminal.initial_rows", self.terminal.initial_rows as u64)?;

        validate_one_of(
            "automation.level",
            &self.automation.level,
            &[
                "ObserveOnly",
                "AutoPrompt",
                "AutoPromptAndApproveSafe",
                "AutoWorkspaceWrite",
                "AutoFullAccess",
            ],
        )?;
        validate_positive(
            "automation.human_input_quiet_ms",
            self.automation.human_input_quiet_ms,
        )?;

        for (field, value) in [
            (
                "policy.allow_read_only_commands",
                &self.policy.allow_read_only_commands,
            ),
            (
                "policy.allow_workspace_write",
                &self.policy.allow_workspace_write,
            ),
            (
                "policy.allow_test_commands",
                &self.policy.allow_test_commands,
            ),
            ("policy.allow_network", &self.policy.allow_network),
            ("policy.allow_git_commit", &self.policy.allow_git_commit),
            ("policy.allow_git_push", &self.policy.allow_git_push),
            ("policy.allow_delete_files", &self.policy.allow_delete_files),
            (
                "policy.allow_secret_access",
                &self.policy.allow_secret_access,
            ),
            ("policy.allow_full_access", &self.policy.allow_full_access),
        ] {
            validate_policy_decision(field, value)?;
        }
        if self.policy.protected_paths.paths.is_empty() {
            return validation_error("policy.protected_paths.paths must contain at least one path");
        }
        for (index, path) in self.policy.protected_paths.paths.iter().enumerate() {
            validate_non_empty(&format!("policy.protected_paths.paths[{index}]"), path)?;
        }

        validate_positive(
            "context.max_inline_chars",
            self.context.max_inline_chars as u64,
        )?;
        validate_positive(
            "context.max_mailbox_file_bytes",
            self.context.max_mailbox_file_bytes as u64,
        )?;

        validate_provider("providers.claude", &self.providers.claude)?;
        validate_provider("providers.codex", &self.providers.codex)?;

        validate_non_empty("test.default_command", &self.test.default_command)?;
        self.validate_teams()
    }

    fn validate_teams(&self) -> Result<()> {
        if self.team.is_empty() {
            return validation_error("team must define at least one team template");
        }

        for (team_name, team) in &self.team {
            validate_non_empty("team name", team_name)?;
            if team.agents.is_empty() {
                return validation_error(&format!("team.{team_name}.agents must not be empty"));
            }

            for (index, agent) in team.agents.iter().enumerate() {
                let prefix = format!("team.{team_name}.agents[{index}]");
                validate_non_empty(&format!("{prefix}.name"), &agent.name)?;
                validate_non_empty(&format!("{prefix}.provider"), &agent.provider)?;
                validate_non_empty(&format!("{prefix}.role"), &agent.role)?;
                validate_one_of(
                    &format!("{prefix}.worktree"),
                    &agent.worktree,
                    &["main", "dedicated", "target", "readonly"],
                )?;
                validate_team_provider(&prefix, &agent.provider, &self.providers)?;
            }
        }

        Ok(())
    }

    fn from_parsed(parsed: &ParsedConfig) -> Result<Self> {
        Ok(Self {
            project: ProjectConfig {
                name: parsed.required_string("project.name")?,
                root: parsed.required_string("project.root")?,
                default_branch: parsed.required_string("project.default_branch")?,
            },
            daemon: DaemonConfig {
                socket_path: parsed.required_string("daemon.socket_path")?,
                log_level: parsed.required_string("daemon.log_level")?,
            },
            tui: TuiConfig {
                prefix_key: parsed.required_string("tui.prefix_key")?,
                status_line: parsed.required_bool("tui.status_line")?,
                show_agent_overlay: parsed.required_bool("tui.show_agent_overlay")?,
                scrollback_lines: parsed.required_usize("tui.scrollback_lines")?,
            },
            terminal: TerminalConfig {
                term: parsed.required_string("terminal.term")?,
                truecolor: parsed.required_bool("terminal.truecolor")?,
                initial_cols: parsed.required_u16("terminal.initial_cols")?,
                initial_rows: parsed.required_u16("terminal.initial_rows")?,
            },
            automation: AutomationConfig {
                level: parsed.required_string("automation.level")?,
                auto_inject_messages: parsed.required_bool("automation.auto_inject_messages")?,
                auto_approve_safe_prompts: parsed
                    .required_bool("automation.auto_approve_safe_prompts")?,
                auto_approve_file_edits: parsed
                    .required_bool("automation.auto_approve_file_edits")?,
                auto_approve_shell_commands: parsed
                    .required_bool("automation.auto_approve_shell_commands")?,
                auto_full_access: parsed.required_bool("automation.auto_full_access")?,
                human_input_quiet_ms: parsed.required_u64("automation.human_input_quiet_ms")?,
            },
            policy: PolicyConfig {
                allow_read_only_commands: parsed
                    .required_string("policy.allow_read_only_commands")?,
                allow_workspace_write: parsed.required_string("policy.allow_workspace_write")?,
                allow_test_commands: parsed.required_string("policy.allow_test_commands")?,
                allow_network: parsed.required_string("policy.allow_network")?,
                allow_git_commit: parsed.required_string("policy.allow_git_commit")?,
                allow_git_push: parsed.required_string("policy.allow_git_push")?,
                allow_delete_files: parsed.required_string("policy.allow_delete_files")?,
                allow_secret_access: parsed.required_string("policy.allow_secret_access")?,
                allow_full_access: parsed.required_string("policy.allow_full_access")?,
                protected_paths: ProtectedPathsConfig {
                    paths: parsed.required_array("policy.protected_paths.paths")?,
                },
            },
            context: ContextConfig {
                max_inline_chars: parsed.required_usize("context.max_inline_chars")?,
                max_mailbox_file_bytes: parsed.required_usize("context.max_mailbox_file_bytes")?,
                redact_secrets: parsed.required_bool("context.redact_secrets")?,
                summarize_before_share: parsed.required_bool("context.summarize_before_share")?,
            },
            providers: ProvidersConfig {
                claude: ProviderConfig {
                    enabled: parsed.required_bool("providers.claude.enabled")?,
                    command: parsed.required_string("providers.claude.command")?,
                    supports_hooks: parsed.optional_bool("providers.claude.supports_hooks")?,
                    supports_slash_commands: parsed
                        .optional_bool("providers.claude.supports_slash_commands")?,
                    startup_prompt: parsed.required_bool("providers.claude.startup_prompt")?,
                },
                codex: ProviderConfig {
                    enabled: parsed.required_bool("providers.codex.enabled")?,
                    command: parsed.required_string("providers.codex.command")?,
                    supports_hooks: parsed.optional_bool("providers.codex.supports_hooks")?,
                    supports_slash_commands: parsed
                        .optional_bool("providers.codex.supports_slash_commands")?,
                    startup_prompt: parsed.required_bool("providers.codex.startup_prompt")?,
                },
            },
            test: TestConfig {
                default_command: parsed.required_string("test.default_command")?,
            },
            team: parsed.teams()?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectConfig {
    pub name: String,
    pub root: String,
    pub default_branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonConfig {
    pub socket_path: String,
    pub log_level: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiConfig {
    pub prefix_key: String,
    pub status_line: bool,
    pub show_agent_overlay: bool,
    pub scrollback_lines: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalConfig {
    pub term: String,
    pub truecolor: bool,
    pub initial_cols: u16,
    pub initial_rows: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationConfig {
    pub level: String,
    pub auto_inject_messages: bool,
    pub auto_approve_safe_prompts: bool,
    pub auto_approve_file_edits: bool,
    pub auto_approve_shell_commands: bool,
    pub auto_full_access: bool,
    pub human_input_quiet_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyConfig {
    pub allow_read_only_commands: String,
    pub allow_workspace_write: String,
    pub allow_test_commands: String,
    pub allow_network: String,
    pub allow_git_commit: String,
    pub allow_git_push: String,
    pub allow_delete_files: String,
    pub allow_secret_access: String,
    pub allow_full_access: String,
    pub protected_paths: ProtectedPathsConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedPathsConfig {
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextConfig {
    pub max_inline_chars: usize,
    pub max_mailbox_file_bytes: usize,
    pub redact_secrets: bool,
    pub summarize_before_share: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvidersConfig {
    pub claude: ProviderConfig,
    pub codex: ProviderConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderConfig {
    pub enabled: bool,
    pub command: String,
    pub supports_hooks: bool,
    pub supports_slash_commands: bool,
    pub startup_prompt: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestConfig {
    pub default_command: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamConfig {
    pub agents: Vec<TeamAgentConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamAgentConfig {
    pub name: String,
    pub provider: String,
    pub role: String,
    pub worktree: String,
}

#[derive(Debug, Default)]
struct ParsedConfig {
    values: BTreeMap<String, String>,
    arrays: BTreeMap<String, Vec<String>>,
    agent_arrays: BTreeMap<String, Vec<BTreeMap<String, String>>>,
}

impl ParsedConfig {
    fn parse(contents: &str) -> Result<Self> {
        let mut parsed = Self::default();
        let mut section = String::new();
        let mut lines = contents.lines().enumerate().peekable();

        while let Some((line_index, line)) = lines.next() {
            let line_no = line_index + 1;
            let stripped = strip_comment(line);
            let trimmed = stripped.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(name) = table_name(trimmed) {
                section = name.to_string();
                continue;
            }

            let (key, value) = split_key_value(trimmed, line_no)?;
            let full_key = qualify_key(&section, key);
            if value == "[" {
                if full_key.ends_with(".agents") {
                    parsed
                        .agent_arrays
                        .insert(full_key, parse_agent_array(&mut lines, line_no)?);
                } else {
                    parsed
                        .arrays
                        .insert(full_key, parse_string_array(&mut lines, line_no)?);
                }
            } else if value.starts_with('[') {
                parsed
                    .arrays
                    .insert(full_key, parse_inline_array(value, line_no)?);
            } else {
                parsed.values.insert(full_key, value.to_string());
            }
        }

        Ok(parsed)
    }

    fn required_string(&self, key: &str) -> Result<String> {
        parse_string(
            key,
            self.values
                .get(key)
                .ok_or_else(|| config_parse_error(format!("missing required key {key}")))?,
        )
    }

    fn required_bool(&self, key: &str) -> Result<bool> {
        parse_bool(
            key,
            self.values
                .get(key)
                .ok_or_else(|| config_parse_error(format!("missing required key {key}")))?,
        )
    }

    fn optional_bool(&self, key: &str) -> Result<bool> {
        self.values
            .get(key)
            .map(|value| parse_bool(key, value))
            .unwrap_or(Ok(false))
    }

    fn required_usize(&self, key: &str) -> Result<usize> {
        parse_usize(
            key,
            self.values
                .get(key)
                .ok_or_else(|| config_parse_error(format!("missing required key {key}")))?,
        )
    }

    fn required_u16(&self, key: &str) -> Result<u16> {
        let value = self.required_usize(key)?;
        u16::try_from(value).map_err(|_| config_parse_error(format!("{key} is too large for u16")))
    }

    fn required_u64(&self, key: &str) -> Result<u64> {
        parse_u64(
            key,
            self.values
                .get(key)
                .ok_or_else(|| config_parse_error(format!("missing required key {key}")))?,
        )
    }

    fn required_array(&self, key: &str) -> Result<Vec<String>> {
        self.arrays
            .get(key)
            .cloned()
            .ok_or_else(|| config_parse_error(format!("missing required key {key}")))
    }

    fn teams(&self) -> Result<BTreeMap<String, TeamConfig>> {
        let mut teams = BTreeMap::new();
        for (key, agents) in &self.agent_arrays {
            let Some(team_name) = key
                .strip_prefix("team.")
                .and_then(|value| value.strip_suffix(".agents"))
            else {
                continue;
            };
            let mut parsed_agents = Vec::new();
            for agent in agents {
                parsed_agents.push(TeamAgentConfig {
                    name: required_agent_field(agent, key, "name")?,
                    provider: required_agent_field(agent, key, "provider")?,
                    role: required_agent_field(agent, key, "role")?,
                    worktree: required_agent_field(agent, key, "worktree")?,
                });
            }
            teams.insert(
                team_name.to_string(),
                TeamConfig {
                    agents: parsed_agents,
                },
            );
        }
        Ok(teams)
    }
}

fn table_name(line: &str) -> Option<&str> {
    line.strip_prefix('[')?.strip_suffix(']')
}

fn qualify_key(section: &str, key: &str) -> String {
    if section.is_empty() {
        key.to_string()
    } else {
        format!("{section}.{key}")
    }
}

fn split_key_value(line: &str, line_no: usize) -> Result<(&str, &str)> {
    let (key, value) = line
        .split_once('=')
        .ok_or_else(|| config_parse_error(format!("line {line_no}: expected key = value")))?;
    let key = key.trim();
    if key.is_empty() {
        return Err(config_parse_error(format!("line {line_no}: empty key")));
    }
    Ok((key, value.trim()))
}

fn strip_comment(line: &str) -> String {
    let mut in_string = false;
    let mut escaped = false;
    let mut output = String::new();

    for ch in line.chars() {
        if ch == '"' && !escaped {
            in_string = !in_string;
        }
        if ch == '#' && !in_string {
            break;
        }
        escaped = ch == '\\' && !escaped;
        if ch != '\\' {
            escaped = false;
        }
        output.push(ch);
    }

    output
}

fn parse_string_array<'a, I>(
    lines: &mut std::iter::Peekable<I>,
    start_line: usize,
) -> Result<Vec<String>>
where
    I: Iterator<Item = (usize, &'a str)>,
{
    let mut values = Vec::new();
    for (line_index, line) in lines.by_ref() {
        let line_no = line_index + 1;
        let trimmed = strip_comment(line)
            .trim()
            .trim_end_matches(',')
            .trim()
            .to_string();
        if trimmed == "]" {
            return Ok(values);
        }
        if trimmed.is_empty() {
            continue;
        }
        values.push(parse_string(&format!("line {line_no}"), &trimmed)?);
    }
    Err(config_parse_error(format!(
        "line {start_line}: unterminated array"
    )))
}

fn parse_inline_array(value: &str, line_no: usize) -> Result<Vec<String>> {
    let Some(inner) = value
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    else {
        return Err(config_parse_error(format!(
            "line {line_no}: inline arrays must end with ]"
        )));
    };
    if inner.trim().is_empty() {
        return Ok(Vec::new());
    }
    split_quoted_csv(inner)
        .into_iter()
        .map(|item| parse_string(&format!("line {line_no}"), item.trim()))
        .collect()
}

fn parse_agent_array<'a, I>(
    lines: &mut std::iter::Peekable<I>,
    start_line: usize,
) -> Result<Vec<BTreeMap<String, String>>>
where
    I: Iterator<Item = (usize, &'a str)>,
{
    let mut agents = Vec::new();
    for (line_index, line) in lines.by_ref() {
        let line_no = line_index + 1;
        let trimmed = strip_comment(line)
            .trim()
            .trim_end_matches(',')
            .trim()
            .to_string();
        if trimmed == "]" {
            return Ok(agents);
        }
        if trimmed.is_empty() {
            continue;
        }
        agents.push(parse_inline_object(&trimmed, line_no)?);
    }
    Err(config_parse_error(format!(
        "line {start_line}: unterminated agents array"
    )))
}

fn parse_inline_object(value: &str, line_no: usize) -> Result<BTreeMap<String, String>> {
    let Some(inner) = value
        .strip_prefix('{')
        .and_then(|value| value.strip_suffix('}'))
    else {
        return Err(config_parse_error(format!(
            "line {line_no}: expected inline table"
        )));
    };
    let mut object = BTreeMap::new();
    for pair in split_quoted_csv(inner) {
        let (key, value) = split_key_value(pair.trim(), line_no)?;
        object.insert(key.to_string(), parse_string(key, value)?);
    }
    Ok(object)
}

fn split_quoted_csv(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut in_string = false;
    let mut escaped = false;

    for (index, ch) in value.char_indices() {
        if ch == '"' && !escaped {
            in_string = !in_string;
        }
        if ch == ',' && !in_string {
            parts.push(&value[start..index]);
            start = index + 1;
        }
        escaped = ch == '\\' && !escaped;
        if ch != '\\' {
            escaped = false;
        }
    }
    parts.push(&value[start..]);
    parts
}

fn parse_string(field: &str, value: &str) -> Result<String> {
    let Some(inner) = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    else {
        return Err(config_parse_error(format!(
            "{field} must be a quoted string"
        )));
    };
    Ok(inner.replace("\\\"", "\"").replace("\\\\", "\\"))
}

fn parse_bool(field: &str, value: &str) -> Result<bool> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(config_parse_error(format!("{field} must be true or false"))),
    }
}

fn parse_usize(field: &str, value: &str) -> Result<usize> {
    value
        .parse::<usize>()
        .map_err(|_| config_parse_error(format!("{field} must be an unsigned integer")))
}

fn parse_u64(field: &str, value: &str) -> Result<u64> {
    value
        .parse::<u64>()
        .map_err(|_| config_parse_error(format!("{field} must be an unsigned integer")))
}

fn required_agent_field(
    agent: &BTreeMap<String, String>,
    array_key: &str,
    field: &str,
) -> Result<String> {
    agent
        .get(field)
        .cloned()
        .ok_or_else(|| config_parse_error(format!("{array_key} entry missing {field}")))
}

fn config_parse_error(message: String) -> AgentmuxError {
    AgentmuxError::UserError(format!("invalid agentmux config TOML: {message}"))
}

fn validate_non_empty(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return validation_error(&format!("{field} must not be empty"));
    }
    Ok(())
}

fn validate_positive(field: &str, value: u64) -> Result<()> {
    if value == 0 {
        return validation_error(&format!("{field} must be greater than 0"));
    }
    Ok(())
}

fn validate_one_of(field: &str, value: &str, allowed: &[&str]) -> Result<()> {
    if allowed.contains(&value) {
        return Ok(());
    }
    validation_error(&format!(
        "{field} must be one of {}; got {value:?}",
        allowed.join(", ")
    ))
}

fn validate_policy_decision(field: &str, value: &str) -> Result<()> {
    validate_one_of(
        field,
        value,
        &["Allow", "AllowIfMatchesRules", "Ask", "Deny"],
    )
}

fn validate_git_ref_like(field: &str, value: &str) -> Result<()> {
    if value.starts_with('-')
        || value.starts_with('/')
        || value.ends_with('/')
        || value.contains("..")
        || value.contains(' ')
        || value.contains('\\')
    {
        return validation_error(&format!("{field} is not a valid branch-like name"));
    }
    Ok(())
}

fn validate_provider(field: &str, provider: &ProviderConfig) -> Result<()> {
    if provider.enabled {
        validate_non_empty(&format!("{field}.command"), &provider.command)?;
    }
    Ok(())
}

fn validate_team_provider(
    field_prefix: &str,
    provider: &str,
    providers: &ProvidersConfig,
) -> Result<()> {
    match provider {
        "shell" => Ok(()),
        "claude" if providers.claude.enabled => Ok(()),
        "codex" if providers.codex.enabled => Ok(()),
        "claude" | "codex" => validation_error(&format!(
            "{field_prefix}.provider references disabled provider {provider:?}"
        )),
        _ => validation_error(&format!(
            "{field_prefix}.provider must be one of claude, codex, shell; got {provider:?}"
        )),
    }
}

fn validation_error<T>(message: &str) -> Result<T> {
    Err(AgentmuxError::UserError(format!(
        "invalid agentmux config: {message}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE_CONFIG: &str = include_str!("../../../docs/config/agentmux.config.example.toml");

    #[test]
    fn parses_docs_example_config() {
        let config = AgentmuxConfig::parse_str(EXAMPLE_CONFIG).unwrap();

        assert_eq!(config.project.name, "example");
        assert_eq!(config.tui.prefix_key, "Ctrl-g");
        assert_eq!(config.automation.level, "AutoPrompt");
        assert_eq!(config.policy.allow_network, "Deny");
        assert_eq!(config.team["claude-codex"].agents.len(), 5);
    }

    #[test]
    fn rejects_invalid_policy_value_with_field_name() {
        let invalid =
            EXAMPLE_CONFIG.replace("allow_network = \"Deny\"", "allow_network = \"Maybe\"");

        let error = AgentmuxConfig::parse_str(&invalid).unwrap_err();
        assert!(error.to_string().contains("policy.allow_network"));
        assert!(error.to_string().contains("Maybe"));
    }

    #[test]
    fn rejects_team_agent_with_disabled_provider() {
        let invalid = EXAMPLE_CONFIG.replace(
            "[providers.codex]\nenabled = true",
            "[providers.codex]\nenabled = false",
        );

        let error = AgentmuxConfig::parse_str(&invalid).unwrap_err();
        assert!(error.to_string().contains("disabled provider \"codex\""));
    }

    #[test]
    fn rejects_missing_required_section_with_field_name() {
        let invalid = EXAMPLE_CONFIG.replace("[terminal]", "[missing_terminal]");

        let error = AgentmuxConfig::parse_str(&invalid).unwrap_err();
        assert!(error.to_string().contains("invalid agentmux config TOML"));
        assert!(error.to_string().contains("terminal"));
    }

    #[test]
    fn loads_config_from_path() {
        let root =
            std::env::temp_dir().join(format!("agentmux-config-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("agentmux.config.toml");
        std::fs::write(&path, EXAMPLE_CONFIG).unwrap();

        let config = AgentmuxConfig::load_from_path(&path).unwrap();
        assert_eq!(config.test.default_command, "cargo test");

        std::fs::remove_dir_all(&root).unwrap();
    }
}
