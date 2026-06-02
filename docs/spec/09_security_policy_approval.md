# 09. Security / Policy / Approval設計書

## 1. 目的

agentmuxはAI coding agentに対して自動入力を行うため、通常のterminal multiplexerよりも安全設計が重要である。本書では、自動入力、context共有、file edit、command execution、git操作、secret保護に関するpolicyを定義する。

## 2. 基本原則

- デフォルトは安全側に倒す。
- 自動入力と人間入力を分離する。
- 危険操作はapproval queueへ送る。
- workspace外への影響を最小化する。
- secretはcontext共有・export・prompt注入から除外する。
- すべての自動操作をevent logに残す。

## 3. Automation Level

```rust
enum AutomationLevel {
    ObserveOnly,
    AutoPrompt,
    AutoPromptAndApproveSafe,
    AutoWorkspaceWrite,
    AutoFullAccess,
}
```

### 3.1 ObserveOnly

- agent起動と表示のみ。
- 自動入力なし。
- messageはinbox保存のみ。

### 3.2 AutoPrompt

- prompt/message/context handoffの自動貼り付けを許可。
- file editやcommand承認はagent TUIまたは人間に任せる。
- v0.1の推奨初期値。

### 3.3 AutoPromptAndApproveSafe

- 明らかに安全なread-only操作を承認可能。
- test実行など設定でallowされたコマンドを承認可能。

### 3.4 AutoWorkspaceWrite

- workspace内の編集やテストを条件付きで許可。
- git commit/pushは手動。

### 3.5 AutoFullAccess

- v0.1では非推奨。
- 明示的な設定と毎回の警告が必要。

## 4. ApprovalPolicy

```rust
struct ApprovalPolicy {
    allow_prompt_injection: PolicyDecision,
    allow_read_only_commands: PolicyDecision,
    allow_workspace_write: PolicyDecision,
    allow_test_commands: PolicyDecision,
    allow_network: PolicyDecision,
    allow_git_commit: PolicyDecision,
    allow_git_push: PolicyDecision,
    allow_delete_files: PolicyDecision,
    allow_secret_access: PolicyDecision,
    allow_full_access: PolicyDecision,
}
```

```rust
enum PolicyDecision {
    Allow,
    AllowIfMatchesRules,
    Ask,
    Deny,
}
```

## 5. 初期policy推奨値

```toml
[automation]
level = "AutoPrompt"
auto_inject_messages = true
auto_approve_safe_prompts = false
auto_approve_file_edits = false
auto_approve_shell_commands = false
auto_full_access = false

[policy]
allow_read_only_commands = "Ask"
allow_workspace_write = "Ask"
allow_test_commands = "Ask"
allow_network = "Deny"
allow_git_commit = "Ask"
allow_git_push = "Deny"
allow_delete_files = "Ask"
allow_secret_access = "Deny"
allow_full_access = "Deny"
```

## 6. Command classification

Agentが実行しようとしているcommand、またはagentmuxが送ろうとしているInputScriptのbodyから危険度を分類する。

### 6.1 Safe-ish

- `git status`
- `git diff`
- `ls`
- `cat` for non-protected files
- configured test commands

### 6.2 Caution

- `cargo test`, `npm test`, `pytest`
- `cargo fmt`, `prettier --write`
- file edit操作
- `git add`
- package install without network disabled

### 6.3 Dangerous

- `rm -rf`
- `chmod -R`, `chown -R`
- `curl | sh`
- `wget | sh`
- `git reset --hard`
- `git clean -fdx`
- `git push`
- deploy commands
- production DB commands
- secret file read
- network exfiltration suspected commands

## 7. Approval Queue

```rust
struct ApprovalRequest {
    id: ApprovalId,
    kind: ApprovalKind,
    risk: RiskLevel,
    title: String,
    description: String,
    proposed_input: Option<InputScript>,
    command: Option<String>,
    context_refs: Vec<ContextItemId>,
    status: ApprovalStatus,
}
```

TUI表示例:

```text
Approval required: git push
agent: integrator
risk: high
reason: remote repository modification

[a] approve  [r] reject  [o] open agent pane  [d] show details
```

## 8. Input safety

```rust
enum InputSafety {
    SafePromptOnly,
    MayTriggerToolUse,
    MayModifyFiles,
    MayRunCommands,
    Dangerous,
}
```

ルール:

- SafePromptOnly以外はpolicy check必須。
- Dangerousはmanual approval必須。
- `PressEnter`のみでも、直前に危険なprompt/commandが入力されている可能性があるため、context-awareに判定する。

## 9. Secret Redaction

### 9.1 検出対象

- `AKIA...`などのcloud key pattern
- `sk-...`などAPI key pattern
- `BEGIN PRIVATE KEY`
- `password=`, `token=`, `secret=`
- `.env` content
- SSH private key
- cookie/session value

### 9.2 対応

- prompt注入前にmaskする。
- context itemに`redacted=true` metadataを付ける。
- high-confidence secretはdeliveryを停止してwarning。
- private context exportには明示flagを要求する。

## 10. Workspace boundary

- agent cwdはproject rootまたはworktreeに限定する。
- context mailboxはworkspace内に置くが、state DBやeventsへのアクセスは制限する。
- protected pathsを設定する。
- 将来、filesystem sandboxやcontainer実行を検討する。

## 11. Human override

ユーザーはいつでも以下を実行できる。

- pause automation
- stop agent
- reject approval
- force inject message
- unlock input lock
- switch automation level

ただしforce injectはevent logに残す。

## 12. Audit log

必須イベント:

- input_script.created
- input_script.approved
- input_script.injected
- approval.created
- approval.approved/rejected
- context.redacted
- dangerous_command.detected
- policy.denied
- human_override

## 13. デフォルトdeny

v0.1で自動化しないもの:

- git push
- deploy
- production DB操作
- cloud credential読み取り
- secret export
- arbitrary network request
- full access sandbox
- repository外への書き込み

## 14. Threat model

| 脅威 | 例 | 対策 |
|---|---|---|
| prompt injection | READMEに危険指示がある | contextをuntrusted扱い、approval必須 |
| secret leak | .envをagent間共有 | redaction、protected path |
| wrong pane input | 別agentにEnter送信 | target verification、input lock |
| destructive command | rm -rfを承認 | denylist、manual approval |
| agent loop | 無限に修正とテスト | timeout、retry上限 |
| context poisoning | 誤った判断が共有される | confidence、source、human pinning |
