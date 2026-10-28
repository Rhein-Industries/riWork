# NSH interface contracts (Step 0, frozen v1)

Status: **frozen v1** for the NSH port of `crates/riwork-desktop` (task a20829f1, branch
`nsh-contracts`). These contracts sit between four parties that are built in parallel:

1. the **TSX app** (one bundle, run once per window),
2. the **`riwork/native` UI set** (Rust, wraps `riwork-core` / `riwork-engine`),
3. the **AppKit host** (Rust process that owns NSWindows, one `AppView` per window),
4. the **`riwork.terminal` native component** (AppKit NSView hosting libghostty).

The behavior each party must reproduce is inventoried in `docs/nsh-parity.md` (IDs such as
`SHELL-012`, `TERM-031`). This document fixes only the seams. A change to anything marked
*frozen* needs a version bump of the affected surface (`schema` fields below), a note in §7,
and sign-off from the owners of both sides.

`verification_environment`: code and documentation reading only (this worktree at
`50b3b41`, NSH 0.3.0 crate sources in the local cargo registry, sibling task details via
`riwork task show`). Nothing was built, run, or tested in a live window.

NSH references are to the 0.3.0 crates in
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/` (abbreviated `nsh-core/`, `nsh-base/`,
`nsh-engine/`, `nsh-js/`, `nsh-platform-macos/`). The installed `nsh` CLI is 0.2.0
(`nsh --version` → `nsh 0.2.0 (68531fb)`); the toolchain task owns moving it to 0.3.

---

## 0. Process and layering (frozen)

```
┌──────────────────────── RiWork NSH process (AppKit) ────────────────────────┐
│ AppKit host (Rust, main thread)                                             │
│   windows: WindowId → (NSWindow, AppView, HostChannel)                       │
│   app menu, Dock menu, runtime registry (riwork restart), quit/reopen        │
│   process services: StoreHub, SessionSampler, ChatHub, ScheduleTicker, ...   │
│                                                                             │
│   AppView #1 (engine thread #1)        AppView #2 (engine thread #2)        │
│     TSX bundle  ── host ──────┐          TSX bundle  ── host ──────┐         │
│     riwork/native (UI set) ───┼─ shared process services ─────────┤         │
│     <riwork.terminal> NSViews ┘          <riwork.terminal> NSViews ┘         │
└─────────────────────────────────────────────────────────────────────────────┘
```

- **One process, N windows.** Each NSWindow holds one `nsh_platform_macos::AppView`
  (`nsh-platform-macos/src/app.rs:194`), each with its own engine and its own
  `nsh_engine::HostChannel` (`AppOptions::host`, `app.rs:139-145`; `AppView::host`,
  `app.rs:255`). Every window loads the **same bundle**; what a window is comes only from
  `host.params` (§2.2).
- **UI sets are process-wide.** `set_ui_sets([nsh_base::registry(), riwork_native::registry()])`
  is called once before the first AppView (`app.rs:39-41`); every engine serves both.
- **Native components are registered once** on the shared `AppOptions::components`
  (`app.rs:112-115`, `nsh-platform-macos/src/native.rs:195`) with `riwork.terminal` (§3).
- **Who does what** (frozen rule used throughout):

| Concern | Goes through | Why |
|---|---|---|
| Domain data and commands (projects, sessions, chats, schedules, settings, layouts) | `riwork/native` (§1) | Window-agnostic, same in every window, unit-testable in Rust. |
| Anything that names another window, the process, or needs AppKit main-thread objects outside the AppView (open/close/focus/route windows, Quick Look, open/save panels, typed pasteboard, notifications, restart/quit, Dock and app menus) | host channel (§2) | Only the host owns NSWindows and the run loop. |
| A terminal surface | `riwork.terminal` (§3) | libghostty needs an NSView and a PTY; no JS on the input or render path. |
| This window's own chrome (title, title bar, frame, fullscreen, context menus, keymap, clipboard, file drop) | NSH built-ins (`nsh/desktop` `Window`, `Menu.popup`, `nsh/keymap`, `nsh/clipboard`, `onFileDrop`) | Already provided per AppView. |

- **TSX has no filesystem, process, or socket capabilities.** `nsh.json` grants
  `"host": ["window.*", "app.*", "system.*"]` and, from W3, the `clipboard`
  built-in for Edit/session-ID copy (grant syntax: `nsh-core/src/access.rs:20`).
  Everything else is reached through `riwork/native`. This keeps one implementation of every
  file format (riwork-core) and makes the TSX layer testable against fake backends.
- **One NSH process per RIWORK_HOME.** GPUI already runs N windows in one process but has no
  single-instance lock, so every launch and `riwork open` starts another process and shell
  claims are per process (parity SHELL “process model”). The NSH host takes
  `RIWORK_HOME/run/desktop.lock`; a second launch forwards its request (`open` project/path,
  or nothing) over `RIWORK_HOME/run/desktop.sock` to the running host and exits. The only
  allowed overlap is a reload replacement started by the running host (§2.5) until it
  confirms. The CLI is unchanged: it still launches the app binary, which forwards.
- **RIWORK_HOME is fixed per process.** It is resolved once by `riwork_core::paths::riwork_home()`
  (`crates/riwork-core/src/paths.rs:13`, default `~/.local/share/riwork`) and is never
  passed in from JS. Tests use an isolated `RIWORK_HOME`.

---

## 1. `riwork/native` module surface

### 1.1 Shape (frozen)

- Crate `riwork-nsh-native` (new). One UI set: `#[native_module("riwork/native")]`, exporting
  `#[module]` objects (`Store`, `Sessions`, `Chat`, `Schedules`, `Layouts`, …) and `#[class]`
  handles (`Subscription`, `ChatHandle`, `LayoutSession`). TS declarations are generated into
  `.nsh/types/riwork/native.d.ts`; TSX imports `import { Store, Chat } from "riwork/native"`.
- Names: modules PascalCase, functions camelCase, payload fields camelCase. Payloads are the
  existing serde types of riwork-core/engine re-exported with `#[serde(rename_all = "camelCase")]`
  wrapper DTOs; DTOs live in `riwork-nsh-native` so the engine's on-disk/wire formats never
  change for the port.
- Ids are strings (UUIDs, tab keys) except `PaneId`, a JS number. A `PaneId` above
  `Number.MAX_SAFE_INTEGER` (legal on disk up to `u64::MAX - 256`, `crates/riwork-core/src/layouts.rs:21-22`)
  is remapped by `LayoutSession` on load (§4.6).

### 1.2 Call model (frozen)

NSH runs a `#[module]` function in one of three ways (`nsh-core/src/element/module.rs:1-11`):
a plain function runs inside the JS call; a function taking `ModuleCx` is an ordered op; an
`async fn` runs as a task of the engine thread and returns a Promise. Tasks are polled inside a
tokio context, `Send` work goes to background threads, and wakers may fire from any thread and
wake the engine (`nsh-core/src/element/task.rs:1-10`).

| Kind | Rule |
|---|---|
| **Sync** | Only for pure, in-memory work that returns in < 1 ms (formatting, key parsing, layout projection of an already-loaded `LayoutSession`). Never touches disk, sockets, tmux, subprocesses or locks held by other threads. |
| **Async** | Everything else. The function body only hops: it moves the blocking riwork call to the blocking pool (`tokio::task::spawn_blocking`) and awaits it. Nothing blocks the engine thread. |
| **Errors** | A rejected Promise carries `Error("<code>: <message>")`. Codes: `refused` (the backend said no and did nothing; maps `CallError::Refused`, `crates/riwork-engine/src/chat/client.rs:25-31`), `broken` (unknown whether it acted; `CallError::Broken`), `not_found`, `invalid`, `conflict` (lost a merge/revision race), `unavailable` (backend not running), `io`. |
| **Cancellation** | Promises are not cancellable. Long work returns a handle (`ChatHandle`, `Subscription`) whose `close()` ends it. |

### 1.3 Subscriptions and events — no polling (frozen)

Every live view of data is a **subscription**, never a timer in TSX.

- `X.subscribe(args, listener) → Subscription` where `Subscription` is a `#[class]` with
  `close()`. Created during render, it is owned by the component: when the owner goes, the
  engine-side task ends (`spawn` ends with its owner, `nsh-core/src/element/ambient.rs:538-552`),
  which drops the hub listener.
- **First delivery is a snapshot**, later ones are deltas or snapshots:
  `{ kind: "snapshot", rev, data }` | `{ kind: "delta", rev, ... }`. `rev` is monotonic per
  topic per process. A listener that sees a gap or `resync: true` treats the next snapshot as
  authoritative.
- **Delivery path:** source thread → process hub (one per RIWORK_HOME per topic) →
  per-subscription bounded queue (`async_channel`, capacity 64) → engine task awaiting the
  queue → `Callback::post` to the listener (`nsh-core/src/element/callback.rs:1-7,158`).
  On overflow the queue is cleared and one snapshot with `resync: true` is queued.
- **Coalescing:** a listener gets at most one delivery per topic per engine turn, and a
  streaming topic (chat patches, command output) at most one per 16 ms: the first change is
  delivered at once, later ones are merged and delivered on the trailing edge (chat text appends
  are concatenated, §1.6). Today's chat feed does the same: one drain per frame, then a 16 ms
  wait (`crates/riwork-desktop/src/chat_view.rs:876-891`). Nothing is ever sent per token.
- **Text offsets:** every text position or range that crosses the bridge is in UTF-16 code
  units (JS strings, NSH selection); riwork-core/engine use UTF-8 byte offsets, and the bridge
  converts.
- **Clock:** time-dependent outputs (notice expiry, elapsed labels, schedule “next run”) take
  the bridge's clock, which tests replace (today `TEST_NOW`,
  `crates/riwork-desktop/src/chat_view/notices.rs:22`).
- **Sources are pushes.** The hubs reuse the push sources the desktop already has: the shared
  RIWORK_HOME `notify` watcher (`crates/riwork-desktop/src/subshells.rs:240-300`, one FSEvents
  stream per home per process, not per window), the chat host's sequenced event stream
  (`crates/riwork-engine/src/chat/client.rs:486-560`), and engine channels.
- **Unavoidable polling stays in Rust, once per process.** Where the engine only offers a poll
  today (tmux sampling, schedule ticking, usage, dashboard reads), it runs in a process service,
  at an interval no shorter than today's, compares with the last result, and emits only on
  change. It never runs on an engine thread and never per window. Inventory of today's loops
  (each becomes one process service or disappears behind a watcher):

| Today (GPUI) | Interval | Becomes |
|---|---|---|
| Workspace refresh per window, `crates/riwork-desktop/src/main.rs:2296-2299` | 2 s | `SessionSampler` (one per process) woken by the home watcher; 2 s fallback poll only for tmux liveness. |
| Schedule ticker, `crates/riwork-desktop/src/schedules.rs:9-31` (`ScheduleStore::tick_tracked`) | 1 s | `ScheduleTicker` process service (not UI); status published on `Schedules.subscribe` topic `runtime`. |
| Appearance sync, `main.rs:13724-13729` | 2 s | Watchers on `settings.json` and the Ghostty config files (parse skipped when stamps are unchanged, as today); `Appearance.subscribe`. |
| Cua status, `main.rs:13730-13742` | 15 s while not ready | `System` process service; stops when ready (as today). |
| Panels presence, `crates/riwork-desktop/src/panels.rs:108` | 2 s | Folded into `SessionSampler`. |
| Notifications, `crates/riwork-desktop/src/notifications.rs:51` | 1 s | Host notification service (§2). |
| Orchestrator wakes, `crates/riwork-desktop/src/orchestrator_wakes.rs:12` | 1 s | Process service. |
| Global window, `crates/riwork-desktop/src/global_window.rs:648` | 2 s | `Dashboard.subscribe` fed by store/session hubs. |
| Dashboard reads, `main.rs:1349` | 10 s | Same. |
| Chat activity clock, `crates/riwork-desktop/src/chat_view.rs:1240-1250` | 1 s, only while a turn runs | Stays a Rust timer inside `ChatHandle` (`refresh_timing`, `crates/riwork-engine/src/chat/projection.rs:376`), delivered as an ordinary patch; stops when the turn ends. |
| Schedule panel, `crates/riwork-desktop/src/schedule_panel.rs:293` | 1 s | `Schedules.subscribe`. |

  The row-level list is in `docs/nsh-parity.md`: SHELL “Background timers & polling”, the
  polling rows of PANEL and TERM, and DOCS “Watch / polling / idle-work mitigation”.
- **TSX must not** use `setInterval`, `requestAnimationFrame` or repeated `resource().refetch()`
  to watch data. A lint rule in the app (`nsh check` custom rule or CI grep) enforces it;
  animation code is exempt.

### 1.4 Backends and tests (frozen)

- Each module is a thin adapter over a Rust trait (`StoreBackend`, `SessionsBackend`,
  `ChatBackend`, `SchedulesBackend`, `LayoutBackend`). Production implements them with
  riwork-core/engine; tests use in-memory fakes.
- Bridge tests (`Verify = bridge` in the parity list) are plain Rust tests against the adapters
  with an isolated `RIWORK_HOME`.
- TSX tests (`Verify = nsh-test`) run `nsh test` (0.3 CLI) with the fake backends registered
  under the same specifier. The host spike ran `nsh check` and `nsh test` headless on a mixed
  Rust + TSX bundle importing `riwork/native` (2 passed, 0 failed; `WORKER_REPORT.md` “What was
  proved” on `nsh-spike-host`), so no JS stand-in module is needed.
- **No durable state in `nsh/storage`.** Every window runs the same app id, and two engines
  writing one `storage.json` would race. All persistence goes through `riwork/native`;
  `AppOptions::data_dir` is `RIWORK_HOME/nsh` and holds only NSH caches.

### 1.5 `Store` (projects, folders, worktrees, tasks)

Backed by `riwork_core::store::Store` (`crates/riwork-core/src/store.rs:666`), whose truth is
`RIWORK_HOME/state.json` (`store.rs:687`, `snapshot` at `store.rs:692`).

| JS | Kind | Wraps | Returns |
|---|---|---|---|
| `Store.snapshot()` | async | `Store::snapshot` `store.rs:692` | `StoreState` (projects, folders, worktrees, tasks, active project) |
| `Store.subscribe(listener)` | event | home watcher on `state.json` + writes made through the bridge | `snapshot` deliveries only (the state is small and already cached per stamp, `store.rs:668-671`) |
| `Store.search(query)` | async | `State::search` `store.rs:415` | `SearchHit[]` |
| `Store.inspectProject(root)` | async | `Store::inspect_project` `store.rs:862` | `ProjectInspection` |
| `Store.createProject(req)` / `createNewProject(req)` / `addProject(req)` | async | `store.rs:974`, `1017`, `1089` | `Project` |
| `Store.useProject(selector)` | async | `store.rs:1195` | `Project` |
| `Store.renameProject`, `updateProjectMetadata`, `setProjectNotifications`, `setProjectOrchestratorWakes`, `setProjectOrchestrator`, `setProjectDelegationPolicy`, `setProjectCodexAccount`, `setProjectMemo` | async | `store.rs:1342-1511` | `Project` |
| `Store.createFolder`, `renameFolder`, `moveFolder`, `removeFolder`, `moveProjectToFolder` | async | `store.rs:1203-1316` | `ProjectFolder` / `void` |
| `Store.syncWorktrees`, `refreshWorktrees`, `forgetMissingWorktree`, `createWorktree`, `createWorktreeInRepo` | async | `store.rs:1537-1739` | `Worktree[]` / `Worktree` |
| `Store.addTask`, `assignTasks`, `unassignTasks`, `setTaskStatus` | async | `store.rs:1638-1719` | `Task` / `Task[]` |

A mutation resolves after the write; the same change also reaches every `Store.subscribe`
listener (no caller re-fetches).

### 1.6 `Chat`

Backed by the chat host (`riwork chat serve`) over `RIWORK_HOME/run/chat.sock`
(`crates/riwork-engine/src/chat/client.rs:19-21`). The bridge keeps one request client per
burst and one subscription thread per open chat, exactly as the GPUI window does
(`client.rs:1-5`). Transcript folding (`Transcript::apply`, `crates/riwork-engine/src/chat/model.rs:1218`)
and grouping (`project`, `crates/riwork-engine/src/chat/projection.rs:448`) stay in Rust;
TSX receives projected rows.

| JS | Kind | Wraps |
|---|---|---|
| `Chat.list()` | async | `Client::list` `client.rs:223` |
| `Chat.subscribeList(listener)` | event | chat host list changes (home watcher on the chat registry; snapshot per change) |
| `Chat.create(newChat)` | async | `Client::create` / `create_identified` `client.rs:110,144` |
| `Chat.command(chatId, command)` | async | `Client::command_checked` `client.rs:336` (send, stop, approve, mode/model/effort changes: `ChatCommand`) |
| `Chat.stageAttachment(chatId, file)` | async | `Client::stage_attachment` `client.rs:188` |
| `Chat.close(chatId)` / `Chat.delete(chatId)` | async | `client.rs:464,472` |
| `Chat.suggest(chatId, text, caret)` | async | `chat::suggest` (`crates/riwork-engine/src/chat/suggest.rs`) |
| `Chat.open(chatId, opts) → ChatHandle` | sync (starts a subscription thread) | `Subscription::open(socket, chatId, since)` `client.rs:509` |

`ChatHandle` (class):

- `subscribe(listener) → Subscription`: first `{ kind: "reset", rev, seq, info, state, rows, approvals }`,
  then `{ kind: "patch", rev, seq, upsert: Row[], remove: string[], order?: string[], append?: { rowId, part, text }[], state?, approvals? }`.
  `seq` is the last applied envelope sequence (1-based, gapless per chat, `crates/riwork-engine/src/chat/wire.rs:117-121`);
  reconnects resume with `since = seq` so nothing is replayed twice.
  `Row` is `projection::Row` (`projection.rs:93`) as a DTO; streaming text is sent as `append`
  ops (O(delta) per patch), never by re-sending the whole message.
- **Markdown, highlighting and diffs are parsed in Rust** with RiWork's rules (single newlines,
  nesting depth 8, Codex citations; `crates/riwork-desktop/src/chat_view/markdown.rs`,
  `code_highlight.rs`, `diff.rs`, moved out of the GPUI crate) and sent as RiWork's own blocks
  and spans; TSX renders them with `nsh/base` `TextView` (app blocks, `streaming`) and
  `DiffView`. TSX never runs NSH's Markdown parser on transcript text.
- **Selection and copy are Rust's.** Selection endpoints are `{ leafId, offset }` keyed to the
  stable leaves of `selection_leaves` (`projection.rs:1552`), so they survive recycled List cells
  and streaming; `exportSelection(anchor, focus) → string` builds the copied text from source
  (today `crates/riwork-desktop/src/chat_view/selection_document.rs:19-51`,
  `select.rs:125,308`). Find (`find(query) → { hits, disclosures, generation }`) is a
  `ChatHandle` method for the same reason.
- `setMode(mode)` (`DisplayMode`, `projection.rs:43`), `setDisclosure(id, open)` (`DisclosureState`,
  `projection.rs:123`), `page(window)` (`PageWindow`, `projection.rs:302`), `anchor(itemId)`
  (`visible_anchor`, `projection.rs:1434`), `selectionLeaves()` (`projection.rs:1552`).
- `close()`: closes the subscription with its `SubscriptionCloser` (`client.rs:595-600`) so no
  thread stays blocked on a quiet chat.
- Patches are coalesced (§1.3). The host spike (task 2a537a8b) measures patch rate and
  List+TextView cost against the GPUI baselines in [`docs/nsh-parity.md`, Perf baselines](nsh-parity.md#perf-baselines-to-beat-p-docs).

#### Phase 4a: Chat payload schema 1 (task `6a41bb58`)

Phase 4a adds the first `Chat` surface. The §1.6 shape holds (sync `open`, a
`ChatHandle` whose subscription delivers `reset` then `patch`, folding and
projection in Rust, Markdown parsed in Rust into TextView documents, append ops
for streaming). The payload is `schema: 1` of these TS declarations; the engine
wire and log formats and `chat-drafts/` files are unchanged.

| JS | Kind | Rule |
|---|---|---|
| `Chat.list()` | async | `Client::list` on an existing host; never starts one. |
| `Chat.create(NewChatRequest)` | async | Ensures the host; strict decoding (UUID project/worktree, non-empty cwd). The host registers the shared tab entry (`TabStore::register_created`); the bridge adds no tab write. |
| `Chat.delete(chatId)` | async | `Client::delete`. |
| `Chat.open(chatId, { history?, mode? }) → ChatHandle` | sync | Validates the UUID and starts the handle's threads; no IO on the engine thread. `history` follows only an existing host (CHATT-013). |
| `Chat.fixtureDirectory(name)` | async | Debug/test-support fixture only (`RIWORK_NSH_CHAT_FIXTURE=1`, `/tmp` home); rejects `unavailable` otherwise. |
| `ChatHandle.subscribe(listener)` | sync handle | `ChatDelivery`: `reset { rev, seq, panel, rows, bodies }`, then `patch { rev, seq, panel?, upsert?, remove?, order?, bodies?, drop?, splice?, append? }`, at most one per 16 ms (first change at once, trailing edge after). `resync: true` replaces an overflowed queue (capacity 64). A second subscription replaces the first. |
| `ChatHandle.setViewport(first, last)`, `toggle(rowId)`, `expandAll(open)`, `setMode(mode)` | sync | Enqueue to the handle's worker; the worker reprojects (never for token deltas) and patches. |
| `ChatHandle.submit({ text, editor, generation })` | async | Exactly one exchange after the draft recovery snapshot is durable; `ChatReceipt { status: submitted\|refused\|uncertain, error?, key?, clear }`. |
| `ChatHandle.interrupt()`, `stop()`, `approve(requestId, decision)`, `configure(...)`, `switchProvider(provider, model?)` | async | Ordered, retried once only on a broken connection; rejects with `refused:`/`broken:`. |
| `ChatHandle.answer({ requestId, answers })` | async | Exactly once, with a receipt. |
| `ChatHandle.restoreDraft()` / `saveDraft(text, editor, generation)` / `dismissSubmission(key)` | async / sync / sync | The process draft store (byte-identical `chat-drafts/<uuid>` files); saves run in order on the worker, never on the engine thread. |
| `ChatHandle.close()` | sync | Stops following; queued commands are still delivered. |

Rows (`ChatRow`) are flattened, visible headers keyed by row id; `hasBody` rows
get a `ChatBody` (`markdown` blocks, `text`, `command`, `detail`, `steps`) only
near the reported viewport (overscan 16 rows; far bodies are `drop`ped).
Differences from the §1.6 wording, all within its intent: the chat-level fields
(`info`, `state`, `approvals`, questions, toolbar choices, banners, failures)
travel as one `panel`; `page(window)` is `setViewport` paging of bodies (rows
are headers, so the whole visible list is cheap); `Chat.command` is split into
typed handle methods; drafts are `ChatHandle` methods over the one process
store rather than a separate `Drafts` module. Not in this slice (seams only):
`Chat.subscribeList`, `stageAttachment`, `suggest`, `close`, selection/find
(`exportSelection`, `find`), `anchor`, `setDisclosure(ids)`, Verbose internals,
attachments, dictation, and `Chat.claimTab`/`releaseTab`.

### 1.7 `Sessions` (shells, harnesses, tmux)

Backed by `riwork_engine::sessions::SessionManager` (`crates/riwork-engine/src/sessions.rs:197`).
Attaching, resizing and input of a visible terminal are **not** bridge calls: they are inside
`riwork.terminal` (§3), which calls `SessionManager` directly on the native side.

| JS | Kind | Wraps |
|---|---|---|
| `Sessions.list()` | async | `list_with_activity` `sessions.rs:701` |
| `Sessions.subscribe(listener)` | event | `SessionSampler`: `sample` / `invalidate_sample` `sessions.rs:2060,2066`, woken by the home watcher (`subshells.rs:240-300`) |
| `Sessions.create(req)`, `createEditor(req)`, `createHarness(req)` | async | `sessions.rs:235,261,291,317` |
| `Sessions.createSubshell({projectId, parentShellId})` | async, Phase 2b | Explicit same-project live parent; creates a separate plain tmux session with parent provenance, independent process/scrollback, deriving worktree/cwd from the parent. Never types a shell command into the parent. |
| `Sessions.openEditor({projectId, worktreeId?, path, line?})` | async, Phase 2b | Resolves the trusted project/worktree root and validates a no-follow regular file inside it. Reuses a live editor by project/canonical path after saves; otherwise captures identity and calls engine `create_editor` (V1–V3). Optional positive line jumps use engine `send_keys`, never JS terminal input. Returns `ShellSessionDto`. |
| `Sessions.close(id)` | async | `sessions.rs:2071` |
| `Sessions.send(id, text)`, `sendKeys(id, items)`, `paste(id, text)`, `pasteFiles(id, paths)` | async | `sessions.rs:1853,1400,1860,1879` (for sessions not shown in a terminal view, e.g. scheduling or CLI-like actions) |
| `Sessions.capture(id, lines)`, `readOutput(id, query)`, `readHistory(...)` | async | `sessions.rs:1210,1330,1362` |
| `Sessions.currentDirectory(id)`, `metrics(id)` | async | `sessions.rs:1967,1983` |
| `Sessions.orchestrator(projectId?)`, `createOrchestrator(projectId?)` | async | `sessions.rs:379,571` |

Shared tab membership (order, titles, hidden) is its own topic because it is shared with the
phone and across windows (`docs/shared-tabs.md:3`, `crates/riwork-engine/src/project_tabs.rs:291`):

| JS | Kind | Wraps |
|---|---|---|
| `Tabs.subscribe(projectId, listener)` | event | `refresh_snapshot_with_activity` `project_tabs.rs:932` on home-watcher wakes; `{epoch, revision, entries}` |
| `Tabs.update(projectId, update)` | async | `TabStore::update` `project_tabs.rs:731` (`hide`, `unhide`, `move`, `rename`) |

### 1.8 `Schedules` (automations)

Backed by `riwork_engine::schedule_service::ScheduleService`
(`crates/riwork-engine/src/schedule_service.rs:109`). Dispatch is not a UI concern: the
`ScheduleTicker` process service runs `ScheduleStore::tick_tracked` every 1 s exactly as
`crates/riwork-desktop/src/schedules.rs:9-31` does today, once per process.

| JS | Kind | Wraps |
|---|---|---|
| `Schedules.list(scope?)` | async | `schedule_service.rs:142` |
| `Schedules.show(id)` | async | `schedule_service.rs:152` |
| `Schedules.create(req)` / `createChat(req)` | async | `schedule_service.rs:162,200` |
| `Schedules.update(req)` | async | `schedule_service.rs:243` |
| `Schedules.pause(key, paused)` / `delete(key)` | async | `schedule_service.rs:269,276` |
| `Schedules.chatOptions(...)` | async | `schedule_service.rs:318` |
| `Schedules.subscribe(listener)` | event | topics `schedules` (store file watcher, snapshot) and `runtime` (ticker error string, today `RuntimeStatus`, `schedules.rs:6-8`) |

### 1.9 `Layouts` — see §4.

### 1.10 Remaining modules (names frozen, payloads follow the same rules)

These follow §1.1-§1.4 unchanged; each port task adds its function table to this file under a
`schema` bump before first use. What each needs is listed per area in `docs/nsh-parity.md`
(“Bridge needs” sections, and [Bridge needs → frozen modules](nsh-parity.md#bridge-needs--frozen-modules)).

| Module | Covers |
|---|---|
| `Settings` | Global settings (typed get/set, change events replacing the 2 s reload), interface text size |
| `Appearance` | Resolved theme and terminal colors (`Appearance::resolve`, moved out of `riwork-desktop`), `appearance.json` publishing |
| `ProjectSettings` | Per-project settings, memo |
| `Accounts` | Provider and Codex accounts, API keys (Keychain stays in Rust) |
| `Usage` | Codex/Claude/Grok usage with today's backoff |
| `Activity` | Agent activity states and completions |
| `Dashboard` | Global overview, decisions, global window state (`global-window.json`) |
| `Orchestrators` | Orchestrator choice, global orchestrator, wakes status |
| `Workers` | Worker shelf/cards, summaries, open/send/stop |
| `Handoff` | Handoff flow |
| `Drafts` | Chat drafts store (one per process), references |
| `Files` | File explorer tree, file preview data, link targets, open-refusal rules |
| `Preview` | App preview and preview control |
| `Remote` | Paired hosts, remote tree, remote shells and projects |
| `Notices` | Notice banners and history |
| `Dictation` | Dictation state machine over the `riwork-speech` helper |
| `Markdown`, `Diff`, `Diagram`, `CommandLog`, `Media` | Rust-side parsing/highlighting, diff display, diagram rendering, command-output paging, image loading for non-chat surfaces and for `ChatHandle` rows |
| `ProjectRecency` | Recency scan |

#### Phase 1 W3 payload schema 2: Settings, Appearance and global-window state

Task `c62ea465` adds these tables before their first TSX use. `SettingsDto`,
`AppearanceDto` and `GlobalWindowDto` carry `schema: 2`. The native producer and
TSX consumer are introduced and checked together. Host params remain schema 1;
on-disk core/engine schemas and serde names are unchanged. Subscription envelopes
remain §1.3 snapshots, with these DTOs in `data`.

| JS | Kind | Result / rule |
|---|---|---|
| `Settings.get()` | async | Typed SettingsDto: selected theme, core-owned `themeChoices` labels/descriptions, terminal-color preferences, interface font/size, computed text face/points/scale, normalized `statusBar`, optional load diagnostic. |
| `Settings.set(patch)` | async | Typed optional camelCase fields; strict unknown-field rejection. One core store lock applies the patch and preserves unknown saved keys. Explicit choices replace unknown future values. |
| `Settings.changeTextSize(action)` | async | `bigger`, `smaller`, `reset`; read/modify/write and snapping in core under its lock; returns SettingsDto. |
| `Settings.moveItem(kind, up)`, `setSide(kind, side)`, `setVisible(kind, visible)`, `setEnabled(enabled)` | async | Core status-bar operations in one locked update, returning SettingsDto. Kinds use snake_case, side is `left` or `right`. |
| `Settings.subscribe(listener)` | sync handle | Immediate owner-bound Subscription; initialization/registration happen on background workers; shared home watcher invalidates settings. |
| `Settings.cased(text, native)`, `sentenceCase(text)` | sync | Pure core casing rules. |
| `Appearance.get()` | async | AppearanceDto for current settings (light Native baseline). |
| `Appearance.resolve(theme, systemDark)` | async | Resolve for the caller's window; never changes another window's effective appearance. Returns selected theme, palette, dark, diff colors, terminalOverride or null, and retained diagnostic. |
| `Appearance.subscribe(listener)` | sync handle | Process settings invalidations; each window resolves against its own `colorScheme()` after every snapshot. |
| `Dashboard.getGlobalWindow()` | async | CamelCase GlobalWindowDto through core GlobalWindowStore. Only P1 global-window preferences. |
| `Dashboard.saveGlobalWindow(state)` | async | Validate schema/view/filter/sort/size before saving through the same core store. No dashboard activity or chat creation. |

Appearance does not parse Ghostty configuration or write `appearance.json` in
P1. Follow Ghostty uses the RiWork palette, keeps a diagnostic and has unknown
terminal colors unless `useRiworkColors` selects the RiWork override.

W3 also supplies `LayoutSession.openPanel(panel)` and `defaultLayout()` as
synchronous in-memory edits, scheduling persistence like existing methods.
Default placement uses the saved-state successor in `riwork_core::layouts` and
preserves every known tab. A layout containing opaque future tabs refuses this
reset so their persisted pane identity cannot be lost.

#### Phase 3b Files and previews (payload schema 3)

The Files native module owns disk access; TSX receives display values and
bounded preview data. Every disk read, watcher setup and Git process runs on a
blocking worker. No window timer polls Files. The wire additions are:

| Function | Return / behavior |
|---|---|
| `Files.listDirectory(root, path, showHidden)` | async `DirectoryListingDto`; one level, folders first, natural case-insensitive ordering, 5,000 displayed / 50,000 scanned maximum. |
| `Files.subscribe(root, path, showHidden, listener)` | sync `Subscription`; schema-3 directory snapshots in the standard `{kind, rev, data, resync?}` envelope. A recursive native watcher is shared by root; only subscribed listings refresh. Linked-worktree Git index changes also wake it. Last-owner disposal releases the watcher. Setup/read failures deliver an error snapshot. |
| `Files.preview(root, path, identity?, pdfRequested?, page?)` | async `PreviewDto`; text/code, Markdown, PNG bitmap, explicitly requested PDF page, or a safe message. |
| `Files.readText(root, path, identity)` | async exact UTF-8 content, limited to 1 MiB; no preview clipping or newline conversion. |
| `Files.validateEdit(root, path, identity)` | async contained regular-file target; the selected identity must still match. |
| `Files.validateOpen(root, path, identity?)` | async contained file/folder target, refusing symbolic links, executables and launcher/bundle extensions before the system host call. |
| `Store.stageProjectMemo(projectId, text)` | sync memory-only staging, returning a revision. Host close/quit/reload flushes accepted drafts even after the JavaScript owner is destroyed. |
| `Store.setProjectMemo(projectId, text, revision?)` | async saved `ProjectMemoDto`, using the core normalized 32 KiB memo writer and Store snapshot publication. A staged write must still have the current revision, preventing late writes from overwriting a newer close save. |
| `LayoutSession.revealPanel(panelId, asked, enabled)` | sync controlled layout projection; only `files`/`preview`. Uses core main-pane and preview placement plans, preserves active pane/keyboard focus, and keeps a terminal clicked for a link visible. |

`DirectoryListingDto` contains `{schema:3, root, path, entries, omitted,
scanLimited, error?}`. Each entry contains `{name, path, relativePath, kind,
identity?, gitStatus?}`. Kinds are `directory`, `file`, `symlink`, `other`.
Identity is an opaque string encoding the engine's device/inode/length/mtime
tuple; JavaScript never rounds those integers. `gitStatus` retains Git's two
porcelain status columns, including spaces, and directories inherit a changed
descendant's decoration. Symlinks have no identity and are never expanded.

`PreviewDto` contains `{schema:3, kind, text?, lines?, truncated, markdown,
dataUrl?, mime?, width?, height?, page?, pages?, message?}`. Text is bounded to
1 MiB / 10,000 lines with 4,000-character line clipping. Images are magic-checked,
dimension/decode bounded, oriented and downscaled before PNG transport. PDF
parsing occurs only for an explicit request; the engine renders bounded bytes
into a PNG page with page count. Invalid UTF-8, binary, malformed media and
unsupported items produce messages. Markdown defaults to rendered TextView with
a source toggle; image references are placeholders and links are reported,
preventing filesystem/network reads from document content.

All Files access starts at a verified directory descriptor and walks descendants
with `openat`/`O_NOFOLLOW`; listings use descriptor-relative metadata. Relative
roots, root symlinks, parent traversal, sibling-prefix escapes, symbolic-link
ancestors and non-regular preview targets fail closed. Identity is checked
before and after reading. `Sessions.openEditor` additionally accepts optional
`identity`, which is enforced for a new Vim launch against the trusted
project/worktree root. A live editor for the same validated path is reused after
its own saves. Files changes use subscriptions; hidden surfaces release them.

`Files.fixtureDirectory(name)` is a debug/test-support acceptance seam, requiring
`RIWORK_NSH_FILES_FIXTURE=1` and an isolated temporary process home; it seeds
disposable fixtures and is refused in production release builds without test support.

#### Phase 2a terminal projection (consolidated into Appearance schema 2)

Phase 2a first published a terminal-only `Appearance.snapshot()` payload
(`schema: 1`). Phase 1 consolidation (task `c12f5e14`) keeps exactly one
`Appearance` module: the W3 schema 2 table below. Every `AppearanceDto` also
carries the terminal phase's fields:

| Field | Type | Rule |
|---|---|---|
| `terminalLight`, `terminalDark` | `TerminalOverrideDto \| null` | Core `terminal_override` resolved for the light/dark window scheme. `null` means the terminal follows Ghostty; §3.2 `colors` is then `{ source: "ghostty" }`. |
| `tabCloseBehavior` | `"ask" \| "detach" \| "exit"` | Core `tab_close_behavior`. |

The app chooses light/dark using NSH's reactive system appearance; Native
follows it. Ghostty config parsing/watching belongs to the native terminal
component. The presets preserve the desktop's sixteen ANSI colors and
terminal-color options. `riwork.terminal` schema 1 and persistence schemas are
unchanged.

The already-frozen `Sessions.create(req)` accepts
`{ projectId, worktreeId?, cwd?, command? }` for this local-shell slice; omitted
`cwd` resolves the selected worktree/project root in Rust. Decoding is strict:
unknown fields, non-canonical project/worktree UUIDs and an explicit empty
`cwd` are `invalid` before any backend work. `Sessions.close(id)`
is explicit Exit only, never ordinary surface destruction. It first hides the
shared local entry through `TabStore`; a failure refuses Exit before any tmux
operation. The existing `Tabs.update(projectId, update)` adapter serves local
shell hide/unhide (and the engine's order/title operations); the full membership
subscription remains a later port. A normal tab Detach hides membership before
removal, with legacy local detachment allowed only when the shared store is
unavailable. Reopening a detached local shell unhides it before placement. Both
Sessions mutations publish inventory
after successful mutation and never perform engine-thread I/O.

The workspace's existing `LayoutSession` gains in-memory `openShell(shellId)`
and `detachShell(shellId)` commands returning its controlled DockArea projection.
They change placement and durable detached IDs only. Creating or exiting sessions
requires the separate async `Sessions` calls. Opening an already-placed identity
selects it, and a detached identity reopens without creating another session.
An explicit reopen within the same window remembers its former pane while that
pane exists and is unlocked; otherwise it follows the main/active/unlocked
routing rule. The hint is in memory only, with no persistence-format change.

Process-wide claims stay in Rust: a local shell is shown by at most one window (native
`riwork.terminal` registry, §3.5) and a chat tab is owned by one window (`Chat.claimTab` /
`Chat.releaseTab`, today `crates/riwork-desktop/src/chat_tabs.rs:21-108`). Startup, runtime
registration, reload, the Dock menu and background services (schedules, wakes, notifications)
are host-internal and have no JS surface (§2.5).

---

## 2. AppKit host channel (per window)

### 2.1 Topology (frozen)

- The host creates one `HostChannel` per window (`nsh-engine/src/host_channel.rs:67`), passes
  it as `AppOptions::host` and keeps `WindowId (UUID v4) → (NSWindow, AppView, HostChannel)`.
  A window keeps its `WindowId` for its whole life, including project switches inside it.
- From TSX: `import { host } from "nsh"` (the 0.3 generated types export `host` from `nsh`;
  `nsh/host` failed type checking in the host spike, `WORKER_REPORT.md` “Exploratory failures”
  on `nsh-spike-host`, commit `6475d17`): `host.params`, `host.call(name, data, { timeout })`,
  `host.post(name, data)`, `host.on(name, listener) → off` (`nsh-js/src/host_channel.rs:1-16,130-178`).
  The host spike proved two AppViews with per-window `host.params.windowId` and isolated
  per-window state (`spikes/host-multiwindow/host/src/main.rs:85-106`).
- Limits that bind both sides: names are 1-128 bytes of `[A-Za-z0-9_.:-]` without `..`
  (`nsh-core/src/access.rs:815-827`); at most 1024 calls pending per app
  (`nsh-core/src/host_channel.rs:22`); events for names without a listener are kept, up to
  256 (`host_channel.rs:18`); a call times out after 30 s unless `timeout` says otherwise
  (`nsh-core/src/runtime/host.rs:23`). Host handlers run on the engine thread and must return
  at once, answering later from any thread (`nsh-engine/src/host_channel.rs:152-167`).
- Calls that take longer than 1 s (open panel, permission prompts) pass `{ timeout: 0 }`.

### 2.2 `host.params` (frozen, `schema: 1`)

Read once when the bundle starts (`host_channel.rs:110-118`); anything that changes later
arrives as an event (§2.4).

```jsonc
{
  "schema": 1,
  "windowId": "6f0c…",              // UUID v4, stable for the window's life
  "kind": "project",                // "project" | "global"   (runtime::WindowKind, crates/riwork-engine/src/runtime.rs:89-94)
  "projectId": "b1e2…",             // null for the global window; a remote project key for a remote project
  "path": "/Users/…/repo",          // RuntimeWindow.path (runtime.rs:102-104); "<home>/global-window" for the global window (runtime.rs:126)
  "restore": {                      // present only when this window is restored by `riwork restart`/reload
    "layout": { /* ProjectLayout as on disk, layouts.rs:1036 */ },
    "focusMode": false,
    "focusCentered": false,
    "global": { /* GlobalWindowState, crates/riwork-core/src/global_window.rs:165 */ }
  },
  "cascade": 0,                     // place in the opening cascade (WindowFrame::opening, layouts.rs:1104)
  "global": { "view": "dashboard", "start": false },  // global window only: what to show first
  "notice": null,                   // a startup notice the host already knows (e.g. forwarded-open failure)
  "appearance": "system",           // "light" | "dark" | "system": avoids a first-frame flash
  "build": { "version": "0.1.0", "commit": "50b3b41", "debug": false }
}
```

The restore payload is the `RuntimeWindow` the previous GUI published
(`runtime.rs:102-125`, `crates/riwork-desktop/src/main.rs:4526-4539`); the host reads it with
`restore_from_env` (`runtime.rs:530`). TSX hands `restore.layout` to
`Layouts.openSession` (§4.5) instead of loading `layouts.json`.

### 2.3 `host.call` / `host.post` names (frozen v1)

| Name | Kind | Data → Result | Notes |
|---|---|---|---|
| `window.open` | call | `{ kind, projectId?, path?, view?, start?, activate? }` → `{ windowId, created }` | Opens a project window (cascaded by the open-window count) or the global window (`view`: `dashboard` \| `chat`, `start`: start the global orchestrator). The global window is a singleton: if it exists it is focused and gets `global.show` (`created: false`). Works with no window open (Dock “New Window”, notification click). |
| `window.route` | call | `{ projectId, step }` → `{ windowId }` | Delivers `route` to the frontmost window showing `projectId`, opening one if none. `step` ∈ `showShell {shellId}`, `chat {chatId}`, `task {taskId}`, `panel {panel}`, `apiKeySettings`, `projectMemo`. |
| `window.placement` | call | `{}` → `{ mode, frame, normalFrame, visibleFrame }` | Mode `windowed` \| `maximized` \| `fullscreen` (`runtime::WindowMode`, `runtime.rs:77-84`); `normalFrame` is the unzoomed frame; `visibleFrame` the display's usable area (for `WindowFrame::opening`). |
| `window.list` | call | `{}` → `[{ windowId, kind, projectId, title, key, mode }]` | Window menus and routing. |
| `window.focus` | call | `{ windowId }` → `{}` | |
| `window.close` | call | `{ windowId? }` → `{}` | Default: this window. The host first flushes this window's `LayoutSession` (§4.5) from Rust. |
| `window.setDockInfo` | post | `{ project, branch }` | Dock menu entry for this window (today `announce_to_dock`, `main.rs:4497-4520`). |
| `app.restart` | call | `{ session?: boolean, shellId?: string }` → `{}` | `riwork restart` semantics (`runtime.rs:421-530`). |
| `app.quit` | post | `{}` | Host flushes every `LayoutSession` and publishes the runtime registry before quitting. |
| `app.restoreReady` | post | `{}` | This window finished restoring (feeds `mark_restore_ready`, `runtime.rs:553`). The host confirms the reload when all restored windows posted it. |
| `system.revealInFinder` | call | `{ path }` → `{}` | |
| `system.openWith` | call | `{ path, app? }` → `{}` | Default app or a named one. |
| `system.quickLook` | call | `{ paths, index, titles? }` → `{}` | QLPreviewPanel is AppKit-global; `paths: []` closes it. Replaces today's `qlmanage -p` child process. |
| `system.openPanel` | call | `{ files, directories, multiple, prompt?, startIn?, types? }` → `{ paths }` (empty when cancelled) | NSOpenPanel (project folder, “Files and images…”); `{ timeout: 0 }`. |
| `system.savePanel` | call | `{ directory?, suggestedName, types? }` → `{ path }` (`null` when cancelled) | NSSavePanel (plan/snippet export); `{ timeout: 0 }`. |
| `system.readPasteboard` | call | `{ maxBytes }` → `{ files: string[], image?: { type, path }, text? }` | Typed NSPasteboard read on the main thread with today's priorities (file URLs and `NSFilenamesPboardType`, then PNG > JPEG > TIFF with byte caps, then text); an image is written to a temp file and its path returned. `nsh/clipboard` alone is too coarse (parity CHATC attachments). |
| `system.notify` | call | `{ id, title, body, projectId?, target?, actions? }` → `{}` | `actions: [{ id, title }]` (e.g. `open` / “Open agent”). Needs the app bundle identity `dev.riwork.shell`. A click or action raises `notification.activate` in the right window (the host routes like `window.route`). |
| `system.share` | call | `{ paths, anchor? }` → `{}` | NSSharingServicePicker for files (file preview “Share”). |
| `system.fonts` | call | `{}` → `{ families }` | Installed font families (SF Mono detection). |
| `app.setAppearance` | call | `{ appearance: "light" \| "dark" \| "system" }` → `{}` | NSApp appearance. |
| `system.requestPermission` | call | `{ kind: "microphone" \| "speech" \| "notifications" }` → `{ granted }` | `{ timeout: 0 }`. |
| `system.openUrl` | — | — | Not a host call. In-app targets (files, folders, previews, Vim) are routed by TSX before anything is opened. What reaches NSH's `openUrl` (and TextView links) goes through the host's `HostChannel::handle_open_url` (`nsh-engine/src/host_channel.rs:189-200`), which opens `http`, `https`, `mailto` and `x-apple.systempreferences:` with the system and declines everything else (parity TERM links, CHATC dictation). |

Reserved names (not served in v1, because GPUI has no such behavior: a tab dragged to another
project's window is refused): `tab.transfer` (call) and `tab.receive` (event).

Window-local chrome stays on NSH built-ins: `Window.setTitle`, `setTitleBar`, `setFrame`,
`toggleFullScreen`, `minimize`, `close` (own window only), `Menu.popup`, `nsh/keymap`,
`nsh/clipboard`, `onFileDrop`.

### 2.4 `host.on` events (frozen v1)

| Event | Data | When |
|---|---|---|
| `command` | `{ id, args? }` | An app-menu item, Dock-menu item or global shortcut chose command `id` while this window is key. The **application menu belongs to the host** (it is process-global on macOS); TSX does not call `Menu.setApplicationMenu`. The v1 menu keeps GPUI's RiWork, Edit and View menus and ⌘W closing a tab, never a window (parity SHELL “menus”); adding AppKit's Window/Help menus is a separate product decision. Command ids are the menu items of parity rows SHELL-250 to SHELL-252 and SHELL-299 and the command list under SHELL “Host needs” in `docs/nsh-parity.md`. **Edit menu:** Cut/Copy/Paste/Select All use the standard responder selectors; a native text field or a `riwork.terminal` handles them itself, otherwise the AppView forwards them as `command { id: "copy" }` (etc.) so the transcript can copy its Rust-built selection (parity CHATT “Edit ▸ Copy”). **Key-equivalent order:** when a `riwork.terminal` view is first responder, ⌘=, ⌘- and ⌘0 go to the terminal (Ghostty font size) and are not also dispatched as text-size commands. |
| `window.state` | `{ key, visible, occluded, mode, backingScale, frame, appActive }` | On change. Pauses sampling/metrics subscriptions and idle redraws in occluded or inactive windows (parity SHELL “activation”); terminals read occlusion natively. |
| `window.willClose` | `{ reason: "user" \| "quit" \| "restart" }` | Informational; persistence does not depend on TSX answering (the host flushes Rust state itself). |
| `window.closeCancelled` | `{}` | A close, quit or restart failed and the host reopened all affected native layout gates. Quit/restart failure bookkeeping finishes first; one main-thread transition clears the host quit gate, reopens native gates, then notifies surviving Apps, so their one-shot `app.restoreReady` can be accepted. Sent to every surviving window in the cancelled batch, including one whose pending `Layouts.openSession` was rejected by the closing marker. App retries that rejection with at most one open in flight and ignores cancellation once a session is installed. Delivery can precede the rejection reaching JavaScript. Successful teardown never sends this event. |
| `project.open` | `{ projectId, path? }` | `riwork open PROJECT` (possibly forwarded from a second launch) targeted this window. |
| `route` | `{ step }` | `window.route` chose this window. |
| `global.show` | `{ view, start }` | Global window only: show Dashboard or Chat. |
| `notification.activate` | `{ id, target? }` | The user clicked a notification for this window. |
| `appearance` | `{ appearance }` | The macOS or forced appearance changed (the resolved RiWork theme comes from `Appearance.subscribe`, §1.10). |

### 2.5 What the host owns that TSX never sees

- Runtime registration and heartbeat (`runtime.rs:314`, `HEARTBEAT` 30 s at `runtime.rs:33`),
  publishing windows for reload (`runtime.rs:706`). The host builds each `RuntimeWindow` from
  the window's `LayoutSession` (Rust, in-process) and its NSWindow frame/mode, so no JS round
  trip is needed at quit or restart.
- Reload choreography (`riwork restart`/`reload`): replacement launch, confirmation, watchdog
  (`runtime.rs:1-60`). Before launching the replacement the host flushes chat drafts in Rust
  (`Drafts.flush` with a 5 s limit; today `main.rs:13975-13980`) and every `LayoutSession`, and
  cancels the reload if the draft flush fails.
- Quit: the host flushes drafts (newest draft per chat, up to 3 retries 250 ms apart; today
  `crates/riwork-desktop/src/chat_drafts.rs:546,709`) and every `LayoutSession` from Rust before
  terminating; no TSX hook is involved.
- App menu, Dock menu (`crates/riwork-desktop/src/dock_menu.rs`; activating a window and
  “New Window” are handled in the host), single-instance lock and forwarding (§0),
  `riwork open` routing, quit when the last window closes.
- Window chrome set at creation: transparent title bar with full-size content, traffic lights
  at (10, 8), minimum size 640 × 400, title-bar double-click honouring the system
  zoom/minimize preference (parity SHELL “window chrome”). TSX marks its own drag regions with
  `desktop={{ windowDrag: true }}` and lays out under `titleBar()`.

---

## 3. `riwork.terminal` native component

Behavior references in this section: `T*`, `Z*`, `R*`, `I*`, `K*`, `H*` are the rows of the
sibling spec `docs/nsh-spec-terminal-layout.md` (branch `nsh-spec-terminal-layout`,
commit `a3bc62d`); `TERM-*` rows are in `docs/nsh-parity.md`.

### 3.1 Registration and use (frozen)

- The host registers the component once on the shared registry before the first AppView:
  `components.register("riwork.terminal", factory)` (`nsh-platform-macos/src/native.rs:192-200`).
  The factory returns a `NativeComponent` (`native.rs:100-126`) whose `view()` is the NSView
  that *is* the libghostty surface view: first responder, `NSTextInputClient`, mouse and drag
  destination. (Today the gpui-libghostty shim's view returns `nil` from `hitTest:` and GPUI
  routes input; that cannot carry over.) The ghostty spike (db4930af,
  `spikes/ghostty-nsh/WORKER_REPORT.md` on branch `nsh-spike-ghostty`, commit `018a535`) built
  this shape without GPUI: AppView hosting, tmux attach, keyboard, dead-key IME, mouse, resize,
  live theme and retained detach/reattach passed; **teardown failed** (§3.7).
- TSX declares it once:

```ts
import { nativeComponent } from "nsh/native";
export const Terminal = nativeComponent<TerminalProps, TerminalEvents>("riwork.terminal", {
  props: ["target", "visible", "pinned", "focusOnAttach", "colors", "frozen"],
  events: ["state", "focusChange", "linkOpen", "drop", "inputRefused", "findOpen", "findResult"],
});
// <Terminal ref={term} target={{ kind: "local", shellId }} visible … />; term.command("focus")
```

- NSH 0.3 toolchain compatibility: its generated declarations export
  `nativeComponent` from `nsh` (the `nsh/native` runtime alias is marked moved).
  The production TSX imports it from `nsh`; the component name and frozen
  props/events/commands above are unchanged.
- Props arrive as JSON on change, all of them right after creation (`native.rs:104-106`);
  commands arrive through `ref.command(name, args)` (`native.rs:108-111`); events leave with
  `ComponentContext::emit` (`native.rs:145-150`) from any thread.
- **Keystrokes, IME, mouse, scroll, selection, link hover and underline, drag feedback and
  rendering never cross into JS.** They stay inside the NSView and its Rust side, which calls
  riwork-engine directly (same process). JS sees only the props, commands and events below.
- v1 freezes what GPUI parity needs. GPUI handles only Ghostty's render action today
  (title, working directory and activity come from tmux sampling through `Sessions.subscribe`;
  there is no bell, OSC 9/777 notification or scroll command). Those names remain
  **reserved** (§3.3, §3.4). Phase 2b adds native terminal Find below; it does not
  send scrollback text, search matches or per-frame rendering through JavaScript.

### 3.2 Props (frozen, `schema: 1`)

| Prop | Type | Meaning |
|---|---|---|
| `target` | `{ kind: "local", shellId } \| { kind: "remote", desktopId, shellId }` (required) | What to attach. Local: a tmux client of the shell's session on RiWork's isolated socket via `SessionManager::attach_exec_command` (`crates/riwork-engine/src/sessions.rs:1050`), default attach flags (T1); `attach_argv` refuses a dead session, so attaching never creates one (`sessions.rs:1014-1018`). Remote: the remote bridge for the `(desktopId, shellId)` pair, never confused with a local id (R1, T4). Changing `target` detaches the old client and attaches the new one in the same view. |
| `visible` | `boolean` | The tab is the selected tab of a shown pane (in focus mode only the active pane's, T2). Only visible views attach (lazy attach, T4); hidden views stop rendering and follow the release policy (§3.5). |
| `pinned` | `boolean` | Keep the surface (its last screen) even when hidden: set by the app for a local tab whose session is dead or unknown and a remote tab whose bridge ended (T4). Never released. |
| `focusOnAttach` | `boolean` | Take first responder once attached: true for a tab the user just opened, false for restored, re-shown or re-attached tabs (H3). |
| `colors` | `{ source: "ghostty" } \| { source: "override", background, foreground, palette: string[16] }` | Resolved by the app from the RiWork theme (H1; today `Appearance::terminal_override`, `crates/riwork-desktop/src/theme.rs:205-216`). An override change applies in place (H3). Switching from `override` to `ghostty`, or a change of Ghostty's own parsed colors while following it, **re-creates the attach client** for the same target, hidden until ready, then swapped in (H3; today `main.rs:2753-2789`); never a new shell. Fonts, padding and the rest come from the user's Ghostty config, loaded and watched once per process (H2, H4). |
| `frozen` | `boolean` | Show a snapshot of the last presented frame instead of the live surface and take no input. GPUI freezes every shown terminal while a tab drag, menu, popover, modal or notice is open because native views draw above GPUI content (T6, parity SHELL-112). The NSView must be a layer-backed subview inside the AppView's layer tree so NSH overlays (Dialog, Popover, Sheet, Tooltip, drag preview) draw above it; if a parity check shows they do not, the app sets `frozen` during overlays exactly as GPUI does. |

No size, scale, focus or lease props. The view sizes itself from its NSH frame and window
backing scale and fixes its layer `contentsScale` on display changes (today
`crates/riwork-desktop/src/metal_layer.rs:62-125`). Desktop terminals attach as ordinary tmux
clients and **never take viewport leases**: leases are the phone/CLI path (`resize_viewport` is
called only from `crates/riwork-cli/src/cli.rs`), and a desktop view shows what tmux gives it
while another client holds one (Z1-Z3).

### 3.3 Events (frozen, `schema: 1`)

| Event (`on…`) | Data | Meaning |
|---|---|---|
| `state` | `{ state, message?, attempts? }`, `state` ∈ `detached`, `attaching`, `attached`, `failed`, `claimed`, `reconnecting`, `ended`, `gone` | Attach lifecycle, pushed (GPUI polls a remote `is_alive` every 2 s). `failed` after 3 attempts 2 s apart per reset cycle (T5); `claimed` = the local shell is shown by another window (T4); `reconnecting`/`ended` drive the remote strip “RECONNECTING · HOST” / “Ended on HOST” (R2); `gone` = the local tmux session no longer exists. A transient tmux query error is never `gone` (T8). |
| `focusChange` | `{ focused }` | First-responder change; the app makes this pane the active pane. |
| `linkOpen` | `{ action: "open" \| "edit", kind: "url" \| "file" \| "folder", target, line?, column? }` | Local shells only (remote tabs have no link layer). A resolved ⌘-click (`open`) or ⌘⇧-click (`edit`) (K1). The native side detects, underlines and resolves links from tmux captures with riwork-engine's `terminal_links` (K2-K6, `SessionManager::capture_link_view`, `sessions.rs:1114`) and consumes the click; the **app** decides where it opens (Files, Preview, Vim, Finder, browser: K7-K10). |
| `drop` | `{ paths, images, accepted, reason? }` | Files or an image were dropped, or pasted with ⌘V, onto a local shell. The native side already applied the clipboard priority, unsafe-path refusal, escaping and routing by foreground program through tmux (I2-I7: `SessionManager::paste` / `paste_files`, `sessions.rs:1860,1879`, under the per-shell input lock). `accepted` tells the app to select this pane (I3). |
| `inputRefused` | `{ reason }` | Input was refused (tmux input off, copy-mode rules, I7) so the app can show its notice. |
| `findOpen` | `{ query }` | Phase 2b: a Ghostty search binding requests the app's Find bar. Show/focus the bar without starting a second search. |
| `findResult` | `{ total, current }` | Phase 2b: Ghostty search match count and 1-based selected match; zero current means none selected. No captured terminal text crosses the bridge. |

Reserved, not emitted in v1: `resize`, `title`, `cwd`, `bell`, `notify`, `selectionChange`.

### 3.4 Commands (frozen, `schema: 1`)

| Command | Args | Effect |
|---|---|---|
| `focus` / `blur` | — | First responder. `focus` attaches a missing client first (T5). |
| `reattach` | — | Drop the current attach client or remote bridge surface and attach the same target again: remote Reconnect (R2) and Retry after `failed` (T5). Never creates a session. |
| `copy`, `paste`, `selectAll` | — | Programmatic equivalents of the view's own `copy:`, `paste:` and `selectAll:` responder actions, for context menus. |
| `find` | `{ query: string }` | Phase 2b: search through Ghostty's native search action; updates the current query in the same surface. Empty query clears results. |
| `findNext`, `findPrevious` | — | Phase 2b: advance the selected native match. |
| `findClose` | — | Phase 2b: end the native search and restore terminal focus. |

Reserved, not accepted in v1: `scroll`, `reset`, `sendText`, `sendKeys`,
`snapshot`. Typing into a session from app logic (Vim jump to line, chat “send to terminal”,
upload-inbox pastes) goes through `Sessions.send*`/`paste*` (§1.7), as it does today.

Key handling: the view receives keys as first responder. The host menu's Edit items use the
standard responder selectors, which the view implements (Phase 2b ⌘V routes files,
images and text through the engine's locked paste path); ⌘=, ⌘-, ⌘0 reach Ghostty's font-size bindings and are not
also dispatched as RiWork text-size commands (§2.4). Phase 2b routes ⌘F to terminal
Find when a local terminal tab is active and to its panel's search otherwise;
⌘G / ⌘⇧G advance terminal matches. Nested input editors keep normal edit-action priority.
OSC 52: writes of plain text to the pasteboard are allowed; clipboard reads and any write that
asks for confirmation are denied (today's shim policy).

Phase 2b app routes are `route.step {kind: "newSubshell", parentShellId?}`
(omitted parent uses the selected local shell), and `route.step {kind: "openEditor",
path, line?, worktreeId?}`. Window commands `newSubshell` and `openVim` expose the
same paths in menus; `openVim` opens the file-path dialog. `LayoutSession.openEditorShell(id)`
places the returned existing Vim session using V4 routing: existing tab, main
outside focus, unlocked terminal pane, any unlocked pane, or a new side-by-side
unlocked pane when all are locked. Vim's own write/save behavior remains engine-owned.
Remote-Mac tabs remain Phase 6 and are excluded from these Phase 2b routes.

Phase 2b image paste/drop extends the earlier I6 clipboard behavior: a lone
picture becomes a private PNG below `RIWORK_HOME/tmp/terminal-images` and its
escaped path uses the same foreground-program routing as file attachments.
The tab owns this file across hidden-surface release; failed delivery, final
tab disposal and proven dead-owner startup cleanup remove it. Native Cocoa
file URLs use decomposed filesystem Unicode spelling; the escaped path still
addresses the original file. Ordinary text keeps the engine's tmux newline
and bracketed-paste semantics, without an added Return.

### 3.5 Lifecycle invariants (frozen)

1. **The view never creates, respawns or kills a session.** Sessions come from
   `Sessions.create*` and end through the app's close flow (Ask/Detach/Exit, T7). The one
   respawn path (Codex reload, T9) is a `Sessions`/host operation, not the view's.
2. **Release = detach.** Dropping the component or releasing a hidden view detaches the
   client; the tmux session and scrollback stay (T1). The release policy is implemented
   natively per window, from `visible`/`pinned` and the window's occlusion: three most
   recently visible hidden views stay warm 10 min, others get 30 s grace, at most two
   releases per pass, rechecks ≥ 250 ms, frozen windows release none (T2-T3,
   `crates/riwork-desktop/src/terminal_lifecycle.rs:17-34`). A pass is scheduled 250 ms after
   any shown/hidden change, replacing the pending one.
3. **Moves keep the view.** DockArea keeps a panel's rendered content while any element places
   it (`nsh-base/src/dock/dock_area.rs:28-33`), so a tab moved between panes keeps the same
   component instance, NSView and tmux client (T6). This depends on NSH re-parenting a native
   component's view without dropping it; the ghostty spike's retained-view detach/reattach kept
   the same surface and client, but a DockArea move must be checked on its own (parity gate).
   A move to another window
   is not a GPUI behavior (a drag to another project's window is refused); if the reserved
   `tab.transfer` (§2.3) is ever served it relinquishes the local shell claim in the source and attaches in the
   destination. Claims are a process-wide registry on the native side (T4).
4. **Ended surfaces stay.** An ended or unknown local session keeps its last surface; a remote
   surface stays after its bridge exits (T4).
5. **Remote recovery** opens a fresh stream for the same shell at the latest size, reports
   `reconnecting` then `attached` even when instant, and discards outage keys (R3).
6. **Restart** (`riwork restart`): the new process attaches only the views that are shown in
   the restored layout; nothing is recreated (T4, P6).

### 3.6 Other native components (reserved)

`riwork.preview`: the app-preview frame view (CGImage/CALayer, aspect-fit, mouse capture
including up-outside, `NSTextInputClient`; parity TERM app-preview rows). The name is reserved;
its props/events/commands are frozen by the app-preview port under this document's rules
(§3.1: no per-frame data through JS).

### 3.7 Teardown protocol (frozen; from the ghostty spike NO-GO)

The spike's close path blocked the AppKit main thread in `ghostty_surface_free` →
`pthread_join` for more than 47 s until the tmux client was detached externally
(`spikes/ghostty-nsh/WORKER_REPORT.md:11-18,92-123` on `nsh-spike-ghostty`; upstream
ghostty issue #14245). Every implementation of `riwork.terminal`, including a GPUI-hosted
fallback, must therefore:

1. On drop/release, first end the attach client from outside the surface: `tmux detach-client`
   for that client's tty on RiWork's socket (local) or close the bridge stream (remote), off
   the main thread.
2. Free the libghostty surface only after its PTY child has exited; never call a blocking free
   on the main thread while the child may be alive.
3. If the child has not exited within 2 s, park the surface (hidden, off-screen, no input) and
   retry the free off the critical path; the main thread never waits.
4. Pass the active-tmux disposal test before this component ships: create N ≥ 4 surfaces under
   output pressure, close tab / pane / window / quit / restart, assert the main thread never
   stalls > 100 ms and every tmux session survives with zero leaked clients.

This keeps the frozen props/events/commands unchanged whichever runtime passes the gate.


---

## 4. Layout contract: `DockAreaLayout` ⇄ riwork-core layouts

### 4.1 The two models

**RiWork (persisted, unchanged):** `RIWORK_HOME/layouts.json`, schema 1
(`layouts.rs:24`), guarded by `layouts.lock` (`layouts.rs:1979-1989`), atomic temp+rename
writes (`layouts.rs:2033-2064`), entries per project id:

- `ProjectLayout` (`layouts.rs:1036-1069`): binary split tree `layout: Layout`
  (`Pane(PaneId)` | `Split { axis: side_by_side|stacked, ratio, first, second }`,
  `layouts.rs:35-44`), `panes: BTreeMap<PaneId, SavedPane>`, `active_pane`, `locked_panes`
  (`None` = automatic left-navigation lock, `Some(∅)` = explicit no lock), `main_pane`,
  `sidebar_visible`, `window_size`, `selected_worktree_id`, `selected_task_id`,
  `detached_shell_ids`, `detached_chat_ids`, `chat_dismissal_revision`, `panels_initialized`.
- `SavedPane` (`layouts.rs:1022-1030`): ordered `tabs: SavedTab[]`, `active_tab_key`,
  `tab_edge` (always rewritten to `top`, `layouts.rs:1319-1320`), legacy `shell_ids` /
  `active_shell_id` kept in sync.
- `SavedTab` (`layouts.rs:972-996`) with key `SavedTab::key()` (`layouts.rs:998-1011`):
  `shell:<id>`, `panel:<name>`, `remote:<desktop>:<shell>`, `chat:<id>`,
  `app_preview:<uuid>`.
- Invariants enforced by `ProjectLayout::normalize` (`layouts.rs:1298-1392`): unique pane ids
  (0 repaired, ≥ `u64::MAX-256` rejected), each shell / panel kind / remote shell / chat /
  preview at most once per layout, **empty panes allowed**, active tab repaired to the first
  tab, ratio clamped to [0.1, 0.9] when dragged (`layouts.rs:228-234`) but kept in
  [0.02, 0.98] on load (`layouts.rs:236-240`).
- Forward compatibility: tabs of unknown kinds are skipped per pane and written back after the
  pane's own tabs while that pane exists (`layouts.rs:1562-1625,1676-1700,1889-1892`); an
  unparsable project entry is kept verbatim and never overwritten (`layouts.rs:1596-1599,1877-1879`); a
  newer `schema_version` is neither loaded nor overwritten (`layouts.rs:1736-1743`); a corrupt
  file is set aside as `layouts.corrupt-<secs>-<id>.json` on the next save (`layouts.rs:2015-2031`).
- Saves merge chat dismissals under the lock (`save_chat_state`, `layouts.rs:1813-1903`) or
  write the shared-tabs projection (`save_tab_layout`, `layouts.rs:1824-1832`); unchanged
  layouts are not rewritten (`layouts.rs:1880-1883`). Shared membership itself lives in
  `RIWORK_HOME/project-tabs/<project>.json` and is never overwritten by a layout
  (`docs/shared-tabs.md:3`).
- The global window has no pane tree: `RIWORK_HOME/global-window.json` (`global_window.rs:19,165-176`).
  It does not use `DockArea`.

**NSH (in memory, per window):** `DockAreaLayout { center: DockNode, left?, right?, bottom? }`
(`nsh-base/src/dock/value.rs:282-294`), `DockNode = split { id?, axis, children, sizes } |
tabs { id?, panels: string[], activeIndex }` (`value.rs:31-54`), sizes in points with
`null` = unconstrained. `DockArea` is controlled: it reports every edit as the whole next
layout through `onLayoutChange` (`nsh-base/src/dock/dock_area.rs:358-364`), and installing a
layout normalizes it: empty containers removed, one-child splits collapsed (the child keeps
its id and inherits the slot size), **same-axis nested splits spliced into one n-ary split**,
`activeIndex` clamped (`nsh-base/src/dock/layout/normalize.rs:24-35`). Node ids come from one
process-wide counter; ids carried in by a layout are adopted and later allocations start above
them (`nsh-base/src/dock/layout/tree.rs:25-46`). Sizes follow the resizable group's rule: sized
children keep their basis and do not grow while any sibling is unsized, unsized siblings share
the rest equally; when every child is sized, extra room is shared in proportion to the bases
(`nsh-base/src/resizable/mod.rs:37-51`, `resizable/panel.rs:597-627`).

### 4.2 Ownership (frozen)

- **`ProjectLayout` is the source of truth**; `DockAreaLayout` is a projection of it.
  `layouts.json` keeps schema 1 and is written only by riwork-core's `LayoutStore`, so GPUI and
  NSH builds can open the same RIWORK_HOME alternately without loss.
- Each window has one `LayoutSession` (Rust class, `riwork/native`), holding the
  `ProjectLayout`, the skipped tabs, the last projected `DockAreaLayout` and its extent.
- TSX passes the `DockAreaLayout` the session returned as the controlled `layout` prop and
  hands **every** `onLayoutChange` to the session; it never installs a reported layout
  directly and never edits the layout itself.

### 4.3 Projection: ProjectLayout → DockAreaLayout (frozen rules)

`LayoutSession.dock(extent) → DockAreaLayout` (sync; `extent` is the DockArea's own size in
points from its container's `onLayout`):

1. **Only `center` is used.** `left`, `right`, `bottom` are always absent in v1. The left
   navigation region stays what it is in RiWork: a locked pane inside the tree.
2. **Panes → tab groups.** `Layout::Pane(id)` → `{ type: "tabs", id, panels, activeIndex }`
   with `id = PaneId` (the PaneId *is* the group's NodeId), `panels` = the pane's tab keys in
   the order the GPUI strip shows them, `activeIndex` = index of `active_tab_key`. With shared
   membership available, that order is panels first in saved order, then sessions by shared
   order, then unlisted carried views, and hidden shared entries are not projected (spec L5,
   P7); otherwise it is `SavedPane.tabs` order.
3. **Empty panes keep their place** with the sentinel panel `empty:<paneId>` (NSH would
   otherwise delete the empty group). A pane that holds skipped unknown-kind tabs also gets
   the sentinel, so the pane, and with it the skipped tabs, survives every edit. The sentinel's
   `renderTab` renders nothing (it cannot be grabbed); its `renderPanel` is the empty-pane view.
4. **Splits → n-ary splits.** `Split { axis, ratio, first, second }` → `{ type: "split", axis:
   side_by_side ⇒ "horizontal", stacked ⇒ "vertical", children: [first, second] }`; same-axis
   chains are flattened up front (the same splice NSH would do), so the projection is already
   normalized and NSH changes nothing on install. Split nodes carry no id (NSH assigns them).
5. **Sizes** come from riwork-core's own geometry: `Layout::pane_extents(extent)`
   (`layouts.rs:545-570`), including `DIVIDER_THICKNESS` 5 px (`layouts.rs:252`) of the
   split's own length per divider. Every child gets its size in points, except: in a split
   whose children include at least one *holding* child (a locked pane, or a subtree that holds
   by `preserve_locked`'s rule, `layouts.rs:296-317`) and exactly one non-holding child, that
   one child gets `null`, so NSH's flex sharing reproduces “locked panes keep their pixels,
   the other absorbs” during a live window resize.
6. **Panel options** (`panels` prop): every real panel `{ closable: false, zoomable: false }`
   (closing goes through the app's close flow and its Detach/Exit prompt, not DockArea's own
   close); sentinels additionally `{ visible: true }`.
7. **Ids.** A `PaneId` above `Number.MAX_SAFE_INTEGER` is remapped (§4.6) before projection.

### 4.4 Edits: DockAreaLayout → ProjectLayout (frozen rules)

`LayoutSession.applyDock(next, extent) → DockAreaLayout` (sync, in-memory; persistence is
scheduled, §4.5). The session diffs `next` against the layout it last projected and classifies
the single gesture DockArea reports (one gesture per `onLayoutChange`):

| Gesture (diff) | Applied to `ProjectLayout` as |
|---|---|
| Only an `activeIndex` changed | that pane's `active_tab_key`; `active_pane` = that pane. |
| Only `sizes` changed | ratios recomputed: for each binary `Split`, `ratio = along(first) / (along(first) + along(second))`, where `along(x)` sums the reported sizes of the flattened children `x` covers (unsized children get an equal share of what is left of `extent`). |
| Panels reordered inside one group | that pane's `tabs` order; with shared membership, also the shared `Move` (leftward lands before the target, rightward after; spec L6) through `Tabs.update`. |
| A panel moved to another existing group | tab removed from the source pane, inserted in the target pane at the reported index, selected (DockTarget default `activate: true`, `dock_area.rs:206-227`). |
| A panel moved to a group edge (a new tabs group appears beside group `X`) | `Layout::split_with_ratio(X, axis, new, new_first, ratio)` (`layouts.rs:62-91`) with `new` = the new group's NodeId, `new_first` for a left/top drop, `ratio` from the reported sizes; then the tab move. |
| A group disappeared (its last panel left) | as GPUI's in-project drag: the emptied source pane is removed unless it is the main pane, and kept (empty) when removing it would enlarge a locked sibling subtree (spec L4, L9; `Layout::removal_resizes_locked`, `layouts.rs:856`). A kept pane comes back with its sentinel. |
| A sentinel ended up next to real panels | the sentinel is dropped; the pane is no longer empty. |
| Anything else (more than one of the above, or docks appeared) | **fallback**: the tree is rebuilt from `next` by right-nesting each n-ary split (`[a,b,c]` → `Split(a, Split(b, c))`, ratios from sizes), pane metadata kept by PaneId, a `layout.fallback` diagnostic logged. |

After applying, the session runs `ProjectLayout::normalize`, re-projects with §4.3 and
returns the result; TSX installs that. Edits made by app logic (open a panel, reveal a preview,
gather tabs into the main pane, close a tab) are session methods that run riwork-core's
placement code (`plan_main_reveal` `layouts.rs:813`, `reuse_open_tab` `layouts.rs:794`,
`preview_placement` `layouts.rs:590`, `beside_placement` `layouts.rs:626`, `gather_sources`
`layouts.rs:846`) and return the next projection; they never go through DockArea's ref methods.

A window resize is an edit too: `LayoutSession.setExtent(extent)` runs
`Layout::preserve_locked(old, new, locked, MIN_PANE_EXTENT)` (`layouts.rs:309-317`,
`MIN_PANE_EXTENT` 100 px at `layouts.rs:255`) and re-projects, so after the live resize settles
the sizes equal GPUI's for the same extent. Rule 5 of §4.3 makes the frames during the live
resize already match in the common case (one absorber).

### 4.5 Sessions, persistence and reopening RIWORK_HOME layouts (frozen)

- `Layouts.openSession({ windowId, projectId, restore? }) → Promise<LayoutSession>`:
  1. with `restore` (from `host.params.restore.layout`): use it (it is the snapshot the previous
     GUI published, `main.rs:4421-4456`);
  2. else `LayoutStore::load(projectId)` (`layouts.rs:1782-1793`), which normalizes, skips
     unknown tabs and reports unreadable entries as an error;
  3. on `None` or an error: the default layout the GPUI build makes (a locked left navigation
     pane with Projects, Files, Worktrees, Tasks, Sessions beside an unlocked main pane, ratio
     0.27; spec L10), and the error is shown as the window notice, **without** touching the
     stored entry (an unreadable entry is never overwritten, `layouts.rs:1877-1879`).
  4. reconcile with live state exactly as the GPUI restore does: shared-tabs membership
     (`Tabs`), missing/dead shells, unreachable remote shells, chats to subscribe again
     (`chat_open_requests`, `layouts.rs:1932`) — parity rows SHELL “restore”.
- `LayoutSession.switchProject(projectId)`: loads the destination and applies
  `carry_locked_regions_from(previous)` (`layouts.rs:1149-1296`), as GPUI does when a window
  switches project.
- W2 window-command adapters: `openPanel(panel)`, `splitActive(down)`,
  `closeActive(wholePane)` and `defaultLayout()` return the controlled DockArea
  projection after a Rust-owned model edit. `setFocusMode(focused, centered)`
  updates the Rust host's runtime-only focus flags; it does not alter storage
  schema or rewrite the underlying pane layout.
- Persistence: after each edit the session schedules a save on the blocking pool, trailing
  **250 ms**, through `save_chat_state` or `save_tab_layout` (whichever the GPUI window would
  use, `main.rs:4458-4485`). It flushes synchronously (from Rust, no JS) on `window.close`,
  `app.quit`, `app.restart`, project switch and session drop. Unchanged layouts are not
  rewritten (riwork-core already skips them).
- The host reads the session's current `ProjectLayout` directly (same process) when it
  publishes `RuntimeWindow`s for reload.
- The native `host_flush::flush_blocking()` hook is invoked on the host service
  thread before close/quit/restart teardown. Deferred draft writers can register
  barriers; failure keeps the old windows and cancels the reload before launch.
- The window's ordinary size (`ProjectLayout.window_size`) is captured as GPUI does: ordinary,
  non-zoomed, non-fullscreen frames only, debounced 250 ms, gated by `remember_window_size`
  (spec P10); the host reports frames to the session from Rust.
- Two windows on the same project behave as in GPUI (same file entry, last writer wins except
  for chat dismissals, which merge by revision, `layouts.rs:1853-1874`).

### 4.6 Ids and limits (frozen)

- `PaneId` ⇔ tab-group `NodeId`. Ids installed from RiWork are adopted by NSH's counter, so
  groups NSH creates later get fresh, larger ids that become new `PaneId`s.
- PaneIds `> 2^53 − 1` (JS-unsafe) are remapped on load to the smallest free ids; `active_pane`,
  `main_pane`, `locked_panes` and skipped tabs follow the mapping; the remap is persisted on
  the next save. riwork-core's limits still apply: depth 32, 256 panes (`layouts.rs:20-21`).
- Panel ids are `SavedTab::key()` strings, `empty:<paneId>`, and `chat-pending:<uuid>` for a
  chat tab created before the chat host returned its id (normalize drops chat tabs without an
  id, `layouts.rs:1351-1352`). **A panel id never changes while the window session lives**:
  when the chat id arrives the session maps `chat-pending:<uuid>` to `SavedTab::Chat { chat_id }`
  internally (persisted as `chat:<id>`), so DockArea never remounts the chat view;
  `LayoutSession.panel(panelId)` returns the `SavedTab` behind any panel id. The next session
  (reload) uses `chat:<id>`.

### 4.7 Round-trip oracle (frozen)

`project(ProjectLayout)` followed by `applyDock(unchanged)` must return a `ProjectLayout`
equal to the input after `normalize`, for every fixture in the riwork-core layout tests,
including: `nested_layout_round_trip_preserves_tabs_selections_and_detachment`
(`layouts.rs:2162`), `split_ratios_round_trip_and_survive_insertions_and_collapses` (`2415`),
`ratios_a_resize_leaves_on_narrow_locked_panes_survive_a_restart` (`2460`),
`left_navigation_locks_by_default_but_explicit_unlock_survives_round_trip` (`2533`),
`surplus_unlocked_slots_remain_empty_and_all_locked_keeps_sessions_recoverable` (`2712`),
`tabs_of_an_unknown_kind_are_skipped_and_the_rest_of_the_layout_still_loads` (`2836`),
`app_preview_launch_settings_round_trip_and_deduplicate_by_identity` (`2917`),
`remote_shell_tabs_round_trip_and_are_not_this_macs_shells` (`2989`),
`the_main_pane_is_kept_per_project_and_comes_back_with_its_layout` (`3194`),
`newer_schema_is_neither_loaded_nor_overwritten` (`3408`), and the `preserve_locked` resize
tests (`3547-3742`). Ratios compare within 1e-4; a binary shape changed by flattening must give
the same `pane_extents` at three extents (1280×800, 1920×1080, 640×400) within 0.5 px.
Plus one NSH-side test per gesture row of §4.4 (DockArea driven headless by `nsh test` or by
`PaneTree` edits in Rust).

---

## 5. Cross-cutting rules (frozen)

- **Threads:** riwork-core/engine calls never run on an engine thread or the main thread;
  AppKit objects are touched only on the main thread (host, native component).
- **Perf budgets:** the GPUI baselines in [`docs/nsh-parity.md`, Perf baselines](nsh-parity.md#perf-baselines-to-beat-p-docs) are the floor, except the guard that already fails on GPUI (parity “Doc/code disagreements” #10), whose NSH target is set explicitly.
  Chat streaming must not re-send whole messages (§1.6); no window may run per-frame work while
  idle (§1.3).
- **Security:** no `fs`, `process`, `net` capabilities in `nsh.json`; host names are an allow
  list (§2.3); the bridge validates every id from JS with the same validators the CLI uses
  (`project_tabs::valid_key`, `project_tabs.rs:119`, UUID parsing).
- **Compatibility:** every file under RIWORK_HOME keeps its format; GPUI and NSH builds may run
  against the same home one after the other. Running both at once is out of scope for v1
  (single-instance per home is the host's job).

## 6. Open items and decisions outside the frozen surface

| Item | Owner | Affects |
|---|---|---|
| Two AppViews, per-window `host.params`, `nsh check`/`nsh test` headless with a Rust UI set | host spike 2a537a8b: **GO** (`6475d17`) | confirms §0, §1.4, §2 |
| Dev hot reload in the custom host remounts both windows and resets their state; NSH 0.3's state-preserving dev session is private | host spike 2a537a8b | developer experience only; needs an NSH embedding API (upstream request). Not a contract change. |
| Chat streaming cost through List + `TextView streaming` vs GPUI: 475-chunk long answer, two windows, surface p95 3.0-4.7 ms, no retained frame > 16.67 ms; no matched GPUI comparison yet | host spike 2a537a8b | §1.3/§1.6 coalescing (≤ 1 patch per 16 ms per chat); a matched benchmark is a parity port gate |
| libghostty without GPUI | ghostty spike db4930af: **NO-GO on teardown** (`018a535`), all other gates passed or synthetic-only (display-scale migration unverified) | §3.7 teardown protocol is mandatory for any terminal runtime; the GPUI-host fallback must pass the same disposal test |
| Exact terminal/layout behaviors (attach, leases, paste/drop, links, restore) | spec task 9770b73c (`docs/nsh-spec-terminal-layout.md`) | §3.5, §4.4 “group disappeared”, §4.5 reconcile |
| DockArea drop zones differ from RiWork's: 23 % pane-edge zones, 12 pt window-edge strips including a top root edge (parity SHELL “drag and drop”). DockArea splits at group edges by its own geometry and has no root-edge or top targets | upstream NSH request (custom zone thresholds + root-edge targets) | Until then, root-edge drops are a parity gap; when added, a root-edge drop maps to `Split(axis, new, root)` / `Split(axis, root, new)` at the tree root. |
| Pane header inset under the traffic lights, per-group header widths | TSX (`renderTab`/`tabBarStyle` + `titleBar()`) | none |
| DockNode has no per-child `fixed` flag (ResizablePanel has one, `resizable/panel.rs:235-239`) | upstream NSH request | §4.3 rule 5 covers the one-absorber case; other cases settle one frame later via `setExtent`. |

Decisions the owner has taken since the freeze are recorded, dated, in [§8](#8-decisions-addendum).

## 7. Change log

- v1 (this document): initial freeze for Step 0.
- 2026-10-27 Phase 2a: Appearance first-use payload promoted to `schema: 1` with
  snapshot/subscribe table in §1.10; local Sessions request and LayoutSession
  placement operations documented. Terminal props/events/commands stay frozen v1.
- v2, 2026-10-27 W3 (task c62ea465): §1.10 adds Settings/Appearance and P1
  Dashboard function tables with payload schema 2; DTO producer/consumer and
  generated-type probes land together. Persistence and host params retain their
  existing schemas. The W3 assignment explicitly applies the D-THEME-1 proposal.
- 2026-10-28 Phase 1 consolidation (task c12f5e14): W1 layout members are
  documented above with the session-write divergence; W2's interim chrome
  `Appearance.get()` is removed in favour of the W3 module.
- 2026-10-27 Phase 1 consolidation (task c12f5e14): one `Appearance` module
  (W3 schema 2) also carries Phase 2a `terminalLight`/`terminalDark`/
  `tabCloseBehavior`; the schema 1 `Appearance.snapshot()` is removed.
  `Sessions.create` keeps optional `cwd` with strict decoding.
- 2026-10-28 Phase 4a (task 6a41bb58): §1.6 gains the Chat payload schema 1
  table above (reset/patch `ChatDelivery`, viewport-paged bodies, typed handle
  commands, drafts on the handle). No engine wire, log or draft-file format
  changed; host params and terminal schemas unchanged.
- v1, 2026-10-27 addendum (task 8fa16659): §8 records three owner decisions (D-OWN-1, D-PERF-1,
  Phase 2 entry gate). No frozen surface, `schema` field or §3.7 metric changed, so there is no
  version bump.

## 8. Decisions addendum

Decisions by the project owner, newest last. Each entry gives its date, where the original
proposal text lives (kept there unchanged), and what it settles here. None of them changes a frozen
surface or a `schema` field.

### 8.1 D-OWN-1: `riwork/native` module ownership for Phase 1 (ADOPTED 2026-10-27)

**Adopted as proposed** in [`docs/nsh-phase1-acceptance.md` §1](nsh-phase1-acceptance.md#1-decisions-needed-before-acceptance).
The orchestrator relayed it to the three Phase 1 branches. Phase 1 ownership of the §1.10 modules
and the code moves:

- **P1a** (task 5c4cb942, `nsh-p1-native`): crate skeleton, registry, subscription hubs, `Store`,
  `Sessions` (read side plus `create`/`close` adapters), `Tabs`, `Settings`, `Appearance` (without
  Ghostty parsing), test support.
- **P1c** (task 40e6ac8b, `nsh-p1-workspace`): `crates/riwork-nsh-native/src/layouts/` (`Layouts`,
  `LayoutSession`), the global-window state functions of `Dashboard`, and moving `gather`, the
  default layout and tab routing out of `crates/riwork-desktop/src/main.rs` into riwork-core.
- **P1b** (task ddba58a0, `nsh-p1-host`): host-internal modules, plus moving
  `crates/riwork-desktop/src/dock_menu.rs` logic into the host.

Unchanged: the module names of §1.10. Also unchanged is the §1.10 rule that each module's function
table is added to this document under a `schema` bump before first use.

### 8.2 D-PERF-1: NSH frame budgets (ADOPTED 2026-10-27)

**Adopted as proposed** in [`docs/nsh-phase1-acceptance.md` §7](nsh-phase1-acceptance.md#7-frame-budget-decision-d-perf-1),
where the proposal and its alternatives stay as written. §5 "Perf budgets" asks for an explicit NSH
target for the guard that already fails on GPUI (parity disagreement #10). This is that target.
Alternatives (A) and (B) of the proposal were not taken. The adopted text, unchanged from the
proposal:

> **NSH frame budgets.**
>
> 1. *Structural guards are hard and machine-independent.* They are asserted in every test build:
>    fewer than 100 rows or selection bodies materialized per frame for the 2,200-call disclosed
>    turn; no reprojection for token deltas; under 1.5 ms per streamed event at the bridge;
>    exactly one projection per layout gesture; zero deliveries while idle.
> 2. *Wall-clock budgets are asserted only on release builds* of the Rust host, the UI set and the
>    JS bundle. They are measured with NSH's settled surface-frame monitor, as the median of at
>    least 5 runs, on a quiet host: no cargo, rustc or nsh process at start, and a 1-minute load
>    average below half the core count, recorded with the run.
> 3. *Targets.* For the 2,200-call disclosed turn at 1220×780, Native theme: median ≤ 16.7 ms and
>    p95 ≤ 25 ms. Each must also be no worse than the matched GPUI release measurement of the same
>    fixture, viewport and theme on the same host. For the Phase-1 busy workspace (24 tabs,
>    120 projects): median ≤ 16.7 ms and p90 ≤ 25 ms, with the same matched comparison.
> 4. *Debug timings are reported, never asserted.*
> 5. *The GPUI 25 ms Debug guard is retired from the NSH acceptance set.* It stays a GPUI
>    regression test for as long as the GPUI desktop builds.

Phase 1 perf stays **report-only** until the chat phase. The Phase 1 measurements (P1-PERF-01:
busy-workspace frame, idle CPU) are recorded against these targets but fail no Phase 1 check. The
structural guards of rule 1 that exist in Phase 1 (P1-PERF-02, P1-POLL-02) are hard from Phase 1.

### 8.3 Phase 2 entry gate: pinned-runtime teardown proof (ADOPTED 2026-10-27)

The evidence it rests on is the Phase 2 teardown proof (task b915a54a, merged to `nsh` as
`d50ac3e`; `spikes/ghostty-nsh/WORKER_REPORT.md`,
`spikes/ghostty-nsh/evidence/phase2/cached/verification-summary.json`). It ran only on the cached
libghostty `1.3.2-dev` archive:

- **Passed:** 20 batches × 4 surfaces under tmux output pressure, 80/80 freed exactly once,
  detach and child exit confirmed before every free, `ghostty_surface_free` max 21.135 ms, zero
  hangs, zero leaked clients or child processes, same session and pane after every close.
- **Failed:** main-loop delivery gaps. Seven timer gaps of 100 ms or more, on surfaces 1–4 and 52,
  max **156.212 ms**. That fails §3.7 item 4 ("the main thread never stalls > 100 ms").
- The rebuilt pinned runtime (Ghostty `9f0e1719dc918368367d368bfe300f59bb68b5a4` plus the
  declared vendor patches) was built and linked, but **never launched**.

Decision:

1. **Phase 2 does not open** until the pinned-runtime teardown proof (task e940bd0a) passes.
2. **The cached-runtime result is not accepted.** Its 110–156 ms main-loop gaps remain a §3.7
   failure.
3. **§3.7 is not amended.** Item 4's 100 ms limit and the rest of the teardown protocol stand as
   frozen.

Task e940bd0a's GO criteria, as written in the task: "80/80 freed, zero hangs/leaks, session/pane
survival, AND (<100ms gaps OR gaps proven non-terminal noise with profiling evidence)". Its stop
rule, as written: "NO-GO again = stop, no metric relaxation".

### 8.4 W3 P1 appearance fallback (2026-10-27)

Task `c62ea465` explicitly directs the P1 implementation to apply the D-THEME-1
proposal: no Ghostty configuration parsing, the RiWork palette on Follow
Ghostty, the diagnostic retained, and no `appearance.json` publishing. The
terminal phase retains ownership of real parsing and publishing. This records
the assignment’s scope; it does not settle D-PROC-1 or D-COPY-1.

### Phase 1 W2 window chrome (consolidated)

W2 first exposed an interim, read-only `Appearance.get()` window-chrome
projection while the full appearance service was integrated. Phase 1
consolidation (task `c12f5e14`) removes that second module: window chrome
(controls island, drag strip, borders) reads the W3 schema 2 `AppearanceDto`
palette (`controlsIsland`, `panelActive`, `divider`, `bg`, `panel`, `text`)
through the window's resolved interface theme, and text size uses W3 Settings.
Native and Hermes keep their P-SHELL unpainted controls area. Follow Ghostty
uses the RiWork palette per §8.4; no `appearance.json` is read or published.

Window size is captured directly from host Rust geometry through
`HostLayout::observe_window_frame(width, height, ordinary, remember)`; it is not
a JavaScript host method. The existing layout worker applies the trailing 250 ms
save delay. Model edits retain the latest host size, and a resize that returns to
the settled size reaches LayoutStore's unchanged-data no-write path. The service
worker supplies the current `remember_window_size` setting, including capturing
the current normal frame when the setting becomes enabled. Global windows
persist their normal size independently of that setting.

### Phase 1 W1 workspace members (consolidated, task `c12f5e14`)

W1 (task `71ac437e`) added these `riwork/native` layout members without a
§1.10 table; consolidation records them. All are owned by
`riwork-nsh-workspace` and composed into the single registry.

| JS | Kind | Rule |
|---|---|---|
| `Layouts.fixtureManifest()` | async | Debug builds only; reads `fixtures.json` from an isolated `/tmp` home. |
| `Layouts.openSession({ windowId, projectId, restore?, focusMode?, focusCentered? })` | async | Adds runtime focus restore. Saved layouts are rewritten on open only for a reload snapshot (P1-LAY-03 unchanged bytes); first-run homes create one initial shell. |
| `LayoutSession.newShell({ split?, cwd?, worktreeId? })` | async | Creates a local shell and places it (optionally splitting, `right`/`down`); a failed create leaves the tree unchanged. |
| `closePanel(id, exit)`, `closePane(id)` | async | Hide shared membership first (a failure refuses), Exit only on explicit choice, then place-only removal recording detached identity and its former pane. |
| `renameTab(id, title)`, `moveTab(id, delta)` | async | Shared-strip membership edits through the owning project's TabStore. |
| `toggleFocus()`, `toggleCentered()`, `setFocusMode(focused, centered)` | sync | Presentation only; never rewrites the saved layout. Host restart snapshots carry the flags. |
| `setInteraction(active)`, `subscribe(listener)`, `refreshSessions()`, `refreshTabs()` | sync/event/async | Deliveries queue while menus, drags and dialogs are open; the process watcher wakes subscriptions. |
| `openPendingChat(id)`, `resolvePendingChat(id, chatId)` | sync | Pending chats are never saved; resolution keeps the mounted identity. |

Consolidation decisions:

- `defaultLayout()` uses W1's core placement, which relocates opaque future
  tabs with the remapped saves W1 added. This supersedes W3's earlier refusal,
  which existed only because core could not migrate opaque tabs.
- New panels follow the GPUI routing rule (main pane, else the active pane);
  Files joins the navigation pane (W2).
- **Known divergence, not resolved here:** W1's `newShell`/`closePanel`/
  `renameTab`/`moveTab` write sessions and TabStore membership through a
  workspace-private adapter (`session_io`), duplicating the §1.10 Phase 2a rule
  that session creation and exit go through `Sessions`. Both paths use the same
  engine `SessionManager`/`TabStore` semantics (hide before exit, refuse on a
  failed hide), and the TSX uses one path per action. The adapter also samples
  sessions per layout session on watcher wakes rather than once per process.
  Routing these writes through `Sessions`/`Tabs` is a follow-up.
- W1's debug fixture inventory (`fake-sessions.json`) is honoured only in debug
  or `test-support` builds and only for isolated `/tmp` homes; the process
  `Tabs` service reads through the same adapter so the two never reconcile one
  TabStore against different inventories.
