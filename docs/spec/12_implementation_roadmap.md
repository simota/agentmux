# 12. 実装ロードマップ

## 1. 方針

v0.1では、対話TUIを複数paneで動かし、自動prompt注入とmessage/context連携を実現することに集中する。MCP/SDK、remote、PR自動化などは後回しにする。

## 2. Phase 0: PTY host PoC

### 目的

Claude Code / CodexをPTY上で起動し、agentmux pane内で実用的に操作できるか検証する。

### 作業

- `portable-pty`または代替PTY crateでPTY作成。
- `codex`起動。
- `claude`起動。
- PTY outputを読み取り。
- basic terminal parserで表示。
- input forwarding。
- bracketed paste。
- resize。

### 完了条件

- `agentmux-poc codex`でCodex TUIが表示・操作できる。
- `agentmux-poc claude`でClaude Code TUIが表示・操作できる。
- 長文prompt pasteができる。
- Ctrl+Cが効く。

### 失敗時の判断

Terminal engine候補を変更する。ここで破綻する場合、プロダクト全体を成立させにくい。

## 3. Phase 1: Terminal Engine / Pane

### 作業

- ScreenGrid実装。
- ANSI/VT parser統合。
- Ratatui rendering。
- split/focus/zoom。
- alternate screen対応。
- scrollback tail。

### 完了条件

- 2つ以上のagent TUIを同時表示。
- pane resizeでTUIが追随。
- detach/attach相当のbuffer snapshotを復元。

## 4. Phase 2: Daemon / IPC

### 作業

- daemon process。
- Unix socket IPC。
- client attach/detach。
- agent session registry。
- event bus。
- basic SQLite store。

### 完了条件

- client終了後もagent processが残る。
- 再attachできる。
- agent list/statusが取れる。

## 5. Phase 3: Input Automation

### 作業

- InputScript model。
- bracketed paste。
- key sequence送信。
- precondition。
- input lock。
- human activity detection。
- audit event。

### 完了条件

- 任意agentへprompt注入。
- 人間入力中は自動入力しない。
- 自動入力がevent logに残る。

## 6. Phase 4: Message Bus

### 作業

- AgentMessage CRUD。
- target resolution。
- inbox。
- delivery mode。
- prompt renderer。
- message view。

### 完了条件

- `agentmux send impl-codex "..."` がinboxへ入る。
- InjectWhenIdleで自動pasteされる。
- delivered/failed statusが管理される。

## 7. Phase 5: Context Broker

### 作業

- ContextItem CRUD。
- context board UI。
- mailbox file writer。
- redaction basic。
- context attach/inject。

### 完了条件

- 長いtest logをmailbox fileに保存。
- handoff promptにpathを含めてagentへ共有。
- contextがmessageに関連付く。

## 8. Phase 6: Worktree Manager

### 作業

- project init。
- git worktree create/list/remove。
- branch naming。
- diff capture。
- test command pane。
- artifacts保存。

### 完了条件

- implementerごとにworktreeを作成。
- diff artifactをreviewerへ共有。
- test logをcontext化。

## 9. Phase 7: Orchestrator

### 作業

- team template。
- task run。
- planner bootstrap。
- result marker parser。
- result-driven routing。
- status probe。
- stalled detection。

### 完了条件

- planner -> implementer -> tester -> reviewer の自動handoff。
- final summary生成。

## 10. Phase 8: Approval / Policy

### 作業

- automation level。
- approval queue。
- command/input safety classification。
- dangerous pattern detection。
- policy denial events。

### 完了条件

- 危険操作が自動実行されずapprovalに出る。
- manual approve/rejectできる。

## 11. Phase 9: Hardening

### 作業

- recovery。
- crash handling。
- terminal restoration。
- log rotation。
- config validation。
- test suite。
- dogfooding。

## 12. v0.1 Milestone

v0.1 release criteria:

```text
- macOS/Linuxで動作
- Claude/Codex TUIをPTY paneで起動
- split/focus/zoom/detach/attach
- prompt injection
- typed message bus
- context mailbox
- worktree isolation
- basic orchestrator
- approval queue
- event log
```

## 13. v0.2候補

- Codex app-server/MCP連携
- Claude SDK bridge
- richer terminal compatibility
- mouse/copy mode
- PR creation
- remote daemon over SSH
- multi-user shared context
- plugin system

## 14. 推奨Issue分割

1. `pty: spawn interactive process`
2. `terminal: render screen grid`
3. `tui: pane split/focus`
4. `daemon: session registry`
5. `ipc: JSONL protocol`
6. `input: bracketed paste`
7. `input: input lock`
8. `agent: claude adapter`
9. `agent: codex adapter`
10. `message: typed bus`
11. `context: mailbox file`
12. `worktree: create/diff`
13. `orchestrator: result marker parser`
14. `policy: approval queue`
15. `docs: user guide`

## 15. リスク駆動の順番

最初にterminal/PTYを検証する。message/contextの実装が簡単でも、TUIが実用的に表示できなければ製品価値が成立しないため。
