# Headless interactive control (P0)

## Problem

`run` works when a human already owns a terminal, but an automation agent cannot inject the next input into a host-managed running job. Opening Terminal.app is platform-specific and requires GUI automation. Fixed input scripts are not genuinely interactive because they cannot inspect the current TUI before deciding what to send.

## Interface

```sh
# Long-running PTY host
agent-cli-lab host --socket /tmp/pi.sock --record pi.jsonl -- pi --no-session

# Separate agent calls while host is running
agent-cli-lab send --socket /tmp/pi.sock --enter 'Reply exactly: ONE'
agent-cli-lab screen --socket /tmp/pi.sock
agent-cli-lab send --socket /tmp/pi.sock --enter '/exit'
```

## Protocol

The host listens on a Unix-domain socket. Each client sends one JSON line and receives one JSON line.

- `{"op":"send","data":[...bytes...]}` writes exact bytes to the PTY.
- `{"op":"screen"}` returns the current terminal screen as plain text.
- Every response contains `ok`; errors also contain `error`.

`send --enter` appends carriage return (`0x0d`), matching a real terminal Enter key.

## Acceptance criteria

- No Terminal.app, AppleScript, or GUI automation is required.
- The host exposes a real PTY to the child.
- Separate CLI invocations can submit input while the child is alive.
- `screen` returns a rendered text snapshot rather than raw ANSI bytes.
- Socket cleanup occurs after normal child exit.
- Existing direct `run` behavior remains unchanged.

## Scope

The first implementation supports Unix platforms. Authentication and remote network access are out of scope; callers must protect the local socket path with filesystem permissions.
