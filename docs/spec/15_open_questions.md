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

## 14. Meeting thread の永続化と進行制御

**v0.1 decision:** マルチパーティ会議(`MessageThread`、ADR-0006)は messages と同じく in-memory + event log(`thread.opened`/`thread.closed`)で管理し、SQLite には永続化しない。発言順序の調停(turn-taking)は実装せず、`max_messages_per_participant`(既定 7)の発言上限のみ daemon が強制する。

理由:

- messages 本体が v0.1 で SQLite 非永続のため、thread だけ永続化しても再起動後に整合しない。
- 進行制御は opener(人間 or facilitator agent)が `kind: Question` で指名する運用で十分に始められる。

Deferred:

- thread/messages の SQLite 永続化は同時に設計する。
- 発言順序の調停(ラウンドロビン、指名制の強制)は実運用での会議ログを見てから判断する。

## 15. レイアウト DSL の絶対サイズ単位

検討点:

- `:N` による比率指定（`agy:60 | codex:40`）に加え、`:20c`（セル数）や明示パーセント（`:50%`）などの絶対・パーセント単位を Phase 1 から導入すべきか。
- 比率と絶対値・パーセントを混在させた場合の残余セル計算の仕様をどうするか。

**v0.1 decision:** Phase 1 はサイズ指定自体を未サポートとし、`:N` 構文を含む記法は構文エラーとして拒否する（`NotYetSupported`）。比率・絶対値・パーセントの区別は、Phase 2 でサイズ指定を導入するときに一括して設計する。

理由:

- Phase 1 のスコープはフラットな分割方向指定のみであり、サイズ計算の仕様を先行導入すると実装範囲が大きくなりすぎる。
- 均等割りで十分なユースケースが Phase 1 では大多数を占めると想定される。

Deferred:

- `:N`（比率）・`:Nc`（セル数）・`:N%`（パーセント）の文法と残余計算規則は Phase 2 で一括設計する。

## 16. `focused/zoom` とネストツリーの統合

検討点:

- `agentmux start "(agy ― codex) | messages"` のような DSL 由来の固定ネストツリーと、`Ctrl-g %`/`Ctrl-g "` による実行時動的分割をどう統合するか。
- 動的分割後に元の DSL ツリー構造へ戻る操作をどう設計するか。

**v0.1 decision:** Phase 1 はフラット構造のみで、既存の `set_split_direction` をそのまま使う。動的分割（`Ctrl-g %`/`Ctrl-g "`）の挙動は現行どおりに維持し、ツリー統合は Phase 2 で設計する。

理由:

- Phase 1 の DSL がフラットに限定されている間は、内部レイアウト表現も `SplitDirection` + pane list のフラットモデルで十分。
- ネストツリーと動的分割の統合は、IPC プロトコルとの連携を含む非自明な設計問題であり、フラット実装後の知見を得てから着手する。

Deferred:

- `LayoutNode` ツリーと動的分割の統合モデルは Phase 2 で設計する（`## 19. LayoutNode 型の置き場所` も参照）。

実装状況（Phase 2 完了時点）:

- ネストツリーは `PaneLayout.root: LayoutNode` として導入済み。動的分割（`Ctrl-g %`/`"`）と実行時 spawn は引き続きフラット運用（root 直下末尾に追加・`set_split_direction` は root に作用）とし、focus 巡回は葉の DFS 順で行う。DSL 由来の固定ネストと動的分割の双方向な往復編集（任意ノードへの挿入・分割の解除）は依然 deferred。

## 17. `-` の追加 ASCII 別名

検討点:

- `―`（U+2015）の ASCII 別名として `-`（前後空白必須）を採用したが、`--`（二重ハイフン）や `_`（アンダースコア）を追加の別名として認めれば前後空白ルールの煩雑さを回避できるか。
- 別名が増えると構文が複雑になり、エラーメッセージが難しくなるトレードオフがある。

**v0.1 decision:** Phase 1 では正式な上下分割記号を `―`（U+2015）と `-`（前後空白あり）のみとし、`--` や `_` などの追加別名は導入しない。

理由:

- `-` は「前後空白があれば上下分割、なければハイフン（pane 名の一部）」という1つのルールで覚えられる。
- 別名を増やすほどパーサーの分岐が増え、曖昧性エラーのケースが増える。
- 利用実績がない段階で別名を先行追加するとのちに削除しにくくなる。

Deferred:

- 前後空白ルールが実運用で不評であれば、追加別名を Phase 2 以降で再検討する。

## 18. `,` の最終的な扱い

検討点:

- `,` を後方互換エイリアスとして無期限維持するか、将来のメジャーバージョンで hard-deprecate するか。
- soft-deprecate（警告表示のみ）の中間段階を設けるかどうか。

**v0.1 decision:** `,` は `|` の後方互換エイリアスとして**警告なしで無期限維持**（soft-deprecate）する。hard-deprecate はメジャーバージョン（v1.0 以降）の判断事項として保留し、v0.x の間は行わない。

理由:

- 既存の `agentmux start "agy,codex"` の記法はドキュメント・スクリプト・ユーザー習慣に広く根付いており、警告を出すだけでユーザー体験が悪化する。
- `|` に統一するユーザーメリットは現時点では小さく、移行コストに見合わない。

Deferred:

- `,` の hard-deprecate は v1.0 の API 安定化判断と同時に検討する。

## 19. `LayoutNode` 型の置き場所

検討点:

- Phase 2 でネストレイアウトツリーを導入するとき、`LayoutNode` 型を `agentmux-core`（ドメイン層）に置くか、`agentmux-tui` のローカル型として閉じ込めるか。
- IPC で daemon → client にレイアウトツリーを送る場合は `agentmux-ipc` か `agentmux-core` に置く必要がある。

**v0.1 decision:** Phase 1 はネストツリーを導入しないため `LayoutNode` 型を作成しない。Phase 2 で IPC 経由のツリー送信が必要になった時点で、`agentmux-core` への昇格を検討する。

理由:

- 型を早期に `agentmux-core` に置くとすべての依存 crate に影響し、型変更のコストが高くなる。
- `agentmux-tui` ローカルで始め、IPC 連携が必要になってから昇格するのがリスク駆動の順序と整合する。

Deferred:

- Phase 2 の ネストツリー設計時に、`agentmux-tui` / `agentmux-ipc` / `agentmux-core` のどこに置くかを改めて決定する。

実装状況（Phase 2 完了時点）:

- `LayoutNode` / `LayoutChild` は `agentmux-tui` のローカル型として実装した。`PaneSnapshot` は in-process の内部状態にとどまり IPC の wire には乗らないため、`agentmux-core` への昇格も protocol version の変更も不要だった。daemon がレイアウトを永続化・送信する機能を追加する段階で `agentmux-core` 昇格を再検討する。

## 20. 最小幅クランプと端末幅不足時の折り返し

検討点:

- 端末幅が狭い場合、`Constraint::Percentage` の計算結果が 0 セルや 1 セルになる pane が生じる。この pane を折りたたむか、最小幅を強制するか、エラーを出すか。
- `:N` サイズ指定（Phase 2）導入後は、ユーザー指定の比率が端末幅を超過する場合の正規化規則も必要になる。

**v0.1 decision:** Phase 1 は既存の `even_constraints`（均等割り・余りセルを先頭 pane に加算）の挙動を踏襲し、最小幅クランプや折り返しロジックを新たに追加しない。

理由:

- Phase 1 のフラットレイアウトは均等割りのみであり、0 セル問題は極端に小さい端末でのみ発生する。
- 最小幅規則はサイズ指定（Phase 2）と合わせて設計しないと不整合が生じる。

Deferred:

- Phase 2 のサイズ指定導入時に最小幅クランプ規則と正規化アルゴリズムを合わせて設計する。

実装状況（Phase 2 完了時点）:

- `:N` サイズ比率の正規化（比率を `Constraint::Percentage` に変換し合計 100、省略 pane へ残余を均等配分、余りは先頭から）は実装済み。一方で**最小幅クランプは未導入**であり、極端に狭い端末や偏った比率では 0 セルの pane が生じうる。最小幅クランプ／折りたたみ規則は引き続き未決のまま残す。
