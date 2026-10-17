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
agent-cli-lab host -r pi.jsonl -- pi --no-session
agent-cli-lab send -e 'Reply exactly: HELLO'
agent-cli-lab screen
agent-cli-lab attach
agent-cli-lab send -e '/exit'
```

`ask [-i IDLE_MS] [-t TIMEOUT_S] 'text'` is send + Enter + wait until the screen stops changing + screen in one call (`agent-cli-lab ask -s SOCK 'Reply: HELLO'`). `screen` renders the current ANSI TUI into plain text. `attach` requires terminal stdin, foregrounds the hosted PTY in the current terminal, and forwards keys until Ctrl-]. `foreground [-s SOCKET] [-- COMMAND...]` opens an attached terminal in a new window when called from a non-terminal context; with a command, it first starts a detached host for it (`agent-cli-lab foreground -- pi`). Set `AGENT_CLI_LAB_TERMINAL` or pass `foreground --terminal NAME`; macOS defaults to Ghostty. `--enter`/`-e` sends carriage return, matching a real terminal Enter key.

## Named sessions and what they are doing

Several hosted programs at once, each in its own window, by name (spec 007):

```sh
agent-cli-lab open -n ux --title "ux lab" --cwd D:\work -- pi --no-session   # a visible window, QuickEdit off, sized, titled
agent-cli-lab list                                   # name, state, size, pid, program of every running session
agent-cli-lab send -n ux --paced --file brief.txt -e # typed in chunks so a TUI does not read it as a paste
agent-cli-lab key  -n ux Down*2 Enter                # named keys instead of escape sequences
agent-cli-lab wait -n ux --asking                    # block until a dialog is showing (exit 124 on timeout)
agent-cli-lab watch -n ux                            # one line each time it starts asking, stalls, or ends
agent-cli-lab status -n ux --json                    # working | asking | stalled | gone, size, quiet time
agent-cli-lab screen -n ux --tail 12 --compact       # only the last lines; --json [--spans] for cells and colours
agent-cli-lab stop -n ux
```

`stalled` means no output for a while and no dialog: the program ended its turn and waits for you, even if it only asked a question in plain words. Every command also works against hosts started before these commands existed (it falls back to `send` and `screen`), and `-s PATH` still works wherever `-n NAME` does.

## Platforms

Linux, macOS and Windows (native, through ConPTY) are supported. [`.github/workflows/ci.yml`](.github/workflows/ci.yml) builds and tests all three; see [`specs/006-cross-platform.md`](specs/006-cross-platform.md) for what has been verified on each.

| | `--socket PATH` is | Access |
|---|---|---|
| Linux, macOS | a Unix-domain socket file, removed when the child exits | mode `0600` |
| Windows | only a name: it is mapped to the named pipe `\\.\pipe\agent-cli-lab-<hash>` and no file is created; the pipe disappears when the host ends | a DACL that allows only the current user |

By default `host`, `send` and `screen` use `$AGENT_CLI_LAB_SOCKET`, or `agent-cli-lab.sock` under `$XDG_RUNTIME_DIR`, `$TMPDIR`, or the system temp directory. Override with `--socket PATH`/`-s PATH`; `agent-cli-lab socket -p` prints the resolved default. Short forms include `-r` for `--record`, `-C`/`-R` for `--cols`/`--rows`, and `-e`/`-c`/`-d` for `send --enter`/`--ctrl-c`/`--ctrl-d`. On Windows any path-like name works, for example `C:\Temp\pi.sock`.

On Windows, `run` ends a piped stdin with Ctrl-Z and Enter, because a console ignores Ctrl-D.

## Build and test

```sh
cargo build --locked
cargo test --locked
```

Needs Rust 1.85 or newer. On Windows, either the MSVC or the GNU toolchain works. See [`specs/005-local-lifecycle.md`](specs/005-local-lifecycle.md) for setup and teardown, including Docker.

See [`specs/`](specs/) for prioritized problems and acceptance criteria.
