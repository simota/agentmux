# ADR-0003: agent別worktree分離を採用する

## Status

Accepted

## Context

複数の実装agentが同じworking treeを同時に編集すると、ファイル競合、テスト結果混線、変更意図の混同が発生する。

## Decision

implementer agentには専用git worktreeを割り当てる。planner/reviewer/tester/integratorは用途に応じてread-only、target worktree、integration worktreeを使う。

## Consequences

### Positive

- 複数案を安全に並列実装できる。
- diff/test/reviewを候補ごとに比較しやすい。
- 失敗した候補を破棄しやすい。

### Negative

- ディスク使用量が増える。
- worktree cleanupが必要。
- merge conflict処理が必要。

### Mitigation

- archive/cleanupコマンドを提供する。
- promote flowで人間確認を挟む。
