//! Project initialisation and AGENTMUX_RESULT protocol installation.

use std::path::{Path, PathBuf};

use agentmux_core::{AgentmuxError, error::Result};

pub(crate) const DEFAULT_PROJECT_CONFIG: &str =
    include_str!("../../../docs/config/agentmux.config.example.toml");
pub(crate) const RESULT_PROTOCOL_MARKER_START: &str = "<!-- agentmux-result-protocol:start -->";
pub(crate) const RESULT_PROTOCOL_MARKER_END: &str = "<!-- agentmux-result-protocol:end -->";
pub(crate) const MESSAGE_CONFIRM_AFTER_TURNS_ENV: &str = "AGENTMUX_MESSAGE_CONFIRM_AFTER_TURNS";
pub(crate) const DEFAULT_MESSAGE_CONFIRM_AFTER_TURNS: usize = 3;
pub(crate) const RESULT_PROTOCOL_BLOCK_TEMPLATE: &str = r#"<!-- agentmux-result-protocol:start -->
## agentmux result protocol

`AGENTMUX_RESULT` is a **turn-status notification**, not the message channel. End each completed turn by emitting it so the orchestrator can track your state — `status`, `summary`, `changed_files`, `needs`, and `next`. Keep `messages: []` in normal operation:

```text
AGENTMUX_RESULT:
{
  "status": "completed",
  "summary": "<short summary>",
  "changed_files": [],
  "messages": [],
  "context_updates": [],
  "needs": [],
  "next": null
}
```

To send work to another coding agent, the **first choice is the CLI**:

```text
agentmux message send --to <target> --kind <Kind> --priority <low|normal|high|urgent> "<body>"
```

Prefer the CLI over `messages[]` because the CLI does not travel through the PTY display channel: its payload is never corrupted by terminal line-wrapping or control/escape characters (unlike text rendered into a pane), it is auto-injected into an idle target, and — because the command reads your `AGENTMUX_AGENT_ID` environment variable — the message is correctly attributed to your agent session (`from: agent`), so the recipient can reply to exactly you with `agent:<your-session-name>`.

`messages[]` inside `AGENTMUX_RESULT` remains a **fallback for agents that have no shell access** and therefore cannot run `agentmux message send`. When you do use it, the whole `AGENTMUX_RESULT` block is not stored as a message; only entries inside `messages[]` are routed.

Always send messages with inject delivery. Both `agentmux message send` and `messages[]` entries default to `delivery_mode: inject_when_idle`: the daemon automatically injects the rendered prompt into the target session's PTY as soon as that session is idle. Keep that default — never pass `delivery_mode: inbox_only`, because inbox-only messages are not injected and the target agent will never see them. After sending, do not wait for or request a manual injection; delivery happens automatically.

When an agentmux bus message is injected into your session, always reply. Prefer the CLI: `agentmux message send --to agent:<sender-session-name> --kind <Kind> "<reply>"` (use the requested `reply_to` / target context if the injected prompt provides one). If you have no shell access, reply through `messages[]` in the next `AGENTMUX_RESULT` instead. Do not ask the user for confirmation before sending normal message replies or progress updates. If no substantive answer is ready yet, send a brief `StatusProbe`, `Question`, or `Handoff` that says what is pending instead of staying silent.

Avoid unbounded agent-to-agent loops. This applies to both `agentmux message send` and `messages[]`. Only if the same pair of agents has exchanged messages for {message_confirm_after_turns} or more back-and-forth turns on the same topic, the next reply must ask for human confirmation before continuing. Use `kind: "Question"` and include the current conclusion, the remaining uncertainty, and the exact decision needed from the user. Configure this threshold before installing the protocol with `AGENTMUX_MESSAGE_CONFIRM_AFTER_TURNS`; the default is 3.

Allowed message kind values (for both `--kind` and `messages[].kind`) are: `TaskAssignment`, `Question`, `Finding`, `PatchProposal`, `ReviewComment`, `TestResult`, `FailureReport`, `Decision`, `Handoff`, `ApprovalRequest`, `ContextUpdate`, `StatusProbe`. Do not invent other kinds such as `Greeting`; an invalid kind is rejected and the message is not stored.

Multi-party meetings: when three or more sessions must discuss one topic, use a meeting thread instead of pairwise messages. Open with `agentmux meeting open "<topic>" --participants <name1>,<name2>,<name3>` (session names via `agentmux sessions`); the daemon injects the agenda into every participant. Post with `agentmux message send --thread <thread_id> --kind <Kind> "<body>"` — it is delivered to all participants except you, so never re-send your own statement. Each participant has a per-thread message limit (default 5, set with `--max-turns`); when you hit the limit, summarize your conclusion and ask the human for a decision with `kind: Question` outside the thread. Inspect with `agentmux message history --thread <thread_id>` and `agentmux meeting list`; close with `agentmux meeting close <thread_id>`.

Agent sessions register a stable role and a unique session name at startup. Use role targets (`role:tester`, `role:implementer`, `role:reviewer`) when every session with that role should receive the message. Use `agent:<session-name>` or a session id when the message is for exactly one session. Check available sessions with `Ctrl-g s` in the TUI or `agentmux sessions`.

Each live session receives its own identity through environment variables: `AGENTMUX_AGENT_NAME`, `AGENTMUX_AGENT_ROLE`, and `AGENTMUX_AGENT_ID`. Use `AGENTMUX_AGENT_NAME` when another session needs to reply to exactly this session.

Common TUI workflows:

- Start multiple panes with `agentmux start "agy,codex"` or include message history with `agentmux start "agy,messages,codex"`.
- Inside the TUI, `Ctrl-g %` and `Ctrl-g "` open the new pane picker. Choose `Claude Code`, `Codex`, `Antigravity`, or `Conversation List`.
- `Conversation List` opens the message history as a normal pane. `Ctrl-g m` opens the same history as a temporary overlay.
- `Ctrl-g s` shows running sessions with their names, roles, and process IDs.
- `Ctrl-g x` closes the focused local pane or stops the focused agent pane.

Message inspection commands:

- `agentmux message list` shows stored bus messages newest first.
- `agentmux sessions` shows live agent sessions and their stable names/roles.
- `agentmux start "messages"` opens only the message history pane.

Manual injection is a fallback for messages that were not auto-delivered yet (for example the target was busy or had not spawned when the message was created). In that case use `agentmux message inject <message_id>` only when the message target resolves to exactly one session. If the target can resolve to multiple sessions (for example `role:tester`) or you need a specific pane, use `agentmux agent inject <message_id> <agent_id>` after checking `agentmux sessions`; this explicitly selects the session that receives the PTY input.

Injection is asynchronous: the daemon records the message first, then waits briefly before writing the rendered message into the target PTY. If the TUI list updates before the text appears in the agent pane, wait a few seconds before retrying.

Two-session exchange example (CLI-first):

```text
impl finishes work, then notifies the tester via the CLI:
agentmux message send --to role:tester --kind TestResult --priority normal \
  "Please verify copy mode: Ctrl-g [, drag inside the focused pane, release to copy, Esc/q to exit."

impl then emits its turn-status notification:
AGENTMUX_RESULT:
{
  "status": "completed",
  "summary": "Implemented copy mode.",
  "changed_files": ["crates/agentmux-cli/src/main.rs"],
  "messages": [],
  "context_updates": [],
  "needs": [],
  "next": null
}
```

```text
tester replies to the sender (attributed automatically from AGENTMUX_AGENT_ID):
agentmux message send --to agent:codex-a1b2c3 --kind Finding \
  "Focused-pane drag selection worked. OSC52 clipboard support depends on the host terminal."

then emits its own turn-status notification:
AGENTMUX_RESULT:
{
  "status": "completed",
  "summary": "Copy mode verification completed.",
  "changed_files": [],
  "messages": [],
  "context_updates": [],
  "needs": [],
  "next": null
}
```

A shell-less agent would instead place those `Finding` / `TestResult` entries inside `messages[]` of its `AGENTMUX_RESULT`.

Check delivery with `Ctrl-g m` in the TUI or `agentmux message list`.
<!-- agentmux-result-protocol:end -->
"#;

pub(crate) fn init_project(path: &Path) -> Result<PathBuf> {
    let project_dir = path.canonicalize().map_err(|error| {
        AgentmuxError::UserError(format!(
            "failed to resolve project path '{}': {error}",
            path.display()
        ))
    })?;
    let agentmux_dir = project_dir.join(".agentmux");
    std::fs::create_dir_all(&agentmux_dir).map_err(|error| {
        AgentmuxError::StoreError(format!(
            "failed to create '{}': {error}",
            agentmux_dir.display()
        ))
    })?;

    let config_path = agentmux_dir.join("config.toml");
    if !config_path.exists() {
        std::fs::write(&config_path, DEFAULT_PROJECT_CONFIG).map_err(|error| {
            AgentmuxError::StoreError(format!(
                "failed to write '{}': {error}",
                config_path.display()
            ))
        })?;
    }

    Ok(project_dir)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResultProtocolInstallReport {
    pub(crate) path: PathBuf,
    pub(crate) status: ResultProtocolInstallStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResultProtocolInstallStatus {
    Added,
    AlreadyPresent,
    Updated,
    Missing,
}

pub(crate) fn install_result_protocol(
    path: &Path,
    global: bool,
) -> Result<Vec<ResultProtocolInstallReport>> {
    let targets = if global {
        global_result_protocol_targets()?
    } else {
        local_result_protocol_targets(path)?
    };

    targets
        .into_iter()
        .map(|target| install_result_protocol_to_file(&target, global))
        .collect()
}

pub(crate) fn local_result_protocol_targets(path: &Path) -> Result<Vec<PathBuf>> {
    let dir = path.canonicalize().map_err(|error| {
        AgentmuxError::UserError(format!(
            "failed to resolve instruction directory '{}': {error}",
            path.display()
        ))
    })?;

    Ok(["AGENTS.md", "CLAUDE.md", "GEMINI.md"]
        .into_iter()
        .map(|name| dir.join(name))
        .collect())
}

pub(crate) fn global_result_protocol_targets() -> Result<Vec<PathBuf>> {
    let Some(home) = std::env::var_os("HOME") else {
        return Err(AgentmuxError::UserError(
            "HOME is not set; cannot install global result protocol".to_string(),
        ));
    };
    let home = PathBuf::from(home);
    Ok(vec![
        home.join(".codex/AGENTS.md"),
        home.join(".claude/CLAUDE.md"),
        home.join(".gemini/GEMINI.md"),
    ])
}

pub(crate) fn result_protocol_block() -> String {
    result_protocol_block_with_threshold(message_confirm_after_turns_from_env())
}

pub(crate) fn result_protocol_block_with_threshold(threshold: usize) -> String {
    RESULT_PROTOCOL_BLOCK_TEMPLATE.replace(
        "{message_confirm_after_turns}",
        &threshold.max(1).to_string(),
    )
}

pub(crate) fn message_confirm_after_turns_from_env() -> usize {
    message_confirm_after_turns(
        std::env::var(MESSAGE_CONFIRM_AFTER_TURNS_ENV)
            .ok()
            .as_deref(),
    )
}

pub(crate) fn message_confirm_after_turns(raw: Option<&str>) -> usize {
    raw.and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MESSAGE_CONFIRM_AFTER_TURNS)
}

pub(crate) fn install_result_protocol_to_file(
    path: &Path,
    create_missing: bool,
) -> Result<ResultProtocolInstallReport> {
    let block = result_protocol_block();
    if !path.exists() {
        if !create_missing {
            return Ok(ResultProtocolInstallReport {
                path: path.to_path_buf(),
                status: ResultProtocolInstallStatus::Missing,
            });
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                AgentmuxError::StoreError(format!(
                    "failed to create '{}': {error}",
                    parent.display()
                ))
            })?;
        }
        std::fs::write(path, format!("{block}\n")).map_err(|error| {
            AgentmuxError::StoreError(format!("failed to write '{}': {error}", path.display()))
        })?;
        return Ok(ResultProtocolInstallReport {
            path: path.to_path_buf(),
            status: ResultProtocolInstallStatus::Added,
        });
    }

    let contents = std::fs::read_to_string(path).map_err(|error| {
        AgentmuxError::StoreError(format!("failed to read '{}': {error}", path.display()))
    })?;
    if let Some(next) = replace_result_protocol_block(&contents, &block) {
        if next == contents {
            return Ok(ResultProtocolInstallReport {
                path: path.to_path_buf(),
                status: ResultProtocolInstallStatus::AlreadyPresent,
            });
        }
        std::fs::write(path, next).map_err(|error| {
            AgentmuxError::StoreError(format!("failed to write '{}': {error}", path.display()))
        })?;
        return Ok(ResultProtocolInstallReport {
            path: path.to_path_buf(),
            status: ResultProtocolInstallStatus::Updated,
        });
    }

    let mut next = contents;
    if !next.ends_with('\n') {
        next.push('\n');
    }
    next.push('\n');
    next.push_str(&block);
    next.push('\n');
    std::fs::write(path, next).map_err(|error| {
        AgentmuxError::StoreError(format!("failed to write '{}': {error}", path.display()))
    })?;

    Ok(ResultProtocolInstallReport {
        path: path.to_path_buf(),
        status: ResultProtocolInstallStatus::Added,
    })
}

pub(crate) fn replace_result_protocol_block(contents: &str, block: &str) -> Option<String> {
    let start = contents.find(RESULT_PROTOCOL_MARKER_START)?;
    let after_start = start + RESULT_PROTOCOL_MARKER_START.len();
    let mut end = contents[after_start..]
        .find(RESULT_PROTOCOL_MARKER_END)
        .map(|relative| after_start + relative + RESULT_PROTOCOL_MARKER_END.len())
        .unwrap_or(contents.len());
    if contents[end..].starts_with('\n') {
        end += 1;
    }

    let mut next = String::with_capacity(contents.len() - (end - start) + block.len() + 2);
    next.push_str(&contents[..start]);
    next.push_str(block);
    next.push_str(&contents[end..]);
    Some(next)
}

pub(crate) fn print_result_protocol_report(report: &[ResultProtocolInstallReport]) {
    for entry in report {
        let status = match entry.status {
            ResultProtocolInstallStatus::Added => "added",
            ResultProtocolInstallStatus::AlreadyPresent => "already-present",
            ResultProtocolInstallStatus::Updated => "updated",
            ResultProtocolInstallStatus::Missing => "missing",
        };
        println!("{status}: {}", entry.path.display());
    }
}
