# 11. CLI / TUIユーザー仕様書

## 1. CLI設計方針

CLIはnon-interactive操作、daemon操作、task起動、message/context操作に使う。agentの主操作はTUI内で行う。

## 2. Top-level commands

```bash
agentmux --help
agentmux doctor
agentmux sessions
agentmux attach [task|session]
agentmux daemon start|stop|status
agentmux project init|open|status|install-result-protocol
agentmux task run|status|pause|resume|cancel|summary
agentmux agent ls|spawn|stop|send|inject|focus|interrupt
agentmux message list|history|show|send|inject
agentmux context add|list|show|search|attach|inject|export
agentmux worktree list|diff|test|promote|archive
agentmux approval list|approve|reject
agentmux layout save|load|list
```

## 3. 代表コマンド

### 3.1 project init

```bash
agentmux project init .
```

作成:

```text
.agentmux/
  config.toml
  .gitignore entry recommendation
```

### 3.1.1 project install-result-protocol

```bash
agentmux project install-result-protocol .
agentmux project install-result-protocol --global
```

ローカル実行では、指定ディレクトリに既存の `AGENTS.md`, `CLAUDE.md`, `GEMINI.md` がある場合だけ `agentmux result protocol` を追記する。既に同じmarkerがある場合は追記しない。

`--global` は以下のグローバル指示ファイルへ設定する。

```text
~/.codex/AGENTS.md
~/.claude/CLAUDE.md
~/.gemini/GEMINI.md
```

### 3.2 task run

```bash
agentmux task run "refresh token bugを修正し、テストも追加して" --team claude-codex
```

挙動:

- daemon未起動なら起動する。
- 既存のcoding agent sessionがなければprovider pickerを表示する。
- taskを作成する。
- TUIをattachする。
- team templateに従ってpane/agent/worktreeを作成する。

### 3.3 attach

```bash
agentmux attach task-123
```

既存task sessionへ再接続する。

### 3.4 sessions

```bash
agentmux sessions
```

起動中のinteractive sessionを一覧表示する。

### 3.5 send message

```bash
agentmux send impl-codex "testerの失敗ログを確認してください"
```

または:

```bash
agentmux message send --to role:reviewer --kind ReviewComment --context ctx_123 "このdiffをレビューしてください"
```

### 3.6 inject

```bash
agentmux message inject msg_123 --to impl-codex
```

delivery modeを無視して明示注入する場合はapprovalが必要になることがある。

### 3.7 message history

```bash
agentmux message history
agentmux message history --limit 20
agentmux message history --agent impl-codex
agentmux message history --task task_123 --status delivered
agentmux message history --kind handoff
```

`message list` はdaemon payloadをJSONで表示する。`message history` は履歴確認用に
`created_at`, `delivery_status`, `kind`, `from`, `to`, `message_id`, `body`
を人間向けの表で表示する。

## 4. TUI画面構成

標準layout:

```text
┌ planner: claude ───────────────┬ impl-codex: codex ─────────────┐
│ Claude Code TUI                │ Codex TUI                       │
│                                │                                 │
├ impl-claude: claude ───────────┼ reviewer: codex ───────────────┤
│ Claude Code TUI                │ Codex TUI                       │
│                                │                                 │
├ messages ──────────────────────┴ context / approvals ───────────┤
│ tester -> impl-codex: failing test                               │
│ ctx: auth-refresh-rule, test-log-17, diff-codex                  │
└──────────────────────────────────────────────────────────────────┘
```

## 5. Pane types

| Pane | 内容 |
|---|---|
| AgentTui | Claude/Codex TUI |
| Shell | test runner, git commands |
| MessageBus | message一覧 |
| ContextBoard | context item一覧 |
| ApprovalQueue | 承認待ち |
| WorktreeDiff | diff表示 |
| TaskTimeline | event timeline |
| AgentList | agent状態一覧 |

## 6. Keymap

prefix key初期値: `Ctrl-g`

### 6.1 基本操作

```text
Ctrl-g d        detach
Ctrl-g ?        help
Ctrl-g z        zoom current pane
Ctrl-g arrow    focus pane
Ctrl-g q        close TUI client without stopping sessions
Ctrl-g s        running session list
Ctrl-g a        agent list
Ctrl-g m        message bus
Ctrl-g c        context board
Ctrl-g A        approval queue
```

### 6.2 Pane操作

```text
Ctrl-g %        split vertical and choose coding agent
Ctrl-g "        split horizontal and choose coding agent
Ctrl-g x        close/stop current pane
Ctrl-g r        resize mode
Ctrl-g Space    rotate layout
```

`Ctrl-g %` / `Ctrl-g "` は空のshell paneを作らず、provider pickerを表示する。選択肢は `Claude Code`, `Codex`, `Antigravity`。Enterで選択したcoding agentを起動し、そのpaneへattachする。Esc/qで閉じる。

### 6.3 Agent操作

```text
Ctrl-g p        paste queued message to current agent
Ctrl-g i        inject selected message
Ctrl-g R        request AGENTMUX_RESULT/status
Ctrl-g C        attach context to current agent
Ctrl-g T        run tests for current worktree
Ctrl-g I        interrupt current agent
```

Running session list操作:

```text
Up/Down, j/k    move selection
Enter           focus selected session
Esc, q          close list
```

Message bus overlay:

```text
Ctrl-g m        open message history
Esc, q          close message history
```

表示列:

```text
created_at, delivery_status, kind, from, to, message_id, body
```

## 7. Command palette

```text
Ctrl-g :
```

入力例:

```text
send impl-codex testerのログを確認して
context add decision refresh token検証はservice層に寄せる
approval approve appr_123
worktree diff current
```

## 8. Status line

```text
task=task-123 running | focused=impl-codex AwaitingInput | auto=AutoPrompt | approvals=1 | msgs=3 | ctx=12
```

## 9. Agent pane border

active agent:

```text
impl-codex | role=implementer | status=AwaitingInput | wt=task-123-codex | msg=2
```

risk/approvalがある場合:

```text
impl-codex | AwaitingApproval | risk=medium | approval=appr_123
```

## 10. Message view

```text
Messages
[unread] reviewer -> impl-codex  ReviewComment  high
[queued] tester -> impl-codex    TestResult     normal
[done]   planner -> impl-claude  TaskAssignment normal
```

操作:

```text
Enter: show
I: inject
A: approve injection
C: show context
```

## 11. Context view

```text
Context Board
ctx_001 CodingRule     public API互換性を壊さない
ctx_002 Decision       refresh token検証はservice層
ctx_003 TestResult     impl-codex cargo test passed
ctx_004 ErrorLog       auth_refresh failure excerpt
```

操作:

```text
Enter: show
A: attach to selected message/agent
I: inject to focused agent
E: export
```

## 12. Approval view

```text
Approval Queue
appr_001 high  git push requested by integrator
appr_002 med   auto paste may trigger shell command
```

操作:

```text
a: approve
r: reject
o: open agent pane
d: details
```

## 13. UXルール

- 人間がtyping中のpaneには自動入力しない。
- 自動入力前にtoast/overlayを出す設定を持つ。
- Ctrl+Cはfocused paneにのみ送る。
- prefix keyはagent TUIへ流さない。
- raw mode解除失敗に備え、panic hookでterminalを復旧する。

## 14. doctor

```bash
agentmux doctor
```

確認項目:

- daemon socket
- config parse
- SQLite access
- `claude` command availability
- `codex` command availability
- PTY creation
- terminal raw mode
- git worktree support
- protected paths
- hooks setup可能性

## 15. エラーメッセージ例

```text
error: agent 'impl-codex' is not awaiting input

hint:
  agentmux agent status impl-codex
  agentmux message inject msg_123 --manual
```

```text
error: unsafe input requires approval

reason:
  message may trigger shell command execution

next:
  agentmux approval list
```
