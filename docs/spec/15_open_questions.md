# 15. 未決定事項・検討課題

## 1. Terminal Engine選定

候補:

- `vte` + 自前screen buffer
- `vt100`
- Alacritty/WezTerm系engine流用

決定にはPoCが必要。評価軸はClaude/Codex TUIの表示再現性、実装量、依存重量、resize/alternate screen対応、Unicode対応。

## 2. PTY crate選定

候補:

- `portable-pty`
- `nix`/`rustix`でUnix PTY直接実装
- platform別実装

v0.1はmacOS/Linux優先のためUnix PTY直接実装も可能。ただし将来Windows ConPTYを見据えるならportable-ptyが候補。

## 3. Claude Code hooksの自動設定方法

検討点:

- project local設定にhookを書き込むか。
- user settingsを変更しない方針にするか。
- hook設定は明示opt-inにするか。
- hookが使えない環境でのfallbackをどうするか。

## 4. Codex slash commandの安定性

slash commandは便利だが、UI/command名変更に影響されうる。adapter内に閉じ込め、feature flagで無効化できるようにする。

## 5. 状態検出のconfidence閾値

AwaitingInput判定をどこまで自動化するか。誤検出は入力事故につながるため、v0.1ではconservativeにする。

## 6. `AGENTMUX_RESULT` の強制力

agentがmarkerを出さない場合にstatus probeで再要求するが、何回までretryするか。markerなしでも人間が手動でresult登録できるUIが必要。

## 7. context検索

v0.1ではSQLite LIKEまたはFTS5で十分か。将来embedding検索を導入するか。

## 8. Multi-user対応

v0.1は同一userローカル前提。将来チーム共有する場合、認証、権限、暗号化、監査、コンフリクト解決が必要。

## 9. agent別権限分離

Claude/Codex側のsandbox/approvalに加えてagentmux独自のfilesystem sandboxを導入するか。container実行も検討対象。

## 10. 成果統合の自動化レベル

v0.1ではpromoteまで。commit/push/PR作成をいつ自動化するかはpolicy設計と利用者の信頼度次第。

## 11. UIの情報密度

agent TUIをそのまま見せるとinternal viewsの領域が狭くなる。layout template、zoom、secondary monitor対応、separate client windowなどを検討する。

## 12. transcript保存範囲

agent出力にはsecretや機密が含まれる可能性がある。どの範囲を保存し、いつrotate/deleteするかを設定可能にする必要がある。

## 13. ライセンスと配布

Rust crateとして分割公開するか、単一binary配布にするか。Claude/Codexの商標表記、設定ファイル書き換えの扱いも確認が必要。
