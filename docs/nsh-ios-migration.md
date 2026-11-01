# iOS migration to NSH

The `nsh` branch now uses `NSHMobileRoot` as its application entry UI. The
packaged NSH 0.3.0 bundle in `apps/riwork-mobile` owns the navigation and the
pages: the hosts and the All hosts overview, a host's dashboard (the desktop's
shared `DashboardView`), its projects, and a project's page with its tab strip
and terminal, and the New terminal and New project forms. The window's width
picks the shell (`App.tsx`, `windowSize()`, never the device): narrower than
600 points `PhoneShell` (a native `NavStack` of `NavPage`s with swipe back),
wider `PadShell` (the hosts, then a host's projects in a column beside its
dashboard or the project chosen there). Both draw the same routes
(`app_state.ts`), so a window resized across 600 points keeps its place. A
project's chat, the global orchestrator and the pairing, host, settings, memo
and New chat sheets are still native Swift screens.

## Ownership

| Layer | Current owner |
| --- | --- |
| Navigation, `PhoneShell` and `PadShell` | NSH TSX in `apps/riwork-mobile` (`App.tsx`, `app_state.ts`, `shell/`) |
| Hosts | `pages/Hosts.tsx`, from `relay.hosts`; pairing, host details, recovery and settings through `nav.present` |
| All hosts overview | `pages/Hosts.tsx`, drawn from `mobile.snapshot` (Swift's `HostDashboardStore` reads it over its own short connections); a row through `mobile.action` `openDashboardRow` |
| A host's own dashboard (and its decision reviews) | The shared desktop `DashboardView` (`apps/riwork/dashboard`) in `pages/HostDashboard.tsx`, through `createRemoteServices` (`apps/riwork/client/remote.ts`) over `swift_transport.ts` |
| Projects (search, order, folders, New project) | `pages/Projects.tsx` and `pages/NewProject.tsx` over the projects port (`projects.list`, `native.invoke` `Store.snapshot`, `project.create`), the tree from the desktop's `project_tree.ts` |
| A project's page (tab strip, terminal, New terminal, Open a worker or shell, Move, Close) | `pages/Project.tsx` and `pages/NewTerminal.tsx` over the workspace port (`tabs.*`, `shells.list`, `chats.list`, `worktrees.list`, `shell.create`, `shell.close`) |
| The terminal | `riwork.terminal` (`apps/riwork/terminal_view.ts`, schema 1.1), the Swift capture terminal registered as an NSH component |
| iOS window, scene lifecycle, runtime loading | `NSHMobileHost.swift` and the thin app entry |
| The overview snapshot, where the pages are, Swift's own navigation (`nav.open`) | `NSHMobileBridge.swift` over the existing `RemoteModel` |
| The shared screens' host calls (`relay.*`, `platform.*`, `nav.*`) | `NSHHostAPI.swift` over the same `RemoteModel` and bridge |
| Pairing, encrypted transport, Keychain, reconnects | Existing Swift model and `RiWorkCore` |
| A project's chat, the global orchestrator, file previews, pairing, settings, memo and New chat sheets | Existing native views, presented by `NSHMobileSurfaces.swift` |

The shared TSX lives in `apps/riwork`; `apps/riwork-mobile` imports it by
relative path. Shared code reaches data only through the ports in
`apps/riwork/client` (`types.ts`, `ports.ts`, `transport.ts`, `remote.ts`) and
the look through `apps/riwork/theme.ts`; the Mac wires the same ports over its
native module in `apps/riwork/adapters/local.ts`. Two guards keep Mac-only code
out of the phone bundle: `nsh check apps/riwork-mobile` (the mobile types have
no `riwork/native`, so even a type import fails), and
`ios/scripts/prepare-nsh.py`, which refuses a built `bundle.js` that names
`riwork/native`.

`mobile.snapshot` (schema 2, `apps/riwork-mobile/model.ts` `OverviewSnapshot`)
now carries only what Swift still reads for the pages: the All hosts overview,
how fresh each host's read of it is, whether a native screen is up, the
pairing library's state, the keyboard (hardware, Clicks) and, for a Mac
without `dashboard.get`, its global orchestrator. `mobile.action` has three
actions left: `refresh` (the overview, or the lists of the page on top when it
has no refresh of its own), `openDashboardRow` and `navigate` (`{page, hostID?,
projectID?}`: which page is on top). Snapshots carry a schema and revision;
observation publishes changes without a JavaScript polling loop. Pairing keys
and transport tokens stay in Swift. All hosts overview row IDs resolve to their
original host and target; approval responses, from the overview's native sheet
or from the shared review, go through the existing fresh-request validation
and uncertain-delivery protection.

The pages own the navigation and tell Swift where they are (`navigate`): the
overview's connections run only while the hosts are on top, a host's page keeps
the lists its rows open with, and a project's page makes its project the
model's (`chooseProject`), as the native project screen did, so the terminal
component and the native chat follow it, and keeps that project's lists fresh
(`keepFresh(.sessions)`, which skips a list the page itself has just read: the
page's `relay.call` reads of a project's lists, the projects and its writes are
installed into `RemoteModel` as Swift's own would be). When Swift moves the
pages itself (a host just paired, an overview row, the global orchestrator's
Projects or ⌘N, a terminal made in the native New chat sheet) it sends
`nav.open`.

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
on this page leaves `dashboard.get` to it). A row opens the project's page at
its terminal or chat, as the Mac's global window routes it (`routeTarget`); the
global orchestrator is a native screen (`nav.present` `global`). A Mac
whose RiWork predates `dashboard.get` is never asked; the page says so, and its
projects and its global orchestrator (the snapshot's `global`, from
`orchestrators.list`: Open, or Start when there is none) still open.
The All hosts overview keeps its filters, saved read markers, coverage and
truncation notices, activity history and Haiku summaries.

The Projects page lists `projects.list` (what each project's agents are doing,
read again every 4 seconds while it is on screen, every 10 in the iPad's
column) in the person's order (`riwork.projectSort`: Recent, Name, Date added,
as `ProjectSorting` ordered it), with the Mac's folders when the connection
lists `native.invoke` `Store.snapshot` (the tree is the desktop Projects
panel's, `apps/riwork/project_tree.ts`; a search lists its hits flat). New
project (＋, ⌘⇧N) checks the name as the Mac does and sends `project.create`
once; a lost answer says the project may have been made and reads the list
again. A project made here opens its page with New terminal.

A project's page draws the Mac's shared tab list (`tabs.list`, visible entries
in shared order; `shells.list` says which tab is a project terminal), or on a
Mac without it the live terminals and the chats (`chats.list`), read again
every 4 seconds while it is on screen. A terminal tab shows the
`riwork.terminal` component (`target: {kind: "remote", desktopId, shellId}`);
a chat tab opens the native chat (`nav.present` `chat`) over the page, which
keeps its terminal. The tab on screen is remembered per project
(`platform.prefs` `projectTab.<project>`, the Keychain library's
`projectTabKeys`). A long press (or ⋯, or VoiceOver's actions) has Move left,
Move right and Close; closing always asks first: Detach hides the tab (on every
device), Exit (a project terminal that is not a worker) hides it and then sends
`shell.close`; a chat is detached here and stopped from its own menu; on a Mac
without shared tabs, Close terminal sends `shell.close` after the same
question. New terminal (＋, ⌘N) offers Shell, Codex, Claude, Grok and Muse in
the project or one of its worktrees, remembers the kind
(`riwork.newTerminal.kind`), sends `as_settings` on a Mac that decides agents'
restrictions (`shell_create_as_settings`) and otherwise an Unrestricted switch,
and sends `shell.create` once; a lost answer reads the tabs again. Its "New chat
or orchestrator…" opens the native New terminal sheet at Chat (`nav.present`
`newChat`). Open a worker or shell (⌘⇧O) lists the hidden tabs and opens one
with `tabs.open`. The page's ⋯ menu also has, while a terminal is on screen,
Copy screen text (the terminal's `copy`: its screen and scrollback, since the
capture terminal has no selection) and Refresh output (`reattach`).

The app chords have one table, `apps/riwork/app_chords.ts` (design §2.5's
`keyboard.ts`; that name is the Mac's command keymap), by the ids the terminal
component sends (docs/nsh-contracts.md §3): `tab.select.1`…`tab.select.9`
(⌘1–9, Clicks Tab 1–9), `tab.prev` / `tab.next` (⌘⇧[ / ]), `terminal.new` (⌘N,
Clicks Tab N), `nav.back` (⌘⇧P), `nav.projects` (Clicks Tab P: the host's
Projects page; on iPad its column, beside the dashboard), `display.open` (⌘⇧,:
Display settings) and `picker.open` (⌘⇧O). `chat.model` (Clicks Tab M) is the
native chat's own. They reach the page three ways, and it acts on them only
while it takes keys (no form, picker or menu up):
- its keymap, while TSX holds the keyboard;
- the terminal's `command` event, while the terminal holds it;
- Swift's `keyboard.command {id}` event: from the root key host on a Clicks
  keyboard in its hardware layout, which on a project's page takes the
  keyboard only when nothing that types holds it (no terminal on screen, as the
  native project screen's own key host), and then carries the ⌘ chords too
  (it sits beside the NSH view, so the page's keymap may not be reached while
  it holds the keyboard; only one of them answers a key); and from the
  native chat, whose ⌘1–9, ⌘⇧[ / ], ⌘N, ⌘⇧O and Clicks Tab 1–9 and N go back to
  the page first and are then sent (⌘⇧P there is Back to the page, ⌘⇧, opens
  Display settings over the chat, Clicks Tab P the Projects page).

⌘R (the page on top's refresh) and ⌘, (Display settings) are the app's keymap
on every page while no native screen is up, and the root key host's while it
holds the keyboard (`mobile.command`). With a hardware keyboard attached the
strip moves to the bottom.

Not on the TSX project page yet, from the native project screen
(`TerminalTabsView`): Rename, drag reordering, Edit tabs, focus mode, Session
info, Jump to latest, and the Tabs setting "When closing a tab"
(`tab_close_behavior`, still in Display settings, which this page does not
read: closing always asks).

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
| `relay.call` | call | `{host, method, params?, timeoutMs?}` | the RPC's `result`; the interactive host only, and only `dashboard.get`, `projects.list`, `worktrees.list`, `shells.list`, `chats.list`, `tabs.list`, `tabs.open`, `tabs.update`, `shell.create`, `shell.close`, `project.create` and `native.invoke` (`Store.snapshot` only) |
| `relay.link` | event | | `{host, link: LinkState}` |
| `relay.decision.review` | call | `{host, chatId, requestId, kind?}` | `DecisionReviewDto` (`status` `pending`, or `unavailable` / `uncertain` with `error`) |
| `relay.decision.respond` | call | `{host?, reviewId, decision?, answers?}` | `DecisionReviewDto` with `status` `sent`, `pending` (nothing went out; `error` says why), `unavailable` or `uncertain` |
| `platform.prefs.get` / `.set` | call | `{name}` / `{name, value}` | `{value}` / `{}`; `projectSort` (`riwork.projectSort`), `newTerminal.kind` (`riwork.newTerminal.kind`) and `projectTab.<project>` (the interactive host's last tab of a project, `shell:<id>` or `chat:<id>`, in the Keychain library's `projectTabKeys`) |
| `platform.theme` | event | | `{theme: InterfaceTheme, native, mic, dark}`; the first value is `host.params.theme` |
| `nav.present` | call | `{name, host?, projectId?, chatId?, start?}` | `{}`; presents a remaining native Swift screen |
| `nav.open` | event | | `{page, host?, projectId?, name?, tab?, newTerminal?}`: Swift moves the pages (`hosts`, `dashboard`, `projects`, `project`) |
| `keyboard.command` | event | | `{id}`: an app chord for the project page (`app_chords.ts` ids), from Swift's root key host or the native chat |

```
HostInfo  = {id, name, address?, selected: boolean, link: LinkState, features: string[]}
LinkState = {state: "connected"|"connecting"|"offline"|"failed"|"suspended", label, error?, since?}
```

- **`relay.call`** goes through `RemoteModel`'s connection to the interactive
  host and so through `RequestValidation`; JS may not send `wait_ms` above 0 or
  any `pty.*`, and `native.invoke` only for the reads the pages make
  (`NSHHostAPI.nativeReads`: `Store.snapshot`) that the Mac lists. `timeoutMs`
  (1 to 60 s, 30 s by default; up to 100 s, and that by default, for
  `shell.create` and `project.create`) lets go of the wait without closing the
  connection. The writes (`tabs.open`, `tabs.update`, `shell.create`,
  `shell.close`, `project.create`) are never repeated; one whose answer was lost
  (the link dropped, the wait ran out) rejects `uncertain`, and the page reads
  its list again. `shell.close` is refused, as `RemoteModel.closeTerminal`
  refuses it, for anything but a project terminal of the project Swift follows
  and for a shell this device has ever seen as a shared worker (only ever
  detached). `HostInfo.features` lists the connection's `ready.features` by
  name, `dashboard` left out for a Mac that refuses it, plus `native` and
  `native:<Module.method>` for each native read the Mac lists.
- **The link** is `connected` only once what the Mac offers is known
  (`capabilitiesKnown`: its `ready.features` and first lists); until then it
  is `connecting`, `features` is empty and `relay.call` answers `offline`. So
  no page reads a Mac that shares tabs as one that does not.
- **Decisions** keep the Swift flow the overview's sheet uses.
  `relay.decision.review` reads the request from the host's last parsed
  dashboard (`DashboardDecision`; a request this phone cannot read in full is
  not offered) and returns an opaque `reviewId`. `relay.decision.respond`
  answers through `NSHMobileBridge.respondToDecision`: `DashboardResponder`
  reads the chat again and checks the exact request and its choices before
  exactly one `chat.command` goes out, never retried. A response that went
  out, or whose delivery is uncertain, is remembered (`sentDecisions`): the
  review then answers `uncertain` and nothing is sent again from this phone.
- **`nav.present`** names: `chat` (`chatId`, and `projectId`, by default the
  project the pages show: the project's chat, natively, over its page; the model follows the project and puts the chat up
  as a dashboard row's tab request does, and leaving it gives the page its
  terminal back), `global` (the global orchestrator; `start: true` starts it
  when there is none, an explicit Start only), `memo` (`projectId`, on a Mac
  with `project_memo`), `newChat` (`projectId`: the native New terminal sheet at
  Chat, for chats and orchestrators; a chat it makes opens, a terminal it makes
  becomes the page's tab through `nav.open`), `settings`, `addHost`,
  `hostDetails` (`host`), `libraryRecovery`. The project workspace
  (`TerminalTabsView`) and the native New project sheet are no longer presented.
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
screen, on the hosts, a host's dashboard and a project's page (there only when
no terminal or text field holds the keyboard). Native screens keep their own
responder ownership.

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

The next migration boundaries are the chat presentation/composer, the
remaining panels (worktrees, tasks, files, settings, schedules) and the
remaining forms. A desktop screen moves by taking its data through the ports in
`apps/riwork/client` instead of `riwork/native`; the Mac keeps its native
module behind `adapters/local.ts`. The terminal remains a platform-native
surface (`riwork.terminal`) while its surrounding controls are TSX. The old
`RemoteRootView`, `TerminalTabsView` and `NewProjectSheet` are retained as
source (the hosted app tests mount them); the app entry no longer reaches them.
`NewTerminalSheet` stays in use: `nav.present` `newChat` opens it at Chat, with
its terminal kinds still offered, and a terminal made there becomes the page's
tab.
