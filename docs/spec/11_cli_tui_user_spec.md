# 11. CLI / TUIユーザー仕様書

## 1. CLI設計方針

CLIはnon-interactive操作、daemon操作、task起動、message/context操作に使う。agentの主操作はTUI内で行う。

## 2. Top-level commands

```bash
agentmux --help
agentmux start ["agy,messages,codex"]
agentmux doctor
agentmux sessions
agentmux attach [task|session]
agentmux daemon start|stop|status
agentmux project init|open|status|install-result-protocol
agentmux task run|status|pause|resume|cancel|summary
agentmux agent ls|spawn|stop|send|inject|focus|interrupt
agentmux message list|history|show|send|inject
agentmux meeting open|close|list
agentmux context add|list|show|search|attach|inject|export
agentmux worktree list|diff|test|promote|adopt|archive
agentmux approval list|approve|reject
agentmux layout save|load|list
```

`start` の引数には `"agy | codex"` / `"(a ― b) | c"` 形式のレイアウト記法も受理する（§3.0.1 参照）。

## 3. 代表コマンド

### 3.0 start

```bash
agentmux start "agy,messages,codex"
```

daemon未起動なら起動し、指定されたpaneを開いてからTUIを表示する。指定はcomma-separatedで、provider sessionの `claude`, `codex`, `agy` と、message履歴paneの `messages` を受け付ける。provider指定なしの `agentmux start` は通常のTUI起動と同じく、既存sessionがなければprovider pickerを表示する。

`agy` provider は既定で `--dangerously-skip-permissions` を付けて起動し、tool permission prompt で停止しにくい強いpermission modeにする。`agent.spawn` payloadで明示的に `args` を渡した場合は、その指定を優先する。

#### 3.0.1 レイアウト記法（split layout DSL）

`agentmux start` の引数には comma 区切りの従来構文に加え、分割方向を明示した記法を使える（ADR-0007）。

**基本記号**

| 記号 | Unicode | 意味 | ASCII 別名 |
|------|---------|------|-----------|
| `\|` | U+007C | pane を**左右**に分割（縦の分割線） | `/` |
| `―` | U+2015 | pane を**上下**に分割（横の分割線） | `-`（前後に空白が必要） |
| `()` | — | グルーピング・入れ子（Phase 2） | — |
| `name:N` | — | サイズ比率 `N`（Phase 2） | — |

視覚的な記憶の手がかり: 縦棒 `|` を pane の間に置くと縦の仕切り線になり左右に分かれる。横棒 `―` を pane の間に置くと横の仕切り線になり上下に分かれる。

**シェルの注意**

- `|` はシェルのパイプ文字のため**クォートが必須**: `agentmux start "agy | codex"`。
- `/` はクォート不要: `agentmux start agy/codex`（`|` と等価）。
- `-` を上下分割に使うときは前後に空白を置く: `agy - codex`。ハイフン入り pane 名（`claude-code` 等）と区別するためである。

**使用例**

```bash
agentmux start "agy | codex"           # 左右2分割（クォート必須）
agentmux start agy/codex               # 同上（/ はクォート不要）
agentmux start "agy - codex"           # 上下2分割
agentmux start "agy | codex | messages" # 左右3分割
agentmux start "agy,codex"             # 従来構文（| と等価）、引き続き動作
```

**後方互換**

`,` 区切りの従来構文は `|`（左右並び）と等価なエイリアスとして引き続き動作する。`,` と `|`/`―` の混在は構文エラーになる。

**内部実装との命名対応**

内部の `SplitDirection` enum は TUI レイアウトエンジンの慣習で命名されており、本 DSL の記号と名前が**逆転**している。

| 本 DSL の記号 | 内部 `SplitDirection` |
|---|---|
| `\|`（縦棒、左右分割） | `SplitDirection::Vertical` |
| `―`（横棒、上下分割） | `SplitDirection::Horizontal` |

実装時はこの橋渡し関係を本 ADR-0007 と合わせて参照すること。

**段階導入**

- **Phase 1（現在）**: フラットな方向指定のみ（`agy | codex`、`agy ― codex`、`agy / codex`、`,`互換）。
- **Phase 2（後続）**: `()` ネストと `:N` サイズ比率（`(agy ― codex) | messages`、`agy:60 | codex:40`）。

Phase 1 では `()` と `:N` は構文エラーとして拒否される。

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

ローカル実行では、指定ディレクトリに既存の `AGENTS.md`, `CLAUDE.md`, `GEMINI.md` がある場合だけ `agentmux result protocol` を追記する。既に同じ managed marker がある場合は、その marker 範囲を最新の `agentmux result protocol` に置換する。これにより、再実行で `messages[]` の使い方、message受信時の返信必須ルール、通常返信では送信前確認を求めないルール、一定回数以上続くagent間対話でのみ人間確認を求めるルール、2セッション間の対話例、確認手順が最新化される。人間確認を求める往復回数は `AGENTMUX_MESSAGE_CONFIRM_AFTER_TURNS` で指定でき、未設定時は `3`。

`--global` は以下のグローバル指示ファイルへ設定する。

```text
~/.codex/AGENTS.md
~/.claude/CLAUDE.md
~/.gemini/GEMINI.md
```

### 3.1.2 agent role registration

`agent spawn --role <role>` と TUI/provider picker から生成される agent は、daemon 側の session metadata に role を保存する。message bus の `to: "role:<role>"` はこの metadata を使って解決し、agent 名からの推定は role が明示されない場合の fallback とする。`agentmux sessions` と TUI の session list は role を表示し、agent / human が利用可能な宛先を確認できるようにする。

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

**Arena modeオプション（Cargo feature `arena` が必要）:**

```bash
agentmux task run "機能Xを実装して" --arena claude,codex
agentmux task run "機能Xを実装して" --arena claude,codex --base-branch main
```

- `--arena <p1>,<p2>,...`: 指定した provider ごとに専用 worktree を作成して agent を spawn する（runner=arena）。
- `--base-branch <branch>`: arena worktree の base branch を指定する（省略時は daemon の project base branch）。
- provider が重複している場合は副作用の発生前にエラーで終了する。
- 実行前に daemon が `ARENA_PROTOCOL_VERSION`（3）以上をサポートするか確認する。サポートしていない場合はエラーメッセージを表示して終了する。

### 3.2.1 worktree adopt

```bash
agentmux worktree adopt <worktree_id>
```

指定した arena candidate worktree の adoption approval を queue する。成功した場合は `approval_id` を表示する。`agentmux approval approve <approval_id>` で承認すると integration branch への merge が実行される。

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

message送信は既定でinjectする。つまりmessageは保存された後、宛先sessionが解決できれば入力可能なタイミングでagent paneへ注入される。保存だけにしたい場合は `--no-inject` を指定する。互換性のため `--inject` も受け付けるが、既定と同じ挙動になる。

### 3.6 inject

```bash
agentmux message inject msg_123
agentmux agent inject msg_123 agent_01HX...
```

`message inject` は、message target がちょうど1つのsessionに解決できる場合に既存messageをそのsessionへ即時注入する。`role:tester` のように複数sessionへ解決される可能性がある場合や、明示的にpaneを選びたい場合は、`agentmux sessions` で対象を確認してから `agent inject <message_id> <agent_id>` を使う。

### 3.7 message history

```bash
agentmux message history
agentmux message history --limit 20
agentmux message history --agent impl-codex
agentmux message history --task task_123 --status delivered
agentmux message history --kind handoff
agentmux message history --thread thread_01HX...
```

`message list` はdaemon payloadをJSONで表示する。`message history` は履歴確認用に
`created_at`, `delivery_status`, `kind`, `from`, `to`, `message_id`, `body`
を人間向けの表で表示する。

### 3.8 meeting(マルチパーティ会議)

```bash
agentmux meeting open "X の設計方針" --participants claude-a,codex-b,agy-c
agentmux meeting open "障害の原因切り分け" --participants claude-a,codex-b --max-turns 3
agentmux message send --thread thread_01HX... --kind Finding "私の見解は..."
agentmux meeting list
agentmux meeting close thread_01HX...
```

`meeting open` は `MessageThread`(ADR-0006)を作成し、議題 kickoff message を
参加者全員へ inject する。`--thread` 付きの message send は参加者全員
(送信者を除く)へ fan-out 配送される。1 参加者あたりの発言数は
`--max-turns`(既定 5)で制限され、上限到達後の投稿は拒否される。
詳細は `06_message_bus_context_broker.md §3.6`。

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
| ActivityFeed | live activity feed（sitrep header + event tail）。Cargofeature `activity-feed` が必要。 |

**Arena overlay:** arena candidate を一覧表示するcentered popup。pane ではなくoverlay として実装されており、`Ctrl-g a` でトグルする（Cargo feature `arena` が必要）。

## 6. Keymap

prefix key初期値: `Ctrl-g`

### 6.1 基本操作

```text
Ctrl-g d        detach
Ctrl-g ?        help
Ctrl-g z        zoom current pane
Ctrl-g arrow    focus pane
Ctrl-g s        running session list
Ctrl-g a        agent list
Ctrl-g m        message bus
Ctrl-g c        context board
Ctrl-g A        approval queue
Ctrl-g f        activity feed toggle（feature: activity-feed）
Ctrl-g a        arena overlay toggle（feature: arena）
```

### 6.2 Pane操作

```text
Ctrl-g %        split vertical and choose coding agent
Ctrl-g "        split horizontal and choose coding agent
Ctrl-g x        close/stop current pane
Ctrl-g r        resize mode
Ctrl-g Space    rotate layout
```

`Ctrl-g %` / `Ctrl-g "` は空のshell paneを作らず、provider pickerを表示する。選択肢は `Claude Code`, `Codex`, `Antigravity`, `Conversation List`。coding agentをEnterで選択すると起動してそのpaneへattachする。`Conversation List` はmessage履歴を通常paneとして開く。`Enter` / `Space` / `d` でcompact/detail表示を切り替える。`Ctrl-g x` で閉じる。

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
Enter/Space/d   toggle compact/detail
Esc, q          close message history
```

Message history表示:

```text
compact: delivery_status / kind / message_id / created_at, route, body
detail: delivery_status, kind, message_id, created_at, from, to, body
```

## 6.4 Activity Feed操作

`Ctrl-g f` でActivityFeed paneをトグルする（Cargo feature `activity-feed` が必要）。

```text
j / Down    次のeventを選択
k / Up      前のeventを選択
Enter       選択中のeventに紐づくagent paneへfocus
Esc / q     paneを閉じる
```

Activity Feed paneの構成:

- **sitrep header**: 全live sessionのAgentStatus集約。要介入状態（`awaiting_input` / `needs_human` / `awaiting_approval` / `blocked` / `stalled`）を上位ソートして表示。
- **event tail**: actor / action / target を正規化して表示。最大500件のring buffer。PTY output chunkとscreen diffは表示から除外。
- **tail追従**: 末尾のentryを選択しているときのみ自動追従する。

daemon側のprotocol versionが`EVENT_SUBSCRIBE_PROTOCOL_VERSION`未満の場合、`event.subscribe`は送らず "Activity Feed unsupported by this daemon" をnoticeとして表示し、TUIは落とさない。

## 6.5 Arena Overlay操作（feature: arena）

`Ctrl-g a` でArena overlay（centered popup）をトグルする。daemon の `protocol_version` が `ARENA_PROTOCOL_VERSION`（3）未満の場合は "Arena unsupported by this daemon" をnoticeとして表示し、overlayは開かない。

```text
j / Down    次のcandidate を選択
k / Up      前のcandidate を選択
a / Enter   選択中の candidate を adopt（worktree.adopt を送信し、approval_id をstatus barに表示）
Esc / q     overlayを閉じる
```

Arena overlay の各 candidate panel に表示する情報:

- provider 名
- diff stat（追加/削除行数）
- test status（色分け: 緑=passed / 赤=failed / 黄=pending）
- summary（AGENTMUX_RESULT の `summary` フィールド）
- worktree_id

選択中の candidate は反転ハイライトで表示する。

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
  agentmux sessions
  agentmux agent inject msg_123 agent_01HX...
```

```text
error: unsafe input requires approval

reason:
  message may trigger shell command execution

next:
  agentmux approval list
```
