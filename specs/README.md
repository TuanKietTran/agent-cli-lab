# Specifications

Specs are ordered by filename. Work on the lowest-numbered unfinished spec first.

| Priority | Spec | State |
|---|---|---|
| P0 | [000-problem-and-order.md](000-problem-and-order.md) | Active |
| P0 | [001-interactive-proxy.md](001-interactive-proxy.md) | Complete |
| P1 | [002-agent-scenarios.md](002-agent-scenarios.md) | Proposed |
| Evidence | [003-pi-tui-smoke-test.md](003-pi-tui-smoke-test.md) | Passed |
| P0 | [004-headless-interactive-control.md](004-headless-interactive-control.md) | Complete |
| Ops | [005-local-lifecycle.md](005-local-lifecycle.md) | Verified (Linux in Docker before 006, Windows native after) |
| P0 | [006-cross-platform.md](006-cross-platform.md) | Verified on Windows; Linux and macOS unverified until CI runs |

A spec is complete only when all acceptance criteria are covered by automated tests or explicitly marked as manual checks.
