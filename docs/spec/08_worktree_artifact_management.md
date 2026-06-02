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

## 9. Promote flow

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
