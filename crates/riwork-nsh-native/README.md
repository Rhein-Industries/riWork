# riwork/native (Phase 1 integration)

NSH 0.3.0 bridge for the Phase 1 surface of the frozen contract in
[`docs/nsh-contracts.md` §1](../../docs/nsh-contracts.md). The root workspace's
`crates/*` member pattern includes this crate; `Cargo.lock` pins its dependencies.
It does not depend on the desktop crate or an AppKit host.
This crate owns the sole `#[native_module("riwork/native")]` registry. It composes
`riwork_nsh_workspace::register_layouts` from the root-workspace member
`apps/riwork`; that crate exports layout classes, never a second UI set.
`apps/riwork/nsh.json` names this crate in `rust`, so the CLI's generated
declarations and headless runner use exactly the same registry as the host.

Implemented JavaScript surface:

- `Store.snapshot()`, `Store.search(query)`, `Store.inspectProject(root)` return
  Promises and run blocking core calls through `spawn_blocking`.
- `Store.subscribe(listener)` pushes `StoreEvent` snapshots.
- `Store.createFolder(name, parentId?)`, `renameFolder(id, name)`,
  `moveFolder(id, parentId?)`, `removeFolder(id)` and
  `moveProjectToFolder(projectId, folderId?)` validate canonical UUIDs, commit
  through the core Store and publish the resulting snapshot before resolving.
- `Sessions.list()` returns a Promise of `{ sessions }`, using
  `SessionManager::list_with_activity` for the inventory. Activity timestamps are
  reserved for a later port; both reads and subscription payloads contain the
  same inventory fields.
- `Sessions.subscribe(listener)` pushes `SessionsEvent` snapshots from the
  shared session sampler.
- `Sessions.create(request)` and `Sessions.close(id)` adapt the engine on the
  blocking pool and wake the same process sampler. Tests inject fakes and never
  create a real shell.
- `Tabs.subscribe(projectId, listener)` and `Tabs.update(projectId, update)`
  share one topic per project. Updates use the engine's exact key/title
  validators; the home watcher refreshes subscribed project inventories.
- `Settings.get/set/changeTextSize` and status-bar item mutations use the core
  settings lock. `Settings.subscribe` and `Appearance.subscribe` share the same
  home watcher. `Appearance.resolve(theme, systemDark)` returns the RiWork
  palette, terminal override, diff colors and diagnostics. Phase 1 does not
  parse Ghostty config or publish `appearance.json`.
- `Dashboard.getGlobalWindow/saveGlobalWindow` adapt durable global-window
  preferences; activity/dashboard feeds remain deferred.
- `Subscription.close()` is idempotent. JS owner disposal, VM shutdown and
  garbage collection also cancel delivery and close the listener queue.
- `Layouts.openSession(request)` and `LayoutSession` expose workspace layout
  projection, edits and persistence. The host uses the Rust-only
  `riwork_nsh_workspace::host_layouts` handoff for lifecycle barriers.

DTOs and generated declarations use camelCase. Core persistence is unchanged.
Identifier-keyed maps and opaque future configuration retain their data keys.
The registry supplies TypeScript declarations to NSH under `riwork/native`;
the consuming app generates `.nsh/types/riwork/native.d.ts` as usual.

The host must await initialization before constructing its first AppView:

```rust,ignore
riwork_nsh_native::initialize().await?;
// Once, before AppViews (in the host's own startup code):
set_ui_sets([nsh_base::registry(), riwork_nsh_native::registry()]);
```

Initialization resolves `RIWORK_HOME` once. Every engine uses one strongly held
process service, with one recursive home watcher and one background worker.
The layout registration receives a Rust-only resolver for this same fixed home,
including a home selected by an injected test service. Resolving it happens on
the blocking pool. Layout-only headless runners resolve the home without
starting the session sampler; a later service initialization reuses that home.
Store reads happen on filesystem changes, including atomic replacements and
watcher rescan requests. Session metadata changes invalidate and wake the shared
sampler. Only tmux liveness has a fallback timer while the last successful
inventory has shells, two seconds after the last sample. Empty inventories
wait for a watcher event. Unchanged values do not wake subscribers. A temporarily unavailable
session backend does not block Store; session subscriptions wait for their first
successful snapshot when the shared sampler is next woken. `Sessions.list()` rejects
on backend errors, and `ProcessService::last_error()` exposes background errors
to host diagnostics. Failed store/watcher startup can be retried.

Every listener has an `async_channel` queue of capacity 64. Overflow clears that
queue and enqueues an authoritative snapshot with `resync: true`. Revisions are
monotonic within a topic. Delivery drains intermediate snapshots and uses
`Callback::post`. One requested frame gates the next delivery to a later engine
turn; quiet listeners await the channel without requesting frames. `CallCx`
binds cancellation to the actual JS owner, even when JS retains a handle.

`StoreBackend`, `SessionsBackend`, `TabsBackend`, `SettingsBackend` and
`DashboardBackend` are injectable synchronous traits. The
blocking `ProcessService::with_backends` constructor plus
`service::install_process_service` allow tests to register fakes with the same
native module specifier. Installing a second process service is rejected.

Headless verification uses isolated fixtures and actual NSH QuickJS engines:

```sh
NSH_NATIVE_TEST="$(mktemp -d /tmp/riwork-nsh-native-test.XXXXXX)"
RIWORK_HOME="$NSH_NATIVE_TEST/home" CARGO_TARGET_DIR="$NSH_NATIVE_TEST/target" \
  cargo test --locked -p riwork-nsh-native --jobs 4
```

Tests cover bridge and external core store writes pushed to two engines, async read calls,
owner/handle/engine teardown, idle frame behavior, queue overflow/resync,
process-wide session fallback and watcher wakes, DTO boundaries, unavailable
session recovery, and blocking-thread boundaries. The two-engine test imports
Store, Sessions and Layouts from the one module, then verifies layout edits and
host snapshots use the injected process home and persist through LayoutStore.
The CLI acceptance probe in `tests/app` composes the production registry with
the isolated in-memory backends in `riwork-nsh-native-test-support`. It imports
all seven P1 modules and proves an Add folder action reaches two engines.
Fixtures inject session data;
they never start tmux, launch app windows or touch live RiWork data. Chat and the
other contract modules are left for their port phases; no chat feed is touched.
