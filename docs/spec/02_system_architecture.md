# 02. システムアーキテクチャ設計書

## 1. アーキテクチャ概要

agentmuxはclient-server型で構成する。

```text
agentmux CLI/TUI client
  <-> local IPC
agentmux daemon
  <-> PTY
Claude Code / Codex / shell processes
```

daemonがagent process、PTY、terminal buffer、message、context、worktree、event logを所有する。clientはTUI表示とユーザー入力を担当する。clientが終了してもdaemonが残るため、sessionを維持できる。

## 2. コンポーネント図

詳細なMermaid図は `diagrams/architecture.mmd` を参照。

```text
+--------------------------------------------------+
| agentmux-client                                  |
| - Ratatui view                                   |
| - keymap                                         |
| - pane renderer                                  |
| - human input dispatcher                         |
+--------------------------+-----------------------+
                           |
                           | IPC: JSONL over Unix socket
                           v
+--------------------------------------------------+
| agentmux-daemon                                  |
|                                                  |
|  Session Manager                                 |
|  Pane Manager                                    |
|  PTY Supervisor                                  |
|  Terminal Engine                                 |
|  Input Automation Engine                         |
|  Agent State Detector                            |
|  Message Bus                                     |
|  Context Broker                                  |
|  Worktree Manager                                |
|  Approval Engine                                 |
|  Store / Event Log                               |
+--------------------------+-----------------------+
                           |
                           | PTY master/slave
                           v
+--------------------------------------------------+
| Agent Processes                                  |
| - claude                                         |
| - codex                                          |
| - shell/test runners                             |
+--------------------------------------------------+
```

## 3. プロセス構成

### 3.1 agentmux daemon

責務:

- agent session lifecycle管理
- PTY作成とprocess spawn
- terminal output読み取り
- virtual terminal buffer更新
- attached clientへのscreen diff配信
- key inputのtarget paneへの転送
- 自動入力scriptの実行
- message queue / delivery管理
- context broker / mailbox file生成
- worktree作成・diff取得
- approval policy判定
- SQLite永続化
- JSONL event log追記

### 3.2 agentmux client

責務:

- daemonへ接続
- pane layoutを描画
- terminal cellを表示
- keymap処理
- human inputをdaemonへ送信
- internal view表示
- command palette表示

### 3.3 agent process

Claude Code、Codex、shellなど。agentmuxはprocess内部を直接変更せず、PTY input/output、hooks、sidecar filesを介して連携する。

## 4. Rust workspace構成案

```text
agentmux/
  crates/
    agentmux-cli/
    agentmux-daemon/
    agentmux-core/
    agentmux-ipc/
    agentmux-pty/
    agentmux-terminal/
    agentmux-tui/
    agentmux-agent/
    agentmux-message/
    agentmux-context/
    agentmux-worktree/
    agentmux-store/
    agentmux-policy/
```

### 4.1 agentmux-core

- domain IDs
- common error type
- time utilities
- enums
- event definitions

### 4.2 agentmux-ipc

- request/response/event protocol
- JSONL framing
- protocol versioning
- client session management

### 4.3 agentmux-pty

- PTY creation
- process spawn
- input write
- output read
- resize
- process termination

### 4.4 agentmux-terminal

- ANSI/VT parser integration
- screen grid
- alternate screen handling
- cursor state
- style attributes
- scrollback
- dirty region tracking

### 4.5 agentmux-tui

- ratatui layout
- pane rendering
- internal views
- keymap
- overlays

### 4.6 agentmux-agent

- AgentSession manager
- provider-specific adapters
- status detection
- result marker parser

### 4.7 agentmux-message

- typed message bus
- inbox
- delivery queue
- prompt renderer

### 4.8 agentmux-context

- context item CRUD
- context pack selection
- mailbox file writer
- redaction

### 4.9 agentmux-worktree

- git worktree wrapper
- branch naming
- diff artifact capture
- test target management

### 4.10 agentmux-policy

- automation level
- approval policy
- command classification
- safety guardrails

## 5. データフロー

### 5.1 人間入力

```text
keyboard
  -> client keymap
  -> focused pane input event
  -> daemon input router
  -> PTY writer
  -> agent process stdin
```

### 5.2 agent output

```text
agent stdout/stderr
  -> PTY reader
  -> terminal parser
  -> screen buffer
  -> state detector
  -> event bus
  -> attached clients
  -> TUI render
```

### 5.3 自動message injection

```text
AgentMessage queued
  -> delivery policy check
  -> target status detection
  -> prompt renderer
  -> input precondition check
  -> input lock acquire
  -> bracketed paste
  -> Enter
  -> event log
```

### 5.4 context共有

```text
ContextItem created
  -> optional redaction
  -> attach to message
  -> if short: inline prompt
  -> if long: mailbox file
  -> injected handoff prompt
```

## 6. IPC方針

v0.1では、Unix domain socket上のJSON Linesとする。

理由:

- 実装が簡単
- debugしやすい
- serdeで扱いやすい
- protocol evolutionが容易

将来、screen diff配信が重くなった場合はMessagePackやbinary protocolを検討する。

## 7. 状態管理

### 7.1 in-memory state

- running agent sessions
- active PTY handles
- terminal buffers
- pane layouts
- message delivery queues
- input locks
- short-lived state signals

### 7.2 persistent state

- projects
- tasks
- agent session metadata
- messages
- context items
- artifacts metadata
- approvals
- event log path

### 7.3 persistent but not fully restorable

- PTY screen snapshots
- scrollback tail
- latest agent output excerpts

### 7.4 not restorable in v0.1

- daemon停止後の生きたprocess復元
- agent TUI内部状態の完全復元

## 8. スレッド/タスクモデル

Tokio runtime上で以下を動かす。

- IPC listener task
- per-client connection task
- per-PTY read task
- terminal parse/update task
- orchestrator task
- message delivery task
- file watcher task
- store writer task

PTY crateがblocking readを返す場合は、dedicated blocking taskまたはthreadを使い、async channelでdaemonへ出力を渡す。

## 9. エラー分類

| 種別 | 例 | 対応 |
|---|---|---|
| UserError | unknown session, invalid target | CLIにhint表示 |
| ProviderError | claude/codex command not found | doctorで診断 |
| PtyError | PTY creation failed | session起動失敗として記録 |
| TerminalError | parse unsupported escape | warning + fallback |
| StoreError | SQLite write failed | retry / degraded mode |
| PolicyError | unsafe input rejected | approval queueへ送る |
| OrchestratorError | result marker invalid | human intervention要求 |

## 10. 将来拡張

- MCP / SDK adapter
- Codex app-server remote TUI連携
- web UI bridge
- multi-user collaboration
- PR creation integration
- remote daemon over SSH
- encrypted context bundle
