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

Use `messages[]` to send work to another coding agent through the agentmux message bus.

```json
{
  "to": "role:tester",
  "kind": "TestResult",
  "body": "Run the focused regression tests.",
  "priority": "normal"
}
```
<!-- agentmux-result-protocol:end -->

