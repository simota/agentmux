# ADR-0002: PTY + Terminal Engineをv0.1必須とする

## Status

Accepted

## Context

対話TUIをagentmux pane内で動かすには、外部CLIをPTYで起動し、ANSI/VT出力を解釈してvirtual terminal bufferへ反映する必要がある。Ratatuiだけでは外部TUIアプリのterminal emulationは提供されない。

## Decision

v0.1ではPTY SupervisorとTerminal Engineを実装対象に含める。Phase 0でterminal engine候補を比較し、Claude/Codex TUIが実用的に表示できる構成を選定する。

## Consequences

### Positive

- tmux風の複数agent paneが実現できる。
- detach/attach、resize、scrollback、snapshotが可能になる。

### Negative

- 実装難易度が高い。
- xterm完全互換は難しい。

### Mitigation

- v0.1は完全互換ではなく実用互換を目標にする。
- unsupported escapeはwarningにし、panicしない。
