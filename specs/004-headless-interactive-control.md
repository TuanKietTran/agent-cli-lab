# Headless interactive control (P0)

## Problem

`run` works when a human already owns a terminal, but an automation agent cannot inject the next input into a host-managed running job. Opening Terminal.app is platform-specific and requires GUI automation. Fixed input scripts are not genuinely interactive because they cannot inspect the current TUI before deciding what to send.

## Interface

```sh
# Long-running PTY host (on Windows `--socket` is a name, see spec 006)
agent-cli-lab host --socket /tmp/pi.sock --record pi.jsonl -- pi --no-session

# Separate agent calls while host is running
agent-cli-lab send --socket /tmp/pi.sock --enter 'Reply exactly: ONE'
agent-cli-lab screen --socket /tmp/pi.sock
agent-cli-lab send --socket /tmp/pi.sock --enter '/exit'
```

## Protocol

The host listens on a local socket: a Unix-domain socket on Unix, a named pipe on Windows (spec 006). Each client sends one JSON line and receives one JSON line. The protocol is the same on every platform.

- `{"op":"send","data":[...bytes...]}` writes exact bytes to the PTY.
- `{"op":"screen"}` returns the current terminal screen as plain text.
- Every response contains `ok`; errors also contain `error`.

`send --enter` appends carriage return (`0x0d`), matching a real terminal Enter key.

## Acceptance criteria

- No Terminal.app, AppleScript, or GUI automation is required.
- The host exposes a real PTY to the child.
- Separate CLI invocations can submit input while the child is alive.
- `screen` returns a rendered text snapshot rather than raw ANSI bytes.
- Socket cleanup occurs after normal child exit (the Unix socket file is removed; a Windows pipe disappears with the host process).
- Starting a second host on the same `--socket` fails instead of replacing the first.
- Existing direct `run` behavior remains unchanged.

## Scope

Linux, macOS and Windows are supported; see [006-cross-platform.md](006-cross-platform.md) for how `--socket` is interpreted on Windows and how access is restricted. Authentication and remote network access are out of scope. The socket is reachable only by the user who started the host: mode `0600` on Unix, a DACL that names only that user on Windows. Callers should still keep Unix socket paths in a directory that other users cannot write to.
