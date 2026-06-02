# 01. プロダクト要求仕様書

## 1. プロダクト名

仮称: `agentmux`

## 2. 一文説明

`agentmux` は、複数の対話TUI型コーディングエージェントを同一TUI内で起動・監視・制御し、agent間メッセージング、コンテキスト共有、自動キー送信、worktree分離、承認管理によって自律コーディングを支援するRust製CLI/TUIである。

## 3. 想定ユーザー

- CLI中心に開発するソフトウェアエンジニア
- Claude Code / Codex など複数のcoding agentを併用する開発者
- 複数案の実装、レビュー、テストを並列化したい個人開発者
- agentの自律実行を監視しつつ、必要時だけ介入したい開発者
- 将来的にはチーム内の標準化されたagent workflowを構築したい開発チーム

## 4. 主要ユースケース

### UC-001: 複数agentを起動して並列実装させる

ユーザーは1つのtaskを投入する。agentmuxはClaude/Codexを複数起動し、それぞれ別worktreeで異なる実装案を作らせる。

### UC-002: plannerの結果をimplementerへ自動配送する

planner agentが作業分解を出す。agentmuxはその結果を解析し、typed messageとしてimplementerへ送る。対象agentが入力可能になったらpromptとして自動pasteする。

### UC-003: testerの失敗ログを実装agentへ共有する

tester paneがテスト失敗を検出する。agentmuxは失敗ログをartifact化し、必要部分だけcontext cardに要約し、実装agentのmailboxに保存したうえでhandoff promptを注入する。

### UC-004: reviewerがdiffを確認し、統合候補を選ぶ

reviewer agentは各worktreeのdiff、test result、contextを読んでレビューする。agentmuxはreview resultを集約し、採用候補とリスクをユーザーに提示する。

### UC-005: 人間が任意のagentへ直接介入する

ユーザーはpaneをfocusし、通常のClaude/Codex TUIとして直接入力できる。人間入力中はagentmuxの自動入力は保留される。

### UC-006: セッションをdetach/attachする

長時間実行中のagent sessionをdetachし、後でattachする。daemonがagent processとPTYを維持する。

### UC-007: contextを共有黒板として管理する

ユーザーまたはagentmuxは、設計判断、エラー、テスト結果、ファイル参照、作業ルールをcontext itemとして保存し、agentへ注入する。

## 5. 機能要求

### 5.1 TUI / Pane管理

- 複数のPTY-backed paneを同時表示できること。
- paneはsplit horizontal / split vertical / zoom / focus移動をサポートすること。
- agent pane、internal pane、shell paneを区別できること。
- agent paneにはrole、status、worktree、unread messages、automation levelをoverlay表示できること。
- detach/attach後もdaemon上のagent sessionが残ること。

### 5.2 PTY / Terminal

- 各agentをPTY上で起動できること。
- PTY stdinへkey sequence / paste / raw bytesを送信できること。
- PTY stdout/stderrをterminal bufferに反映できること。
- alternate screen、cursor、color、scrollback、resizeを最低限扱えること。
- agent TUIが実用的に表示・操作できること。

### 5.3 Agent管理

- agent providerとしてClaudeCode、Codex、Shell、Customを扱えること。
- agent roleとしてplanner、implementer、reviewer、tester、integrator等を持てること。
- agent statusを推定・表示できること。
- agentごとにcwd、worktree、environment、permission profileを指定できること。

### 5.4 自動入力

- promptをbracketed pasteで送信できること。
- Enter、Esc、Tab、Ctrl+C、slash command等を送信できること。
- 自動入力にはpreconditionを設定できること。
- 人間入力との衝突を防ぐinput lockを持つこと。
- 自動入力内容はevent logに記録されること。

### 5.5 メッセージング

- agent間メッセージはtyped messageとして管理すること。
- targetはagent、role、team、task、broadcastを指定できること。
- delivery modeはInboxOnly、InjectWhenIdle、InjectImmediately、RequireHumanApprovalを持つこと。
- messageはprompt rendererによって対話TUI向けの文章へ変換されること。

### 5.6 Context管理

- context itemとしてTaskBrief、Decision、ErrorLog、TestResult、DiffSummary、FileReference等を扱うこと。
- 短いcontextはinlineでpromptに含められること。
- 長いcontextはmailbox fileに保存し、agentにファイル参照として渡せること。
- secret redactionを実行できること。
- context itemはmessage、artifact、agent sessionと関連付けられること。

### 5.7 Worktree管理

- task単位またはagent単位でgit worktreeを作成できること。
- implementer agentは専用worktreeで作業すること。
- diff、changed files、test resultをartifactとして取得できること。
- integration candidateを作れること。

### 5.8 Orchestration

- task作成時にteam templateからagent群を起動できること。
- planner -> implementer -> tester -> reviewer -> integrator の基本フローを実行できること。
- `AGENTMUX_RESULT` markerを検出して状態遷移できること。
- stalled agentを検出し、status probeまたはhuman interventionを要求できること。

### 5.9 承認・安全性

- automation levelを設定できること。
- 危険な自動入力はapproval queueに送ること。
- git push、deploy、本番DB操作、secret読み取りはデフォルトで手動承認にすること。
- destructive commandはdenylist/confirm listで検出すること。
- audit logを残すこと。

## 6. 非機能要求

### 6.1 信頼性

- daemonクラッシュ時にmetadata、message、context、event logが失われないこと。
- agent processはdaemonが生きている間は維持されること。
- agentがexitした場合にstatusとexit codeを記録すること。

### 6.2 操作性

- tmux経験者が理解しやすいprefix key体系を採用すること。
- ただしagent操作を優先した独自keymapを持つこと。
- human interventionは常に可能であること。

### 6.3 拡張性

- agent providerをtraitで差し替えられること。
- Claude/Codex以外のCLI agentをcustom providerとして登録できること。
- MCP/SDK integrationはv0.2以降のadapterとして追加できること。

### 6.4 セキュリティ

- daemon socketとstate DBは同一userのみアクセス可能にすること。
- workspace外への編集やcontext exportに制限を持つこと。
- secret redactionとprivate contextの扱いを明確化すること。

### 6.5 パフォーマンス

- 複数PTY出力を低遅延でscreen bufferへ反映できること。
- inactive paneは描画頻度を落とせること。
- scrollbackは上限付きring bufferにすること。

## 7. MVPスコープ

v0.1で必須:

- daemon / client
- Claude/Codex PTY起動
- pane split/focus/zoom/detach/attach
- automatic paste / key send
- input lock
- typed message bus
- context board / mailbox file
- worktree manager
- `AGENTMUX_RESULT` parser
- approval queue最小版
- JSONL event log
- SQLite metadata store

v0.1で後回し:

- MCP/SDK深い統合
- remote daemon
- cloud sync
- PR自動作成
- multi-user collaboration
- tmux完全互換
- 高度なcopy mode/mouse support

## 8. 受け入れ条件

### AC-001: agent pane

`agentmux task run` により、少なくとも2つのagent paneでClaude/Codex TUIを起動し、通常操作できる。

### AC-002: 自動prompt注入

messageを作成し、対象agentが入力待ちになったらagentmuxがpromptとしてpasteできる。

### AC-003: context共有

長いテストログをmailbox fileに保存し、agentへファイルパスとして通知できる。

### AC-004: worktree分離

2つのimplementerが別worktreeで同時にファイル編集しても衝突しない。

### AC-005: 自律フロー

plannerのresult markerからimplementerへのmessage配送、testerへのテスト依頼、reviewerへのレビュー依頼まで自動で進む。

### AC-006: 安全停止

人間入力中は自動入力が実行されない。危険操作はapproval queueに出る。
