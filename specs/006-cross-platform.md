# Cross-platform support (Windows, macOS, Linux)

## Problem

`host`, `send` and `screen` (spec 004) were built on `std::os::unix` (`UnixListener`, `UnixStream`, `PermissionsExt`), so the crate did not compile on native Windows. Spec 005 told Windows users to use a container or WSL. The PTY layer (`portable-pty`, `crossterm`, `vt100`) is already cross-platform, so only the local IPC, its access control and a few ConPTY details block Windows. macOS differs from Linux only in details (socket path length, `fchmod` on sockets) and had never been built.

Goal: `cargo build --locked` and `cargo test --locked` pass on Windows (native), macOS and Linux, and `run`, `host`, `send` and `screen` work on all three. The wire protocol and CLI flags do not change. Existing `run` behaviour on Unix does not change.

## Design

### Local IPC

Use the [`interprocess`](https://crates.io/crates/interprocess) crate's `local_socket` API: a Unix-domain socket on Unix, a named pipe on Windows. The protocol stays one JSON line per request and one per response (spec 004).

| | `interprocess` | `uds_windows` (AF_UNIX on Windows) |
|---|---|---|
| Unix | std Unix-domain socket | not applicable |
| Windows transport | named pipe | AF_UNIX socket file |
| Access control on Windows | explicit DACL through `SecurityDescriptor` (SDDL) | NTFS ACL on the socket file; semantics poorly documented |
| Stale state after a crash | none (the pipe dies with the process) | socket file remains |
| Minimum Windows | any with named pipes | Windows 10 1803 |

`interprocess` was chosen because named pipes have a well-defined access-control model and the same crate covers every platform. It is pinned in `Cargo.lock` to 2.4.4, not the newest patch release, which was a day old when this work started.

### `--socket PATH`

- **Unix (Linux, macOS):** unchanged. `PATH` is the socket file. It is created with mode `0600` and removed when the host exits. macOS limits socket paths to about 104 bytes.
- **Windows:** `PATH` is a name, not a file. It is made absolute (`std::path::absolute`), lowercased, mixed with the current user's SID, hashed (FNV-1a 64) and mapped to `\\.\pipe\agent-cli-lab-<16 hex digits>`. The same `--socket` value therefore reaches the same host from any later call in the same directory, exactly as on Unix, and two users can use the same path without colliding. No file is created. A value that already is a pipe path (`\\.\pipe\NAME`) is used verbatim. The pipe disappears when the host process ends, including when it is killed.

### Owner-only access

- **Unix:** `chmod 0600` right after `bind`, as before. `interprocess`'s `.mode()` (`fchmod` before `bind`) is not used because `fchmod` on a socket is not supported everywhere, macOS included.
- **Windows:** the default named-pipe security is not enough: it grants Everyone read access. The listener is created with an explicit protected DACL `D:P(A;;GA;;;<current user SID>)`: full control for the current user only, no inheritance, no Everyone, no Anonymous, no network access. The user's SID is read from the process token. The "owner rights" alias is not used because an elevated administrator's token owner is the Administrators group. The listener also rejects remote clients (`PIPE_REJECT_REMOTE_CLIENTS`, the `interprocess` default) and creates the first instance with `FILE_FLAG_FIRST_PIPE_INSTANCE`, so a host cannot start on a pipe name somebody else already owns.
- Not covered: a local attacker who creates the pipe name before the host starts can receive what a client sends. Same limitation as a pre-created Unix socket in a world-writable directory.

### ConPTY details (Windows)

Found by reading `portable-pty` 0.9 and then checked by running; the results are in the table below.

1. Dropping the master closes the pseudo-console and kills the child. `open_pty` therefore keeps the master alive and drops it only after the child exits, which is also what makes the output reader see EOF (conhost never closes it by itself).
2. `portable-pty` creates the pseudo-console with `PSEUDOCONSOLE_INHERIT_CURSOR`: conhost writes `ESC[6n` (cursor position request) and expects a reply. The proxy answers `ESC[1;1R` and removes the request from the forwarded, recorded and rendered output, because it is conhost's handshake and not the child's output.
3. Ctrl-D is not end-of-file for a Windows console program. When `run` reads EOF from a non-terminal stdin it sends the Windows end-of-file key instead of `0x04` (see the table for what was verified).
4. When stdin or stdout is a real console, the proxy enables virtual-terminal input and output processing and restores the previous modes on exit, so escape sequences and arrow keys pass through on classic conhost as well as Windows Terminal.
5. `send --enter` is still carriage return (`0x0d`) on every platform; ConPTY turns it into an Enter key.
6. Resize: the proxy has no resize feature on any platform (the size is fixed by `--cols` and `--rows`), so there is nothing to port.
7. Exit codes: the child's exit code is propagated as before (`GetExitCodeProcess` on Windows).

### Tests

`tests/proxy.rs` no longer has `#![cfg(unix)]`. Each scenario picks its child command per platform (`sh` and `cat` on Unix; `cmd /c` and `powershell` on Windows) and has the same assertions. The host test waits for readiness by probing `screen` instead of checking that a socket file exists, and checks cleanup by confirming the host is no longer reachable. Pure logic (pipe-name mapping, ConPTY handshake filter) has unit tests that run on every platform.

### CI

`.github/workflows/ci.yml` runs `cargo build --locked` and `cargo test --locked` on `ubuntu-latest`, `macos-latest` and `windows-latest`. macOS can only be verified there.

## Acceptance criteria

1. `cargo build --locked` and `cargo test --locked` pass on Windows (native), macOS and Linux.
2. The three integration scenarios (TTY and output, piped input with EOF, hosted TUI control through `send` and `screen`) plus the exit-code and recording scenario pass on all three.
3. CLI flags and the wire protocol are unchanged; `send --enter` sends `0x0d`.
4. Unix socket mode is `0600`. On Windows the pipe's DACL contains exactly one allow entry, for the current user, and an identity other than that user cannot open the pipe.
5. After the host exits it is unreachable on every platform; on Unix the socket file is removed.
6. Starting a second host on the same `--socket` fails with an error instead of replacing the first.
7. `run` behaviour on Unix is unchanged.

## Verification status

Filled in with real results as the work proceeds (see the end of this file).
