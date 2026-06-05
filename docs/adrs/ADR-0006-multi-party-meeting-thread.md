# ADR-0006: マルチパーティ会議は MessageThread として実装する

## Status

Accepted

## Context

3 者以上の agent(例: claude / codex / agy)が同一議題を相互に参照しながら議論する
「会議」を実現したい。既存の message bus は点対点(`agent:` / `role:`)が中心で、
次のギャップがあった。

- `Broadcast` / `Role` の fan-out 解決に送信者自身が含まれ、自分の発言が自分の
  pane に再注入されるエコーループが起きる。
- 議題(トピック)単位でメッセージを束ねる概念がなく、横断議論の文脈追跡が
  本文頼みになる。
- ループガードがペア間 3 往復ルール(プロトコル文書)のみで、3 者以上の
  組み合わせではすり抜ける。

## Decision

会議を first-class の `MessageThread` として message bus に追加する。

- `AgentMessage.thread_id: Option<ThreadId>` と `MessageTarget::Thread(ThreadId)`
  を追加する。`to: Thread(id)` は参加者全員(送信者を除く)へ fan-out する。
- fan-out 宛先(`Role` / `Task` / `Team` / `Thread` / `Broadcast`)は配送時に
  送信者を必ず除外する(エコーループの仕様レベル禁止)。
- thread は `max_messages_per_participant`(既定 5)の発言上限を持ち、上限到達後の
  agent 投稿は「要約して人間に判断を仰ぐ」誘導付きで拒否する。
- 参加者以外の agent の投稿と Closed thread への投稿は拒否する。
- 入口は IPC `meeting.open` / `meeting.close` / `meeting.list`、CLI
  `agentmux meeting open|close|list` と `agentmux message send --thread <id>`。
- 永続化は messages と同じく v0.1 では in-memory + event log
  (`thread.opened` / `thread.closed`)とする。

## Consequences

### Positive

- 既存の idle delivery・injection・監査ログをそのまま流用でき、会議専用の
  配送経路を持たない(daemon が状態を一元所有する原則を維持)。
- ループガードが daemon 側で強制され、プロトコル文書頼みでなくなる。
- スレッド単位でメッセージ履歴をフィルタできる(`message history --thread`)。

### Negative

- 発言順序の調停(turn-taking)は行わないため、複数 agent が同時に投稿し得る。
- in-memory 管理のため daemon 再起動でスレッド状態が失われる(messages と同等の制約)。

### Mitigation

- 進行制御が必要な会議は opener(人間 or facilitator agent)が `kind: Question`
  で指名しながら進める運用とする。
- 永続化が必要になった場合は SQLite への保存を messages と同時に検討する
  (`15_open_questions.md` に追記)。
