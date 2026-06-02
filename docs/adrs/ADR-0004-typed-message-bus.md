# ADR-0004: typed message busを採用する

## Status

Accepted

## Context

agent間で自由文を直接やり取りすると、routing、優先度、context添付、承認、監査、再配送が難しくなる。

## Decision

agent間通信はdaemon上のtyped AgentMessageとして管理する。TUIへ配送する時点でprovider別promptへrenderする。

## Consequences

### Positive

- routingとdelivery statusを管理できる。
- context/artifact参照を構造化できる。
- audit logに残しやすい。
- workflow automationと相性がよい。

### Negative

- message schema設計が必要。
- agentが自由形式で返した結果をmarker/schemaに合わせる必要がある。

### Mitigation

- AGENTMUX_RESULT schemaを定義する。
- invalid marker時はstatus probeを送る。
