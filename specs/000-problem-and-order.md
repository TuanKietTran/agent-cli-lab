# Problem and implementation order

## Problem

AI agents can execute ordinary commands, but interactive CLI programs expect a terminal, incremental input, terminal sizing, and observable output. Pipes alone often change program behavior. We need a thin PTY proxy that works for both a human terminal and agent-provided stdin, while preserving an auditable transcript.

## Risks

1. **Deadlock or lost output** when input, output, and child exit are coordinated incorrectly.
2. **Terminal corruption** if raw mode is enabled and not restored after failure.
3. **Unclear command boundaries** when proxy flags and child flags overlap.
4. **Non-deterministic automation** without timestamps and stream/event structure.
5. **Secrets in recordings**; recording must be opt-in.
6. **Platform differences** in PTY behavior and exit status.

## What to do first

1. Implement `run -- <command> [args...]` using a PTY.
2. Forward stdin to the PTY and PTY output to stdout concurrently.
3. Return the child command's exit status.
4. Add opt-in JSON Lines recording.
5. Test argument parsing, transcript structure, output, and exit propagation.
6. Later, add declarative agent scenarios with expected output and timed input.

## Non-goals for the MVP

- Shell parsing or evaluation.
- Full terminal emulation.
- GUI terminal-window automation.
- Sending secrets safely to commands.
- Network proxying.
