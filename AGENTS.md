# Repository Guidelines

## Project Structure & Module Organization

`agentmux` is a Rust workspace. Production code lives under `crates/`, with one crate per subsystem: `agentmux-cli` for the command line, `agentmux-daemon` for the background service, `agentmux-tui` and `agentmux-terminal` for terminal UI behavior, and shared libraries such as `agentmux-core`, `agentmux-ipc`, `agentmux-message`, `agentmux-store`, and `agentmux-policy`. Integration tests live beside their owning crate, for example `crates/agentmux-daemon/tests/task_run_e2e.rs`. Project specifications, ADRs, diagrams, schemas, and example config are in `docs/`.

## Build, Test, and Development Commands

- `make build`: build the full workspace in debug mode.
- `make test`: run all unit and integration tests with `cargo test --workspace`.
- `make lint`: run Clippy for all targets with warnings denied.
- `make fmt` / `make fmt-check`: format code or verify formatting.
- `make check`: run the local quality gate: format check, build, tests, and lint.
- `make run ARGS="doctor"`: run the CLI via `agentmux-cli`.
- `make daemon`: run `agentmux-daemon` in the foreground.
- `make doc`: build workspace API docs without dependencies.

## Coding Style & Naming Conventions

Use Rust 2024 edition and keep compatibility with the workspace MSRV in `Cargo.toml` (`rust-version = "1.85"`). Rely on `rustfmt`; do not hand-format around it. Use idiomatic Rust naming: `snake_case` for functions/modules, `PascalCase` for types and traits, and `SCREAMING_SNAKE_CASE` for constants. Keep shared dependency versions in the root `Cargo.toml` under `[workspace.dependencies]`, and reference internal crates through workspace paths where possible.

## Testing Guidelines

Place focused unit tests near the implementation and crate-level integration tests under `<crate>/tests/`. Name integration tests after the behavior or workflow they cover, such as `task_run_e2e.rs`. Before opening a PR, run `make check`; for narrow work, run the relevant `cargo test -p <crate>` first, then the full gate before final review.

## Commit & Pull Request Guidelines

Recent history uses concise Conventional Commit-style subjects, for example `fix(cli): ...`, `fix(daemon): ...`, `feat: ...`, and `chore: ...`. Keep subjects imperative, scoped when useful, and focused on the engineering change. Pull requests should include a short summary, linked issue or rationale, verification commands, and screenshots or terminal output when TUI/CLI behavior changes. Call out config, storage, IPC, or security-policy changes explicitly.

## Security & Configuration Tips

Use `docs/config/agentmux.config.example.toml` as the configuration reference. Do not commit local runtime state, credentials, private keys, or generated build artifacts. Avoid logging secrets, agent prompts containing sensitive data, or raw IPC payloads unless they are sanitized.

<!-- agentmux-result-protocol:start -->
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

Avoid unbounded agent-to-agent loops. This applies to both `agentmux message send` and `messages[]`. Only if the same pair of agents has exchanged messages for 3 or more back-and-forth turns on the same topic, the next reply must ask for human confirmation before continuing. Use `kind: "Question"` and include the current conclusion, the remaining uncertainty, and the exact decision needed from the user. Configure this threshold before installing the protocol with `AGENTMUX_MESSAGE_CONFIRM_AFTER_TURNS`; the default is 3.

Allowed message kind values (for both `--kind` and `messages[].kind`) are: `TaskAssignment`, `Question`, `Finding`, `PatchProposal`, `ReviewComment`, `TestResult`, `FailureReport`, `Decision`, `Handoff`, `ApprovalRequest`, `ContextUpdate`, `StatusProbe`. Do not invent other kinds such as `Greeting`; an invalid kind is rejected and the message is not stored.

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
