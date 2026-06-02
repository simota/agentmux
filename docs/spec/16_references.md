# 16. 参考資料

調査日: 2026-06-02

## Claude Code

- Claude Code Overview: https://docs.anthropic.com/en/docs/claude-code/overview
- Claude Code Interactive mode: https://docs.anthropic.com/en/docs/claude-code/interactive-mode
- Claude Code Hooks guide: https://docs.anthropic.com/en/docs/claude-code/hooks-guide
- Claude Code Hooks reference: https://docs.anthropic.com/en/docs/claude-code/hooks
- Claude Code Terminal configuration: https://docs.anthropic.com/en/docs/claude-code/terminal-config
- Claude Code Common workflows: https://docs.anthropic.com/en/docs/claude-code/common-workflows
- Claude Code CLI reference: https://docs.anthropic.com/en/docs/claude-code/cli-reference

## Codex CLI

- Codex CLI: https://developers.openai.com/codex/cli
- Codex CLI reference: https://developers.openai.com/codex/cli/reference
- Codex CLI features: https://developers.openai.com/codex/cli/features
- Codex slash commands: https://developers.openai.com/codex/cli/slash-commands
- Codex config basics: https://developers.openai.com/codex/config-basic
- Codex advanced config: https://developers.openai.com/codex/config-advanced
- Codex config reference: https://developers.openai.com/codex/config-reference
- Codex non-interactive mode: https://developers.openai.com/codex/noninteractive
- Codex subagents: https://developers.openai.com/codex/subagents

## Rust crates / 技術要素

- Tokio: https://docs.rs/tokio
- Ratatui: https://docs.rs/ratatui/latest/ratatui/
- Crossterm: https://docs.rs/crossterm/
- portable-pty: https://docs.rs/portable-pty
- vte: https://docs.rs/vte/
- vt100: https://docs.rs/vt100
- rusqlite: https://docs.rs/rusqlite/
- Serde: https://serde.rs/
- clap: https://docs.rs/clap

## 注意

Claude Code / Codex CLI は継続的に更新されるため、画面文言、slash command、hooks schema、設定項目は変更される可能性がある。本設計ではこの不安定性を前提に、画面解析への依存を低くし、adapter層で差分を吸収する。
