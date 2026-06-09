# ADR-0007: `agentmux start` のレイアウト記法を split layout DSL として定義する

## Status

Accepted

## Context

`agentmux start` は comma-separated の pane 指定（例: `"agy,messages,codex"`）で起動時 pane を指定できる。しかしこの形式では次のギャップがあった。

- 分割方向（左右 vs 上下）を指定できない。すべてが暗黙的に左右並びになる。
- 入れ子レイアウト（`(planner ― impl) | messages` 等）を表現できない。
- pane ごとのサイズ比率を指定できない。均等割りのみ。
- tmux の `split-window -h`/`-v` は `-h` が水平分割（縦線・左右分割）、`-v` が垂直分割（横線・上下分割）であり、
  名前と見た目が反転して直感に反する。同様の混乱を agentmux に持ち込みたくない。

将来の team template や layout save/load（§2, §3 参照）と整合する、可読で段階的に拡張可能な記法が必要になった。

## Decision

`agentmux start` の pane 指定に **split layout DSL** を導入する。

### 記号セマンティクス

| 記号 | Unicode | 意味 | ASCII 別名 |
|------|---------|------|-----------|
| `\|` | U+007C | pane を**左右**に分割（縦の分割線を引く） | `/` |
| `―` | U+2015 | pane を**上下**に分割（横の分割線を引く） | `-`（前後に空白が必要） |
| `()` | — | グルーピング（入れ子） | — |
| `name:N` | — | 同一分割内のサイズ比率（`N` は整数） | — |
| `,` | U+002C | `\|` の後方互換エイリアス（左右並び） | — |

### EBNF の要点

```ebnf
layout   ::= expr
expr     ::= hbar_expr ("|" hbar_expr)*        (* "|" は左結合、最低優先度 *)
hbar_expr ::= atom ("―" atom)*                 (* "―" は左結合、"|" より高優先度 *)
atom     ::= "(" expr ")" | sized_pane
sized_pane ::= name (":" INTEGER)?
name     ::= IDENTIFIER
```

- 優先度（低 → 高）: `|` < `―` < `()`/`:`
- 同方向の連鎖は N-ary に flat 化する（`a | b | c` → 3-way 左右並び）。
- `,` は `|` の後方互換エイリアスであり、`,` と `|`/`―` の混在は構文エラーとする。

### ASCII 別名の使い分けとシェルの注意

- `|` はシェルのパイプ文字のためクォートが必須: `agentmux start "agy | codex"`。
- `/` はクォート不要: `agentmux start agy/codex`。
- `―`（U+2015）は一般的なキーボードから直接入力が困難なため、ASCII `-` を案内する。
- `-` を上下分割として使う場合は前後に空白を置く: `agy - codex`。ハイフン入り pane 名（`claude-code` 等）と区別するためである。

### 内部実装との命名対応

内部の `SplitDirection` enum は、TUI レイアウトエンジンの慣習（"Vertical split = 縦の線 = 左右2分割"）に従い次のように命名されている。これは本記法の記号と**名前が逆**になるため、実装者は混同しないよう注意する。

| 本 DSL の記号 | 見た目の分割線 | 内部 `SplitDirection` | 記憶の手がかり |
|---|---|---|---|
| `\|`（縦棒）| 縦線・左右分割 | `SplitDirection::Vertical` | 「縦線を引く → Vertical split」 |
| `―`（横棒）| 横線・上下分割 | `SplitDirection::Horizontal` | 「横線を引く → Horizontal split」 |

### 段階導入

**Phase 1（初期実装）: フラットな方向指定のみ**

- `agy | codex`（左右並び）
- `agy ― codex`（上下並び）
- `agy / codex`（`|` と等価、クォート不要）
- `agy,codex`（後方互換、`|` と等価）
- 3 つ以上のフラット連鎖: `agy | codex | messages`

**Phase 2（後続実装）: ネストとサイズ指定**

- `()` グルーピング: `(agy ― codex) | messages`
- `:N` サイズ比率: `agy:60 | codex:40`

Phase 1 では `()` と `:N` は構文エラーとして明確に拒否し、Phase 2 で追加する。

## 代替案と却下理由

### (a) tmux 互換の `-h`/`-v` フラグ

```bash
agentmux start agy codex --split-h
```

- `split-window -h` が「水平フラグで縦線・左右分割」というカウンターインテュイティブな tmux 慣習をそのまま持ち込む。
- 記法が宣言的でなく、pane が増えると `--split-h --split-v --split-h` の並びが混乱する。
- **却下**: 直感性・可読性の欠如。

### (b) JSON / TOML レイアウト記述

```bash
agentmux start --layout '{"split":"vertical","panes":["agy","codex"]}'
```

- CLI ワンライナーには不向きで、`agentmux start "agy | codex"` の簡潔さを失う。
- team template は既に `config.toml` の `[team.<name>]` セクションで構造化記述を提供しており、start の引数まで JSON にする必要はない。
- **却下**: CLI UX 不適合。

## Consequences

### Positive

- `|` と `―` の記号が分割線の向きと一致しており、直感的に読める（「縦棒を入れると左右に分かれる」）。
- `,` 互換により既存スクリプト・ドキュメントへの影響なし。
- Phase 1/2 の段階分割で実装リスクを制御しながら DSL を拡張できる。
- 将来の `agentmux layout save/load` や team template の `layout` フィールドと同じ構文を共有できる。

### Negative

- `|` はシェルのパイプ文字のため、コマンドラインで使う際は常にクォートが必要。
- `―`（U+2015 HORIZONTAL BAR）はキーボードから直接入力が困難。
- 内部 `SplitDirection` enum の命名が DSL 記号と逆転しているため、実装者が混同するリスクがある。

### Mitigation

- `|` の代替として `/`（クォート不要）を正式 ASCII 別名として提供する。
- `―` の代替として `-`（前後空白付き）を正式 ASCII 別名として提供する。ヘルプテキストには `―` ではなく `-` を先に案内する。
- 命名逆転に関しては本 ADR とコード内コメントに **橋渡し表**（上記「内部実装との命名対応」の表）を常駐させる。
- `|` を含む引数をクォートなしで与えた場合は、エラーメッセージに `agentmux start "agy | codex"` の形式を案内する。
