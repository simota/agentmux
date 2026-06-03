# 15. 未決定事項・検討課題

この章は v0.1 の実装ゲートで扱う未決定事項を記録する。各項目は v0.1 の決定または先送り理由を持ち、実装が依存してよい前提を明確にする。

## 1. Terminal Engine選定

候補:

- `vte` + 自前screen buffer
- `vt100`
- Alacritty/WezTerm系engine流用

**v0.1 decision:** `vte` + agentmux-owned `ScreenGrid` を採用する。

理由:

- `agentmux-terminal` が `vte` parser と `ScreenGrid` 更新を実装済みで、adapter snapshot も `ScreenGrid` ベースに統一済み。
- 依存重量を抑えつつ、spec §04 の v0.1 必須範囲（printable text、cursor movement、clear、SGR、alternate screen、resize）を段階的に拡張できる。
- Alacritty/WezTerm 系 engine は成熟しているが、v0.1 の組み込み API と依存重量のリスクが大きい。

Deferred:

- `vt100` と成熟 terminal engine の再評価は、Claude/Codex TUI の表示再現性で `ScreenGrid` 実装に具体的な欠落が見つかった場合に行う。

## 2. PTY crate選定

候補:

- `portable-pty`
- `nix`/`rustix`でUnix PTY直接実装
- platform別実装

**v0.1 decision:** `portable-pty` を採用する。

理由:

- `agentmux-pty` が `portable-pty` による PTY spawn/read/write/resize を実装済み。
- spec §04 の macOS/Linux 優先要件を満たしつつ、将来 Windows ConPTY 対応の余地を残せる。
- 直接 `nix`/`rustix` 実装より provider adapter との境界が単純になる。

Deferred:

- OS 固有の挙動差や性能問題が実測で出た場合のみ、platform-specific backend を追加する。

## 3. Claude Code hooksの自動設定方法

検討点:

- project local設定にhookを書き込むか。
- user settingsを変更しない方針にするか。
- hook設定は明示opt-inにするか。
- hookが使えない環境でのfallbackをどうするか。

**v0.1 decision:** hooks は明示 opt-in とし、agentmux は user settings を自動変更しない。

理由:

- user/global settings の変更は利用者環境への影響が大きい。
- spec §05 は hooks を補助信号として扱い、hooks が無い場合も screen pattern、explicit marker、sidecar file で動作する方針を定めている。
- provider capability は config から読み込み、`supports_hooks=false` の環境を正常系として扱える。

Deferred:

- project-local hooks の自動生成は、設定ファイルへの明示 opt-in と dry-run 表示を備えてから追加する。

## 4. Codex slash commandの安定性

**v0.1 decision:** slash command は Codex adapter 内に閉じ込め、config capability で無効化可能にする。

理由:

- Codex 側の UI/command 名は変更されうるため、orchestrator や daemon IPC に slash command 名を漏らさない。
- spec §05 の `supports_slash_commands` を provider capability として扱い、無効時は explicit prompt / `AGENTMUX_RESULT` / status probe に fallback する。

Deferred:

- slash command 名や出力の互換性テストは、実 Codex CLI のバージョン固定が可能になった時点で追加する。

## 5. 状態検出のconfidence閾値

**v0.1 decision:** AwaitingInput 自動判定は conservative にし、単独の低信頼 screen pattern だけでは自動入力しない。

理由:

- 誤検出は agent TUI への入力事故につながる。
- 実装は `StateSignal.confidence` と explicit marker / hook / activity signal を併用できる。
- v0.1 の自動 handoff は `AGENTMUX_RESULT` を主信号とし、曖昧な場合は status probe または人間承認へ倒す。

Deferred:

- 閾値の数値チューニングは、実 transcript と false positive/negative の記録が集まってから行う。

## 6. `AGENTMUX_RESULT` の強制力

**v0.1 decision:** `AGENTMUX_RESULT` を orchestrator の標準完了信号とし、欠落または壊れた JSON は修復せず status probe を送る。

理由:

- spec §05 は壊れた JSON を agentmux 側で修復しない方針を定めている。
- 実装は `parse_agent_result_marker` と orchestrator status probe を備え、shell-stub E2E も marker を必須信号として検証している。
- marker なし完了を自動推測しないことで、誤った handoff を避ける。

Deferred:

- retry 回数、backoff、手動 result 登録 UI は telemetry と利用者操作設計が必要なため v0.2 以降に送る。

## 7. context検索

**v0.1 decision:** in-memory broker と SQLite-compatible な文字列検索を基本にし、embedding 検索は導入しない。

理由:

- v0.1 の `ContextBroker` は keyword/source/tag ベースの検索で message/context handoff を満たす。
- embedding は network、model、index 永続化、privacy policy を追加で設計する必要があり、v0.1 の local-first 範囲を超える。

Deferred:

- FTS5 か embedding は、context 件数と検索失敗例が増えた後に storage migration と privacy policy を含めて再検討する。

## 8. Multi-user対応

**v0.1 decision:** 同一 user のローカル daemon のみをサポートする。

理由:

- daemon socket は local IPC 前提で、remote/multi-user の認証境界を持たない。
- spec §12 でも multi-user shared context は v0.2 以降候補であり、v0.1 は local orchestration と PTY/TUI 安定化を優先する。

Deferred:

- team/shared daemon は認証、権限、暗号化、監査、コンフリクト解決を含む別設計にする。

## 9. agent別権限分離

**v0.1 decision:** provider-native sandbox/permission profile と policy approval を使い、agentmux 独自 filesystem sandbox や container 実行は導入しない。

理由:

- `PermissionProfile` は Codex sandbox / approval と Claude permission mode に整合済み。
- spec §09 は destructive/risky action を approval queue へ送る方針で、v0.1 の安全境界は provider sandbox と policy gate を組み合わせる。
- 独自 sandbox は OS 権限、path mapping、PTY 起動、artifact 保存に影響し、v0.1 の範囲を超える。

Deferred:

- filesystem sandbox/container 実行は、multi-user または untrusted agent 実行を扱う段階で設計する。

## 10. 成果統合の自動化レベル

**v0.1 decision:** 自動化は `worktree promote` までとし、commit/push/PR 作成はデフォルト手動にする。

理由:

- spec §07 は reviewer approve 後も final summary と promote command を提示し、完全自動 merge は行わない。
- spec §09 は git commit/push を manual/approval 対象として扱う。
- PR 作成は credential、remote policy、branch naming、review workflow に影響する。

Deferred:

- commit/push/PR 作成は policy 設定、dry-run、approval audit、rollback guidance を備えてから追加する。

## 11. UIの情報密度

**v0.1 decision:** standard layout、focus、zoom、layout save/load/list を提供し、secondary monitor や separate client window は対象外にする。

理由:

- TUI keymap と layout IPC は v0.1 の pane 操作に必要な最小機能を満たす。
- agent TUI の表示を優先し、internal views は status line、message/context pane、layout preset で補助する。
- 複数 window は client state 同期と focus/input routing の設計が増える。

Deferred:

- secondary monitor と separate client window は、single-client TUI の操作課題が明確になってから追加する。

## 12. transcript保存範囲

**v0.1 decision:** transcript は full log ではなく tail/artifact 用の必要最小範囲を保存し、event log は rotation 対象にする。

理由:

- agent 出力には secret や機密が含まれる可能性がある。
- spec §06 は context を共有黒板として扱い、各 agent の全 transcript を丸ごと共有しない。
- daemon/store は log rotation と crash recovery を実装し、永続化量を制御できる。

Deferred:

- transcript retention、redaction、delete policy は config 化と secret scanner の検証を含めて追加する。

## 13. ライセンスと配布

**v0.1 decision:** workspace crate は内部境界として維持し、利用者向け配布単位は単一 `agentmux` binary を優先する。

理由:

- v0.1 は daemon、CLI、TUI、PTY adapter の統合動作が主目的であり、crate ごとの public API 安定化はまだ不要。
- 単一 binary は導入手順、version skew、support matrix を単純にできる。
- Claude/Codex の商標表記と settings 変更は docs と opt-in 操作で扱い、自動 user settings 変更は行わない。

Deferred:

- Rust crate 分割公開、package manager 配布、商標レビューは v0.1 の利用者フィードバック後に release checklist として扱う。
