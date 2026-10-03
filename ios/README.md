# RiWork for iPhone and iPad

Choose a paired RiWork desktop and project, then switch between tabs for its
open terminals, or open a new shell or agent on the desktop with ＋. One terminal
fills the screen at a time. The app never splits or replaces a desktop session; it
creates one only when asked (＋ or ⌘N, then Create), and closes one only after a
confirmation. It creates a project only when asked too (＋ or ⌘⇧N on the project list, then Create).
Reconnecting and switching tabs never create or close anything.

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

### New terminal and close

**＋ opens a terminal on the desktop** (`shell.create`, then `shell.close` for closing; see the "Terminal creation extension" in
`docs/remote-protocol.md`). It is in the terminal screen's header and on the "no open terminals" page, in a project's swipe
and long-press actions on the project list (that opens the project, then the sheet), and on **⌘N** from a hardware keyboard,
also in focus mode. The sheet is a medium-height sheet with big rows and Create at the bottom, for one thumb:

- **Where.** The project on screen and, if it has worktrees, a menu of them (the main one first). Preselected: the worktree of the
  terminal you are looking at, else the main worktree, else the project itself. Another project is chosen on the project list.
- **What.** Shell, Codex, Claude or Grok, as radio rows. The last kind that was created is remembered (UserDefaults,
  `riwork.newTerminal.kind`; a plain shell the first time). There is no "installed agents" information from the desktop, so all four
  are always offered and a missing agent is reported after Create.
- **Unrestricted.** For agents only, a toggle "Unrestricted: no approval prompts" that is **off every time** the sheet opens and is
  never remembered. The desktop's own menu offers "Codex · unrestricted" etc. as separate items, and its plain Codex, Claude and Grok
  items are restricted; the phone sends `unrestricted` only when the toggle is on.
- **Create** sends one `shell.create`, shows progress, and on success the sheet goes away and the new terminal is selected and opened
  like any tab. The app never sends it twice: a second tap or a held Return while it is on its way is
  ignored, and a lost answer (timeout, dropped link) is **not retried**: the sheet stays and says "the terminal may or may not have
  been created. Check the terminal list before trying again", the button then reads "Try again". Request timeout: 90 s (an agent can
  take 20 to 30 s to start).
- **Errors are inline and the sheet stays open:** an older desktop ("Update RiWork on your Mac to open terminals from the phone."; found
  from the first answer `invalid_request` "unsupported RPC method", kept for the connection, and reset by every new connection, so ＋
  is dimmed and Create is off, and ⌘N does nothing), a project or worktree that no longer exists, an agent that is not installed
  ("Codex isn’t installed on the Mac (or isn’t on its PATH)."), a terminal that exited right away, or the desktop's own message.

**Keyboard** (a Clicks or any hardware keyboard; nothing needs a tap first, the sheet takes the keyboard when it appears):
↑ ↓ choose the kind (or the worktree when that row has the focus ring), ⇥ / ⇧⇥ / ← → move the focus ring between Where, What,
Unrestricted (when shown) and Create, Space flips the toggle (and presses Create when it has the ring), ↩ creates, ⎋ cancels. The focus ring
appears with the first key. ⌘N opens the sheet; it is a SwiftUI keyboard shortcut, which SwiftUI registers as a key command on the
hosting controller, and the terminal's key view claims no ⌘ combination, so it works while the terminal has the keyboard.

**Close.** A terminal tab's long-press menu (and "Close this terminal…" in the header menu) closes it after a confirmation, "Close
<terminal>? This ends its running process on the Mac." The phone first moves to the neighbouring tab, then `shell.close` ends the
process and removes it from the Mac's list (a tab the Mac's window still has for it is left to the window, as with the CLI). Not offered for orchestrators. A
failed close puts you back where you were; "already gone" counts as closed; a lost answer says the outcome is unknown and is not retried.

### New project

**＋ on the project list creates a project on the desktop** (`project.create`; see the "Project creation extension" in
`docs/remote-protocol.md`), or **⌘⇧N** from a hardware keyboard while the list is the screen on top. There is a "New project" button in
the empty list too. ⌘N is "New terminal" on the terminal screen, so ⌘⇧N is the same thing for a project; it is registered only on the
project list, where the terminal's key view (and with it ⌘K, ⌘, ⌘/ and ⌘. of the hotkey menu and help, a person's hotkey shortcuts and the Clicks
template's ⌘ and ⌘⇧ letters, none of which is N) is not on screen, so it collides with nothing. The sheet is a medium-height sheet:

- **Name.** A text field that has the keyboard the moment the sheet is up, with autocorrection, smart punctuation, autocapitalization
  and prediction off. The phone never sends a place: the desktop creates the folder in its own default projects folder and the sheet says
  so. The name is trimmed, and checked as it is typed against the desktop's own rules (`Core/NewProject.swift`, `NewProjectName`, unit
  tested): 1 to 100 characters (Unicode scalars, and at most 255 UTF-8 bytes), no control characters, no `/` or `\`, not starting with a
  dot or a dash. An empty field says nothing until Return asks for a name; any other problem is shown under the field at once and Create
  stays off. A line break in a paste is dropped.
- **Create Git repository.** A switch, **on** every time the sheet opens (never remembered); the phone sends `git: false` only when it is
  off, so the default stays the desktop's.
- **Create** sends one `project.create`, shows progress, and on success the sheet goes away, the new project is selected (its terminal
  screen opens) and the **New terminal** sheet comes up for it with its usual defaults, so a terminal in it is one Return away. Nothing is
  started without that second Create: Escape leaves you in the new, empty project. Request timeout: 90 s.
- **Errors are inline and the sheet stays open, the name is kept:** the desktop's own sentence for `already_exists` (a project of that
  name, compared ignoring case, or a folder of that name in the Mac’s projects folder, which it never reuses; for example "A project
  named "x" already exists on the desktop"), an
  older desktop ("Update RiWork on your Mac to create projects from the phone."; found from the first answer `invalid_request`
  "unsupported RPC method", kept for the connection and reset by every new connection, so ＋ and ⌘⇧N disappear and the open sheet
  explains), or the desktop's own message (git failing, a CLI too old to create projects from the phone). Editing the name takes a
  message down.
- **Never retried.** A second tap or a held Return while a request is on its way is ignored, and a lost answer (timeout, dropped link)
  is not asked again: the sheet says the project "may or may not have been created", to check the project list first (asking again would
  only answer "already exists" if it was), and the button reads "Try again". An answer that is not the project that was asked for is
  treated the same way, and the project list is read again at once.

**Keyboard** (a Clicks or any hardware keyboard; nothing needs a tap first): type the name; ↩ creates (from the name, the switch or
Create), ⎋ cancels, ⇥ / ⇧⇥ (and ↑ ↓) move the focus ring name → Git switch → Create, ← → move it while the switch or Create has it, Space
flips the switch or presses Create when it has the ring. A keyboard without Tab or Esc (the Clicks keyboard has neither) has **⌘G**, which
flips the Git switch from anywhere, also while typing, **⌘↩** for create and **⌘.** for cancel. Mechanism: the name is a `UITextField`
subclass whose `keyCommands` carry tab, shift-tab, ↑ ↓, escape, ⌘., ⌘↩ and ⌘G with priority over the system, and whose delegate turns
Return into create; while the switch or Create has the ring a small invisible key view takes the keyboard instead (it is not a text
input, so no software keyboard appears) and also owns space, return and ← →. The ring follows the form (`NewProjectForm`, pure and
tested), and tapping any control moves it, so touch and keyboard agree.

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
Backspace, Enter (arrows, Backspace, Delete and Page keys repeat while held) and Paste, then a ⌘ button that opens the hotkey menu, a ? button that opens the hotkey help and the hotkeys (those that
want a button), then the
symbols that are awkward on the iOS keyboard (`` | / \ ~ - _ ` * & $ > < { } [ ] ; : ' " ``), then a "+" that opens the
hotkey editor. Hide keyboard stays at the right end. The ends of the row are padded so the first and last key clear
the display's rounded corners (about 20-28 pt derived from the safe area, not from a device model). Focus mode uses
the same strip. The iPhone is portrait only.

**Hotkeys** send a fixed sequence of text and special keys, validated against the `shell.keys` contract (text without
control characters, whitelisted key names only, at most 64 steps). Built in: Ctrl+C, D, Z, L, R, A, E, U, W and Esc Esc.
Your own (a name of up to 12 characters plus steps, e.g. "/clear" then Enter, or Ctrl+C then "exit" then Enter; up to 64 hotkeys) can be
added, edited, deleted and reordered in the editor, and are kept on the device (UserDefaults). A hotkey ignores an armed
Ctrl/Alt. A hardware keyboard sends arrows, Esc, Tab,
Shift-Tab, Home/End/Page keys, Ctrl-letters and Ctrl-[ (Esc, as in every terminal). Newlines in typed or pasted text
become Enter, tabs become Tab, other control characters are dropped.

**Keyboard first.** The next paragraphs are for typing on a hardware keyboard (the Clicks keyboard case, a Bluetooth keyboard)
without touching the screen: focus, the hotkey menu, shortcuts, the editor and the Clicks template.

**Focus when a shell is ready.** A shell is *ready* when it is connected and its first live screen is shown. Whenever a shell becomes
ready (first open, another tab, a reconnect, back from the background) the key capture view takes the keyboard, so typing goes straight
into it. Display → Keyboard → **Focus keyboard when a shell opens**: *Always* (also brings up the on-screen keyboard), *With a
hardware keyboard* (the default; `GCKeyboard.coalesced`, so focusing costs no screen) or *Never*. It never takes the keyboard from a
sheet, an alert or another text field (it looks again for about a second, so the moment a sheet goes it takes it), a keyboard hidden
on purpose with Hide stays hidden in that shell until the terminal is tapped, and a shell that replaced a closed one waits for a tap. The
hotkey editor gives the keyboard back to the shell when it closes. The rules are pure and tested (`Core/KeyboardFocus.swift`).

**Hotkey menu.** ⌘K from any shell, the key bar's ⌘ button, or a tap on a lone modifier key you chose (see below). Type to filter
(case blind, all words, by name, keys sent or shortcut), ↑ ↓ (or Tab, Shift-Tab, Ctrl-N, Ctrl-P, Ctrl-J, Ctrl-K, Page Up/Down) to move,
Return (or Ctrl-M) to send, Shift-Return to edit the chosen hotkey, ⌘N for a new one, Esc, ⌘K again, ⌘. or Ctrl-C/Ctrl-G to close. The
rows are your hotkeys, the built-in ones, plain keys (Esc, Tab, arrows, Page Up/Down, Home, End, Enter, Backspace, Delete) and
"New hotkey…" / "Configure hotkeys…" (also ⌘,). While the menu is open every key goes to it, never to the shell, and a shortcut whose hotkey
is one navigation key stands in for that key: with the Clicks template ⌘S moves down and ⌘W up, as they do in the shell. Focus stays on
the shell throughout (the menu is driven by its own keyboard), and works with the software keyboard and by touch too.

**Hotkey help.** ⌘/ (or the key bar's ? button) opens a cheat sheet over the terminal: every hotkey with its shortcut (`⌘E`, or `—` when
it has none), its name and what it sends, those with a shortcut first, then a small section with the app's own shortcuts (⌘K the menu, ⌘,
the settings, ⌘N a new terminal, ⌘/ the help). It is for looking at on a keyboard without labels, the Clicks case say, before pressing the
right chord, and it lists what the menu does: yours (an installed keyboard template included), then the built-in ones your hotkeys do not
already send. It is not modal and never takes the keyboard (the capture view stays first responder), so typing, arrows, Tab and every
chord work exactly as without it: pressing a hotkey's shortcut sends it and leaves the sheet up for the next one, and a tap on a row sends
that hotkey too. ⌘/ again, Esc or ⌘. closes it (that Esc is not sent to the shell); opening the hotkey menu or the settings, a sheet, or
the keyboard going away closes it too, and the menu and the help never show together. A list that does not fit scrolls by touch. ⌘/ is
reserved like ⌘K and ⌘,: no hotkey can take it (a hotkey stored with it by an earlier version keeps everything but the shortcut), nothing
in the template uses it, and no iOS system shortcut is known on it. More shortcuts for the help, or a tap on a lone modifier, are added in
the hotkey editor (Keyboard → Hotkey help → Add a shortcut for the help…), learned by pressing the key like the menu's; a chord belongs to
a hotkey, the menu or the help, never two of them. The rows are in `Core/HotkeyHelp.swift` (pure and tested).

**Shortcuts.** A hotkey can have a shortcut that sends it without opening the menu. It is matched by the key's HID usage and the
modifiers, not by the character, so ⌥E is still E. Typing keys need ⌃, ⌥ or ⌘ (a bare letter would stop typing); function keys,
Page keys and the like may be bare; ⌘K, ⌘, and ⌘/ are reserved. A modifier key pressed and let go on its own is a *tap* and can open the
menu. Keys a `UIKeyCommand` can name are registered with `wantsPriorityOverSystemBehavior`, since iOS keeps ⌘E, ⌘F, ⌘G … for itself;
the rest (function keys, taps) are taken from the key presses. Hotkeys made for a shortcut only can stay off the key bar. Everything is in
`Core/KeyChord.swift`, `Core/KeyShortcuts.swift`.

**Editor from the keyboard.** The list: ↑ ↓ (Ctrl-P/N, Tab) select a row, Return (Ctrl-M) opens or runs it, ⌘N adds, ⌘⌫ deletes,
⌘↑ ⌘↓ reorder, Esc or ⌘. closes. The form: Tab and Shift-Tab move between the name and the text steps, Return saves, Esc or ⌘. cancels,
⌘S saves, ⌘T adds a text step, ⌘R records a key step by pressing the key, ⌘L **learns the shortcut**: press a key or chord and it is
stored as its HID usage and modifiers (a lone modifier is a tap; Esc cancels).

**Key readout.** Display → Show key events overlays the last key event over the terminal (event type, `UIKey.keyCode` as decimal and HID
usage, key name, modifiers and the raw `UIKeyModifierFlags`, `characters` and `charactersIgnoringModifiers`); the hotkey editor's *Key
tester* shows the same while you press keys. It sees key presses and key commands, so it settles what a key such as the Clicks button
sends; if a key shows nothing, it is not an event an app can receive (a system or consumer-page key).

**Clicks keyboard template** (hotkey editor → Keyboard templates → Install). Adds, never replaces or edits, and skips what is there
already (the same id, the same steps on the same shortcut, or a shortcut that is yours). All on ⌘, which the case has and which works
whichever way the Clicks Key is set: **⌘ + letter is a key, ⌘⇧ + letter is Ctrl + letter.**

| ⌘E Esc | ⌘T Tab | ⌘⇧T Shift-Tab | ⌘W ⌘A ⌘S ⌘D ↑ ← ↓ → | ⌘B Page Up | ⌘F Page Down |
|---|---|---|---|---|---|
| ⌘⇧C ^C | ⌘⇧D ^D | ⌘⇧Z ^Z | ⌘⇧R ^R | ⌘⇧L ^L | tap Control: hotkey menu |

What is known about the hardware (checked against Clicks' own pages, 2026-10): the case has letters, Shift, 123, Globe, ⌘, Space, the
Clicks Key, Return, Backspace and a keyboard-toggle and dictation key, and no Esc, Alt or arrow keys. The **Clicks Key is a setting in the
Clicks app: Tab or Ctrl** ([app v1.2](https://discover.clicks.tech/clicks-keyboard-app-v12-introduces-cursor-mode-clicks-key-customization-and-more)).
Clicks does not publish the HID usage it sends, and there is no per-key remapping in the app. Arrows come from Cursor Mode (123 + ⌘, then
WASD or IJKL). Clicks Mode (the Clicks Key, ⌘ or Globe as a base key, then a letter) works through iOS *Full Keyboard Access*, so it
makes iOS act and sends the app no keys ([CrackBerry](https://crackberry.com/how-create-custom-shortcuts-your-iphone-clicks-keyboard)).
The most likely story is a plain Control (usage 0xE0) or Tab (0x2B), which the readout will confirm. Practical consequences: with the
Clicks Key set to **Ctrl**, Ctrl-letters (^C ^D ^Z ^R ^L …) work as on any keyboard, **Ctrl-[ is Esc**, and a tap on the Clicks Key alone
opens the hotkey menu (the template binds a tap on either Control); set to Tab you have Tab but no Ctrl and need the ⌘⇧ shortcuts. The template
is a starting point: use *Learn key* to move any shortcut, and ⌘/ (the hotkey help) lists them over the terminal when the case has no labels. Sources: [Clicks for iPhone 17](https://www.clicks.tech/products/clicks-keyboard-for-iphone-17)
(USB-C), [Power Keyboard layout](https://learn.clicks.tech/knowledge-base/kb-power-keyboard-getting-started-get-to-know),
[Power Keyboard shortcuts](https://learn.clicks.tech/knowledge-base/kb-power-keyboard-tips-iphone-keyboard-shortcuts),
[iOS modifier remapping](https://www.macrumors.com/how-to/remap-modifier-keys-ipad-keyboard/).

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
line by line; a line that an earlier answer already held (the same bytes, read under the same running SGR style) is found again in
`StyledLineCache` instead of parsed, so an answer costs what changed in it, not its 160 or 540 lines; the iPhone terminal paints only
the rows in view, each with Core Text on a whole device pixel (`TerminalRowView.swift`), and repaints only rows whose line changed, so a 50,000-line history costs no more than a screen. Every line is exactly one grid row
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
- *Bandwidth.* (`Core/LinkMeter.swift`, pure and tested on traces.) A page's time is the round trip, plus the desktop's own work (its
  CLI starting, tmux capturing, compressing: 50 to 300 ms, growing with the page and the load of the Mac), plus the transfer, and only
  the transfer says how fast the link is. The connector reports its share in every reply (`server_ms`), and every small reply (a key
  acknowledgement, a long poll that found nothing, the lists asked for at connect) is a clean sample of the round trip, so the transfer
  is `elapsed − server_ms − round trip`, worked out on the bytes that really crossed the socket (`ReplyTiming`, from `RelayClient`,
  which stamps a reply when its last byte arrives). A page that shared the socket with other replies (a live answer came in while it
  was out; `RelayClient` notes every arrival) counts only if those were under a fifth of its own size, and then their bytes are added
  back; otherwise it is left out. A page under 8 KB says nothing about the rate. A desktop that does not report `server_ms` is measured
  as it always was: a ten-line probe learns the fixed cost of a request, which is taken out of the bigger pages. The rate kept is the
  best of the last three pages within 45 s. **fast ≥ 1 MB/s, good 250 KB/s to 1 MB/s, slow < 250 KB/s**, with a band so that a link
  sitting on a threshold does not flap: a tier is left only below 700 KB/s (fast) or 175 KB/s (good). What a line weighs, how well it
  compresses and what the desktop spends are about the history and the desktop, not the link, and survive a change of link.
- *The path.* `NWPath.isExpensive` and `isConstrained` are read through Tailscale's tunnel and can switch several times while it
  connects or roams. A restriction applies at once and is lifted only after the path has been free of it for 10 s (`ConditionHold`). A
  change of interface (Wi-Fi to cellular) forgets the link's speed and round trip, but only once it has lasted 2 s; a dropout and the
  return to the same interface change nothing. Both are visible in Settings and in the overlay.
- *How much* (Settings, History download; `HistoryMode`, `HistoryAppetite.policy`). **Automatic** (the default): everything, up to the
  50,000 lines held, on a link that is not slow and not restricted; 10 screens above the reader (a fetch starts when fewer than 5 are
  loaded) on a slow link, a metered one (`isExpensive`) or in Low Power Mode; 5 screens (starts below 2.5) in Low Data Mode
  (`isConstrained`). Compression changed one thing: what is left is fetched whole anyway when it is small enough, estimated from the
  bytes per line seen so far, **256 KB on a slow link, 512 KB when metered or in Low Power Mode, 128 KB in Low Data Mode** (the whole
  of a typical session is a few hundred KB deflated, which is no reason to hold back). **Always everything**: the whole history in
  the background whatever the link and the path say. **Ahead only**: about 10 screens above the reader (5 in Low Data Mode). **Off**:
  nothing in the background; a page comes when the reader is within 1.5 screens of the top of what is loaded (it fills to 3), or when
  the "couldn't load" row is tapped. A reader within 1.5 screens of the top is waiting in every mode: no pauses between pages.
- *How big, how often.* A page is what the link carries in about 0.3 s (fast), 0.2 s (good) or 0.12 s (slow), at most 112 KiB on the
  wire (about 84 KiB sealed, inside the reply cap of 128 KiB), at most 768 KiB of JSON (the phone parses it all) and at most the lines
  the desktop takes (5,000 where it says so in `ready`, else 1,000; `response_too_large` still halves it, kept per session), growing by
  at most double per page once a rate is known. After a page the link is left alone for 0.15× (fast), 1× (good) or 3× (slow) what
  the link spent on it (its time less the desktop's), twice that while the screen is changing. With compression a typical page is
  3,000 to 5,000 lines, so a full 50,000-line history is about a dozen requests instead of fifty. An installed CLI that takes fewer
  lines than the connector announced (an older build) is learned from its first refusal: the connector remembers the limit and
  announces it from then on, and the phone reads it from the refusal. A page cap that `response_too_large` set is raised by half
  after six pages in a row under it, so one heavy page does not keep a session at small pages.
- *Compression.* The connector announces `deflate` in `ready`; the phone asks for it once with `link.configure` (Settings, Compress
  traffic, on by default) and reads both forms of a reply whatever it asked: `Core/LinkFrame.swift` inflates with Apple's
  `Compression` framework (`COMPRESSION_ZLIB`, raw deflate), after the length declared in the frame has been checked against 2 MiB, and
  refuses a stream that does not inflate to exactly that. Styled history compresses 8 to 11 times, a 500-line screen 6 to 9, a dense
  Codex screen 3 times. The settings screen and the overlay show the ratio.
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
applies on top), terminal text size 8–24 pt, **History download** (Automatic, Always everything, Ahead only, Off, with **Compress
traffic** under it), a **Link** readout (path and tier, round trip, transfer rate, the desktop's time per page, the compression ratio,
what restricts the path, what the history download is doing and why, and how many lines are loaded) and **Show latency**. Values are saved and take effect at once; a size that changes
the terminal pane recomputes the grid and resizes the desktop pane (debounced) as before. The latency overlay (top right of the
terminal, top left in focus mode; touches pass through) shows the mode (live/poll), the last payload, the last and average (20
samples) round trip of `shell.keys` and `shell.output`, the echo latency (keys sent → first changed screen) and the age of the
last change, then three lines about the link: `link` (round trip, rate, tier), `desk` (the desktop's time per page, `z` the
compression ratio) and `hist` (what the download is fetching, lines loaded of lines on the desktop, ⚑ when the path is restricted). Long polls have no round trip of their own: a read that did not wait counts, and so does an `unchanged` answer
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

## Performance

What a live answer, a typed key and a scroll frame cost is measured, not guessed. The numbers print as `PERF …` lines:

```sh
# Core: parse, buffer, memory per line, the wire (optimized, as the app ships)
swift test -c release -Xswiftc -enable-testing --package-path ios --filter PerformanceTests
# App: main-thread CPU per answer / key / scroll frame with 50,000 styled lines held, row painting, reconnect over a slow link.
# The counters (view bodies built, rows painted, surface refreshes) are compiled into debug builds and, here, into a release build.
xcodebuild -project RiWorkRemote.xcodeproj -scheme RiWorkRemote -configuration Release ENABLE_TESTABILITY=YES \
  SWIFT_ACTIVE_COMPILATION_CONDITIONS=PERF_COUNTERS CODE_SIGN_IDENTITY=- -destination 'platform=iOS Simulator,name=iPhone 17 Pro' \
  test -only-testing:RiWorkAppTests/PerformanceTests
```

The tests assert the work done (no header, console or phone-terminal body is rebuilt by a live answer or a typed key; the surface is
refreshed once per answer; a connect asks for the four lists in one round trip), not times, which depend on the machine. For a profile,
run one scenario for 40 s with `TEST_RUNNER_PERF_PROFILE=echo|output|type|scroll` and `-only-testing:RiWorkAppTests/PerformanceTests/testForAProfiler`,
attach `xcrun xctrace record --template 'Time Profiler' --device <udid> --attach <pid>`, and look at the main thread. On a device, the
signposts `ParseAnswer`, `ApplyAnswer`, `SurfaceRefresh` and `RowPaint` (subsystem `com.riwork.remote`, category `Performance`) show up
in Instruments' os_signpost track.

Rules the code follows, each found by a measurement:
- A view reads what changes rarely. `output` is a new string with every answer, so views read `hasOutput`; what follows the buffer or the
  keys (the history header, the pill, the chip above the keyboard, the status line) is a view of its own, so a key or an answer rebuilds
  those and not the header, the console and the terminal around them. `@Observable` wakes readers for an equal value only when it is
  mutated in place (`_modify`), so assign whole values and let the macro compare.
- The model refreshes the scroll surface when the buffer changes; its layout pass does not (UIKit on iOS 26 lays a view out again when state
  it read in `layoutSubviews` changes, which doubled every answer).
- Independent requests go out together (the four project lists of a connect), replies being matched by id.

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
