# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## このリポジトリの現状

**実装前の仕様・設計ドキュメント専用リポジトリ**である。ソースコード・`Cargo.toml`・git コミットはまだ存在せず、`docs/` 配下の設計ドキュメント一式（仕様書・ADR・JSON Schema・SQLite スキーマ案・設定例・Mermaid 図）だけが置かれている。

したがって現時点では「ビルド／テスト／lint」コマンドは存在しない。実装を開始する前に、まず該当する設計ドキュメントを読み、設計意図と整合させること。ドキュメントは日本語で書かれている。

ドキュメントの読み順（`docs/README.md` 準拠）:
`00_executive_summary` → `01_product_requirements` → `02_system_architecture` → `04_tui_pty_terminal_design` → `05_agent_adapter_design` → `06_message_bus_context_broker` → `07_orchestrator_workflows` → `09_security_policy_approval` → `12_implementation_roadmap`

確定済みの設計判断は `docs/adrs/` にある（ADR-0001〜0005）。仕様変更時は対応する ADR と仕様書本文の両方を更新する。

## プロダクトの本質

`agentmux` は Rust 製の **CLI/TUI-first マルチエージェント・コーディングコックピット**。Claude Code / Codex などの対話 TUI 型コーディングエージェントを複数の PTY 上で起動し、tmux 風 pane に同時表示しながら、自動キー送信・型付きメッセージング・コンテキスト共有・git worktree 分離・承認ゲートを提供する。

最重要の設計思想: **既存 CLI エージェントの内部能力は再実装しない**。エージェントはそのまま対話 TUI として活かし、agentmux はその上で作業分担・状態管理・入力制御・文脈同期・承認・統合を行う「コックピット／オーケストレーター」に徹する。非対話 `exec` は補助的な `BackgroundJob`（要約・schema validation・CI 的単発テスト等）に限定し、主制御経路にはしない。

## アーキテクチャの要点（複数ファイルを跨ぐ全体像）

### client-server 構成（`docs/spec/02_system_architecture.md`）

```
agentmux CLI/TUI client  <-(JSONL over Unix domain socket)->  agentmux daemon  <-(PTY)->  claude / codex / shell
```

**daemon が全ての状態を所有する**: agent process、PTY、terminal buffer、message、context、worktree、event log。client は TUI 描画とユーザー入力のみ担当する。この分離により client 終了後も agent session が生き続け、detach/attach が可能になる。実装時はこの所有境界を崩さないこと（状態を client 側に持たせない）。

daemon は Tokio runtime 上で複数タスク（IPC listener / per-client / per-PTY read / terminal parse / orchestrator / message delivery / file watcher / store writer）を回す。PTY の blocking read は専用 blocking task で受け、async channel で daemon へ渡す。

### 計画中の Rust workspace 構成（crate ごとに責務を閉じる）

```
crates/
  agentmux-cli/        CLI エントリ
  agentmux-daemon/     デーモン本体（全状態の所有者）
  agentmux-core/       domain ID・共通 error・enum・event 定義
  agentmux-ipc/        request/response/event プロトコル、JSONL framing、protocol versioning
  agentmux-pty/        PTY 生成・spawn・I/O・resize
  agentmux-terminal/   ANSI/VT parser・ScreenGrid・alternate screen・scrollback・dirty region
  agentmux-tui/        ratatui レイアウト・pane 描画・keymap・overlay
  agentmux-agent/      AgentSession 管理・provider adapter・状態検出・result marker parser
  agentmux-message/    typed message bus・inbox・delivery queue・prompt renderer
  agentmux-context/    ContextItem CRUD・context pack 選択・mailbox file writer・redaction
  agentmux-worktree/   git worktree ラッパー・branch 命名・diff artifact 取得
  agentmux-store/      SQLite 永続化
  agentmux-policy/     automation level・approval policy・command 分類・safety guardrail
```

provider 固有の挙動（Claude の hooks/settings、Codex の slash command 等）は **必ず adapter 内に閉じ込める**。orchestration 側は `AgentSession` / `AgentMessage` / `InputScript` / `StateSignal` の共通モデルだけを扱う。

### ドメインモデル（`docs/spec/03_domain_model.md`）

集約は `Project → Task → {AgentSession, Message, ContextItem, Worktree, Artifact, Approval}`。ID は prefix 付き ULID/UUIDv7（`proj_` / `task_` / `agent_` / `msg_` / `ctx_` / `art_` / `appr_`）でログ可読性を確保する。Rust の型は仕様書内の struct/enum 定義に従う。

### 状態検出は多重シグナルの優先度で決める（脆弱性回避の核心）

画面文言（screen scraping）は agent のバージョン変更に弱いため、単独依存しない。`StateSignal` を複数ソースから集め、次の優先度で `AgentStatus` を確定する:

```
HumanOverride > ExplicitMarker > HookEvent > Process > FileSystemEvent > PtyActivity > ScreenPattern
```

ここを実装するときは、新しい検出ソースを足す場合でもこの優先順位を尊重し、screen pattern を上位シグナルより優先させないこと。

### agent 連携プロトコル: `AGENTMUX_RESULT` marker

各 agent は turn 完了・失敗・入力要求時に `AGENTMUX_RESULT:` に続けて JSON を出力する（schema は `docs/schemas/agent_result.schema.json`）。orchestrator はこれを検出して次 agent へ routing する。JSON が壊れている場合は **修復を試みず**、Status Probe（状態を AGENTMUX_RESULT で返すよう促す prompt）を agent へ送る。

### `.agentmux/` ランタイムディレクトリ

`agentmux project init` がプロジェクト直下に作る。エージェント間の context/inbox/event の受け渡しはここを介する:

```
.agentmux/
  config.toml                  プロジェクト設定（docs/config/agentmux.config.example.toml 参照）
  state.db                     SQLite（protected）
  events.jsonl                 監査ログ（protected）
  context/                     共有 context（current-task.md, shared.md 等）
  inbox/<agent-name>/          agent 宛の長文 handoff
  events/<agent>.jsonl         provider hook 出力
```

## 自動入力・セキュリティの不変条件（`docs/spec/09_security_policy_approval.md`）

UI を自動操作する性質上、以下は仕様レベルの不変条件として常に守る:

- **人間が typing 中の pane には自動入力しない**（`human_input_quiet_ms` の quiet 期間と input lock で保証）。
- 破壊的操作・外部送信・本番影響・`git commit`/`git push`・secret access は **手動承認（approval gate）** を必須とし、自動実行しない。`policy` の既定は network/push/secret/full-access を `Deny`、それ以外を `Ask`。
- `protected_paths`（`.git/**`、`.env`、`*secret*`、`.agentmux/state.db` 等）への書き込みはブロックする。
- 自動入力・メッセージ・context・結果はすべて JSONL の event log に記録する（監査可能性）。
- `Ctrl+C` は focused pane にのみ送る。prefix key（既定 `Ctrl-g`）は agent TUI へ流さない。
- raw mode 解除失敗に備え、panic hook で terminal を必ず復旧する。

context 共有は長さで使い分ける: 短い（`max_inline_chars` 以下）→ inline prompt、長い → mailbox file に書いて path を handoff prompt に含める（ADR-0005）。共有前に redaction と要約をかける。

## 実装の進め方（`docs/spec/12_implementation_roadmap.md`）

リスク駆動の順序。**最初に PTY/terminal を検証する** — message/context が簡単に作れても、Claude/Codex の TUI が pane 内で実用的に表示・操作できなければ製品価値が成立しないため。

Phase 0 (PTY host PoC) → 1 (Terminal Engine/Pane) → 2 (Daemon/IPC) → 3 (Input Automation) → 4 (Message Bus) → 5 (Context Broker) → 6 (Worktree Manager) → 7 (Orchestrator) → 8 (Approval/Policy) → 9 (Hardening)。

Phase 0 が破綻する（PTY 上で Claude/Codex TUI が描画・操作できない）場合は terminal engine 候補（vte/vt100/portable-pty 等）を変更する判断ポイントになる。

## 計画中の主要コマンド体系（`docs/spec/11_cli_tui_user_spec.md`、実装後に有効）

```bash
agentmux doctor                                   # 環境診断（socket/config/SQLite/claude/codex/PTY/worktree）
agentmux project init .                           # .agentmux/ を作成
agentmux task run "<指示>" --team claude-codex     # task 起動・TUI attach・team の pane/agent/worktree 生成
agentmux attach <task|session>                    # 既存 session へ再接続
agentmux daemon start|stop|status
agentmux agent ls|spawn|stop|send|inject|focus|interrupt
agentmux message list|show|send|inject
agentmux context add|list|show|search|attach|inject|export
agentmux worktree list|diff|test|promote|archive
agentmux approval list|approve|reject
```

team template は `config.toml` の `[team.<name>]` で定義し、各 agent の `provider` / `role` / `worktree`（`main`/`dedicated`/`target`/`readonly`）を指定する。

## このリポジトリで作業するときの方針

- 仕様の変更・追加を行ったら、本文（`docs/spec/`）・ADR・該当する schema/diagram/config を整合させる。図は `docs/diagrams/*.mmd`(Mermaid)、データ契約は `docs/schemas/*.schema.json` と `docs/sql/schema.sql`。
- 設計上の未決事項は `docs/spec/15_open_questions.md` にある。新たな未決事項はここに追記する。
- ドキュメントは日本語で統一されている。新規ドキュメントも日本語で書く。

<!-- agentmux-result-protocol:start -->
## agentmux result protocol

`AGENTMUX_RESULT` is a **turn-status notification**, not the message channel. End each completed turn by emitting it so the orchestrator can track your state — `status`, `summary`, `changed_files`, `needs`, and `next`. Keep `messages: []` in normal operation:

```text
AGENTMUX_RESULT:
{
  "status": "completed",
  "summary": "<short summary>",
  "changed_files": [],
  "messages": [],
  "context_updates": [],
  "needs": [],
  "next": null
}
```

To send work to another coding agent, the **first choice is the CLI**:

```text
agentmux message send --to <target> --kind <Kind> --priority <low|normal|high|urgent> "<body>"
```

Prefer the CLI over `messages[]` because the CLI does not travel through the PTY display channel: its payload is never corrupted by terminal line-wrapping or control/escape characters (unlike text rendered into a pane), it is auto-injected into an idle target, and — because the command reads your `AGENTMUX_AGENT_ID` environment variable — the message is correctly attributed to your agent session (`from: agent`), so the recipient can reply to exactly you with `agent:<your-session-name>`.

`messages[]` inside `AGENTMUX_RESULT` remains a **fallback for agents that have no shell access** and therefore cannot run `agentmux message send`. When you do use it, the whole `AGENTMUX_RESULT` block is not stored as a message; only entries inside `messages[]` are routed.

Always send messages with inject delivery. Both `agentmux message send` and `messages[]` entries default to `delivery_mode: inject_when_idle`: the daemon automatically injects the rendered prompt into the target session's PTY as soon as that session is idle. Keep that default — never pass `delivery_mode: inbox_only`, because inbox-only messages are not injected and the target agent will never see them. After sending, do not wait for or request a manual injection; delivery happens automatically.

When an agentmux bus message is injected into your session, always reply. Prefer the CLI: `agentmux message send --to agent:<sender-session-name> --kind <Kind> "<reply>"` (use the requested `reply_to` / target context if the injected prompt provides one). If you have no shell access, reply through `messages[]` in the next `AGENTMUX_RESULT` instead. Do not ask the user for confirmation before sending normal message replies or progress updates. If no substantive answer is ready yet, send a brief `StatusProbe`, `Question`, or `Handoff` that says what is pending instead of staying silent.

Avoid unbounded agent-to-agent loops. This applies to both `agentmux message send` and `messages[]`. Only if the same pair of agents has exchanged messages for 3 or more back-and-forth turns on the same topic, the next reply must ask for human confirmation before continuing. Use `kind: "Question"` and include the current conclusion, the remaining uncertainty, and the exact decision needed from the user. Configure this threshold before installing the protocol with `AGENTMUX_MESSAGE_CONFIRM_AFTER_TURNS`; the default is 7.

Allowed message kind values (for both `--kind` and `messages[].kind`) are: `TaskAssignment`, `Question`, `Finding`, `PatchProposal`, `ReviewComment`, `TestResult`, `FailureReport`, `Decision`, `Handoff`, `ApprovalRequest`, `ContextUpdate`, `StatusProbe`. Do not invent other kinds such as `Greeting`; an invalid kind is rejected and the message is not stored.

Multi-party meetings: when three or more sessions must discuss one topic, use a meeting thread instead of pairwise messages. Open with `agentmux meeting open "<topic>" --participants <name1>,<name2>,<name3>` (session names via `agentmux sessions`); the daemon injects the agenda into every participant. Post with `agentmux message send --thread <thread_id> --kind <Kind> "<body>"` — it is delivered to all participants except you, so never re-send your own statement. Each participant has a per-thread message limit (default 7, set with `--max-turns`); when you hit the limit, summarize your conclusion and ask the human for a decision with `kind: Question` outside the thread. Inspect with `agentmux message history --thread <thread_id>` and `agentmux meeting list`; close with `agentmux meeting close <thread_id>`.

Agent sessions register a stable role and a unique session name at startup. Use role targets (`role:tester`, `role:implementer`, `role:reviewer`) when every session with that role should receive the message. Use `agent:<session-name>` or a session id when the message is for exactly one session. Check available sessions with `Ctrl-g s` in the TUI or `agentmux sessions`.

Each live session receives its own identity through environment variables: `AGENTMUX_AGENT_NAME`, `AGENTMUX_AGENT_ROLE`, and `AGENTMUX_AGENT_ID`. Use `AGENTMUX_AGENT_NAME` when another session needs to reply to exactly this session.

Common TUI workflows:

- Start multiple panes with `agentmux start "agy,codex"` or include message history with `agentmux start "agy,messages,codex"`.
- Inside the TUI, `Ctrl-g %` and `Ctrl-g "` open the new pane picker. Choose `Claude Code`, `Codex`, `Antigravity`, or `Conversation List`.
- `Conversation List` opens the message history as a normal pane. `Ctrl-g m` opens the same history as a temporary overlay.
- `Ctrl-g s` shows running sessions with their names, roles, and process IDs.
- `Ctrl-g x` closes the focused local pane or stops the focused agent pane.

Message inspection commands:

- `agentmux message list` shows stored bus messages newest first.
- `agentmux sessions` shows live agent sessions and their stable names/roles.
- `agentmux start "messages"` opens only the message history pane.

Manual injection is a fallback for messages that were not auto-delivered yet (for example the target was busy or had not spawned when the message was created). In that case use `agentmux message inject <message_id>` only when the message target resolves to exactly one session. If the target can resolve to multiple sessions (for example `role:tester`) or you need a specific pane, use `agentmux agent inject <message_id> <agent_id>` after checking `agentmux sessions`; this explicitly selects the session that receives the PTY input.

Injection is asynchronous: the daemon records the message first, then waits briefly before writing the rendered message into the target PTY. If the TUI list updates before the text appears in the agent pane, wait a few seconds before retrying.

Two-session exchange example (CLI-first):

```text
impl finishes work, then notifies the tester via the CLI:
agentmux message send --to role:tester --kind TestResult --priority normal \
  "Please verify copy mode: Ctrl-g [, drag inside the focused pane, release to copy, Esc/q to exit."

impl then emits its turn-status notification:
AGENTMUX_RESULT:
{
  "status": "completed",
  "summary": "Implemented copy mode.",
  "changed_files": ["crates/agentmux-cli/src/main.rs"],
  "messages": [],
  "context_updates": [],
  "needs": [],
  "next": null
}
```

```text
tester replies to the sender (attributed automatically from AGENTMUX_AGENT_ID):
agentmux message send --to agent:codex-a1b2c3 --kind Finding \
  "Focused-pane drag selection worked. OSC52 clipboard support depends on the host terminal."

then emits its own turn-status notification:
AGENTMUX_RESULT:
{
  "status": "completed",
  "summary": "Copy mode verification completed.",
  "changed_files": [],
  "messages": [],
  "context_updates": [],
  "needs": [],
  "next": null
}
```

A shell-less agent would instead place those `Finding` / `TestResult` entries inside `messages[]` of its `AGENTMUX_RESULT`.

Check delivery with `Ctrl-g m` in the TUI or `agentmux message list`.
<!-- agentmux-result-protocol:end -->
