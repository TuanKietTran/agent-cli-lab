# Sessions, state and the commands that replaced hand-written scripts (P0)

## Problem

Orchestrating several hosted programs (three to five interactive agent TUIs at once, each in its own window, driven and watched by a commander agent) worked with spec 004's `host`, `send` and `screen` only because the commander wrapped them in PowerShell and Bash. Each wrapper was a command that was missing. They are listed here, because the list is the requirement: every row below is something that was done with a script on 2026-10-17 and now is one command.

| # | What was done by hand (PowerShell / Bash) | What it cost | Now |
|---|---|---|---|
| 1 | `Start-Process cmd.exe /c "title ... & mode con cols=150 lines=40 & agent-cli-lab host --stdio -C 150 -R 40 -s <absolute socket> -- <program>"` with `-WorkingDirectory`, to open a visible, interactive window | quoting in three layers, an absolute socket path, no cwd or env option | `open -n NAME --cwd DIR --env K=V -- program` |
| 2 | A 15-line `no-quickedit.ps1` (P/Invoke `SetConsoleMode`) run in every new console, after the owner twice froze a hosted session by clicking in its window (QuickEdit starts a selection, "Select" appears in the title bar, and the console stops its process until Esc) | a script per launch, and a freeze when forgotten | `host --stdio` and `attach` turn QuickEdit off (`--quick-edit` keeps it) |
| 3 | `title` in the launching `cmd`, which the hosted program then overwrote with its own title | windows could not be told apart | `--title` is pinned: title sequences are filtered from the stream shown in a titled window |
| 4 | `mode con cols=150 lines=40` to size the window | one more shell command in the launcher | `--fit-window` (`open` does it) |
| 5 | An absolute socket path in every call, because a relative name on Windows resolves against the current directory | long, error-prone commands | `-n NAME` everywhere; `socket` rules unchanged |
| 6 | `Get-CimInstance Win32_Process` filtered by command-line text to find hosts, and counting them to see if a window was still alive | process hunting by string match | `list`, `status` |
| 7 | `taskkill /PID <cmd> /T /F` to close a finished window | risk of killing the wrong tree | `stop -n NAME` |
| 8 | A loop sending 50 characters, sleeping 150 ms, repeating, a pause, then Enter, because a fast burst is read as a paste by some TUIs and the Enter that follows is swallowed | the loop in every script | `send` and `ask`: `--paced`, `--chunk`, `--pause-ms`, `--enter-delay-ms` |
| 9 | Writing a long prompt to a file and reading it back in the script, to avoid shell quoting | glue code | `--file PATH` |
| 10 | Raw escape bytes for arrow keys to answer a selection dialog (`ESC [ B`), which `send` could not express | opaque and wrong when the program uses application cursor keys | `key Down Down Enter`, `send --key`, `Down*3` |
| 11 | A Bash loop polling `screen` and `grep`-ing for "Enter to select" or "Do you want to", with `md5sum` to report each dialog once | a hand-written classifier | `wait --asking`, `watch` |
| 12 | The same loop for "the worker stopped": the screen unchanged for 12 seconds and no "esc to interrupt" in the status line. It missed a worker that ended its turn with a question in plain words, **twice** | the owner had to notice | `watch` classifies by output silence (`stalled`), not by words |
| 13 | `screen \| Where-Object Trim \| Select -Last N` and `grep -v '^\s*$' \| tail` in every call | whole 40-line screens read for 3 lines | `screen --tail N`, `--compact` |
| 14 | Plain text drops attributes, so grey "suggestion" text in a prompt looked like typed input, and Enter was sent on it | a wrong keystroke | `screen --json --spans` (coloured runs) |
| 15 | `ask` returned exit 0 on timeout | a script could not tell "answered" from "gave up" | exit 124 on timeout, 125 when the program ended |
| 16 | `Start-Sleep` and polling until a pattern showed, or until nothing changed | many requests per second | `wait --for REGEX --idle-ms N --gone REGEX`, one request, server side |
| 17 | `Start-Process -WorkingDirectory`, environment set in the launcher | not part of the command | `--cwd`, `--env` on `host`, `run`, `open` |
| 18 | A background host started from a tool that captures output kept the caller waiting until the host ended (the host inherited the caller's pipe) | a hang, found by an integration test | `open` and `foreground -- cmd` stop their own standard handles from being inherited |

## Interface

```sh
# Start a program in its own visible window and name the session
agent-cli-lab open -n ux --title "ux lab" --cwd D:\work\clone -C 150 -R 40 -- pi --no-session
agent-cli-lab open -n bg --detached -- python3 -i          # no window; control it with the same commands

# Everything below accepts -n NAME instead of -s PATH (and $AGENT_CLI_LAB_NAME)
agent-cli-lab list [--json] [--all]
agent-cli-lab status -n ux [--json] [--stalled-ms 8000]
agent-cli-lab send -n ux --paced "a long prompt" -e         # or --file brief.txt
agent-cli-lab send -n ux -k Down -k Down -k Enter
agent-cli-lab key  -n ux Down*2 Enter                       # Esc, Tab, Shift-Tab, Ctrl-C, Alt-x, F1-F12, y ...
agent-cli-lab ask  -n ux --paced --file brief.txt --until "ALL DONE" -t 600 --tail 20
agent-cli-lab wait -n ux --asking | --for REGEX | --idle-ms N | --gone REGEX  [-t 120] [--tail 12] [-q]
agent-cli-lab watch -n ux [--only asking,stalled,gone] [--stalled-ms 8000] [--json] [--once]
agent-cli-lab screen -n ux [--tail N] [--compact] [--json [--spans]]
agent-cli-lab stop -n ux
agent-cli-lab host -n NAME [--title T] [--cwd D] [--env K=V] [--stdio [--fit-window] [--quick-edit]] -- program
```

### Targets

`-s PATH` (spec 004), or `-n NAME`, or `$AGENT_CLI_LAB_SOCKET`, or `$AGENT_CLI_LAB_NAME`, or the default socket; `-s` and `-n` together are an error. A name is 1 to 40 of letters, digits, `.`, `_`, `-`, not starting with `.`. `-n NAME` is the socket `<session dir>/NAME.sock`; the session directory is `$AGENT_CLI_LAB_DIR`, else `%LOCALAPPDATA%\agent-cli-lab\sessions` on Windows, else `$XDG_RUNTIME_DIR/agent-cli-lab`, else `$TMPDIR/agent-cli-lab`. A host started with `-n` (or by `open`) writes `NAME.json` there (pid, socket, command, cwd, size, start time) and removes it when it exits. The file is only a list for `list` and the pid `stop` falls back on; **the socket stays the single source of truth**: `list` asks each entry's host and deletes entries that do not answer (`--all` shows them as `dead`).

### States

`status` and `watch` report one of: `working` (the program wrote output within `--stalled-ms`), `asking` (a question or permission dialog is at the bottom of the screen and the screen has been quiet 400 ms), `stalled` (no output for `--stalled-ms` and no dialog: the program finished its turn and waits for input, whatever it printed last), `gone` (the host no longer answers or the program exited). A dialog is recognised by phrases such as "Enter to select", "Esc to cancel", "Do you want to", "(y/n)", "[Y/n]", "Allow once", "wants to use", looked for in the last 20 non-empty lines only. A question asked in ordinary words is deliberately **not** `asking`: it is `stalled`, which is what the commander must react to. Quietness is measured by the host from the program's output, not by diffing screens, so a blinking cursor or a spinner is not mistaken for "idle", and a status line that changes while the program is busy is not either.

### Waiting

`wait` blocks until every given condition holds at once: `--idle-ms N` (no output for N ms), `--for REGEX` (repeatable, all must match the screen), `--asking`, `--gone REGEX` (none may match). It exits 0 when met (printing the last `--tail` lines unless `-q`), 124 when `-t` seconds pass first, 125 when the program ended first. `ask` is `send` + Enter + `wait`: by default until the screen has been quiet `--idle-ms` (at least that long after the text was sent, so an earlier quiet screen cannot count as the answer); with `--until REGEX` or `--asking` until that holds; the same exit statuses.

### Typing

`--paced` is `--chunk 50 --pause-ms 150 --enter-delay-ms 800`; each flag overrides its part. Chunks are counted in characters and never split one. Without any of them the text, keys and Enter go in a single write exactly as in spec 004. `--file` strips the file's final line break (Enter is a separate choice). Named keys: `Enter Esc Tab Shift-Tab Space Backspace Delete Insert Home End PageUp PageDown Up Down Left Right F1..F12 Ctrl-<letter or @[\]^_?> Alt-<key>`, a single character is itself, `Name*N` repeats up to 100. Arrow keys use `ESC O x` when the program has turned on application cursor mode (read from the host), `ESC [ x` otherwise.

### `screen --json`

`{rows, cols, cursor:[row,col], hide_cursor, application_cursor, alternate_screen, title, lines:[...]}`, and with `--spans` also `spans:[{row,col,len,text,fg,bg,bold,italic,underline,inverse}]` for every run of cells that is not in the default style. A caller finds grey suggestion text by its foreground colour. (The VT parser does not track the "faint" attribute, so faint is reported as whatever colour the program sent.)

## Protocol additions

Same transport and one-line framing as spec 004. New operations: `status`, `screen_json` (`{spans:bool}`), `wait` (`{spec:{idle_ms,patterns,asking,gone}, timeout_ms, min_wait_ms}`), `stop`. Existing `send`, `screen`, `screen_ansi`, `resize` are unchanged. Every connection is served on its own thread (at most 64), so a `wait` that blocks for minutes never delays a `send` or a `screen`. After the program exits the host keeps answering for 300 ms so that a waiter can read the final screen and the exit code, then goes away as before.

**Compatibility.** A host that does not know an operation answers `unknown operation`. The client then does the job itself with `send` and `screen`: `status` reports `legacy` and can only classify by screen, `screen --json` returns lines, `wait`/`ask` poll, `key` assumes normal cursor keys. `stop` needs the new operation or a registry entry with the host's pid; for an older host that has neither it refuses ("close its window or end its process") instead of guessing. Hosts that are already running therefore keep working with a newer client.

## Acceptance criteria and where they are covered

Automated (`tests/session.rs`, 12 integration tests; unit tests in `keys`, `state`, `session`, `commands`, `conpty`):

- A named host is listed, reachable by `-n`, answers `ask`, and `stop -n` ends it and its entry. (`a_named_session_is_listed_reachable_and_stopped`)
- `wait --for` matches; a condition that never holds exits 124 and `-q` prints nothing; a wait without a condition is an error. (`wait_matches_a_pattern_and_times_out_with_124`)
- A dialog is detected (`state: asking`) and answered with `key y Enter`. (`a_dialog_is_detected_and_answered_with_named_keys`)
- `status` tells `stalled` from `working` and reports size, name, running. (`status_tells_working_from_stalled_and_reports_the_host`)
- `watch --once --only stalled` reports a stall as one JSON object. (`watch_reports_a_stall_once_and_exits`)
- `screen --json --spans`, `--tail`, `--compact`. (`screen_can_be_trimmed_and_read_as_json`)
- Paced typing delivers non-ASCII text intact; `--file` works. (`paced_typing_and_a_text_file_reach_the_program_intact`)
- `ask --until` that never shows exits 124. (`ask_exits_124_when_the_pattern_never_shows`)
- `--cwd` and `--env` reach the program. (`cwd_and_env_reach_the_hosted_program`)
- `open --detached` names a session, refuses a duplicate name, and `stop` removes it. (`open_detached_names_a_session_that_list_and_stop_can_reach`)
- Bad names and missing sessions fail with a clear message; `-s` with `-n` is refused. (`bad_names_and_missing_sessions_fail_clearly`)
- A stale registry entry is pruned by `list`. (`a_stale_registry_entry_is_pruned_by_list`)
- Title sequences are removed, other sequences pass through, a sequence split over reads is handled. (`conpty::tests`)
- Key encoding, application cursor mode, repeat limits, unknown keys. (`keys::tests`)

Manual, on the machine that has the windows (not automatable without a desktop):

- `open` without `--detached` shows a window titled as asked, sized to `-C`/`-R`, and a click in it does not enter "Select" mode. **Verified:** the window opens, the program runs in it, `status`, `screen`, `key` and `list` reach it; the title is set; QuickEdit-off and window fitting are implemented from the Win32 documentation and were **not** inspected visually by the author.

## Bugs found by building this, and kept fixed

1. **A background host inherited the caller's output pipe** (Windows), so `open` hung any caller that captured its output until the host ended. Found by the `open` integration test hanging. Fix: the launcher clears the inherit flag on its own standard handles and starts the host with NUL standard handles; a host in its own new console then re-points its standard handles at that console (`adopt_own_console`).
2. **`portable-pty`'s Windows `kill` returns an error with OS code 0 when it succeeds** (its result check is inverted), so `stop` reported "The operation completed successfully" as a failure. Treated as success; any other error is still an error.
3. **The hosted program replaced the window title** (cmd and most TUIs set their own). Fixed by filtering title sequences in a window that was given `--title`.
4. **`ask` after a long quiet period could return before the program reacted**, because quiet time was measured from the last output, which could be minutes old. `ask` now also waits at least `--idle-ms` after sending.
5. **A hand-written watcher classified by words and missed a worker that ended with a question**: the reason `stalled` is defined by silence.

## Verification (2026-10-17)

- Windows (MSVC), native: `cargo test`: 43 unit tests, 8 existing integration tests and 12 new ones passed; `cargo clippy --all-targets` and `cargo fmt --check` clean.
- Linux (x86_64-unknown-linux-gnu) and macOS (aarch64-apple-darwin): `cargo clippy --all-targets --target ...` clean (type-checked and linted, **not executed**); the Unix-only paths (`kill -TERM` fallback, process group, opening the attach terminal after `open`) have therefore not run. CI (spec 005) is the place that runs them.
- Against three hosts started by the previous binary and still running during the work (a Claude Code TUI and two pi TUIs): `status`, `screen --json`, `screen --tail`, `wait --for` (124), `wait --idle-ms` and `stop` (refused, nothing killed) behaved as the compatibility rules say, and the sessions were undisturbed.
- Used in anger: two pi labs were opened with `open`, briefed with `send --paced --file`, and watched with `watch`, which reported a lab going quiet at its review gate.

## Out of scope

Remote hosts, authentication beyond spec 004's owner-only socket, recording of the new operations, a `wait --exit` flag (the exit statuses already say), key sequences with modifiers other than Ctrl and Alt, and Linux/macOS window sizing and QuickEdit (they have no equivalent).
