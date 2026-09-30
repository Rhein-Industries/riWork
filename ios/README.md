# RiWork for iPhone and iPad

Choose a paired RiWork desktop and project, then switch between tabs for its
existing open terminals. One terminal fills the screen at a time. The app never
creates, splits, closes or replaces a desktop session.

The interface follows RiWork’s compact desktop layout: flat rows and tab strips,
thin dividers, Menlo text and a small workspace bar. Light appearance uses the
desktop’s Gruvbox Light palette; dark appearance uses its RiWork palette. Buttons
retain 44-point touch targets, accessible labels and text scaling. v1 does not
export desktop theme preferences, so automatic theme synchronization is not available.

## Open, build and install

Open `ios/RiWorkRemote.xcodeproj` in Xcode 26, select `RiWorkRemote`, and choose an
iPhone or iPad. The deployment target is iOS 18.0. Select your development team
in Signing & Capabilities for a physical device. No third-party packages or AI
service keys are needed. Regenerate the checked-in project from `project.yml`:

```sh
cd ios
xcodegen generate
xcodebuild -project RiWorkRemote.xcodeproj -scheme RiWorkRemote \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' \
  -derivedDataPath .derivedData CODE_SIGN_IDENTITY=- test
xcrun simctl install booted .derivedData/Build/Products/Debug-iphonesimulator/RiWorkRemote.app
xcrun simctl launch booted com.riwork.remote
```

Use a specific simulator UDID when several are running. Keep ad-hoc simulator
signing enabled. `CODE_SIGNING_ALLOWED=NO` compiles but cannot use Keychain (-34018).
`xcodegen generate` also derives `RiWorkRemote/Info-Debug.plist` (`Info.plist` plus the
Debug-only local-networking ATS exception, via `postGenCommand`); commit both plists.
Release builds keep default App Transport Security and hide the local-relay switch.

## Pair and connect

Follow `remote/README.md` and `docs/remote-deployment.md` in the combined repository.
Create a device pairing, provision its route hashes on the relay, and run the
desktop connector with the rebuilt RiWork CLI and its existing `RIWORK_HOME`.
Terminal fitting requires the published `shell.resize` extension and the new
desktop CLI's `shell resize` / `shell resize-clear` commands. An older installed
CLI can list/read sessions but cannot fit them; update both desktop and connector.

Tap **Add desktop**, enter a name, and paste the complete pairing JSON or
`riwork://pair?v=1&data=…` link. The code field is a UIKit text view with smart
punctuation, predictions and writing tools off, so a pasted or typed quote or hyphen
is stored unchanged. The form shows the parsed relay host, device name
and desktop ID before anything is saved. Tap **Save pairing & connect**. Opening a
link from another app or web page, which any of them can do, also opens the form,
hides the raw data and asks for an explicit **Pair** confirmation naming the relay
host and device. Scanning a QR code fills the form like a paste. Supported physical
devices can scan with camera permission; simulator/unsupported devices can paste.
Links with userinfo, a port, percent-encoding or an uppercase scheme are rejected.

Physical devices require a trusted `wss://…/v1/ws` relay. Simulator testing in a
Debug build can explicitly enable **Allow local development relay**, which permits
`ws://` only on literal loopback hosts (Release builds have neither the switch nor the
ATS exception). Pair each device separately. Transfer the secret export
directly to the intended device and remove it after import.

Pairing keys, relay tokens, selections and unconfirmed input are stored only in
device-local Keychain (`WhenUnlockedThisDeviceOnly`). Only a missing Keychain item
means "no desktops". If the stored library cannot be read or decoded, the app shows
**Saved pairings unavailable** with **Try again**, refuses every write so nothing is
overwritten, and offers a confirmed **Erase saved pairings** as a last resort. The app does not use
UserDefaults for secrets, save terminal output to disk, or log pairing/frames.
Rename/remove a desktop through its row’s options menu, context menu or swipe actions. Removal keeps
desktop sessions running; revoke the device on the desktop to deny access.

## Project and terminal tabs

Select a project, then choose a worker, shell or orchestrator tab. Tabs show actual
short session IDs and worktree branches. **Session info** exposes the full UUID,
kind and working directory. The compact status row keeps snapshot freshness,
connection state and measured terminal dimensions visible.

Each project remembers its selected terminal. On refresh/reconnect, a saved tab
that is closed or absent falls back to an available live project tab; selection
clears if none remain. Old draft/output state clears on fallback. Pending input
stays attached to its original shell and is never resent or moved to the new tab.
If output returns `not_found`, the app refreshes project sessions once, reconciles,
and stops polling that UUID. Explicit refresh or reconnect can check it again.
Refresh or follow output from the toolbar. Output is requested with `lines: 500`; if
the desktop answers `response_too_large` or `cli_error` (its reply cap is 128 KiB, which
wide grids with multibyte scrollback can exceed) the app halves `lines` down to 20,
remembers the size that fits for that session, and resets it on reconnect or refresh.
Backing out of a project while it loads only abandons that load; the connection stays
up and the project loads again when reopened.

The visible terminal measures its character cells and calls the exact v1
`shell.resize` RPC before reading output. Selecting a tab, rotating the device,
opening the keyboard or changing text size updates the selected shell's grid.
Ghostty renders that same existing tmux shell; no split or new session is created.
The override restores desktop sizing on tab release, backgrounding or disconnect.
Reconnect authenticates with fresh keys before reapplying the selected tab's grid.
The server also releases lost connections and recovers a connector crash within
15 seconds. Another device's active override produces a useful error.

Input is one control-free physical line, at most 8192 UTF-8 bytes. The command field
is a single-line UIKit text field with smart quotes, smart dashes, autocorrection,
autocapitalization, inline prediction, math completion and writing tools off, so
`--oneline` and `"` reach the shell as typed. Multi-line
pastes are refused (one trailing newline is dropped). Tap **Send** (the upward arrow)
or the keyboard's Send key to submit directly to the visibly selected tab. Send captures
that shell ID and the current line; a changed selection prevents delivery to a
different tab. The desktop sends that line followed by Return. The app persists
its request UUID
and line before sending. An uncertain result blocks further submission and
survives reconnect/app restart. Review the indicated session before acknowledging
the warning. Acknowledgement does not submit or retry anything.

### Direct typing, focus mode and text size

When the desktop supports `shell.keys`, tapping the terminal opens the keyboard and
every key goes straight to the shell, which echoes it on the screen you are looking
at. The line composer above stays for older desktops (detected on the first key: an
`invalid_request` "unsupported RPC method" hands what was typed back to the composer)
and can be chosen anyway from the terminal menu. Detection is per connection.

A hidden `UIKeyInput` view captures the keys (autocorrection, smart punctuation and
prediction off; return key "return"). A key bar sits above the keyboard: Esc, Tab, a
sticky Ctrl (armed until the next letter, sent as `C-<letter>`), arrows (hold to
repeat), Paste and Hide keyboard. A hardware keyboard sends arrows, Esc, Tab,
Shift-Tab, Home/End/Page keys and Ctrl-letters. Newlines in typed or pasted text
become Enter, tabs become Tab, other control characters are dropped.

Keys are queued in order per shell and sent by one sender with exactly one batch in
flight, coalescing adjacent text every ~40 ms (or at once when idle). A batch ends at an
Enter (the desktop pauses ~150 ms whenever a key follows text, so typical batches are
`[text, Enter]`), and `shell.keys` gets a request timeout of at least 10 s. A batch gets its
UUID when it is formed and keeps it until it succeeds; after a lost connection or timeout it
is resent unchanged with a new request id, and the desktop answers `duplicate` if it
already arrived. While the connection is down (the app reconnects by itself while input
waits) keys stay in a local buffer of at most 4096 characters / 512 items, per shell; more
is refused with "Buffer full". A small chip above the keyboard shows the pending
input (⏎ ⇥ ⌫ ⎋ ↑ ↓ ← → ^C), cut at the front, only once it is older than 300 ms or the
link is down; its ✕ discards it. `input_unavailable` and `not_found` keep the buffer and say
why; `uncertain` shows a short note. The buffer lives in memory only.

The screen is read every ~300 ms for 2 s after typing, every 1 s for the next 10 s, then
every 3 s, one read in flight at a time. When `shell.output` carries `cursor`/`rows`, a block
cursor is drawn there; `in_mode` shows a COPY MODE badge.

**Focus mode** (header button, or double-tap the header) hides the header, tabs, status rows and badges and gives the shell
the whole screen inside the safe area, in portrait or landscape, keeping the display awake. Only the
terminal, the keyboard with its key bar and the pending chip remain, plus a translucent
corner control (text size, leave) that fades after a few seconds and returns on tap.
The choice is remembered per session for the app run. Text size (8–24 pt, default 12) is set by
pinching, the menu or the corner control, and is saved. Focus mode, rotation, the keyboard and text
size all recompute the terminal grid; the resize request is debounced (150 ms).

Backgrounding runs the viewport release under a UIKit background task, then discards
connection keys and marks output stale; if iOS runs out of time the desktop restores its
size within 15 seconds anyway. Returning reconnects if the connection was active, with a
fresh handshake and the same selected session. Manual disconnect remains disconnected,
including through backgrounding. While connected the phone sends a WebSocket ping every
10 seconds (the relay never pings mobile sockets) and drops the connection if no pong
arrives within 20; the URLSession idle timeout is 30 seconds. Relay close codes and
reasons are shown as readable messages, for example a duplicate or unauthorized device. Reconnect/tab selection never recreates
a terminal or retries input.

## Tests and real relay smoke

`Core/RelayClient.swift` uses `URLSessionWebSocketTask` behind the `WebSocketConnection`
seam in `Core/WebSocketTransport.swift`, so `Tests/RelayClientTests.swift` can drive its
handshake, cancellation, timeout, keepalive and close-code paths with a scripted desktop
that enforces the exact-next-counter rule. Cancelling one request detaches only that
caller: its already-sealed frame is still sent in order and its late response is dropped.
`SessionCrypto.swift` implements the relay's exact v1 HMAC/HKDF/ChaCha20-Poly1305 contract in
`docs/remote-protocol.md`. The checked-in fixture is copied verbatim from the
relay's independently generated `remote/fixtures/v1.json`. Counter vectors above 0
(`Tests/Fixtures/counter-vectors.json`) come from an independent RFC 8439 implementation,
`scripts/gen-counter-vectors.py`, which first reproduces that fixture at counter 0.

```sh
swift test --package-path ios
swift build --package-path ios
python3 ios/scripts/local-smoke.py \
  --relay-binary /absolute/path/to/remote/target/release/riwork-remote \
  --riwork /absolute/path/to/rebuilt/riwork --viewport
```

The runner creates a temporary registry, Git project, task, zsh shell and
zsh-backed orchestrator, then starts a real Rust relay/connector. The Swift tool
uses the app's transport/crypto to list entities, read the existing session,
validate eight concurrent read RPCs on one authenticated connection, submit one
line, reconnect with fresh keys, explicitly repeat the same UUID to
check deduplication, verify exactly one execution and shell survival, and verify
revoked-device denial. With `--viewport`, the real shell's `stty size` must report
17 rows / 43 columns, then desktop dimensions must return to their baseline with
the same pane and process after peer loss. Cleanup closes only recorded fixture sessions/processes.
The eight-request batch stays within the relay's 16-message queue and exercises
actual encrypted send ordering/counters, rather than a mock transport.
Parent `RIWORK_HOME` stays unchanged. `--hold SECONDS` keeps the disposable
fixture available for simulator inspection.
Send SIGUSR1 to the reported fixture runner PID to finish the hold early, verify
revocation and clean up; Ctrl-C also cleans up its recorded sessions/processes.

For a pre-existing isolated fixture:

```sh
swift run --package-path ios riwork-ios-smoke /secure/fixture.pairing.json \
  --local --project FULL_PROJECT_UUID --shell FULL_SHELL_UUID \
  --columns 43 --rows 17 --send 'printf "explicit isolated continuation\n"'
```

`--send` executes input in the explicitly selected shell. The duplicate UUID
operation is an explicit acceptance test; the app never performs it automatically.

## Current limits

v1 carries readable text snapshots and physical-line submission. Arbitrary key
events, Ctrl-C and a complete ANSI terminal emulator are not part of this wire.
The resize extension pins the existing shell's terminal grid, without resizing
the macOS application window. Bounds are 20–300 columns and 8–160 rows.
Physical camera QR and public TLS deployment require device/operator validation.
v1's PSK handshake has no forward secrecy; rotate compromised pairing keys.

Apple API references: [WebSocket task](https://developer.apple.com/documentation/foundation/urlsessionwebsockettask),
[CryptoKit](https://developer.apple.com/documentation/cryptokit),
[scanner availability](https://developer.apple.com/documentation/visionkit/datascannerviewcontroller/isavailable),
[scroll alignment](https://developer.apple.com/documentation/swiftui/view/defaultscrollanchor(_:for:)).
