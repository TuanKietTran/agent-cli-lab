# Local lifecycle: setup / run / teardown

Linux was verified by hand on 2026-10-10 from a Windows host with Docker (Rancher Desktop), image `rust:1-slim` (Rust 1.99, Linux x86_64), **before** the cross-platform work of spec 006. After it, on 2026-10-11, the automated suite passed on Linux in Docker at commit `ab2cf4a` (10 unit and 7 integration tests, read-only mount, the WSL2 kernel of Rancher Desktop) and on native Windows with the GNU toolchain. GitHub Actions at commit `979a72b` (run 38124801783) then ran the suite on `ubuntu-latest` (10 unit and 7 integration tests), `macos-latest` (10 and 7) and `windows-latest` with the MSVC toolchain (13 and 7); all three passed. Not run by hand: macOS, and the MSVC toolchain locally.

## Platform

- Linux, macOS and native Windows (spec 006). The Windows build uses ConPTY and needs Windows 10 version 1809 or newer.
- Needs Rust with edition 2024 support (1.85 or newer) and a linker: `gcc` or `cc` on Linux and macOS (Xcode command line tools); on Windows either the MSVC toolchain with Visual Studio Build Tools, or the GNU toolchain with a `gcc` from MinGW-w64 on `PATH`.

## Setup

Native (Linux, macOS, Windows):

```sh
cargo build --locked        # dependencies are pinned by Cargo.lock; --locked keeps the file unmodified
cargo test --locked         # unit tests (13 on Windows, 10 elsewhere) and 7 integration tests (6 on every platform plus 1 specific to the OS family)
```

Windows toolchain without admin rights (what was used to verify spec 006). `rustup-init.exe` comes from `https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe`; compare its SHA-256 with the `.sha256` file next to it, then:

```powershell
.\rustup-init.exe -y --no-modify-path --default-host x86_64-pc-windows-gnu --profile minimal
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"    # this shell only; --no-modify-path left your PATH and registry alone
```

The GNU toolchain links through `gcc`; here it was the WinLibs MinGW-w64 package that `winget` had already installed (`BrechtSanders.WinLibs.POSIX.UCRT`). For the MSVC toolchain install Visual Studio Build Tools instead (C++ workload, several GB, needs admin) and use rustup's default host. The OS behaviour (ConPTY, named pipes, DACLs) is the same under both; CI builds with MSVC.

Docker, nothing installed on the host. The repo is bind-mounted and Cargo's registry and `target/` go on a named volume, so the working tree stays clean and Windows file locking is avoided:

```sh
docker volume create agent-cli-lab-cache
MSYS_NO_PATHCONV=1 docker run -d --name agent-cli-lab-dev \
  -v "$(pwd -W):/src" -v agent-cli-lab-cache:/cache -w /src \
  -e CARGO_HOME=/cache/cargo -e CARGO_TARGET_DIR=/cache/target \
  rust:1-slim sleep infinity                  # `$(pwd -W)` and MSYS_NO_PATHCONV are for Git Bash on Windows
docker exec agent-cli-lab-dev cargo build --locked   # about 9s cold
docker exec agent-cli-lab-dev cargo test --locked    # 3 unit + 4 integration tests passed on 2026-10-10; 10 unit + 7 integration tests after spec 006
```

Binary: `target/debug/agent-cli-lab`, or `/cache/target/debug/agent-cli-lab` in the container.

## Run

```sh
agent-cli-lab run [--record PATH] -- COMMAND [ARG]...        # direct proxy, spec 001
agent-cli-lab host --socket PATH [--record PATH] [--cols N] [--rows N] [--stdio] -- COMMAND ...   # spec 004
agent-cli-lab send --socket PATH [--enter] 'text'
agent-cli-lab screen --socket PATH
```

Acceptance criteria exercised by hand (the integration tests cover the same behavior programmatically):

| Spec | Check | Linux (Docker, by hand, before spec 006) | Windows (native, after spec 006) |
|---|---|---|---|
| 001 | `--help` carries the "Recordings ... may contain secrets" warning | pass | pass |
| 001 | Child sees a TTY on stdin and stdout; arguments after `--` arrive verbatim (`--flag`, `a b`) | pass | pass (TTY: integration test; arguments: `run -- cmd /c echo --flag "a b"` printed `--flag "a b"`) |
| 001 | Piped input reaches an interactive child (`printf ... \| run -- cat`; on Windows `sort`) | pass; the text appears twice because the PTY echoes input | pass; also without a trailing newline |
| 001 | Non-zero exit propagates (`exit 7` -> 7, `exit 3` -> 3) | pass | pass (7: integration test, 3: by hand) |
| 001 | No file is written without `--record`; with it the JSONL has a `header` first and an `exit` event with `code` last | pass | pass |
| 004 | `host` socket is owner-only | pass (mode `600`) | pass (not a file: the pipe's DACL has one entry, for the current user; unit test) |
| 004 | `screen` returns plain text with ANSI colour and cursor codes rendered away | pass | pass |
| 004 | `send --enter` from a separate call reaches the live child; a second `screen` shows the result | pass | pass |
| 004 | Host exits 0 when the child exits and removes its socket | pass | pass (the pipe is gone and `screen` fails to connect) |
| 004 | `screen` against a missing socket exits 1 with `failed to connect to <path>` | pass | pass |
| 006 | A second `host` on the same `--socket` fails | pass (integration test, 2026-10-11) | pass ("failed to bind socket ... already in use by another host?") |

The hand checks in the Linux column were not repeated after spec 006. The automated suite covers most of them and passed on Linux at `ab2cf4a`: TTY and output, piped input with and without a trailing newline, exit status and recording, hosted control through `send` and `screen`, socket mode `0600`, socket removal when the host exits, and a second host being refused. Not repeated on Linux: the `--help` warning and the missing-socket message.

`send --enter` appends `\r`, not `\n`; see spec 003 for why that matters to TUIs.

On Windows `--socket PATH` is only a name (spec 006): no file is created, so `ls` will not show it. The same value must be used for `host`, `send` and `screen`.

## Teardown

- Stop a hosted session by letting the child exit, for example `send --enter '/exit'` or the child's own quit command. The host then removes its socket (spec 004).
- Killing the host process itself: on Windows (checked 2026-10-11) the pipe vanishes at once and the child process is terminated together with the host, so nothing is left behind. On Unix a killed host cannot remove its socket file, so a stale file is expected to remain until it is deleted by hand; not checked.
- Recordings (`*.jsonl`, gitignored) contain every input and output byte and may contain secrets; delete them when done.
- Native: `cargo clean`. On Windows a toolchain installed with the commands under Setup is removed with `rustup self uninstall`, which deletes `%USERPROFILE%\.cargo` and `%USERPROFILE%\.rustup` (there is no PATH entry to revert because of `--no-modify-path`).
- Docker:

  ```sh
  docker rm -f agent-cli-lab-dev
  docker volume rm agent-cli-lab-cache     # cargo registry cache and target/
  docker rmi rust:1-slim                   # optional, about 1.3 GB; leave it to keep rebuilds fast
  ```

After teardown `git status --short --ignored` is empty.

## Not covered

- Spec 003 (Pi TUI smoke test) needs the `pi` CLI and a model; it was not run.
- `host --stdio` (human and agent sharing one terminal) was not exercised.
- Spec 002 (`scenario`) is not implemented.
- Native macOS (CI only) and native Linux hosts (Linux was run in a container).
- Windows: the virtual-terminal console modes that `run` and `host --stdio` switch on were exercised only with a ConPTY as the console (a `run` nested inside a `host`), not in a classic conhost window or Windows Terminal.
