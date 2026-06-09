# ADR-0008: Commands パネルと `agent.broadcast_input` で生入力の一括 PTY 注入を実現する

## Status

Accepted

## Context

複数のエージェントへ同じ指示やコマンドを一度に送りたい需要が存在する。

既存の手段は次の 2 種類に限られており、いずれもギャップがあった。

- **1 対 1 の message / inject**: 1 セッションずつしか対象にできない。
- **`agentmux message send --to broadcast`（バス経由）**: バスメッセージとして保存・配送されるため、対象 agent が idle になるまで注入されない遅延がある。また、バスメッセージは agent の prompt 入力フィールドへ「会話としてのテキスト」として届くため、シェルコマンドやキー操作列（例: `Enter` 単押し・`Ctrl-C`）を「同じ生の入力」として即時かつ直接送る手段にはならない。

tmux の synchronize-panes のように、**「同じキーストロークを全 pane に今すぐ打つ」** 操作に相当する手段が agentmux に存在しなかった。

## Decision

専用の **Commands パネル**（sentinel pane）、**`agent.broadcast_input` IPC コマンド**、および **`agentmux agent broadcast` CLI サブコマンド** を追加し、生入力の一括 PTY 注入を実現する。

### Commands パネル

- `agentmux start "commands"`（別名 `command` / `broadcast`）で開く。レイアウト記法の葉としても使える（例: `"agy | commands"`）。
- 内部的には sentinel pane `COMMANDS_PANE_ID` として扱われる（`messages` の `CONVERSATION_LIST_PANE_ID` と同様の実装パターン）。
- 新規 pane ピッカー（`Ctrl-g %` / `Ctrl-g "`）の選択肢に「Broadcast commands」を追加する（5 つ目）。

**パネル UI**:

- 上部: 送信履歴ログ（過去の送信テキスト・対象・`delivered N / skipped M` の結果）。
- 下部: 入力フィールドと現在の送信対象表示。

**操作キー（Commands pane focused 時）**:

| キー | 動作 |
|---|---|
| `Enter` | 入力内容を送信対象へ broadcast |
| `Tab` | 送信対象を巡回（`broadcast` → 実行中エージェントの distinct な `role:<role>` → …） |
| `Esc` | 入力フィールドをクリア |
| `Backspace` / 印字文字 | 入力フィールドを編集 |

prefix（`Ctrl-g`）コマンドはCommandsパネル上でも従来どおり機能する。

### 送信対象

`broadcast`（全エージェント）と `role:<role>`（既存の message target 文法）を受け付ける。既存の `MessageTarget::Broadcast` / `resolve_target` を再利用して対象を解決し、各 PTY へ順次注入する。

### `agent.broadcast_input` IPC

- IPC コマンド: `IpcCommand::AgentBroadcastInput`
- Request payload: `{ target, actions }`
- Response: `{ delivered, skipped }`
- PROTOCOL_VERSION: 据え置き（additive な新コマンド追加のため bump 不要。meeting 系コマンド追加と同じ判断）。

### `agentmux agent broadcast` CLI

```bash
agentmux agent broadcast "<text>"
agentmux agent broadcast --to <target> "<text>"   # 既定: broadcast
agentmux agent broadcast --no-enter "<text>"       # 末尾 Enter 抑止
```

### 安全性

既存の自動入力安全境界をすべて継承する。

- **human-typing skip**: `human_input_quiet` の quiet ガード中の pane には注入しない。見送った対象は `skipped` に含まれる。
- **監査ログ**: 全注入を JSONL event log に記録する。
- **利用者の明示操作起点**: 自律的な broadcast は行わない。利用者が Commands パネルで Enter を押すか CLI を明示実行した場合にのみ発火する。

## 代替案と却下理由

### (a) バスメッセージ broadcast のみ（既存手段）

`agentmux message send --to broadcast` は保存・idle 配信であり、対象 agent が busy な間は届かない遅延がある。また、TUI への「コマンド直接入力」としては機能しない（テキスト会話として届く）。

**却下**: 即時性が無く、生入力注入の用途を満たさない。

### (b) 各 pane を手動で 1 つずつ操作する

同じキー入力を複数 pane に個別に送る既存操作では、pane 数が増えると運用が煩雑になる。

**却下**: 運用効率が低く、規模スケールしない。

### (c) バスメッセージを活用した独立配送経路の新設

broadcast 専用の配送経路を新たに設計し、より細かい制御（確認ダイアログ等）を持たせる案。

**却下**: 設計コストが高い割に既存 `human_input_quiet` / `resolve_target` を再利用すれば同等の安全性が得られるため、シンプルな PTY 注入方式を採用する。

## Consequences

### Positive

- 複数エージェントへの一斉操作が1回の操作で完結し、tmux synchronize-panes 相当の操作感が実現できる。
- 既存の `MessageTarget::Broadcast` / `resolve_target` / `human_input_quiet` / JSONL event log を再利用するため、新規の安全装置を一から設計しなくてよい。
- IPC は additive 追加のため PROTOCOL_VERSION を変更せず、既存クライアント・daemon の互換性が保たれる。
- `role:<role>` 絞り込みにより、全体 broadcast ではなく特定 role のみへの一斉送信も容易にできる。

### Negative

- 破壊的なコマンド（例: `git reset --hard`, `rm -rf`）を全エージェントへ誤って一斉送信し得るリスクがある。
- 生入力注入のため、各エージェント TUI の内部状態（入力モード・プロンプト位置等）が agent ごとに異なる場合、同じ入力でも結果がばらつく可能性がある。

### Mitigation

- `human_input_quiet` ガードにより、人間が typing 中の pane への誤注入は常にスキップされる。
- 全注入を JSONL event log に記録することで操作の追跡・監査が可能。
- Commands パネルの UI（送信履歴ログと対象表示）により、利用者は送信前に対象と内容を把握できる。
- `--to role:<role>` や Tab による対象絞り込みにより、意図しない全体 broadcast を避けられる。
