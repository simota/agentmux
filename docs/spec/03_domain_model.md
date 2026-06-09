# 03. ドメインモデル設計書

## 1. 概要

agentmuxのドメインは、tmux風のsession/pane構造に、agent、task、message、context、worktree、approvalを加えたモデルで構成する。

中心概念は以下である。

```text
Project
  -> Task
      -> AgentSession
          -> Pane
          -> PTY
          -> TerminalBuffer
      -> Messages
      -> ContextItems
      -> Worktrees
      -> Artifacts
      -> Approvals
```

## 2. ID設計

IDはprefix付きULIDまたはUUIDv7を推奨する。

例:

```text
proj_01J...
task_01J...
agent_01J...
pane_01J...
msg_01J...
ctx_01J...
art_01J...
appr_01J...
```

prefixによりログ可読性を高める。

## 3. Project

```rust
struct Project {
    id: ProjectId,
    name: String,
    root_path: PathBuf,
    default_branch: String,
    config_path: Option<PathBuf>,
    created_at: DateTimeUtc,
    updated_at: DateTimeUtc,
}
```

Projectは1つのリポジトリまたは作業ディレクトリを表す。

## 4. Task

```rust
struct Task {
    id: TaskId,
    project_id: ProjectId,
    title: String,
    body: String,
    status: TaskStatus,
    team_template: String,
    root_context_scope_id: ContextScopeId,
    created_by: ActorId,
    created_at: DateTimeUtc,
    updated_at: DateTimeUtc,
    completed_at: Option<DateTimeUtc>,
}
```

```rust
enum TaskStatus {
    Created,
    Starting,
    Running,
    WaitingForHuman,
    Paused,
    Completed,
    Failed,
    Cancelled,
}
```

## 5. AgentSession

```rust
struct AgentSession {
    id: AgentSessionId,
    project_id: ProjectId,
    task_id: Option<TaskId>,
    name: String,
    provider: AgentProvider,
    role: AgentRole,
    mode: AgentMode,
    pty_id: PtyId,
    process_id: Option<u32>,
    pane_id: Option<PaneId>,
    terminal_buffer_id: TerminalBufferId,
    worktree_id: Option<WorktreeId>,
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    status: AgentStatus,
    capabilities: AgentCapabilities,
    input_lock: InputLock,
    inbox_id: InboxId,
    context_scope_id: ContextScopeId,
    created_at: DateTimeUtc,
    last_activity_at: DateTimeUtc,
    exited_at: Option<DateTimeUtc>,
}
```

```rust
enum AgentProvider {
    ClaudeCode,
    Codex,
    Shell,
    Custom(String),
}
```

```rust
enum AgentMode {
    InteractiveTui,
    InteractiveShell,
}
```

```rust
enum AgentRole {
    Planner,
    Implementer,
    Reviewer,
    Tester,
    Debugger,
    Refactorer,
    SecurityReviewer,
    DocsWriter,
    Integrator,
    ContextManager,
    Custom(String),
}
```

```rust
enum AgentStatus {
    Starting,
    InteractiveReady,
    RunningTurn,
    RunningCommand,
    AwaitingInput,
    AwaitingApproval,
    NeedsHuman,
    Blocked,
    CompletedTurn,
    Stalled,
    Exited,
    Failed,
}
```

## 6. Pane

```rust
struct Pane {
    id: PaneId,
    session_id: ClientSessionId,
    kind: PaneKind,
    title: String,
    rect: Rect,
    focus: bool,
    zoomed: bool,
    created_at: DateTimeUtc,
}
```

```rust
enum PaneKind {
    AgentTui { agent_id: AgentSessionId },
    Internal { view: InternalView },
    Shell { pty_id: PtyId },
}
```

```rust
enum InternalView {
    MessageBus,
    ContextBoard,
    ApprovalQueue,
    TaskTimeline,
    WorktreeDiff,
    TestResults,
    AgentList,
    Help,
}
```

## 7. Worktree

```rust
struct Worktree {
    id: WorktreeId,
    project_id: ProjectId,
    task_id: TaskId,
    owner_agent_id: Option<AgentSessionId>,
    path: PathBuf,
    branch_name: String,
    base_branch: String,
    status: WorktreeStatus,
    created_at: DateTimeUtc,
}
```

```rust
enum WorktreeStatus {
    Creating,
    Ready,
    Dirty,
    Testing,
    ReviewReady,
    Promoted,
    Archived,
    Failed,
}
```

## 8. Message

```rust
struct AgentMessage {
    id: MessageId,
    task_id: Option<TaskId>,
    thread_id: Option<ThreadId>,   // 所属する会議スレッド(任意)
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
    read_at: Option<DateTimeUtc>,
}
```

```rust
enum MessageSource {
    User(ClientId),
    Agent(AgentSessionId),
    Role(AgentRole),
    System,
    Orchestrator,
}
```

```rust
enum MessageTarget {
    Agent(AgentSessionId),
    Role(AgentRole),
    Task(TaskId),
    Team(String),
    Thread(ThreadId),   // 会議スレッド参加者全員(送信者を除く)
    Broadcast,
}
```

### 8.1 MessageThread(マルチパーティ会議)

3 者以上の agent が同一議題を議論するための会話スレッド。ID prefix は `thread_`。
`to: Thread(id)` のメッセージは参加者全員(送信者を除く)へ fan-out 配送される。
詳細仕様は `06_message_bus_context_broker.md §3.6` と ADR-0006 を参照。

```rust
struct MessageThread {
    id: ThreadId,
    topic: String,                        // 議題(injected prompt に含まれる)
    participants: Vec<AgentSessionId>,
    opened_by: MessageSource,
    status: ThreadStatus,                 // Open | Closed
    max_messages_per_participant: u32,    // 発言上限(ループガード、既定 7)
    created_at: DateTimeUtc,
    closed_at: Option<DateTimeUtc>,
}
```

```rust
enum MessageKind {
    TaskAssignment,
    Question,
    Finding,
    PatchProposal,
    ReviewComment,
    TestResult,
    FailureReport,
    Decision,
    Handoff,
    ApprovalRequest,
    ContextUpdate,
    StatusProbe,
}
```

## 9. ContextItem

```rust
struct ContextItem {
    id: ContextItemId,
    project_id: ProjectId,
    task_id: Option<TaskId>,
    scope: ContextScope,
    kind: ContextKind,
    title: String,
    body: String,
    source: ContextSource,
    visibility: Visibility,
    confidence: f32,
    tags: Vec<String>,
    related_files: Vec<PathBuf>,
    artifact_refs: Vec<ArtifactId>,
    created_at: DateTimeUtc,
    updated_at: DateTimeUtc,
}
```

```rust
enum ContextKind {
    ProjectSummary,
    ArchitectureNote,
    CodingRule,
    TaskBrief,
    FileReference,
    DiffSummary,
    TestResult,
    ErrorLog,
    AgentFinding,
    Decision,
    Risk,
    OpenQuestion,
    HandoffSummary,
}
```

## 10. Artifact

```rust
struct Artifact {
    id: ArtifactId,
    project_id: ProjectId,
    task_id: Option<TaskId>,
    kind: ArtifactKind,
    path: PathBuf,
    title: String,
    mime_type: Option<String>,
    size_bytes: u64,
    checksum: Option<String>,
    created_at: DateTimeUtc,
}
```

```rust
enum ArtifactKind {
    TestLog,
    DiffPatch,
    ScreenSnapshot,
    Transcript,
    AgentResult,
    ContextBundle,
    FileList,
    CommandOutput,
}
```

## 11. Approval

```rust
struct ApprovalRequest {
    id: ApprovalId,
    task_id: Option<TaskId>,
    agent_id: Option<AgentSessionId>,
    kind: ApprovalKind,
    risk: RiskLevel,
    title: String,
    description: String,
    proposed_input: Option<InputScriptId>,
    command: Option<String>,
    status: ApprovalStatus,
    created_at: DateTimeUtc,
    decided_at: Option<DateTimeUtc>,
    decided_by: Option<ActorId>,
}
```

```rust
enum ApprovalKind {
    AutoInput,
    FileWrite,
    ShellCommand,
    GitCommit,
    GitPush,
    NetworkAccess,
    SecretAccess,
    FullAccess,
    ExternalTool,
}
```

## 12. InputScript

```rust
struct InputScript {
    id: InputScriptId,
    target_agent_id: AgentSessionId,
    reason: String,
    preconditions: Vec<InputPrecondition>,
    actions: Vec<InputAction>,
    safety: InputSafety,
    created_at: DateTimeUtc,
}
```

```rust
enum InputAction {
    TypeText(String),
    PasteText(String),
    PressEnter,
    PressEsc,
    PressTab,
    PressBackspace,
    PressCtrl(char),
    PressAlt(char),
    SendRaw(Vec<u8>),
    Wait(Duration),
}
```

## 13. StateSignal

```rust
struct StateSignal {
    agent_id: AgentSessionId,
    source: StateSignalSource,
    confidence: f32,
    value: AgentStatus,
    evidence: String,
    observed_at: DateTimeUtc,
}
```

```rust
enum StateSignalSource {
    Process,
    PtyActivity,
    ScreenPattern,
    ExplicitMarker,
    HookEvent,
    FileSystemEvent,
    HumanOverride,
}
```

状態判定の優先度:

```text
HumanOverride > ExplicitMarker > HookEvent > Process > FileSystemEvent > PtyActivity > ScreenPattern
```

## 14. 集約ルート

Task aggregate:

- AgentSession
- Message
- ContextItem
- Worktree
- Artifact
- Approval

Project aggregate:

- Task
- global ContextItem
- team templates
- provider config
