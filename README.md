# agent-cli-lab

A small Rust CLI for agents and humans to run another command inside a pseudo-terminal (PTY), interact with it, and optionally save a machine-readable session transcript.

## Direct interactive proxy

```sh
agent-cli-lab run -- command --flag
agent-cli-lab run --record session.jsonl -- command
printf 'answer\n' | agent-cli-lab run -- command
```

## Headless agent control

Start the PTY host as a long-running process, then control it from separate calls:

```sh
agent-cli-lab host --socket /tmp/pi.sock --record pi.jsonl -- pi --no-session
agent-cli-lab send --socket /tmp/pi.sock --enter 'Reply exactly: HELLO'
agent-cli-lab screen --socket /tmp/pi.sock
agent-cli-lab send --socket /tmp/pi.sock --enter '/exit'
```

`screen` renders the current ANSI TUI into plain text. The Unix socket is created with mode `0600` and removed when the child exits. `--enter` sends carriage return, matching a real terminal Enter key.

See [`specs/`](specs/) for prioritized problems and acceptance criteria.
