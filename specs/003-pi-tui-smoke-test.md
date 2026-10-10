# Pi TUI smoke test

## Result

**Passed on Pi 0.87.1.** `agent-cli-lab` launched the real Pi interactive TUI in a 120×36 PTY, submitted two prompts, captured both responses, processed `/exit`, restored terminal modes, and propagated exit code 0.

Observed conversation:

```text
Reply exactly: SMOKE_ONE
SMOKE_ONE
Now reply exactly: SMOKE_TWO
SMOKE_TWO
```

The capture contains Pi's startup screen, input editor, animated `Working` frames, responses, session-close screen, and terminal cleanup sequences.

## Reproduction

```sh
(
  sleep 2
  printf 'Reply exactly: SMOKE_ONE\r'
  sleep 20
  printf 'Now reply exactly: SMOKE_TWO\r'
  sleep 20
  printf '/exit\r'
  sleep 2
) | target/debug/agent-cli-lab run \
      --cols 120 --rows 36 \
      --record artifacts/pi-smoke-cr.jsonl \
      -- pi --no-session --no-tools --no-skills --no-extensions --offline
```

Capture: `artifacts/pi-smoke-cr.jsonl` (111 events, about 150 KiB in this run).

## Problems discovered

1. TUI Enter is carriage return (`\r`), not the line feed (`\n`) normally produced by scripted pipes. LF filled Pi's editor but did not submit it.
2. Input sent before TUI initialization can be handled by the PTY's initial canonical mode, so scripted scenarios need an initial expectation or delay.
3. Fixed sleeps are fragile; the P1 scenario runner should wait for screen text such as the idle prompt or completed response.
4. JSON byte arrays are lossless but large and inconvenient to replay. A replay/export command should reconstruct raw ANSI or rendered text.
5. The current capture records terminal bytes, not screenshots or a terminal screen model.
