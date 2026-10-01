# RiWork for iPhone and iPad

Choose a paired RiWork desktop and project, then switch between tabs for its
existing open terminals. One terminal fills the screen at a time. The app never
creates, splits, closes or replaces a desktop session.

The interface follows RiWork’s compact desktop layout: flat rows and tab strips,
thin dividers, Menlo text and a small workspace bar. Light appearance uses the
desktop’s Gruvbox Light palette; dark appearance uses its RiWork palette. Buttons
retain 44-point touch targets, accessible labels and text scaling.

**Theme sync.** A desktop that supports `appearance.get` (`{"v":1,"updated_at","dark","palette":{bg, panel,
panel_active, divider, cyan, magenta, gold, text, muted},"terminal":{background, foreground, palette[16]}}`, colors
as `#rrggbb`) lets the phone draw with its current colors: cyan is the accent, gold marks warnings, magenta is the
secondary accent (orchestrators, hotkeys), the terminal uses `terminal.background/foreground` (else `bg`/`text`) for its
background, text and block cursor, and `dark` sets the status bar and system controls. The phone asks on connect, when
the app becomes active and every 60 s while connected, one request at a time, and only redraws when the colors actually
changed. The last palette is kept per paired desktop (UserDefaults) and applied at launch and on connect, before the
first answer; the desktop list wears the most recently used desktop's colors. `not_found` ("appearance not published")
keeps the last palette; `unsupported RPC method` stops the asking for that connection and restores the built-in style;
an unreadable palette (invalid hex, wrong count, `v` other than 1) is ignored. A synced text/background pair below a
3:1 contrast ratio falls back to the built-in pair for that element. Without a palette the built-in Gruvbox Light /
RiWork colors apply.

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
Refresh or follow output from the toolbar. Output is requested with `lines: 500` (120 on an iPhone once older lines come from `shell.history`, see Scrolling); if
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
prediction off; return key "return"). A key bar sits on the keyboard, always 44 pt tall and exactly where iOS
puts it: on top of the software keyboard, or alone at the bottom edge when a hardware keyboard is attached. It
scrolls sideways: Esc, Tab, a sticky Ctrl (armed until the next letter, sent as `C-<letter>`), a sticky Alt (Meta,
sent as `Escape` followed by the next key or text, readline style), arrows, Shift-Tab, Home, End, PgUp, PgDn, Delete,
Backspace, Enter (arrows, Backspace, Delete and Page keys repeat while held) and Paste, then the hotkeys, then the
symbols that are awkward on the iOS keyboard (`` | / \ ~ - _ ` * & $ > < { } [ ] ; : ' " ``), then a "+" that opens the
hotkey editor. Hide keyboard stays at the right end. The ends of the row are padded so the first and last key clear
the display's rounded corners (about 20-28 pt derived from the safe area, not from a device model); in focus mode with
a hardware keyboard the bar is a centered pill. The iPhone is portrait only.

**Hotkeys** send a fixed sequence of text and special keys, validated against the `shell.keys` contract (text without
control characters, whitelisted key names only, at most 64 steps). Built in: Ctrl+C, D, Z, L, R, A, E, U, W and Esc Esc.
Your own (a name of up to 12 characters plus steps, e.g. "/clear" then Enter, or Ctrl+C then "exit" then Enter) can be
added, edited, deleted and reordered in the editor, and are kept on the device (UserDefaults). A hotkey ignores an armed
Ctrl/Alt. A hardware keyboard sends arrows, Esc, Tab,
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

**Live sync.** Every `shell.output` asks for `styled: true`. A desktop whose result carries a `hash` is followed by a long
poll: one request in flight with `if_changed: <hash>` and `wait_ms: 8000`, re-issued the moment it comes back, changed or
`unchanged`, so a change (an echo, agent output) reaches the phone as soon as the desktop sees it. Typing starts no extra reads;
the pending one returns on the echo. The request timeout is the wait plus 20 s. A desktop that does not know the new parameters answers `invalid_request` (unknown field), or a connector with an older CLI `cli_error` "does not support styled output": the app then falls back, for that connection, to plain reads and interval polling. The loop does not start a new wait while the
terminal is off screen or the app is not active (one already out runs its course) and resumes from its hash; errors back off
250 ms → 2 s; oversize replies still halve `lines`. The desktop allows two waiting requests per device, so a wait cancelled here
(a session switch, a caller that wants the screen now) is counted until it runs out, and in the rare burst that fills both
slots the screen is followed by short reads meanwhile. An older desktop (no `hash`) keeps the interval polling: about every
300 ms for 2 s after typing, every 1 s for the next 10 s, then every 3 s, one read in flight at a time. `RelayClient` already
matches responses to requests by id, so replies may arrive out of order and a pending wait never delays `shell.keys`.
When `shell.output` carries `cursor`/`rows`, a block cursor is drawn there; `in_mode` shows a COPY MODE badge.

**Colors and symbols.** The styled output holds only SGR sequences (`;` and `:` forms): 16 colors from the synced terminal
palette (or a built-in set that suits the desktop's light or dark side), xterm-256, truecolor, bold, dim (half opacity over the
background, like Ghostty's `faint-opacity`), italic, underline, strikethrough, inverse, hidden and every reset. Malformed or
unknown sequences are dropped whole and never crash. Bold text is not made bright (Ghostty's `bold-is-bright` is off; the
`riwork.boldIsBright` default turns it on). Parsing (`Core/StyledScreen.swift`) happens off the main actor and yields the screen
line by line; the iPhone terminal paints only the rows in view, each with Core Text on a whole device pixel (`TerminalRowView.swift`),
and repaints only rows whose line changed, so a 50,000-line history costs no more than a screen. Every line is exactly one grid row
(its height is the font's line height rounded to a device pixel, which is also what the desktop grid is worked out from) and each glyph
that borrows a font is kerned back to the cells the terminal counts for it, so attributes and fallback fonts never move a column or a
baseline. Backgrounds, underlines and the cursor are painted on the grid, cell by cell.
Symbols that can also be emoji but are text by default (⏺ ⏸ ⚠ ✔ ▶ ℹ …) get U+FE0E, so they draw as monochrome text glyphs in the
foreground color as on the Mac; genuine emoji (✅ 🚀, FE0F, keycaps, ZWJ, flags, skin tones) are left alone. A long press on a line
copies it (or the lines in view); **Copy screen text** in the menu copies the screen and the last 500 lines above it that are loaded.

**Scrolling** (iPhone; the iPad keeps its two-axis scroll view, the latest answer alone and its follow toggle). The terminal scrolls
vertically only: the desktop pane has the phone's width, and a rare longer line is clipped at the edge.

*The surface.* `TerminalSurfaceView` (UIKit, `TerminalSurface.swift`) replaced the SwiftUI `ScrollView` + `LazyVStack` +
`scrollPosition(id:)`, which could not keep the reader in place when lines were added above the view (it found its anchor after the
layout, never under a finger or in a fling, and diffed every identity on each update). Every line has an absolute index that never
changes while lines are added above or below it (`Core/TerminalBuffer.swift`: the first answer numbers the screen's top row
`history_size`, scrolled-in lines keep counting), and line `i` sits at `i × lineHeight` in the scroll view's content
(`Core/TerminalScrollGeometry.swift`). So a page of older lines, or output at the bottom, moves no line at all: the page is put in the
moment it arrives, under a moving finger or a fling, and only how far up the reader may scroll changes (the content inset). The scroll
view is only the physics and the gestures; the rows are drawn into a plain view next to it, relative to the view, one recycled view per
row in view. A live answer reconfigures the visible rows and repaints those whose line differs (usually the last one or two).
The view follows new output only while it is at the bottom or within a line of it. Scrolled up, it stays put, and a pill
"↓ Live · N new" (just "↓ Live" when far away and nothing new) takes the reader back and resumes following; typing, key-bar keys,
sending a line and the menu's "Jump to latest output" do the same. A shell is opened, and switched to, at its bottom. At the top
of the loaded lines is a header row: "Loading…", "Beginning of history", "Showing the last 50,000 lines", or "Couldn't load older
lines · tap to retry".

*History is a second step.* The live screen comes first. Once its first answer is in, the phone fetches older scrollback in the
background (`shell.history`, `styled`), one request at a time, so that scrolling is local with no round trip per scroll. Before every
page `HistoryPrefetch.decide` (`Core/HistoryPrefetch.swift`, pure and tested) chooses whether to fetch, how many lines, or to wait.
- *Bandwidth.* Each page is timed (`LinkMeter`): its bytes as they travelled (JSON-escaped, `HistoryReply.wireBytes`) and its duration.
  The first background request is a ten-line probe that learns the fixed cost of a request (round trip plus the desktop starting its CLI); that is
  taken out of bigger pages, so a long round trip on a fast link is not mistaken for a slow link. The rate is smoothed, and forgotten
  when the kind of link changes (Wi-Fi to cellular). **fast ≥ 1 MB/s** (a 128 KiB page in ~130 ms, the whole 50,000 lines in ~3 s),
  **good 250 KB/s to 1 MB/s**, **slow < 250 KB/s** (below it a worthwhile page blocks the socket for too long).
- *How much.* Fast or good on an unrestricted path: everything, up to the 50,000 lines held. Slow, metered (`NWPath.isExpensive`),
  Low Power Mode: 10 screens above the reader (a fetch starts when fewer than 5 are loaded, so well before the top). Low Data Mode
  (`isConstrained`): 5 screens (starts below 2.5). A reader within 1.5 screens of the top is waiting: no pauses between pages.
- *How big, how often.* A page is what the link carries in about 0.3 s (fast), 0.2 s (good) or 0.12 s (slow), at most 80 KiB on the
  wire and 1000 lines, growing by at most double per page (`response_too_large` still halves it, kept per session). After a page the
  link is left alone for 0.15× (fast), 1× (good) or 3× (slow) its duration, twice that while the screen is changing. The rate kept is
  the best of the last three pages: delays only ever add to a page's time, so one page is more likely too slow than too fast.
- *Never in the way of typing or the live screen.* No page starts while keys are queued, in flight or were typed in the last 0.8 s
  (a page in flight cannot be taken back, but it is sized to hold up an echo for a fraction of a second). The desktop runs the ordered
  lane (`shell.keys`, `shell.input`, `shell.resize`) in a slot of its own, history is a plain read in one of the three shared slots
  (`remote/src/lanes.rs`), and at most two of those hold waiting `shell.output` calls, so a page can neither delay a key batch nor take a
  poll's slot. A background page does not start while two waits hold slots (the live one and a cancelled one still running on the
  desktop), so the third slot stays free for the screen read after Return; a reader at the top, or a tap on the retry row, does not
  wait for that or for the pauses. Nothing is fetched while the terminal is off screen or the app is inactive.
- *Output that outruns an answer.* Once older lines come as history, a live answer asks for 120 lines of scrollback instead of 500
  (about a quarter of the bytes, parse and compare work for every keystroke's echo). More lines than that scrolling by between two
  answers are not thrown away with the history: the gap is kept as blank placeholder lines under their own indexes (a hole,
  `TerminalBuffer.holes`) and fetched first, with a few held lines on both sides so the seams are checked; a hole of several pages
  begins with a 16-line look at its seam with the older lines, so history that is not the history held is found before the hole is
  downloaded. On a slow or restricted link a hole is fetched only where the reader is, and the rest waits for him. A desktop whose
  history is full (it drops its oldest lines, `history_size` stops growing) is followed by matching lines, asks for 500 again, and its
  pages overlap the lines held by 8; output that scrolled in between the last answer and a page (it cannot be announced) is found by
  sliding the overlap up to 24 lines before the page is given up on. A page that was overtaken by a later live answer is placed by the
  difference in `history_size`. Pages are read as the protocol writes them (lines joined by line breaks, none after the last, checked
  against `line_count`), so pages that end in blank lines are pages like any other.
- *History drawn again.* The inline agents (Codex, Grok, Claude Code without the alternate screen) wipe their scrollback and draw the
  transcript again at the new width whenever the pane width changes, which the phone's own `shell.resize` does. A page taken before
  is of a history that is gone. The buffer has an `era` that every rebuild and every answer that shows the history shrinking bumps;
  a page for another era, or one whose `history_size` is below the last answer's, is dropped (`stale`), never stitched on. Nothing is
  fetched for 0.8 s after a resize is acknowledged, or 0.6 s after a shrink, and the fetch then starts again from the live screen.
  A page that does not line up with the lines it meets drops everything older than the live answer and the fetch starts over. None
  of this shows: repeated misses back off 2, 4, … 60 s silently; only a request that really failed puts up "tap to retry".
- *Coming back.* Leaving a shell keeps its lines (up to 4 shells, 120,000 lines, least recently used first, nothing wrapped at another
  width). The first live answer on return lines them up with the desktop's by `history_size` and by comparing text, and starts over
  if they do not fit (a shell cleared or drawn again while away keeps none of its old lines), so a shell that was only looked away from
  costs no history requests. A reconnect keeps the buffer and carries on above what is held.

*Memory.* A held line costs about 285 bytes (measured: 50,000 styled lines of build and test output, 13.6 MB, of which 32 bytes per
line is the array slot), so one shell at the cap of 50,000 lines is about 14 MB (28 MB at the desktop's own limit of 100,000) and about
3 MB on the wire. The row views, a few dozen, are nothing next to it.

A desktop without the method (`unsupported RPC method`) or that rejects `styled` (retried once without it) keeps the earlier
behaviour: the screen and the latest 500 lines, no paging. The page is placed by the `history_size` it carries, so lines that
scrolled in meanwhile are found as a shared overlap, compared and left out. A pane re-wrapped by a resize cannot be matched and
renumbers the lines (the reader keeps the same distance from the bottom). At most 50,000 lines of scrollback are held; past that no
older page is asked for and live output pushes out the oldest (not while the reader is reading them, up to 12,500 lines past the cap).
On the alternate screen (`alternate`: vim, less, htop) there is no scrollback: the normal lines wait unchanged, the program's
screen is shown without scrolling, and a vertical swipe sends Page Up (content dragged down) or Page Down through `shell.keys`,
one per 80 % of the view's height (a quick flick that would carry that far sends one too), at most one every 120 ms; a chip
"Scrolling the app" is clear for the first three programs and faint after.

**Display settings** (terminal menu → Display…, or the slider button in the desktop list): interface size 80–130 % in 5 % steps
(a scale factor on fonts and touch targets of the header, lists, key bar and buttons, in `DesktopStyle`; Dynamic Type still
applies on top), terminal text size 8–24 pt, and **Show latency**. Values are saved and take effect at once; a size that changes
the terminal pane recomputes the grid and resizes the desktop pane (debounced) as before. The latency overlay (top right of the
terminal, top left in focus mode; touches pass through) shows the mode (live/poll), the last payload, the last and average (20
samples) round trip of `shell.keys` and `shell.output`, the echo latency (keys sent → first changed screen) and the age of the
last change. Long polls have no round trip of their own: a read that did not wait counts, and so does an `unchanged` answer
that ran out its wait (its time past the wait).

**Focus mode** (header button, or double-tap the header) hides the header, tabs, status rows and badges and gives the shell
the whole screen inside the safe area, keeping the display awake. Only the
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
