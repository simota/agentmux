# 08. Worktree / Artifact管理設計書

## 1. 目的

複数agentが同じrepositoryを同時に編集すると衝突する。agentmuxでは、実装agentごとにgit worktreeを作成し、変更、テスト、レビュー、統合候補を分離管理する。

## 2. 基本方針

- implementer agentは専用worktreeで作業する。
- planner/reviewerはread-onlyまたはmain worktreeでもよい。
- testerは対象worktreeを明示して実行する。
- integratorのみintegration branchを扱う。
- git pushはv0.1では手動承認必須。

## 3. Directory layout

```text
<project-root>/
  .agentmux/
    config.toml
    state.db
    events.jsonl
    context/
      current-task.md
      shared.md
      decisions.md
    inbox/
      impl-codex/
      impl-claude/
    artifacts/
      task-123/
        diff-impl-codex.patch
        test-impl-codex.log
        result-planner.json
    worktrees/
      task-123-codex/
      task-123-claude/
```

注意: git worktreeの配置はrepository外に置く設定もサポートする。

## 4. Worktree作成

```bash
git worktree add .agentmux/worktrees/task-123-codex -b agentmux/task-123-codex <base_branch>
```

branch naming:

```text
agentmux/{task_slug}-{agent_name}
```

例:

```text
agentmux/task-123-refresh-token-codex
```

## 5. Worktree状態

```rust
enum WorktreeStatus {
    Creating,
    Ready,
    Dirty,
    Testing,
    ReviewReady,
    Promoted,
    Archived,
    Conflicted, // arenaでのmerge conflict発生後、abort済みの状態
    Failed,
}
```

状態更新は以下から検出する。

- git status
- file watcher
- agent result marker
- test completion
- user command

## 6. Artifact

### 6.1 種類

| ArtifactKind | 内容 |
|---|---|
| DiffPatch | `git diff`出力 |
| TestLog | テスト結果ログ |
| CommandOutput | 任意コマンド出力 |
| AgentResult | AGENTMUX_RESULT JSON |
| Transcript | agent output tail |
| ScreenSnapshot | pane snapshot |
| ContextBundle | context export |
| FileList | changed files list |

### 6.2 保存規則

```text
.agentmux/artifacts/{task_id}/{kind}-{agent_name}-{sequence}.{ext}
```

例:

```text
.agentmux/artifacts/task-123/diff-impl-codex-001.patch
.agentmux/artifacts/task-123/test-impl-codex-001.log
.agentmux/artifacts/task-123/result-reviewer-001.json
```

## 7. Diff capture

実装agentがCompletedTurnになったらdiffを取得する。

```bash
git -C <worktree> diff --stat <base_branch>...
git -C <worktree> diff <base_branch>... > diff.patch
```

ContextItemとしてDiffSummaryを作成する。

## 8. Test capture

test commandはproject configで指定する。

```toml
[test]
default_command = "cargo test"
commands = [
  { name = "unit", command = "cargo test --lib" },
  { name = "all", command = "cargo test" }
]
```

TestResult context例:

```markdown
# TestResult: impl-codex

status: passed
command: cargo test
log: .agentmux/artifacts/task-123/test-impl-codex-001.log

summary:
- 128 tests passed
- 0 failed
```

## 8a. Arena Run（Cargo feature `arena`）

`task run --arena <p1>,<p2>` を指定すると、runner が `arena` モードで動作する。

**起動時の処理:**

1. provider リストの重複を確認し、重複があれば副作用の発生前に拒否する。
2. provider ごとに専用 worktree を `WorktreeManager` で実作成する（branch: `agentmux/{task_slug}-{provider}`）。
3. 各 agent を worktree の cwd で spawn し、`worktree_id` を session metadata へリンクする。
4. `--base-branch <branch>` が指定された場合はそのブランチを base とする（省略時は daemon のプロジェクト base branch）。

**完了後の自動 capture:**

- AGENTMUX_RESULT の `status=completed` を検出した arena agent に対して、diff と test を自動 capture する。
- `WorktreeTestCompleted` event を publish して TUI に通知する。
- capture 完了後、その candidate が readiness 条件（diff captured + tests passed）を満たせば adopt 操作が可能になる。

**daemon capability gate:**

- `ARENA_PROTOCOL_VERSION = 3`。client は `daemon.status` の `protocol_version` が 3 以上であることを確認してから `--arena` フラグを送る。protocol version が不足している場合は user-readable なエラーで終了する。

## 9. Promote flow

### 9.1 worktree promote（通常 promote）

```bash
agentmux worktree promote task-123-codex
```

処理:

1. approval確認
2. worktree dirty確認
3. test result確認
4. latest diff表示
5. integration branch作成
6. mergeまたはpatch apply
7. conflictがあれば停止
8. final summary更新

### 9.2 Arena adopt flow（実装済み）

Arena runnerで起動した task では、`promote` は直接 merge を行わず、adoption approval を approval queue へ積む。approve 後に以下の手順で merge が実行される。

**前提条件（`request_worktree_adoption` で検証）:**

1. diff capture 済みであること。
2. test が passed であること。
3. 同一 task で pending adoption が 1 件以下であること（重複防止）。

**approve 後の merge 手順（`merge_to_integration_branch`）:**

1. repo root が clean であることを確認する（dirty なら事前拒否）。
2. integration branch を base branch へ `reset --hard` で refresh する（既存 integration branch がある場合も同様）。
3. `git merge --no-commit --no-ff <candidate-branch>` を実行する。
4. 結果に応じて `MergeOutcome` を返す。
   - `Clean`: 差分なし（no-op merge）。
   - `Dirty`: merge 成功、uncommitted changes あり → commit する。
   - `Conflict`: unresolved conflicts 検出 → `git merge --abort` を即座に実行し、MERGE_HEAD 残存を確認して repo root を元 branch へ復元する。
5. 全パスで repo root を元の branch へ checkout して復元する。

conflict 発生時は `WorktreeStatus::Conflicted` へ遷移し、error event を publish する。reject 時は `Archived` へ遷移する。

**CLI:**

```bash
agentmux worktree adopt <worktree_id>   # adoption approval を queue して approval_id を表示
```

**TUI:** Arena overlay（`Ctrl-g a`）から `a` または Enter で adopt を実行できる。

## 10. Cleanup

```bash
agentmux worktree archive task-123-codex
agentmux task cleanup task-123 --keep-artifacts
```

cleanup policy:

- completed taskのworktreeは手動削除を推奨。
- artifactsは既定で保持。
- transcriptsはサイズ上限でrotate。

## 11. Security boundaries

- `.agentmux/` 内のstate、events、contextはsecretを含む可能性があるため、gitignoreへ追加する。
- worktree内のagentが`.agentmux`の全領域を読む必要がない場合、inbox/contextのみを見せる構成を検討する。
- protected pathsを設定し、agentによる編集を禁止する。

Protected path例:

```toml
[policy.protected_paths]
paths = [
  ".git/**",
  ".agentmux/state.db",
  ".agentmux/events.jsonl",
  ".env",
  "**/*secret*",
  "**/*credential*"
]
```
