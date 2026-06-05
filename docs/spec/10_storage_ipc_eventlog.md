# 10. Storage / IPC / Event Log設計書

## 1. 目的

agentmuxはdaemonがruntime stateを保持しつつ、metadata、message、context、approval、artifact index、event logを永続化する。本書はSQLite schema、JSONL event log、IPC protocolの設計を定義する。

## 2. 永続化方針

### 2.1 SQLiteに保存するもの

- project
- task
- agent session metadata
- pane layout metadata
- message
- context item
- artifact metadata
- worktree metadata
- approval request
- provider config snapshot

### 2.2 JSONL event logに保存するもの

- 状態遷移
- 自動入力
- message delivery
- context共有
- approval decision
- agent result
- policy denial
- errors

### 2.3 ファイルとして保存するもの

- artifact body
- diff patch
- test log
- transcript tail
- mailbox file
- context bundle

## 3. SQLite schema

DDLは `sql/schema.sql` を参照。

## 4. Migration

`schema_migrations` tableを用意する。

```sql
CREATE TABLE schema_migrations (
  version INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  applied_at TEXT NOT NULL
);
```

v0.1では単純なembedded migrationsでよい。

## 5. Event Log

### 5.1 形式

1行1JSON。

```json
{"ts":"2026-06-02T10:00:00Z","type":"task.created","task_id":"task_...","payload":{}}
```

### 5.2 共通field

```json
{
  "id": "evt_...",
  "ts": "2026-06-02T10:00:00Z",
  "type": "message.injected",
  "project_id": "proj_...",
  "task_id": "task_...",
  "agent_id": "agent_...",
  "payload": {}
}
```

### 5.3 必須event types

- daemon.started
- client.attached
- task.created
- task.status_changed
- agent.spawned
- agent.status_signal
- agent.status_changed
- pty.output_chunk
- terminal.snapshot_saved
- input_script.created
- input_script.injected
- message.created
- message.delivered
- context.created
- mailbox.written
- artifact.created
- approval.created
- approval.decided
- worktree.created
- worktree.diff_captured
- worktree.adopt_requested
- worktree.test_completed
- policy.denied
- error

## 6. IPC Protocol

v0.1はJSON Lines over Unix domain socket。

### 6.1 Request

```json
{
  "id": "req_001",
  "type": "task.run",
  "payload": {
    "project_path": ".",
    "body": "refresh token bugを修正",
    "team": "claude-codex"
  }
}
```

### 6.2 Response

```json
{
  "id": "req_001",
  "ok": true,
  "payload": {
    "task_id": "task_123"
  }
}
```

### 6.3 Event

```json
{
  "type": "screen.diff",
  "payload": {
    "pane_id": "pane_...",
    "regions": []
  }
}
```

## 7. IPC Commands

### 7.1 Session/attach

- daemon.status
- client.attach
- client.detach
- event.subscribe
- layout.get
- layout.set

#### event.subscribe

client→daemonのread-onlyコマンド。送信後、daemonはfilter条件に一致するeventだけをそのclientへ配信する。

```json
{
  "id": "req_event_subscribe",
  "type": "event.subscribe",
  "payload": {
    "task_id": "task_001",
    "roles": ["implementer"],
    "kinds": ["agent.status_changed"]
  }
}
```

`EventSubscribeFilter`の各フィールドは省略可能（デフォルト: 空）。複数フィールドはAND、同一フィールド内の複数値はORで評価する。

- `task_id`: 特定taskのeventだけを受け取る。
- `roles`: 指定したroleを持つagentのeventだけを受け取る。eventのpayloadにroleがない場合はagent registryから解決する。
- `kinds`: 特定のevent typeだけを受け取る。

`event.subscribe`を送っていないclientへは従来どおり全eventを配信する（挙動不変）。

本コマンドはprotocol version 2以上でのみ有効（`EVENT_SUBSCRIBE_PROTOCOL_VERSION`参照）。

### 7.2 Task

- task.run
- task.pause
- task.resume
- task.cancel
- task.status

### 7.3 Agent

- agent.spawn
- agent.stop
- agent.interrupt
- agent.resize
- agent.focus
- agent.send_input_script
- agent.snapshot

### 7.4 Message

- message.create
- message.inject
- message.list
- message.show

### 7.5 Context

- context.create
- context.search
- context.attach
- context.inject
- context.export

### 7.6 Worktree（Arena）

- worktree.adopt

`worktree.adopt` は対象 worktree の adoption approval を queue し、`approval_id` を返す。差分未 capture / test 未通過 / 既存 pending adoption が 1 件超の場合はエラーを返す。本コマンドは `ARENA_PROTOCOL_VERSION`（3）以上でのみ有効。

### 7.7 Approval

- approval.list
- approval.approve
- approval.reject

## 8. Screen diff配信

v0.1では以下のいずれかでよい。

1. pane全体snapshot配信
2. dirty region配信
3. terminal bufferをdaemonに保持しclientがpull

推奨初期実装:

- daemonはTerminalBufferを保持。
- client attach時にsnapshotを受け取る。
- 更新時はpane全体またはdirty linesを送る。

## 9. Store writer

SQLite writeは専用taskでserializeする。

理由:

- 複数componentからの書き込み競合を避ける。
- event logとの順序を保ちやすい。
- backpressureを制御しやすい。

## 10. Consistency

- event logをappendしてからSQLite state更新、またはその逆を統一する。
- v0.1では「SQLite stateが正、event logが監査用」とする。
- 将来、event sourcing寄りへ移行可能。

## 11. IPC versioning

各request/eventにprotocol versionを持たせるか、connection handshakeでversionを交渉する。

```json
{"type":"hello","payload":{"client_version":"0.1.0","protocol":"1"}}
```

handshakeのprotocol番号は**厳密等価**で判定し、mismatch時はgraceful closeする。

### 11.1 protocol version定数

| 定数 | 値 | 意味 |
|---|---|---|
| `PROTOCOL_VERSION` | 3 | 現在のprotocol番号。protocol shapeに変更があるたびにbumpする。 |
| `EVENT_SUBSCRIBE_PROTOCOL_VERSION` | 2 | `event.subscribe`コマンドをサポートする最初のprotocol version。clientはdaemonの`protocol_version`がこの値以上のときだけ`event.subscribe`を送る。 |
| `ARENA_PROTOCOL_VERSION` | 3 | `worktree.adopt`・arena run をサポートする最初のprotocol version。clientはdaemonの`protocol_version`がこの値以上のときだけarena系コマンドを送る。v2以前のdaemonへはdowngrade noticeを返す。 |

## 12. Error response

```json
{
  "id": "req_123",
  "ok": false,
  "error": {
    "code": "AGENT_NOT_FOUND",
    "message": "agent 'impl-codex' not found",
    "hint": "agentmux agent ls"
  }
}
```

## 13. File paths

推奨path:

```text
config: ~/.config/agentmux/config.toml
runtime socket: $XDG_RUNTIME_DIR/agentmux/agentmux.sock
project state: <project>/.agentmux/state.db
project events: <project>/.agentmux/events.jsonl
```

project-local stateを使うことで、task/context/artifactsをrepository単位で扱いやすくする。ただし`.agentmux`はgitignore必須。
