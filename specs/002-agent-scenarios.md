# Declarative agent scenarios (P1)

## Goal

Allow an agent to describe a reproducible interactive exchange without manually coordinating terminal timing.

## Proposed interface

```text
agent-cli-lab scenario path/to/scenario.json
```

A scenario contains a command, environment overrides, terminal size, ordered `expect`/`send` steps, per-step timeouts, and an expected exit status.

## Open problems

- Matching text across arbitrary output chunk boundaries.
- ANSI escape handling and screen-oriented applications.
- Secret values and transcript redaction.
- Reliable timeout and child-process-tree termination.
- Cross-platform signal semantics.

## Dependency

Implement only after `001-interactive-proxy.md` is stable and tested.
