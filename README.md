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

`screen` renders the current ANSI TUI into plain text. `--enter` sends carriage return, matching a real terminal Enter key.

## Platforms

Linux, macOS and Windows (native, through ConPTY) are supported. [`.github/workflows/ci.yml`](.github/workflows/ci.yml) builds and tests all three; see [`specs/006-cross-platform.md`](specs/006-cross-platform.md) for what has been verified on each.

| | `--socket PATH` is | Access |
|---|---|---|
| Linux, macOS | a Unix-domain socket file, removed when the child exits | mode `0600` |
| Windows | only a name: it is mapped to the named pipe `\\.\pipe\agent-cli-lab-<hash>` and no file is created; the pipe disappears when the host ends | a DACL that allows only the current user |

Use the same `--socket` value (relative paths are relative to the current directory) for `host`, `send` and `screen`. Examples in this repo use `/tmp/pi.sock`; on Windows any path works, for example `C:\Temp\pi.sock`.

On Windows, `run` ends a piped stdin with Ctrl-Z and Enter, because a console ignores Ctrl-D.

## Build and test

```sh
cargo build --locked
cargo test --locked
```

Needs Rust 1.85 or newer. On Windows, either the MSVC or the GNU toolchain works. See [`specs/005-local-lifecycle.md`](specs/005-local-lifecycle.md) for setup and teardown, including Docker.

See [`specs/`](specs/) for prioritized problems and acceptance criteria.
