# 05. Agent Adapter設計書

## 1. 目的

Agent Adapterは、Claude Code、Codex、shell、custom CLI agentをagentmuxの共通モデルへ接続する層である。v0.1では全agentをPTY-backed interactive sessionとして扱う。

## 2. 設計方針

- agentは原則として対話TUIとして起動する。
- provider固有の機能はadapter内に閉じ込める。
- orchestration側はAgentSession、Message、InputScript、StateSignalの共通モデルだけを扱う。
- screen patternだけでなく、explicit marker、hooks、sidecar fileを活用する。

## 3. trait案

```rust
#[async_trait::async_trait]
pub trait InteractiveAgentAdapter: Send + Sync {
    async fn spawn(&self, spec: AgentSpawnSpec) -> Result<AgentHandle>;

    async fn send_input_script(
        &self,
        handle: &AgentHandle,
        script: InputScript,
    ) -> Result<()>;

    async fn interrupt(&self, handle: &AgentHandle) -> Result<()>;

    async fn resize(
        &self,
        handle: &AgentHandle,
        size: TerminalSize,
    ) -> Result<()>;

    async fn snapshot_screen(
        &self,
        handle: &AgentHandle,
    ) -> Result<ScreenSnapshot>;

    async fn detect_state(
        &self,
        handle: &AgentHandle,
        snapshot: &ScreenSnapshot,
    ) -> Result<Vec<StateSignal>>;

    async fn render_handoff_prompt(
        &self,
        message: &AgentMessage,
        context_pack: &ContextPack,
    ) -> Result<String>;
}
```

## 4. AgentSpawnSpec

```rust
struct AgentSpawnSpec {
    project_id: ProjectId,
    task_id: Option<TaskId>,
    name: String,
    provider: AgentProvider,
    role: AgentRole,
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    initial_size: TerminalSize,
    permission_profile: PermissionProfile,
    startup_prompt: Option<String>,
}
```

## 5. ClaudeCodeTuiAdapter

### 5.1 起動

基本起動:

```bash
claude
```

必要に応じてCLI flags、環境変数、settingsを指定する。v0.1ではプロジェクトrootまたはagent専用worktreeで起動する。

### 5.2 初期prompt

起動後、adapterは次のようなbootstrap promptを貼る。

```text
[agentmux bootstrap]
あなたは agentmux 管理下の Claude Code session です。
role: Implementer
agent_id: agent_...
task_id: task_...

共有context:
- .agentmux/context/current-task.md
- .agentmux/context/shared.md

他agentからの新着は .agentmux/inbox/<agent-name>/ に置かれます。
agentmuxからhandoffが来たら該当ファイルを読んでください。

完了、失敗、入力要求の際は必ず AGENTMUX_RESULT JSON を出力してください。
```

### 5.3 hooks連携

Claude Code hooksが利用可能な環境では、agentmux用hookを追加して状態検出を補助する。

出力先例:

```text
.agentmux/events/claude-impl-a.jsonl
```

利用目的:

- 入力待ち通知
- 編集後通知
- command実行前検査
- task完了通知
- protected file編集ブロック

hookが使えない場合でも、screen patternとexplicit markerで動作する。

### 5.4 状態検出

Claude adapterは以下からStateSignalを作る。

- process alive
- PTY activity
- screen pattern
- `AGENTMUX_RESULT`
- hook JSONL
- mailbox read marker file

## 6. CodexTuiAdapter

### 6.1 起動

基本起動:

```bash
codex
```

sandboxやapproval profileは設定ファイルまたは起動flagsで指定する。

推奨初期profile:

```text
sandbox/workspace-write相当
approval/on-request相当
network disabledまたはmanual
```

### 6.2 slash command制御

Codex TUIではslash commandが操作点になる。agentmuxは以下をInputScriptとして送信できる。

```text
/status
/permissions
/model
/review
```

ただし、slash commandの名称や挙動はCodex側のバージョン変更に影響されるため、adapter内に閉じ込め、設定で無効化できるようにする。

### 6.3 初期prompt

```text
[agentmux bootstrap]
あなたは agentmux 管理下の Codex session です。
role: Reviewer
agent_id: agent_...

このsessionでは、agentmuxからのhandoff promptに従って作業してください。
長いcontextは .agentmux/inbox/<agent-name>/ に置かれます。
結果は AGENTMUX_RESULT JSON で終了してください。
```

### 6.4 状態検出

- process alive
- output activity
- screen pattern
- approval overlayらしき表示
- slash commandのstatus出力
- `AGENTMUX_RESULT`
- file system signal

## 7. ShellAdapter

ShellAdapterは通常のshellまたはtest commandをPTY上で起動する。

用途:

- test runner
- git diff viewer
- build watcher
- log tail
- manual shell

ShellAdapterはagentというよりsupport paneであるが、message targetにできる。

## 8. CustomAdapter

設定例:

```toml
[providers.custom.my_agent]
command = "my-agent"
args = ["--interactive"]
startup_prompt = true
result_marker = "AGENTMUX_RESULT"
```

CustomAdapterはprovider-specific screen patternを持たない。共通のexplicit markerとactivity timeoutで制御する。

## 9. Result Marker

### 9.1 必須形式

agentはturn完了時に次を出す。

```text
AGENTMUX_RESULT:
{
  "status": "completed",
  "summary": "...",
  "changed_files": [],
  "messages": [],
  "context_updates": [],
  "needs": [],
  "next": "tester"
}
```

### 9.2 JSON Schema

詳細は `schemas/agent_result.schema.json` を参照。

### 9.3 検出方法

- screen buffer tail
- raw transcript tail
- hook output
- mailbox result file

`AGENTMUX_RESULT` が壊れたJSONの場合は、agentmuxが修復を試みず、StatusProbeをagentへ送る。

## 10. Status Probe

agentがstalledまたはmarker未出力の場合、次を貼る。

```text
[agentmux status probe]
現在の状態を AGENTMUX_RESULT JSON で返してください。
まだ作業中なら status="needs_input" または "blocked" とし、必要な情報を needs に書いてください。
```

## 11. Adapter Capability

```rust
struct AgentCapabilities {
    supports_bracketed_paste: bool,
    supports_hooks: bool,
    supports_slash_commands: bool,
    supports_permission_profiles: bool,
    supports_result_marker: bool,
    can_edit_files: bool,
    can_run_commands: bool,
}
```

## 12. Provider別リスク

| Provider | リスク | 対策 |
|---|---|---|
| Claude Code | hooks/settings仕様変更 | hooksなしでも動作するfallback |
| Codex | slash command変更 | adapter-local設定、無効化可能にする |
| Shell | command暴走 | timeout、kill、approval |
| Custom | 状態検出弱い | explicit marker必須 |

## 13. 非対話execの扱い

v0.1ではagentではなくBackgroundJobとして扱う。

```rust
struct BackgroundJob {
    id: JobId,
    task_id: Option<TaskId>,
    command: Vec<String>,
    cwd: PathBuf,
    status: JobStatus,
    output_artifact_id: Option<ArtifactId>,
}
```

用途:

- formatting
- schema validation
- one-shot summary
- CI command
- diff stat

Orchestratorの主経路には使わない。
