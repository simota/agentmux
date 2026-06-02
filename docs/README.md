# agentmux 仕様・設計ドキュメント一式

作成日: 2026-06-02
言語: 日本語
対象: Rust製 CLI/TUI-first マルチエージェント・コーディングコックピット

## 前提

`agentmux` は Claude Code / Codex などの対話TUI型コーディングエージェントを複数のPTY上で起動し、tmux風のpane表示、セッション維持、自動キー送信、メッセージング、コンテキスト共有、worktree分離、承認ゲートを提供するソフトウェアとして設計する。

非対話 `exec` は補助ジョブとして扱う。v0.1の主制御経路はあくまで対話TUIである。

## 読み順

1. `docs/00_executive_summary.md`
2. `docs/01_product_requirements.md`
3. `docs/02_system_architecture.md`
4. `docs/04_tui_pty_terminal_design.md`
5. `docs/05_agent_adapter_design.md`
6. `docs/06_message_bus_context_broker.md`
7. `docs/07_orchestrator_workflows.md`
8. `docs/09_security_policy_approval.md`
9. `docs/12_implementation_roadmap.md`

## 同梱物

- `docs/`: 仕様書・設計書本文
- `adrs/`: Architecture Decision Records
- `schemas/`: JSON Schema
- `sql/`: SQLite schema案
- `config/`: 設定ファイル例
- `diagrams/`: Mermaid図

## 注意

本書は実装前の詳細仕様ドラフトであり、Claude Code / Codex CLI の画面文言や承認UIは将来変更される可能性がある。そのため、画面スクレイピングだけに依存せず、explicit marker、hooks、sidecar files、process/file-system signalsを併用する設計にしている。
