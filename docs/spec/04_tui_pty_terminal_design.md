# 04. TUI / PTY / Terminal Engine設計書

## 1. 目的

本書は、agentmuxがClaude Code / Codexなどの対話TUIを複数paneに表示し、安全に入力を送信するためのPTY、terminal buffer、pane rendering、input automationの設計を定義する。

TUI-first方針により、この領域はv0.1の最大リスクかつ最優先PoC対象である。

## 2. 基本要件

- 各agentをPTY上で起動する。
- PTY outputをvirtual terminal bufferへ反映する。
- virtual terminal bufferをRatatui上のpaneへ描画する。
- pane resize時にPTYサイズも変更する。
- human key inputをfocused paneへ転送する。
- agentmux自動入力をtarget paneへ送る。
- Claude/Codex TUIが実用上崩れないこと。

## 3. PTYデータフロー

```text
human keyboard
  -> crossterm event
  -> agentmux client
  -> IPC input event
  -> daemon input router
  -> PTY master write
  -> agent process

agent process
  -> PTY slave stdout/stderr
  -> PTY master read
  -> terminal parser
  -> screen grid update
  -> screen diff event
  -> client render
```

## 4. PTY Supervisor

### 4.1 責務

- PTY pairを作成する。
- commandをslave側でspawnする。
- master readerを起動する。
- master writerを保持する。
- window sizeを変更する。
- process exitを監視する。
- PTY outputをevent busへ流す。

### 4.2 起動spec

```rust
struct PtySpawnSpec {
    command: String,
    args: Vec<String>,
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    size: TerminalSize,
    provider: AgentProvider,
}
```

### 4.3 起動例

Codex implementer:

```text
command: codex
cwd: .agentmux/worktrees/task-123-codex
TERM: xterm-256color
COLORTERM: truecolor
AGENTMUX_AGENT_ID: agent_...
AGENTMUX_TASK_ID: task_...
```

Claude planner:

```text
command: claude
cwd: project root or planner worktree
TERM: xterm-256color
AGENTMUX_AGENT_ID: agent_...
```

## 5. Terminal Engine

### 5.1 役割

Terminal Engineは、PTYから流れるANSI/VT escape sequenceを解釈し、pane描画に使えるscreen gridを維持する。

RatatuiはTUI widget描画用ライブラリであり、外部TUIアプリのterminal emulationを自動で提供するわけではない。そのため、PTY outputを解釈する層が必要である。

### 5.2 構造

```rust
struct TerminalBuffer {
    id: TerminalBufferId,
    size: TerminalSize,
    primary: ScreenGrid,
    alternate: ScreenGrid,
    active_screen: ActiveScreen,
    scrollback: RingBuffer<Line>,
    cursor: CursorState,
    modes: TerminalModes,
    title: Option<String>,
    dirty_regions: Vec<Rect>,
}
```

```rust
struct Cell {
    ch: char,
    style: CellStyle,
    width: CellWidth,
}
```

```rust
struct CellStyle {
    fg: Option<Color>,
    bg: Option<Color>,
    bold: bool,
    italic: bool,
    underline: bool,
    reverse: bool,
    dim: bool,
}
```

### 5.3 対応対象

v0.1で対応必須:

- printable text
- CR / LF / BS / TAB
- cursor movement
- clear screen / clear line
- SGR color/style
- alternate screen enter/exit
- scroll region basic
- terminal resize
- bracketed paste mode awareness
- title update basic

v0.1で努力目標:

- mouse reporting pass-through
- 256 color / truecolor
- double width Unicode
- OSC 8 hyperlink ignore/preserve

v0.1で対象外:

- sixel / iTerm graphics
- kitty graphics protocol
- ligature rendering
- perfect xterm compatibility
- full copy mode

## 6. Parser候補

### 6.1 vte + 自前screen buffer

長所:

- Rust crateとして軽量。
- parserとscreen bufferの責務分離が明確。
- 必要な挙動だけ実装できる。

短所:

- parserは意味付けを行わないため、screen buffer更新は自前実装になる。
- 実装量が多い。

### 6.2 vt100 crate検討

長所:

- terminal applicationを動かす用途に近い。
- screen/snapshotを得やすい可能性がある。

短所:

- Claude/Codex TUIで必要な挙動を満たせるかPoCが必要。

### 6.3 Alacritty/WezTerm系engine利用検討

長所:

- 成熟したterminal挙動を借りられる。

短所:

- crate分離、API安定性、依存重量、rendering modelの相性を確認する必要がある。

### 6.4 推奨

Phase 0では以下を比較する。

1. `portable-pty` + `vt100`
2. `portable-pty` + `vte` + 最小screen buffer
3. 既存terminal engine流用

v0.1では「Claude/Codex TUIが実用的に操作できる」ことを合格基準にし、完全互換性は求めない。

## 7. Pane Renderer

### 7.1 描画単位

- Agent TUI pane: terminal gridをそのまま描画。
- Internal pane: Ratatui widgetで描画。
- Shell pane: terminal gridを描画。

Internal pane種別（Ratatui widgetで描画するpane）の一覧:

| Internal pane | 説明 |
|---|---|
| MessageBus | message一覧 |
| ContextBoard | context item一覧 |
| ApprovalQueue | 承認待ち一覧 |
| AgentList | agent状態一覧 |
| ActivityFeed | sitrep header + event tail（feature: `activity-feed`） |

ActivityFeed paneはPTY terminal gridを持たないためresize時のPTY ioctl対象から除外する。

### 7.2 Overlay

Agent paneには次のoverlay/statusを出す。

```text
[impl-codex] status=AwaitingInput role=Implementer wt=task-123-codex msg=2 ctx=5 auto=PromptOnly
```

overlayはagent TUIの表示を邪魔しないよう、border titleまたはstatus lineに出す。

### 7.3 Dirty region

v0.1では単純にpane全体再描画でもよい。将来、dirty region based renderingへ最適化する。

## 8. Input Routing

### 8.1 Human input

```text
client key event
  -> keymap判定
  -> prefix commandでなければfocused paneへ
  -> daemon InputEvent::HumanKey
  -> PTY writer
```

### 8.2 Automatic input

```text
InputScript queued
  -> precondition check
  -> input lock acquire
  -> action sequence write
  -> input lock release
```

## 9. Bracketed Paste

長いpromptやcontextはbracketed pasteで送る。

```text
ESC [ 200 ~
本文
ESC [ 201 ~
```

Rust概念実装:

```rust
fn bracketed_paste_bytes(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b[200~");
    out.extend_from_slice(text.as_bytes());
    out.extend_from_slice(b"\x1b[201~");
    out
}
```

注意:

- target TUIがbracketed paste modeを有効にしていない場合でも、多くのterminal applicationでは通常入力として扱われる可能性がある。
- 改行を含むpromptの末尾にEnterを追加するかどうかはInputScriptで明示する。
- prompt中の制御文字はsanitizeする。

## 10. Input Lock

```rust
struct InputLock {
    owner: Option<InputOwner>,
    acquired_at: Option<DateTimeUtc>,
    expires_at: Option<DateTimeUtc>,
}
```

```rust
enum InputOwner {
    HumanClient(ClientId),
    Orchestrator,
    MessageBus,
    RecoveryAgent,
}
```

ルール:

- 人間のキー入力が直近N秒以内にあるpaneには自動入力しない。
- 自動入力中にhuman keyが来た場合、設定に応じて自動入力を中断または保留する。
- input lockにはTTLを設定し、異常時に解除されるようにする。

## 11. State Detection from Terminal

画面解析は補助信号として扱う。

ScreenPattern例:

- composer promptが見える。
- approval promptらしき語が見える。
- `/` slash command popupが見える。
- 最終出力から一定秒数経過。
- cursorが入力欄付近にある。

ただし、ScreenPattern単独で危険操作を自動承認してはならない。

## 12. Resize

pane rect変更時:

1. pane inner sizeをterminal cell数に変換。
2. TerminalBuffer sizeを変更。
3. PTY resize ioctl / APIを呼ぶ。
4. Agent processへSIGWINCH相当が伝わる。
5. 画面再描画を待つ。

## 13. Scrollback

- Agent TUIのalternate screenではscrollbackが意味を持ちにくい。
- PTY raw outputのtailをtranscript artifactとして保存する。
- paneごとに`scrollback_lines`上限を設定する。
- 将来的にcopy modeを実装する。

## 14. Terminal QA

PoCテスト対象:

- `codex` interactive TUIが起動・入力できる。
- `claude` interactive TUIが起動・入力できる。
- split/resizeで崩れない。
- 長文pasteが欠落しない。
- Ctrl+Cが対象paneにのみ送られる。
- detach/attach後に表示が復元される。
- alternate screen切替で表示が破綻しない。

## 15. 実装上の注意

- PTY output parserはpanicしてはならない。未知escapeはログに記録し無視する。
- terminal buffer更新は単一taskでserializeし、競合を避ける。
- 大量output時はbackpressureを設計する。
- hidden paneの描画頻度は落とすが、buffer更新は継続する。
