# iOS migration to NSH

The `nsh` branch now uses `NSHMobileRoot` as its application entry UI. The
packaged NSH 0.3.0 bundle in `apps/riwork-mobile` owns the host library, the
All hosts overview and project search/navigation/sorting, and draws a host's
own dashboard with the desktop's shared `DashboardView`. The chat and terminal
screens are still native.

## Ownership

| Layer | Current owner |
| --- | --- |
| A host's own dashboard (and its decision reviews) | The shared desktop `DashboardView` (`apps/riwork/dashboard`), through `createRemoteServices` (`apps/riwork/client/remote.ts`) over `swift_transport.ts` |
| Host library, All hosts overview, projects | NSH TSX in `apps/riwork-mobile`, native `NavStack`/`NavPage`, over `mobile.snapshot` / `mobile.action` |
| iOS window, scene lifecycle, runtime loading | `NSHMobileHost.swift` and the thin app entry |
| Display snapshots and explicit actions | `NSHMobileBridge.swift` over the existing `RemoteModel` |
| The shared screens' host calls (`relay.*`, `platform.*`, `nav.*`) | `NSHHostAPI.swift` over the same `RemoteModel` and bridge |
| Pairing, encrypted transport, Keychain, reconnects | Existing Swift model and `RiWorkCore` |
| Terminal/chat, file previews, pairing and settings forms | Existing native views, presented by `NSHMobileSurfaces.swift` |
| The terminal inside TSX screens (`riwork.terminal`, `apps/riwork/terminal_view.ts`) | `NSHTerminalComponent.swift`: the same capture terminal over the same `RemoteModel`, remote targets of the interactive host only (`docs/nsh-contracts.md` §3.8) |

The shared TSX lives in `apps/riwork`; `apps/riwork-mobile` imports it by
relative path. Shared code reaches data only through the ports in
`apps/riwork/client` (`types.ts`, `ports.ts`, `transport.ts`, `remote.ts`) and
the look through `apps/riwork/theme.ts`; the Mac wires the same ports over its
native module in `apps/riwork/adapters/local.ts`. Two guards keep Mac-only code
out of the phone bundle: `nsh check apps/riwork-mobile` (the mobile types have
no `riwork/native`, so even a type import fails), and
`ios/scripts/prepare-nsh.py`, which refuses a built `bundle.js` that names
`riwork/native`.

`mobile.snapshot` and `mobile.action` still drive navigation and every page
that has not moved. Snapshots carry a schema and revision; observation
publishes changes without a JavaScript polling loop. Pairing keys and transport
tokens stay in Swift. All hosts overview row IDs resolve to their original host
and target; approval responses, from the overview's native sheet or from the
shared review, go through the existing fresh-request validation and
uncertain-delivery protection.

The root overview reuses `HostDashboardStore`. Its temporary connections stop
before the interactive model connects, while native screens are shown, and
when the scene becomes inactive. Terminal and chat screens continue owning
their current visibility, subscriptions, drafts, viewport and input lifecycles.

A host's page draws the desktop dashboard in touch density (`touch`: 44-point
targets, its 11-point text at 15, and below 600 points each project's figures
under its name). It reads `dashboard.get` itself every 10 seconds, only while
it is the screen on top and the host is connected; Swift installs each reply
into `RemoteModel` as its own read would be, and while that page is up its
activity loop reads only the projects and orchestrators its rows open with
(`keepDashboardListsFresh`). Its ↻, ⌘R and the refresh command read
`dashboard.get` there and the lists in Swift (`mobile.action` `refresh`, which
on this page leaves `dashboard.get` to it). A row opens its native screen
(`nav.present`), as the Mac's global window routes it (`routeTarget`). A Mac
whose RiWork predates `dashboard.get` is never asked; the page says so, and its
projects and its global orchestrator (the snapshot's `global`, from
`orchestrators.list`: Open, or Start when there is none) still open.
The All hosts overview keeps its filters, saved read markers, coverage and
truncation notices, activity history and Haiku summaries.

## Swift host API

The calls and events the shared screens use, implemented by `NSHHostAPI.swift`
and declared in `apps/riwork-mobile/nsh.json` (`relay.*`, `platform.*`,
`nav.*`). Values are plain JSON. A call that fails rejects with
`"<code>: <message>"`, the codes of `PortError`: `offline`, `unsupported`,
`not_found`, `conflict`, `limit`, `uncertain`, `failed`.

| Name | Kind | Data | Result / payload |
| --- | --- | --- | --- |
| `relay.hosts` | call | — | `{hosts: HostInfo[]}` |
| `relay.hosts` | event | | `{hosts: HostInfo[]}` whenever the list, the selection or a link changes |
| `relay.select` | call | `{host}` | `{}`; makes it the interactive host (the overview's connect); `conflict` while a native screen is up |
| `relay.disconnect` | call | `{host}` | `{}`; `conflict` while a native screen is up |
| `relay.call` | call | `{host, method, params?, timeoutMs?}` | the RPC's `result`; the interactive host only, `dashboard.get` and `projects.list` only |
| `relay.link` | event | | `{host, link: LinkState}` |
| `relay.decision.review` | call | `{host, chatId, requestId, kind?}` | `DecisionReviewDto` (`status` `pending`, or `unavailable` / `uncertain` with `error`) |
| `relay.decision.respond` | call | `{host?, reviewId, decision?, answers?}` | `DecisionReviewDto` with `status` `sent`, `pending` (nothing went out; `error` says why), `unavailable` or `uncertain` |
| `platform.prefs.get` / `.set` | call | `{name}` / `{name, value}` | `{value}` / `{}`; `projectSort` (`riwork.projectSort`) and `newTerminal.kind` (`riwork.newTerminal.kind`) |
| `platform.theme` | event | | `{theme: InterfaceTheme, native, mic, dark}`; the first value is `host.params.theme` |
| `nav.present` | call | `{name, host?, projectId?, shellId?, chatId?, start?}` | `{}`; presents a remaining native Swift screen |

```
HostInfo  = {id, name, address?, selected: boolean, link: LinkState, features: string[]}
LinkState = {state: "connected"|"connecting"|"offline"|"failed"|"suspended", label, error?, since?}
```

- **`relay.call`** goes through `RemoteModel`'s connection to the interactive
  host and so through `RequestValidation`; JS may not send `wait_ms` above 0 or
  any `pty.*`. `timeoutMs` (1 to 60 s, 30 s by default) lets go of the wait
  without closing the connection. `HostInfo.features` lists the connection's
  `ready.features` by name; `dashboard` is left out for a Mac that refuses it.
- **Decisions** keep the Swift flow the overview's sheet uses.
  `relay.decision.review` reads the request from the host's last parsed
  dashboard (`DashboardDecision`; a request this phone cannot read in full is
  not offered) and returns an opaque `reviewId`. `relay.decision.respond`
  answers through `NSHMobileBridge.respondToDecision`: `DashboardResponder`
  reads the chat again and checks the exact request and its choices before
  exactly one `chat.command` goes out, never retried. A response that went
  out, or whose delivery is uncertain, is remembered (`sentDecisions`): the
  review then answers `uncertain` and nothing is sent again from this phone.
- **`nav.present`** names: `project` (`projectId`, at the `shellId` or `chatId`
  tab when given; a project missing from Swift's list reads `projects.list`
  once before it is called unavailable), `global` (the global orchestrator;
  `start: true` starts it when there is none, an explicit Start only),
  `projects`, `settings`,
  `addHost`, `hostDetails` (`host`), `newProject`, `libraryRecovery`.
- **`platform.theme`** is what `ThemeStore` resolved (the phone's palette
  choice, the desktop's synced palette on iPad, the contrast fixes), with the
  skin (`native`) and the desktop's `mic` switch; TSX never calls
  `appearance.get` itself.

## Phone appearance

iPhone keeps native controls, SF Symbols and system typography across the NSH
root and retained native screens. **Display → Color palette** defaults to
**Native**, which follows the phone's system appearance. It also offers the
12 fixed desktop palettes offline from the shared
[`crates/riwork-core/color-palettes.json`](../crates/riwork-core/color-palettes.json),
plus **Follow desktop** for the selected host's cached colors, including Ghostty.
Follow desktop falls back to Native until a palette is available. The choice is
saved locally on the phone; it changes colors without changing fonts, controls
or layout.

The state colors follow the desktop: working, what needs you (gold) and errors
are told apart. Native uses the desktop Native's values (working `#0058d0` /
`#409cff`, gold `#b34000` / `#ff9f0a`, danger `#c80018` / `#ff6961`), a fixed
palette its catalogue `working` and `danger` (falling back to the accent and to
an older `error` or the terminal's red), and Follow desktop the optional
`palette.danger` of `appearance.get` (missing on older desktops, which keep the
app's error color; working is the accent there). The bridge's theme snapshot
sends these as `working` and `error`, and the bundle's fallback palettes match
Native until the first snapshot; `platform.theme` sends the same colors to the
shared screens in the desktop's names (`working`, `danger`, and `focus` as the
accent). Rows tell the same story, as the desktop
Dashboard does: failed chats, an automation run that failed, unreadable
automations (the dashboard's `automationsError`, apart from its muted `notes`)
and a failed connection use the `error` tone; decisions, waiting agents, missed
or uncertain runs, a stale or failed dashboard read (the freshness line) and
Haiku summary errors and setup prompts stay `gold`; working agents use
`working`. Code and terminals remain monospace. iPad retains its existing
desktop-synchronized appearance, and both devices still receive the desktop
microphone preference independently of palette selection.

## Touch and native navigation

NSH pages use the iOS navigation stack, including interactive Back. NSH route
state reconciles with the host without immediately undoing a user's pop.
Controls have at least 44-point targets, wrapping text and pressed feedback.
Scroll views support refresh and keyboard dismissal; project search keeps
content clear of the keyboard and home indicator.

Project navigation supports hardware-keyboard movement and opening. The root
also connects the existing Clicks prefix shortcuts and shortcut guide through a
native responder beneath the NSH view, without placing an input layer over the
screen. Native screens keep their own responder ownership.

The outer UIKit navigation controller pushes the existing terminal/chat view
controllers. Their touch scrolling, text selection, tab gestures, IME input,
keyboard docking and dictation code is retained. File previews use the same
navigation stack through a small presentation hook. Native sheets preserve
pairing confirmation, QR scanning, project creation and decision flows.
Deep links wait for active sheets to finish before presenting their pairing
confirmation. iPad and iPhone continue using the existing platform-specific
terminal implementations.

These are implementation guarantees about which code owns interaction, not a
claim that all device gestures have been exercised. No automated tests were
run for this migration stage.

## Build and next boundaries

See `ios/README.md` for the release build. `prepare-nsh.py` requires matching
0.3.0 CLI/runtime versions, builds into a staging directory, and replaces the
generated bundle only after success. Xcode links the static native framework
and packages the UI; the installed app needs neither a CLI nor a checkout.
The bundle identifier and Keychain ownership remain `com.riwork.remote`.

The next migration boundaries are the host list and projects (onto
`relay.hosts`, `relay.select` and the projects port), the chat
presentation/composer and the remaining forms. A desktop screen moves by taking
its data through the ports in `apps/riwork/client` instead of `riwork/native`;
the Mac keeps its native module behind `adapters/local.ts`. The terminal
remains a platform-native surface while its surrounding controls can move into
NSH. The old `RemoteRootView` is retained as a source reference; the app entry
no longer selects it.
