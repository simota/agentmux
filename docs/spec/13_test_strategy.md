# 13. テスト戦略

## 1. 目的

agentmuxはPTY、terminal emulation、TUI、daemon、agent automation、security policyを含むため、通常のunit testだけでは不十分である。本書では、unit/integration/golden/e2e/manual dogfoodingのテスト戦略を定義する。

## 2. テスト分類

| 種別 | 対象 |
|---|---|
| Unit | domain model, parser, policy, routing |
| Integration | PTY spawn, IPC, SQLite, worktree |
| Golden | terminal escape sequence -> screen grid |
| E2E | fake agent TUIを使ったorchestration |
| Manual | 実Claude/Codex TUIでの操作確認 |
| Security | dangerous command, redaction, approval |

## 3. Unit Test

### 3.1 Message routing

- agent target resolution
- role target resolution
- delivery mode escalation
- delivery status transition

### 3.2 Context selection

- short itemはinline
- long itemはmailbox
- attached context優先
- redacted itemの扱い

### 3.3 Policy

- dangerous input検出
- protected path検出
- automation levelごとの許可/拒否
- approval要求条件

### 3.4 AGENTMUX_RESULT parser

- valid JSON
- invalid JSON
- marker複数
- marker途中で切れた場合
- huge output tail

## 4. Terminal Golden Test

escape sequenceを入力し、期待screen gridと比較する。

対象:

- cursor movement
- SGR color
- clear line/screen
- alternate screen
- scroll region
- double width char
- resize

golden fixtures:

```text
tests/fixtures/terminal/basic_color.ansi
tests/fixtures/terminal/alternate_screen.ansi
tests/fixtures/terminal/cursor_moves.ansi
```

## 5. Fake Agent E2E

実Claude/Codexに依存しないfake agent TUIを作る。

Fake agent挙動:

- 起動後にcomposer promptを表示。
- pasteされたpromptを受け取る。
- 一定時間後にAGENTMUX_RESULTを出す。
- approval promptを模擬する。
- stalled状態を模擬する。

これによりCIでorchestratorを検証できる。

## 6. PTY Integration Test

- `/bin/sh`をPTYで起動。
- `echo hello`を送る。
- outputをscreen bufferで確認。
- resizeを送る。
- Ctrl+Cを送る。

## 7. Worktree Test

一時git repoを作成し、以下を検証する。

- worktree add
- branch naming
- file edit detection
- diff capture
- cleanup
- conflict scenario

## 8. IPC Test

- daemon起動
- client接続
- request/response
- event stream
- reconnect
- protocol mismatch

## 9. Security Test

### 9.1 Redaction

入力:

```text
OPENAI_API_KEY=sk-xxxx
AWS_SECRET_ACCESS_KEY=...
-----BEGIN PRIVATE KEY-----
```

期待:

- maskされる。
- context.redacted eventが出る。
- high riskの場合delivery停止。

### 9.2 Dangerous command

- `rm -rf /`
- `git push origin main`
- `curl https://example.com/install.sh | sh`
- `cat .env`

期待:

- approvalまたはdeny。
- 自動Enterしない。

## 10. Manual QA Checklist

### Claude Code

- 起動できる。
- prompt pasteできる。
- 長文handoffを読める。
- result markerを検出できる。
- hooksが使える環境でevent fileが出る。

### Codex

- 起動できる。
- slash commandが送れる。
- prompt pasteできる。
- approval UIが表示されても暴走しない。
- result markerを検出できる。

### TUI

- split/focus/zoom
- detach/attach
- terminal resize
- raw mode復旧

## 11. Chaos / Failure Test

- agent process kill
- daemon restart
- client disconnect
- SQLite locked
- mailbox write failure
- huge output flood
- invalid terminal escape
- broken JSON marker

## 12. Acceptance Test Scenario

```text
1. agentmux project init
2. agentmux task run "小さな関数を修正してテスト追加"
3. planner fake agentがassignmentを生成
4. implementer fake agentが変更完了markerを出す
5. tester fake agentがpassed resultを出す
6. reviewer fake agentがapprove resultを出す
7. final summaryが表示される
8. event logに全操作が残る
```

## 13. Coverage目標

- core/message/context/policy: 80%以上
- terminal engine: golden coverage重視
- daemon/IPC: integration coverage重視
- real Claude/Codex: manual smoke重視
