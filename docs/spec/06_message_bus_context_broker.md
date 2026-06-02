# 06. Message Bus / Context Broker設計書

## 1. 目的

agentmuxでは、複数の対話TUI agentが相互に作業結果、質問、失敗ログ、レビューコメント、設計判断を受け渡す。この連携を安定させるため、自由文チャットではなくtyped message busとcontext brokerを導入する。

## 2. 設計原則

- agent間通信は必ずdaemon上のMessage Busを通す。
- messageは型、送信元、宛先、context/artifact参照、delivery modeを持つ。
- agent TUIへ配送するときだけprompt textへ変換する。
- contextは共有黒板として扱い、各agentの全transcriptを丸ごと共有しない。
- 長文contextはmailbox fileに保存し、promptには参照パスだけを書く。
- context共有時はredactionを行う。

## 3. Message Bus

### 3.1 AgentMessage

```rust
struct AgentMessage {
    id: MessageId,
    task_id: Option<TaskId>,
    from: MessageSource,
    to: MessageTarget,
    kind: MessageKind,
    priority: Priority,
    body: String,
    context_refs: Vec<ContextItemId>,
    artifact_refs: Vec<ArtifactId>,
    delivery_mode: DeliveryMode,
    delivery_status: DeliveryStatus,
    requires_response: bool,
    created_at: DateTimeUtc,
    delivered_at: Option<DateTimeUtc>,
}
```

### 3.2 MessageKind

| Kind | 用途 |
|---|---|
| TaskAssignment | 作業依頼 |
| Question | 質問 |
| Finding | 調査結果 |
| PatchProposal | patch提案 |
| ReviewComment | レビュー指摘 |
| TestResult | テスト結果 |
| FailureReport | 失敗報告 |
| Decision | 判断記録 |
| Handoff | 汎用引き継ぎ |
| ApprovalRequest | 承認要求 |
| ContextUpdate | 共有context更新 |
| StatusProbe | 状態確認 |

### 3.3 DeliveryMode

```rust
enum DeliveryMode {
    InboxOnly,
    InjectWhenIdle,
    InjectImmediately,
    RequireHumanApproval,
}
```

- InboxOnly: TUIへ自動注入せず、inboxに保存する。
- InjectWhenIdle: agentが入力可能と判断されたら注入する。
- InjectImmediately: preconditionが許す限り即注入する。
- RequireHumanApproval: approval queueに出す。

### 3.4 DeliveryStatus

```rust
enum DeliveryStatus {
    Queued,
    Rendered,
    WaitingForAgent,
    WaitingForApproval,
    Injecting,
    Delivered,
    Failed,
    Cancelled,
}
```

## 4. Prompt Renderer

Messageはagent provider別にpromptへ変換する。

### 4.1 共通template

```text
[agentmux handoff]
from: {from}
kind: {kind}
priority: {priority}
message_id: {message_id}

message:
{body}

attached context:
{inline_context_or_paths}

required:
- 内容を確認してください
- 必要なら作業してください
- 完了時は必ず AGENTMUX_RESULT JSON を出力してください
```

### 4.2 Claude/Codex差分

v0.1では大きな差分を設けず、以下のみprovider別に調整する。

- Codexにはworkspace内path参照を明確化する。
- Claudeにはhook/mailboxの前提を説明する。
- slash commandを送る場合はCodexAdapter内でのみ扱う。

## 5. Context Broker

### 5.1 ContextItem

ContextItemはagent間で再利用できる作業記憶である。

| Kind | 例 |
|---|---|
| ProjectSummary | プロジェクト概要 |
| ArchitectureNote | 認証層の責務分離 |
| CodingRule | public APIを壊さない |
| TaskBrief | 今回のタスク説明 |
| FileReference | src/auth.rs を確認 |
| DiffSummary | 変更ファイルと要点 |
| TestResult | cargo test結果 |
| ErrorLog | 失敗ログ抜粋 |
| AgentFinding | agentの調査結果 |
| Decision | 採用判断 |
| Risk | 既知リスク |
| OpenQuestion | 未解決質問 |
| HandoffSummary | agent間引き継ぎ要約 |

### 5.2 ContextScope

```text
Global
  -> Project
      -> Task
          -> AgentSession
              -> Message
```

優先度:

```text
Message attached context > AgentSession > Task > Project > Global
```

### 5.3 ContextPack

Agentへ注入するcontextは毎回全量ではなく、ContextPackに選別する。

```rust
struct ContextPack {
    inline_items: Vec<ContextItem>,
    mailbox_files: Vec<MailboxFile>,
    artifact_refs: Vec<ArtifactRef>,
    omitted_items: Vec<ContextItemId>,
}
```

選別基準:

- messageに明示添付されたもの
- target agent roleに関連するもの
- task current phaseに関連するもの
- priority/riskが高いもの
- token/文字数上限に収まるもの

## 6. Inline Context

短いcontextはprompt内に入れる。

例:

```text
[shared context]
- CodingRule: public APIの破壊的変更は禁止
- Decision: refresh token検証はservice層に寄せる
- Risk: middleware側の責務重複に注意
```

使用対象:

- rule
- decision
- short note
- small finding
- task brief

## 7. Mailbox File

長いcontextはtarget agentごとのmailboxに保存する。

```text
.agentmux/
  inbox/
    impl-codex/
      msg-00042.md
      ctx-auth-refresh.md
      test-log-latest.txt
```

agentへのprompt:

```text
新しいcontextがあります。以下を読んで対応してください。

- .agentmux/inbox/impl-codex/msg-00042.md
- .agentmux/inbox/impl-codex/test-log-latest.txt
```

### 7.1 MailboxFile形式

```markdown
---
message_id: msg_...
kind: TestResult
created_at: 2026-06-02T10:00:00Z
source: tester
redacted: true
---

# TestResult: auth refresh failure

## Summary
...

## Relevant log excerpt
...

## Requested action
...
```

## 8. Artifact連携

Artifactはcontextから参照される実体ファイルである。

例:

```text
.agentmux/artifacts/task-123/
  diff-impl-codex.patch
  test-impl-codex.log
  screen-reviewer-001.ansi
  result-planner.json
```

Message promptではartifact refを人間にもagentにも読めるpathとして渡す。

## 9. Redaction

### 9.1 対象

- API key
- token
- password
- secret
- private key
- `.env`内容
- SSH key
- cookie/session
- cloud credential

### 9.2 方針

- context化前にredactionを行う。
- redactionされたcontextには`redacted=true`を付ける。
- private contextは明示許可なしにexportしない。
- secret検出時はapprovalまたはwarningを出す。

## 10. Message routing rules

### 10.1 role宛配送

`to=Role(Reviewer)`の場合:

1. task内のactive reviewer agentを探す。
2. 複数存在する場合はpriority / availabilityで選ぶ。
3. 見つからなければqueuedにする。

### 10.2 stalled agentへの配送

agentがStalled/NeedsHumanの場合、InjectWhenIdle messageはinboxに保持し、status probeを優先する。

### 10.3 human approvalが必要な配送

以下の場合はRequireHumanApprovalへ昇格する。

- safetyがMayRunCommands以上
- contextにsecret疑いがある
- targetがAwaitingApproval
- message bodyに承認/拒否操作を含む

## 11. Context update from AGENTMUX_RESULT

agent resultにcontext_updatesが含まれる場合、Context BrokerがContextItemを作成する。

```json
{
  "context_updates": [
    {
      "kind": "Decision",
      "title": "refresh validation location",
      "body": "service層に寄せる",
      "tags": ["auth", "refresh-token"]
    }
  ]
}
```

## 12. 監査

すべてのmessage/context操作はevent logに残す。

- message.created
- message.rendered
- message.delivery.waiting
- message.injected
- context.created
- context.redacted
- mailbox.written
- artifact.created

## 13. 失敗時の扱い

| 失敗 | 対応 |
|---|---|
| prompt render失敗 | messageをFailedにし、エラー表示 |
| target agent不在 | Queuedのまま保持 |
| mailbox write失敗 | delivery停止、store error表示 |
| redaction high risk | approval queueへ送る |
| injection失敗 | retry上限後Failed |
