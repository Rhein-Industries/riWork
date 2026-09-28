# Local acceptance evidence

Verified 2026-09-27 with Xcode 26.0.1 / Swift 6.2. App and smoke share
`RiWorkCore`; no third-party dependencies or external AI calls.

## Final flow and automated checks

Project picker → flat open-terminal tabs → one measured terminal. Ordinary Send
captures the selected shell UUID and current line and uses the expected-ID guard.
There is no routine review sheet. Full UUID/kind/cwd are available in Session info.
The unconfirmed-input warning and explicit acknowledgment remain, including when
all tabs have closed. Reconciliation clears the old draft/output/notice, preserves
pending input on its original shell, and submits no input.

- `swift build --package-path ios` and `swift test --package-path ios`: PASS;
  9 independent crypto/protocol/geometry tests at the time (31 after the review fixes below).
- Signed `xcodebuild ... CODE_SIGN_IDENTITY=- test`, iPhone 17 Pro / iOS 26:
  **19 tests, zero failures** (9 protocol + 10 state) at the time. Final log:
  `/tmp/riwork-ios-compact-final-tests.log`.
- State coverage includes captured Send line/changed-selection rejection,
  Keychain reload, background resume/manual disconnect, durable uncertain outcomes,
  resize-before-output, previous-tab release, absent saved selection fallback,
  closed last-tab clearing with pending retention, and a single `not_found`
  recovery refresh without repeated dead-UUID polling.
- Final signed iPad Pro 11 M4 / iOS 18.1 build: PASS,
  `/tmp/riwork-ios-compact-final-ipad.log`. Simulator install/launch also passed.
- Xcode emits only the expected AppIntents metadata extraction notice; the app
  has no AppIntents dependency. Ad-hoc simulator signing is needed for Keychain.
- Final desktop-row options-menu polish built on both simulators; native menu and
  immutable removal alert removed only the two revoked fixture pairings. Both
  installed apps were left at the compact empty state. Phone build log:
  `/tmp/riwork-ios-compact-menu-build.log`.

## Review fixes, 2026-09-28

Xcode 26 / Swift 6.2, iPhone 17 simulator (iOS 26.0), ad-hoc signing.

- `swift test --package-path ios`: **31 tests, zero failures** (15 protocol + 16 RelayClient).
- `xcodebuild ... CODE_SIGN_IDENTITY=- test`: **53 tests, zero failures** (31 core + 22
  app). `CODE_SIGNING_ALLOWED=NO ... build` also passes for Debug and Release; the built
  Debug `Info.plist` has `NSAllowsLocalNetworking`, the Release one has no
  `NSAppTransportSecurity`.
- New coverage: the scripted-desktop `RelayClient` tests (cancelling one caller keeps the
  socket and the counter sequence, a queued cancelled request is still sent in order, late
  responses are dropped, handshake failure/timeout, request timeout, keepalive, missing
  pong, relay close codes); counter vectors above 0 for nonce and AAD; hostile pairing
  links; OSC stripping; the command field's UIKit traits, Return handling and paste rules;
  and state tests for release-before-close with a poll in flight,
  Back during a project load, manual disconnect through backgrounding, oversized-output
  `lines` fallback, and an unreadable Keychain library that is never overwritten.
- Not re-run in this pass: the real Rust relay/connector smoke above, physical-device
  camera/TLS checks, and simulator UI walkthroughs. Keepalive over a real network,
  `beginBackgroundTask` timing, the keyboard's smart-punctuation behaviour and the deep-link
  confirmation sheet were checked only by build and unit tests; verify them on a device.

## Real Swift / Rust / tmux

```sh
swift build --package-path ios
python3 ios/scripts/local-smoke.py \
  --relay-binary /absolute/path/to/remote/target/debug/riwork-remote \
  --riwork /absolute/path/to/rebuilt/target/debug/riwork --viewport
```

Used the owner's rebuilt CLI/relay binaries and exact published v1 contract,
including resize/clear, without modifying its worktree. Two fresh private fixture
registries ran the final concurrent smoke; logs:
`/tmp/riwork-ios-review-interop.log`, `/tmp/riwork-ios-compact-interop.log`.
Inherited `RIWORK_HOME` remained unchanged; only fixture children used temporary
homes.

Each run authenticated, listed real entities, read a pre-existing session, and
validated **eight overlapping RelayClient output RPCs** on one authenticated
connection. Every result had the chosen full shell UUID and nonempty output;
all eight completed and the same connection remained authenticated. The bounded
batch is below the relay's 16-message queue. Rust's strict next-counter validation
therefore exercises actual encrypted send ordering, including the sendTail queue.

One explicit input then executed once. Fresh reconnect returned the cached
acknowledgment for the same request UUID; the execution file stayed exactly `x`.
`stty size` reported **17×43**, and peer loss restored **100×30** with the same
single pane/process. Revocation refused a fresh Swift connection. Both runners
completed cleanup of only their recorded fixture processes/sessions.

Run 1: shell `46740cc2-17dd-48d4-8aea-c75611c263bc`, pane `%0`, PID `20015`.
Run 2: shell `ddfb2049-a686-4008-8319-c2e3afcd01b4`, pane `%0`, PID `58335`.

## Native Cua evidence

RiWork **cua-driver MCP** only. Read the actual desktop appearance without
interacting with its sessions, then matched its compact rows/tab strip, Menlo
text, thin separators and warm palette. iPhone inspection confirmed the project
picker, full identity in compact Session info, actual output, direct Send and
stale/disconnected feedback. Final compact iPad empty layout was also inspected.
Screenshots are local ignored artifacts under `ios/Verification/`.

The compact phone grid was **53×41**, or **53×40** with the submission notice.
Cua verified the exact draft before Send; `COMPACT_IOS_CONTINUED` appeared exactly
once in that selected shell's actual CLI output. There was no confirmation sheet.
After closing that disposable shell, its active viewport caused peer disconnect;
explicit reconnect selected the remaining project orchestrator
`2f9fd88e-b418-446a-bad0-b5d3f7659f96`, cleared the previous output/notice, and
read that existing session. Its output contained no continued command. Closing
that final disposable session and reconnecting cleared selection and displayed
“NO OPEN TERMINALS”. No input was sent during either repair.

Prior native keyboard/rotation/background checks remain applicable to the same
viewport lifecycle: iPad keyboard retained focus while resizing, rotation updated
the grid, and background restored desktop dimensions before fresh resume. These
unaffected flows were not repeated during this bounded review. Screen capture
worked; Simulator text focus required the driver's foreground/native paste path.

## Limits

Physical camera QR and public trusted TLS were not exercised. v1 supplies text
snapshots/resize/one-line input, without raw key events, Ctrl-C or full ANSI
emulation. Resize pins the existing tmux grid rendered by Ghostty, not the macOS
window frame. Local adaptive palettes match desktop presets; v1 does not expose
its theme preference for automatic synchronization. PSK v1 has no forward secrecy.
Setup/open/build/install/pair instructions: `README.md` in this directory.
