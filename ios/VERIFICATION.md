# Local acceptance evidence

Verified 2026-09-27 with Xcode 26.0.1 / Swift 6.2. The app and smoke tool use
the same `RiWorkCore` transport, without third-party dependencies.

- `swift test --package-path ios`: 9 tests, zero failures. Independent published
  v1 proof/key/ciphertext vectors, tamper/replay/order/direction/version rejection,
  fresh reconnect keys, pairing/TLS/request validation and terminal geometry.
- Signed `xcodebuild ... CODE_SIGN_IDENTITY=- test` on iPhone 17 Pro / iOS 26:
  15 tests, zero failures (9 protocol + 6 state). Keychain reload, background
  selection, persistent unconfirmed input without retry, immutable reviewed input,
  clear-before-tab-resize-before-output, reconnect resize and no session creation.
- Signed app build/install/launch on iPhone 17 Pro / iOS 26 and iPad Pro 11 M4 /
  iOS 18.1. RiWork **cua-driver MCP** inspected project selection and single
  terminal tabs; Accessibility and Screen Recording were available.

## Real Rust / Swift and tmux

```sh
swift build --package-path ios
python3 ios/scripts/local-smoke.py \
  --relay-binary /absolute/path/to/remote/target/debug/riwork-remote \
  --riwork /absolute/path/to/rebuilt/target/debug/riwork --viewport
```

Used the relay worker's rebuilt binaries from its assigned worktree, the exact
published `docs/remote-protocol.md` including its additive viewport extension,
and a fresh private temporary `RIWORK_HOME` per fixture. The inherited environment
was retained for all other work. No production project or session was modified.

Both isolated runs authenticated, listed real projects/worktrees/tasks/shells/
orchestrators, read pre-existing session output and sent one explicit line.
`stty size` reported **17 rows / 43 columns**. A fresh Swift reconnect re-applied
that viewport, returned the cached acknowledgement for the same logical input
UUID, and left the execution-count file exactly `x`. Peer-loss cleanup restored
**100×30**, retaining the same single pane and process.

First run: shell `da9b1492-fb27-4055-9541-60cfb16dd5fa`, pane `%0`, PID `81668`.
Second run: shell `701d7067-532f-4f7d-b291-88a7137b4f83`, pane `%0`, PID `9742`.

Native iPhone pairing by deep link, project selection and tab switching read the
first real shell. Cua reviewed its full UUID and exact `printf` line, submitted
once, then observed **NATIVE_IOS_CONTINUED** in both app and CLI output. Phone
viewport was **42×23**; disconnect restored **100×30**. Reopening the app and
project reconnected to the same selected shell, retaining its output and state.

The iPad used a separate device fixture. Its selected shell measured **92×47**;
the previous orchestrator tab returned to **100×30**. Showing the software keyboard
reduced the selected shell to **92×28**, preserving its pane/PID.
Native testing caught and fixed a keyboard feedback loop: draft editing now stays
enabled during resizing, while submission waits for the acknowledged grid. A
third isolated fixture rechecked the actual software keyboard, editable prompt
and fresh output after fitting. Unicode line/paragraph separators are rejected
consistently with the desktop contract.
That shell was `da37334a-504b-4999-8bd7-150de122d125`, pane `%0`, PID `86467`:
the software keyboard stayed at **92×28**, landscape became **136×22**, native
backgrounding restored **100×30**, and foreground resume retained the selected
session and re-applied its viewport. No draft was sent during keyboard testing.

Final native cleanup verified the pairing-removal alert on both iOS 18.1 iPad
and iOS 26 iPhone and returned both apps to their empty state. Test pairings were
revoked before removal. A final navigation change makes connection activation an
explicit desktop-selection action; returning to the project list keeps manual
disconnect in place. Both final signed simulator builds pass.

## Limits

Physical camera scanning and a public trusted-TLS relay have not been exercised.
The v1 wire provides text snapshots, resize and one-line input; arbitrary PTY
key events / Ctrl-C / full ANSI emulation are outside this contract. Desktop
resizing affects the existing tmux grid Ghostty renders, rather than the macOS
application window's frame. Production setup requires the rebuilt desktop CLI
and matching connector, as documented in `README.md`.
