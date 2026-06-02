# 14. 運用・トラブルシューティングRunbook

## 1. 目的

本書は、agentmux利用中に発生しうる問題への対応方法をまとめる。v0.1では個人利用・ローカルdaemonを前提とする。

## 2. 基本診断

```bash
agentmux doctor
```

確認項目:

- daemonが起動しているか
- socketに接続できるか
- configがparseできるか
- state DBが読み書き可能か
- `claude` commandが見つかるか
- `codex` commandが見つかるか
- PTYが作成できるか
- terminal raw modeが使えるか
- git worktreeが使えるか

## 3. daemonが起動しない

確認:

```bash
agentmux daemon status
ls -l $XDG_RUNTIME_DIR/agentmux/
```

対応:

1. 古いsocketが残っていれば削除。
2. state DBの権限を確認。
3. config TOML parse errorを確認。
4. foreground modeで起動してログを見る。

```bash
agentmux daemon start --foreground --log-level debug
```

## 4. TUIが崩れる

原因候補:

- terminal engine未対応escape sequence
- pane sizeが小さすぎる
- alternate screen対応不備
- Unicode幅の問題
- agent TUI側のバージョン差分

対応:

1. pane zoomする。
2. terminal sizeを広げる。
3. screen snapshot artifactを保存する。
4. debug logでunknown escapeを確認する。

```bash
agentmux agent snapshot impl-codex --save
```

## 5. 自動入力されない

確認:

```bash
agentmux message list --agent impl-codex
agentmux agent status impl-codex
agentmux approval list
```

原因候補:

- target agentがAwaitingInputではない。
- human input lockが残っている。
- policyによりapproval待ち。
- delivery modeがInboxOnly。
- precondition不一致。

対応:

```bash
agentmux message inject msg_123 --to impl-codex --manual
```

## 6. 間違った入力を送った

対応:

1. 対象agent paneを確認。
2. 必要ならCtrl+Cで中断。
3. event logから送信内容を確認。
4. context/messageを修正して再送。

```bash
agentmux agent interrupt impl-codex
agentmux events tail --agent impl-codex
```

## 7. agentが停止した

確認:

```bash
agentmux agent status impl-codex
agentmux agent logs impl-codex
```

対応:

- exit codeを確認。
- transcript artifactを確認。
- 必要なら同じworktreeで再起動。

```bash
agentmux agent restart impl-codex --same-worktree
```

## 8. agentがStalledになった

対応順:

1. StatusProbeを送る。
2. 反応がなければhuman intervention。
3. 必要ならCtrl+C。
4. 最後にrestart。

```bash
agentmux agent probe impl-codex
agentmux agent interrupt impl-codex
```

## 9. worktree conflict

確認:

```bash
agentmux worktree diff task-123-codex
agentmux worktree status task-123-codex
```

対応:

- integrator paneで手動解消。
- conflict summaryをcontext化。
- reviewerへ再レビュー依頼。

## 10. secret検出

secret検出時、agentmuxはdeliveryを止める場合がある。

対応:

1. context itemを確認。
2. 必要ならredaction版を生成。
3. private contextとして保持するか削除する。

```bash
agentmux context show ctx_123
agentmux context redact ctx_123
```

## 11. ログとファイル

```text
<project>/.agentmux/state.db
<project>/.agentmux/events.jsonl
<project>/.agentmux/artifacts/
<project>/.agentmux/inbox/
```

## 12. 安全な停止

```bash
agentmux task pause task-123
agentmux detach
```

全停止:

```bash
agentmux task cancel task-123
agentmux daemon stop
```

## 13. バグ報告に必要な情報

- agentmux version
- OS / terminal emulator
- `agentmux doctor` output
- relevant event log excerpt
- screen snapshot artifact
- reproduction steps
- Claude/Codex versions

ただしsecretを含まないようredactionすること。
