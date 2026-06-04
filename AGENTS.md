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

When working inside an agentmux-managed session, end each completed turn with:

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

Use `messages[]` to send work to another coding agent through the agentmux message bus. The whole `AGENTMUX_RESULT` block is not stored as a message; only entries inside `messages[]` are routed. Keep `messages: []` when no cross-agent message is needed.

Allowed `messages[].kind` values are: `TaskAssignment`, `Question`, `Finding`, `PatchProposal`, `ReviewComment`, `TestResult`, `FailureReport`, `Decision`, `Handoff`, `ApprovalRequest`, `ContextUpdate`, `StatusProbe`. Do not invent other kinds such as `Greeting`; an invalid kind prevents the result messages from being stored.

Agent sessions register a stable role and a unique session name at startup. Use role targets (`role:tester`, `role:implementer`, `role:reviewer`) when every session with that role should receive the message. Use `agent:<session-name>` or a session id when the message is for exactly one session. Check available sessions with `Ctrl-g s` in the TUI or `agentmux sessions`.

Each live session receives its own identity through environment variables: `AGENTMUX_AGENT_NAME`, `AGENTMUX_AGENT_ROLE`, and `AGENTMUX_AGENT_ID`. Use `AGENTMUX_AGENT_NAME` when another session needs to reply to exactly this session.

To inject an existing bus message into a live session, use `agentmux message inject <message_id>` only when the message target resolves to exactly one session. If the target can resolve to multiple sessions (for example `role:tester`) or you need a specific pane, use `agentmux agent inject <message_id> <agent_id>` after checking `agentmux sessions`; this explicitly selects the session that receives the PTY input.

```json
{
  "to": "role:tester",
  "kind": "TestResult",
  "body": "Run the focused regression tests.",
  "priority": "normal"
}
```

Two-session exchange example:

```text
impl finishes work:
AGENTMUX_RESULT:
{
  "status": "completed",
  "summary": "Implemented copy mode.",
  "changed_files": ["crates/agentmux-cli/src/main.rs"],
  "messages": [
    {
      "to": "role:tester",
      "kind": "TestResult",
      "body": "Please verify copy mode: Ctrl-g [, drag inside the focused pane, release to copy, Esc/q to exit.",
      "priority": "normal"
    }
  ],
  "context_updates": [],
  "needs": [],
  "next": null
}
```

```text
tester replies:
AGENTMUX_RESULT:
{
  "status": "completed",
  "summary": "Copy mode verification completed.",
  "changed_files": [],
  "messages": [
    {
      "to": "agent:codex-a1b2c3",
      "kind": "Finding",
      "body": "Focused-pane drag selection worked. OSC52 clipboard support depends on the host terminal.",
      "priority": "normal"
    }
  ],
  "context_updates": [],
  "needs": [],
  "next": null
}
```

Check delivery with `Ctrl-g m` in the TUI or `agentmux message list`.
<!-- agentmux-result-protocol:end -->
