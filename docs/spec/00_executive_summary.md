# 00. エグゼクティブサマリー

## 1. 目的

`agentmux` は、Claude Code / Codex などの対話TUI型コーディングエージェントを複数起動し、それらをtmux風のpaneで同時に観察・操作しながら、自動的にメッセージ、キー入力、コンテキスト、成果物を受け渡して自律コーディングを実現するRust製CLI/TUIアプリケーションである。

本プロダクトの主目的は、コーディングエージェントの内部能力を再実装することではない。既存CLIエージェントをそのまま活かし、複数エージェントの作業分担、状態管理、入力制御、文脈同期、承認、統合を行う「コックピット」を作ることである。

## 2. 最重要方針

v0.1では、次の方針を固定する。

| 方針 | 内容 |
|---|---|
| TUI-first | Claude/Codexは対話TUIとしてPTY上で起動する |
| tmux風pane | 複数agent TUIを同一画面に分割表示する |
| daemon管理 | agent sessionはdaemon側で保持し、detach/attach可能にする |
| 自動入力 | agentmuxがprompt、slash command、Enter、Ctrl+C等を安全に送信する |
| typed message | agent間通信は自由文チャットではなく型付きメッセージとして管理する |
| context broker | shared context、mailbox file、artifactをagentへ共有する |
| worktree分離 | 実装agentは原則として専用git worktreeで作業する |
| approval gate | 破壊的操作、外部送信、本番影響、git push等は手動承認にする |
| audit log | すべての自動入力、メッセージ、context、結果をJSONLで保存する |

## 3. 成功体験

ユーザーは次のように1コマンドでタスクを起動する。

```bash
agentmux task run "refresh token bugを修正し、テストも追加して" --team claude-codex
```

`agentmux` は自動的に以下を行う。

1. taskを作成する。
2. planner、implementer、tester、reviewerのagent paneを起動する。
3. Claude Code / Codexをそれぞれ対話TUIとしてPTY上で起動する。
4. 必要なgit worktreeを作成する。
5. plannerへ初期promptを貼り付ける。
6. plannerの `AGENTMUX_RESULT` を検出する。
7. implementerへtask assignmentを注入する。
8. test paneへテスト実行を依頼する。
9. reviewerへdiffとtest resultを共有する。
10. 最終サマリ、採用候補、リスク、差分を表示する。

ユーザーはいつでも各agent paneへ直接入力でき、agentmuxの自動入力はhuman input lockにより衝突しない。

## 4. なぜ非対話exec中心にしないか

CodexやClaude Codeには非対話実行の経路もあるが、本プロダクトでは以下の理由で主経路を対話TUIにする。

- ユーザーが各agentの思考過程、diff、承認要求、実行ログをリアルタイムに見たい。
- Claude/CodexそれぞれのTUIが持つslash command、承認UI、履歴、transcript、diff表示を活かしたい。
- 長期セッションを維持しながら、途中で人間が介入できる体験が重要である。
- agent間の協調を「隠れたbatch処理」ではなく、視認可能なoperation centerとして実現したい。

ただし、非対話execは補助jobとして残す。たとえば要約、静的レビュー、schema validation、CI的な単発テストに利用できる。

## 5. v0.1の到達点

v0.1では、次が実現できればよい。

```text
- Claude Code / Codexを複数PTYで起動できる
- tmux風paneに表示できる
- detach/attachしてもagent sessionが残る
- 任意agentへ安全にprompt pasteできる
- agent間messageをprompt化して注入できる
- contextをinlineまたはmailbox fileで共有できる
- AGENTMUX_RESULT markerを検出して次agentへroutingできる
- agent別worktreeで実装・テスト・レビューを進められる
- 自動入力・承認・イベントがaudit logに残る
```

## 6. 主な技術リスク

| リスク | 内容 | 対策 |
|---|---|---|
| terminal emulation | Claude/Codex TUIをpane内で正しく描画する必要がある | PTY PoCを最初に行う。vte/vt100/既存terminal engineを比較する |
| screen scraping脆弱性 | 画面文言依存はバージョン変更に弱い | explicit marker、hooks、sidecar file、activity signalを併用する |
| 自動入力暴走 | 誤ったpaneへEnterや承認を送る危険 | input lock、precondition、approval policyを必須にする |
| context肥大化 | 長いログを貼りすぎるとagentの会話が汚れる | mailbox fileとcontext cardを使い分ける |
| worktree衝突 | 複数agentが同じ作業ツリーを編集すると衝突する | 1 implementer = 1 worktreeを原則にする |
| セキュリティ | secret流出や危険コマンドの自動承認 | redaction、denylist、manual approval、workspace jailを導入する |

## 7. 実装優先順位

1. PTY host PoC
2. Terminal engine / pane rendering
3. Daemon + session persistence
4. Input automation
5. Agent message bus
6. Context broker / mailbox file
7. Worktree manager
8. Orchestrator state machine
9. Approval queue
10. Test strategy / hardening

## 8. 判断済み事項

詳細は `adrs/` を参照。

- ADR-0001: TUI-firstを採用する。
- ADR-0002: PTY + terminal engineをv0.1必須とする。
- ADR-0003: worktree isolationを採用する。
- ADR-0004: typed message busを採用する。
- ADR-0005: 長文context共有にはmailbox fileを採用する。
