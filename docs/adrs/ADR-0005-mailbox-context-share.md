# ADR-0005: 長文context共有にmailbox fileを採用する

## Status

Accepted

## Context

テストログ、diff、大量エラー、長い設計メモをagent TUIのpromptへ直接貼ると、会話が汚れ、入力も壊れやすい。Codex/ClaudeのCLI agentは作業ディレクトリ内のファイルを読めるため、長文contextはファイル参照の方が安定する。

## Decision

短いcontextはinline prompt、長いcontextはtarget agentごとの`.agentmux/inbox/<agent>/`にmailbox fileとして保存し、handoff promptにはファイルパスを渡す。

## Consequences

### Positive

- promptが短く安定する。
- agentが必要に応じて詳細を読める。
- artifactとcontextを分離できる。
- redaction済みファイルを明示できる。

### Negative

- agentがファイルを読まない可能性がある。
- mailbox管理が必要。
- `.agentmux`のsecret管理に注意が必要。

### Mitigation

- bootstrap promptでmailboxのルールを説明する。
- handoff promptでrequired actionにファイル読取を明示する。
- state DB/eventsはprotected pathにする。
