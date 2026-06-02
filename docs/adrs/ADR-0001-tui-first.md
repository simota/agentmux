# ADR-0001: TUI-firstを採用する

## Status

Accepted

## Context

agentmuxはClaude Code / Codexなどのコーディングエージェントを協調させる。これらには非対話実行モードも存在するが、ユーザー要求として対話TUIが必須である。また、対話TUIは承認、diff確認、履歴、slash command、セッション継続、人間介入に優れる。

## Decision

v0.1では、Claude/CodexをPTY上の対話TUIとして起動し、agentmuxのpane内に表示する。非対話execは補助BackgroundJobとして扱い、主workflowにはしない。

## Consequences

### Positive

- ユーザーがagentの状態を常に視認できる。
- 人間が任意タイミングで直接介入できる。
- Claude/CodexそれぞれのTUI機能を活かせる。
- agentの承認UIを自然に利用できる。

### Negative

- Terminal emulation実装が必要。
- 画面解析は壊れやすい。
- automationのタイミング制御が難しい。

### Mitigation

- screen parsingだけに依存せず、explicit marker、hooks、sidecar file、process signalを併用する。
- 自動入力にはinput lockとpreconditionを必須にする。
