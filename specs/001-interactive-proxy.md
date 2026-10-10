# Interactive PTY proxy (P0)

## Interface

```text
agent-cli-lab run [--record PATH] [--cols N] [--rows N] -- COMMAND [ARG]...
```

Everything after `--` is passed directly to the child without shell interpretation.

## Behavior

- Start the child attached to a PTY of the requested size.
- Copy process stdin to the PTY until EOF.
- Copy PTY output to process stdout until EOF.
- Wait for the child and exit with its status when representable.
- If `--record` is present, create a JSON Lines file with a header, input/output chunks, and an exit event.
- Never record unless explicitly requested.
- Restore terminal state after interruption or errors if raw mode is used.

## Acceptance criteria

- A command sees a TTY on stdin.
- Child arguments are preserved exactly.
- Piped input reaches an interactive child.
- Output reaches stdout.
- A non-zero child status is propagated.
- Every transcript line is valid JSON and includes an event type.
- `--help` documents examples and the recording sensitivity warning.
