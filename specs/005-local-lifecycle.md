# Local lifecycle: setup / run / teardown

Verified 2026-10-10 on Windows with Docker (Rancher Desktop) using the `rust:1-slim` image (Rust 1.99, Linux x86_64). Native Unix steps are taken from `Cargo.toml` and the README and were not run on a bare host.

## Platform

- Unix only. `src/lib.rs` imports `std::os::unix` (`UnixListener`, `UnixStream`, `PermissionsExt`) unconditionally, so the crate does not compile on native Windows, and `tests/proxy.rs` is `#![cfg(unix)]`. On a Windows host use a Linux container or WSL.
- Needs Rust with edition 2024 support (1.85 or newer) and a C linker (`gcc`).

## Setup

Native Unix:

```sh
cargo build --locked        # dependencies are pinned by Cargo.lock; --locked keeps the file unmodified
cargo test --locked
```

Docker, nothing installed on the host. The repo is bind-mounted and Cargo's registry and `target/` go on a named volume, so the working tree stays clean and Windows file locking is avoided:

```sh
docker volume create agent-cli-lab-cache
MSYS_NO_PATHCONV=1 docker run -d --name agent-cli-lab-dev \
  -v "$(pwd -W):/src" -v agent-cli-lab-cache:/cache -w /src \
  -e CARGO_HOME=/cache/cargo -e CARGO_TARGET_DIR=/cache/target \
  rust:1-slim sleep infinity                  # `$(pwd -W)` and MSYS_NO_PATHCONV are for Git Bash on Windows
docker exec agent-cli-lab-dev cargo build --locked   # about 9s cold
docker exec agent-cli-lab-dev cargo test --locked    # 3 unit + 4 integration tests, all passing
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

| Spec | Check | Result |
|---|---|---|
| 001 | `--help` carries the "Recordings ... may contain secrets" warning | pass |
| 001 | Child sees a TTY on stdin and stdout; arguments after `--` arrive verbatim (`--flag`, `a b`) | pass |
| 001 | Piped input reaches an interactive child (`printf ... \| run -- cat`) | pass; the text appears twice because the PTY echoes input |
| 001 | Non-zero exit propagates (`exit 7` -> 7, `exit 3` -> 3) | pass |
| 001 | No file is written without `--record`; with it the JSONL has a `header` first and an `exit` event with `code` last | pass |
| 004 | `host` socket has mode `600` | pass |
| 004 | `screen` returns plain text with ANSI colour and cursor codes rendered away | pass |
| 004 | `send --enter` from a separate call reaches the live child; a second `screen` shows the result | pass |
| 004 | Host exits 0 when the child exits and removes its socket | pass |
| 004 | `screen` against a missing socket exits 1 with `failed to connect to <path>` | pass |

`send --enter` appends `\r`, not `\n`; see spec 003 for why that matters to TUIs.

## Teardown

- Stop a hosted session by letting the child exit, for example `send --enter '/exit'` or the child's own quit command. The host then removes its socket (spec 004). Cleanup after killing the host process itself was not checked.
- Recordings (`*.jsonl`, gitignored) contain every input and output byte and may contain secrets; delete them when done.
- Native: `cargo clean`.
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
- Native macOS or Linux hosts, and native Windows (unsupported, see Platform).
