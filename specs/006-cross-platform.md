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
- **Windows:** the default named-pipe security is not enough: it grants Everyone read access. The listener is created with an explicit protected DACL `D:P(A;;GA;;;<current user SID>)`: full control for the current user only, no inheritance, no Everyone, no Anonymous, no network access. The user's SID is read from the process token. The "owner rights" alias is not used because an elevated administrator's token owner is the Administrators group. The listener also rejects remote clients (`PIPE_REJECT_REMOTE_CLIENTS`; this is the `interprocess` default, read in its source and not tested separately) and creates the first instance with `FILE_FLAG_FIRST_PIPE_INSTANCE`, so a host cannot start on a pipe name somebody else already owns (tested: the second host is refused).
- Not covered: a local attacker who creates the pipe name before the host starts can receive what a client sends. Same limitation as a pre-created Unix socket in a world-writable directory.

### ConPTY details (Windows)

Each item says how it was established.

1. Dropping the master closes the pseudo-console and kills the child (read in the `portable-pty` 0.9 source; not tested on its own). The master is therefore kept alive and dropped only after the child exits, from a thread that waits for the child; closing it is also what ends the output stream, because conhost does not close it by itself. Killing a host takes its child with it.
2. `portable-pty` creates the pseudo-console with `PSEUDOCONSOLE_INHERIT_CURSOR`: conhost writes `ESC[6n` (cursor position request) and expects a reply. The proxy answers `ESC[1;1R` and removes the request from the forwarded, recorded and rendered output, because it is conhost's handshake and not the child's output. Confirmed by running: with the reply the child starts within about 0.2 s; with the reply disabled it never starts (no output, 20 s timeout).
3. Ctrl-D is not end-of-file for a Windows console program. When `run` reads EOF from a non-terminal stdin it sends Ctrl-Z followed by Enter (`0x1a 0x0d`) instead of `0x04`. Confirmed by running `sort`: Ctrl-D and a bare Ctrl-Z leave it waiting, Ctrl-Z plus Enter ends it, with or without a trailing newline in the data.
4. When stdin or stdout is a real console, the proxy enables virtual-terminal input and output processing and restores the previous modes on exit, so escape sequences and arrow keys pass through on classic conhost as well as Windows Terminal. Exercised only with a ConPTY as the console (a `run` nested inside a `host`: typing, output and the exit code pass through); not tried in a classic conhost window or Windows Terminal.
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

Results of 2026-10-11. Nothing below is assumed: "unverified" means it has not been run. Linux was run at commit `ab2cf4a` (local `main`; not pushed when this was written); Windows was run on the same code before the commit messages were rewritten, which did not change the tree.

| Platform | Result |
|---|---|
| Windows 10 (build 19045), native, `x86_64-pc-windows-gnu`, rustc 1.99.0, gcc from WinLibs | `cargo build --locked` and `cargo test --locked` pass: 12 unit tests and 7 integration tests. The integration suite was repeated 20 times and run in 4 concurrent processes without a failure. `cargo clippy --all-targets` is clean. |
| Windows, MSVC toolchain | unverified (no MSVC tools on the machine used); only type-checked with `cargo check --all-targets --target x86_64-pc-windows-msvc`. |
| Linux | pass (2026-10-11): Linux 6.18 (WSL2 kernel of Rancher Desktop) x86_64, image `rust:1-slim`, rustc 1.99.0, read-only mount of commit `ab2cf4a`: `cargo build --locked` and `cargo test --locked` pass (10 unit tests, 7 integration tests, exit code 0). Only the Windows-specific tests did not run there (the 2 unit tests under `cfg(windows)` and the Windows-only integration test), which is why the counts differ from Windows. Run from a Windows host, so this is a Linux kernel in a VM, not bare metal. |
| macOS | **unverified.** Cannot be run locally; type-checks for `aarch64-apple-darwin`. Only CI can verify it. |

Acceptance criteria:

| # | Windows (native, GNU) | Linux | macOS |
|---|---|---|---|
| 1 build and test pass | pass | pass | unverified |
| 2 the four scenarios | pass (integration tests) | pass (integration tests) | tests written, unverified |
| 3 flags and protocol unchanged | pass (diff review; `send --enter` test) | same code | same code |
| 4 owner-only | pass: the DACL reads back as exactly one entry for the current user (`D:P(A;;FA;;;<SID>)`), and opening the pipe is denied for an identity whose user SID is deny-only while the current user connects. Mutation-checked: adding an Everyone entry makes both assertions fail. A literal second account was not used. | pass: `the_socket_is_private_to_the_owner` (mode `0600`), also checked by hand on 2026-10-10 | test written, unverified |
| 5 unreachable after exit | pass (also after killing the host) | pass (the socket file is removed and `screen` fails; killing the host was not tried) | test written, unverified |
| 6 second host refused | pass | pass | test written, unverified |
| 7 `run` unchanged on Unix | not applicable | pass for what the tests cover: the original scenarios and the new regression test for the writer's final newline and EOF all pass; interactive use in a real terminal was not re-checked | unverified |

Manual checks outside the automated tests: a `run` nested in a `host` as described above; killing a running host removes the pipe and the child.

## Known limitations

- A client that connects to the host and sends nothing blocks the host's single accept loop. This is as before on Unix and the same on Windows; there is no read timeout.
- A local attacker who creates the pipe name (or Unix socket path) before the host starts can receive what a client sends. The client does not check who the server is.
- The "another user" check on Windows uses a token that differs from the current one only in its user SID, which the DACL evaluation treats like a different user. A test with a real second account (for example in CI) would be stronger.
- On Windows the end-of-input sequence completes the current line first; data that ends mid-line gets Ctrl-Z attached to that line. Unix is unchanged.
- The Unix socket gets its `0600` mode right after `bind`, as before, so there is a short window in which the umask-derived mode applies.
