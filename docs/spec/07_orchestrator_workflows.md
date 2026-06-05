# 07. Orchestrator / Workflow設計書

## 1. 目的

Orchestratorは、task、agent、message、context、worktree、approvalの状態を監視し、複数の対話TUI agentへ安全に指示を注入して自律コーディングを進める。

TUI-first設計では、Orchestratorは「非対話ジョブを順に実行する」のではなく、「生きた対話セッションにhandoff promptを配送する」役割を担う。

## 2. 基本状態機械

詳細図は `diagrams/state-machine-agent.mmd` を参照。

### 2.1 AgentStatus遷移

```text
Starting
  -> InteractiveReady
  -> AwaitingInput
  -> RunningTurn
  -> RunningCommand
  -> AwaitingApproval
  -> CompletedTurn
  -> AwaitingInput
```

例外:

```text
Any -> NeedsHuman
Any -> Stalled
Any -> Exited
Any -> Failed
```

### 2.2 TaskStatus遷移

```text
Created
  -> Starting
  -> Running
  -> WaitingForHuman
  -> Running
  -> Completed
```

例外:

```text
Running -> Failed
Running -> Paused
Paused -> Running
Any -> Cancelled
```

## 3. Team Template

```toml
[team.claude-codex]
agents = [
  { name = "planner", provider = "claude", role = "planner", worktree = "main" },
  { name = "impl-codex", provider = "codex", role = "implementer", worktree = "dedicated" },
  { name = "impl-claude", provider = "claude", role = "implementer", worktree = "dedicated" },
  { name = "tester", provider = "shell", role = "tester", worktree = "target" },
  { name = "reviewer", provider = "codex", role = "reviewer", worktree = "readonly" }
]
```

## 4. 標準Workflow

### 4.1 Task Run Sequence

1. task作成
2. project config読み込み
3. team template解決
4. worktree作成
5. agent session起動
6. bootstrap prompt注入
7. plannerへTaskBrief注入
8. planner result marker検出
9. implementerへTaskAssignment配送
10. implementer result marker検出
11. testerへTestRequest配送
12. test artifact作成
13. reviewerへReviewRequest配送
14. review result marker検出
15. final summary作成
16. user approvalを待つ

## 5. Planner prompt

```text
[agentmux task]
あなたはplannerです。
以下のタスクを分解し、implementer agentへ送る作業指示を作成してください。

Task:
{task_body}

利用可能agent:
- impl-codex: Codex implementer
- impl-claude: Claude implementer
- tester: shell test runner
- reviewer: Codex reviewer

制約:
- 実装agentはそれぞれ専用worktreeで作業します
- public APIの破壊的変更は禁止
- 最小変更を優先してください

最後に必ず AGENTMUX_RESULT JSON を出力してください。
```

## 6. Implementer handoff

```text
[agentmux handoff]
from: planner
kind: TaskAssignment

あなたはimplementerです。
専用worktree内で次の修正案を実装してください。

{assignment}

完了時:
- 変更ファイル
- 実装方針
- テスト状況
- reviewer/testerへの次action
を AGENTMUX_RESULT JSON で返してください。
```

## 7. Tester handoff

```text
[agentmux handoff]
from: orchestrator
kind: TestRequest

対象worktree:
{worktree_path}

実行してください:
{test_command}

結果を .agentmux/artifacts/{task_id}/ に保存し、要約を AGENTMUX_RESULT で返してください。
```

ShellAdapterの場合、agentmuxが直接test commandを実行してもよい。ただし対話TUI原則に沿い、test paneとして表示する。

## 8. Reviewer handoff

```text
[agentmux handoff]
from: orchestrator
kind: ReviewRequest

以下をレビューしてください。

- diff: {diff_path}
- test log: {test_log_path}
- task brief: {task_brief_path}

観点:
- バグが修正されているか
- 変更が最小か
- テストが十分か
- リスクは何か

AGENTMUX_RESULT JSONで approve/request_changes/needs_tests を返してください。
```

## 9. Integrator workflow

v0.1では完全自動mergeは行わない。reviewerがapproveした候補をfinal summaryに出し、ユーザーがpromoteまたはadoptする。

```bash
agentmux worktree promote task-123-codex
agentmux worktree adopt <worktree_id>   # arena runnerの場合
```

promoteは以下を行う。

1. base branchとの差分確認
2. test result確認
3. approval確認
4. integration branch作成
5. patch適用またはmerge
6. final diff表示

commit/pushはデフォルト手動。

### 9.1 Arena runner workflow（Cargo feature `arena`）

`task run --arena <p1>,<p2>` で起動した task では、promote の代わりに adopt flow を使う。

```text
arena task起動
  -> provider ごとにworktree作成 + agent spawn
  -> 各agent: 実装 -> AGENTMUX_RESULT completed
  -> diff capture + test capture（自動）
  -> TUI Arena overlay で candidate を比較（Ctrl-g a）
  -> adopt対象を選択して a/Enter
  -> worktree.adopt -> approval queue へ積む
  -> approval approve -> merge_to_integration_branch 実行
  -> MergeOutcome: Clean/Dirty -> 完了 / Conflict -> WorktreeStatus::Conflicted
```

Arena overlay では provider 別の diff stat・test status・summary を横並びで比較できる。`WorktreeTestCompleted` event を受けて TUI はpollingなしで更新される。

## 10. Event-driven loop

```rust
loop {
    let event = event_bus.recv().await?;

    match event {
        Event::TaskCreated(task) => spawn_team(task).await?,
        Event::AgentReady(agent) => deliver_queued_messages(agent).await?,
        Event::AgentResult(agent, result) => handle_agent_result(agent, result).await?,
        Event::MessageQueued(msg) => route_or_wait(msg).await?,
        Event::ApprovalRequested(req) => handle_approval(req).await?,
        Event::AgentStalled(agent) => recover_or_ask_human(agent).await?,
        Event::WorktreeChanged(wt) => maybe_capture_diff(wt).await?,
        Event::TestCompleted(result) => notify_reviewer(result).await?,
    }
}
```

## 11. AGENTMUX_RESULT routing

### 11.1 Result例

```json
{
  "status": "completed",
  "summary": "AuthServiceのrefresh token expiry検証を修正しました。",
  "changed_files": ["src/auth/refresh.rs", "tests/auth_refresh.rs"],
  "messages": [
    {
      "to": "role:tester",
      "kind": "TestResult",
      "body": "auth_refresh系テストを実行してください"
    }
  ],
  "context_updates": [
    {
      "kind": "Decision",
      "title": "refresh token validation layer",
      "body": "検証はservice層に寄せました"
    }
  ],
  "needs": [],
  "next": "tester"
}
```

`messages[]` はcoding agent同士のmessage bus送信として扱う。daemonは各messageを保存し、delivery mode `InjectWhenIdle` で宛先agentへ配送する。発信元はorchestratorではなく、`AGENTMUX_RESULT` を出したcoding agent名として記録するため、CLI/TUIの履歴では `team_agent:impl-codex -> role:tester` のようにagent間のやり取りとして確認できる。

### 11.2 status別処理

| status | 処理 |
|---|---|
| completed | context更新、message routing、次phaseへ |
| blocked | NeedsHumanまたは別agentへQuestion |
| needs_input | humanまたはplannerへ質問 |
| failed | failure report作成、retry policy適用 |
| cancelled | task/agent停止 |

## 12. Stalled detection

条件例:

- PTY outputがN分ない。
- processは生きているがscreenに変化がない。
- 入力待ちか実行中か不明。
- delivery queueが長時間詰まっている。

対応:

1. StatusProbeを送る。
2. 反応がなければhuman intervention表示。
3. 設定によりCtrl+Cを提案する。
4. それでも不可ならagent restart候補を出す。

## 13. Retry policy

```toml
[orchestrator.retry]
max_status_probe = 2
max_message_injection_retry = 3
max_agent_restart = 1
```

- 同じagentへの無限retryは禁止。
- retryはevent logに残す。
- 高リスクinputのretryはmanual approval必須。

## 14. Pause / Resume

Pause時:

- 新規message injection停止
- 自動approval停止
- human inputは許可
- processは継続

Resume時:

- queued messagesを再評価
- status probeを必要に応じて送る

## 15. Human intervention

Human interventionが必要な例:

- approval queueあり
- secret検出
- agentがblocked
- result marker不正
- dangerous command検出
- merge conflict

TUIではapproval/internal paneへ表示する。

## 16. Final summary

最終出力には以下を含める。

- task title
- candidate worktrees
- changed files
- test results
- reviewer recommendation
- risks
- open questions
- recommended next action
- promote command

## 17. Workflowの拡張

将来追加:

- issue ingestion
- PR description generation
- security reviewer path
- performance benchmark path
- documentation update path
- multiple reviewer quorum
