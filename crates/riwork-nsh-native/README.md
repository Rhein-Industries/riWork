# riwork/native (Phase 2a terminals and Phase 3a panels)

NSH 0.3.0 bridge for the project, task, shell and shared-tab surfaces of the frozen contract in
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
- `Store.createProject`, `createNewProject`, `addProject`, `useProject` and
  `renameProject` delegate durable project writes to core. Creation/inspection
  expands `~`, requires absolute paths and rejects `.git` components. Failed
  creation removes only newly created empty directories. The optional host-only
  default-directory override keeps fixture creation away from the user's home.
- `Store.createFolder`, `renameFolder`, `moveFolder`, `removeFolder`,
  `moveProjectToFolder`, `syncWorktrees`, `refreshWorktrees`, `addTask`,
  `assignTasks`, `unassignTasks` and `setTaskStatus` use the existing core writers.
  `setProjectNotifications` and `setProjectOrchestratorWakes` update their
  individual project preferences without replacing concurrent metadata.
  Each write publishes the resulting snapshot before resolving. Watcher refreshes
  and writes share a lock so an older read cannot overwrite a newer delivery.
- `Sessions.list()` returns a Promise of `{ sessions }`, using
  `SessionManager::list_with_activity` for the inventory. Activity timestamps are
  reserved for a later port; both reads and subscription payloads contain the
  same inventory fields.
- `Sessions.subscribe(listener)` pushes `SessionsEvent` snapshots from the
  shared session sampler.
- `Sessions.create({ projectId, worktreeId?, cwd?, command? })` creates an explicit
  user shell; omitted cwd resolves the worktree/project root. `Sessions.close(id)`
  is explicit Exit and refuses to proceed unless shared membership hides first.
- `Tabs.subscribe(projectId, listener)` and `Tabs.update(projectId, update)` use
  a shared per-project hub and the engine's authoritative tab-store operations.
  Tab events contain `{ kind, rev, data: { epoch, revision, entries } }` and use
  camelCase, including nested `childCount`. Hide/unhide/move/rename writes are
  serialized with watcher and sampler refreshes. Local-shell updates retain the
  registered project identity check. Detach hides before local removal and
  reopen unhides before placement; an unavailable shared store permits local
  Detach, while Exit still requires a successful hide.
- `Appearance.snapshot()` / `subscribe(listener)` project terminal theme colors
  and close preference from core settings. The one home watcher pushes live
  settings changes; the native component owns Ghostty config parsing/watching.
- `Subscription.close()` is idempotent. JS owner disposal, VM shutdown and
  garbage collection also cancel delivery and close the listener queue. Explicit
  close unregisters owner cleanup while JS is alive; finalization only aborts,
  because NSH clears context state before freeing some retained cleanup objects.
- `Layouts.openSession(request)` and `LayoutSession` expose workspace layout
  projection, edits and persistence, including local `openShell`/`detachShell`.
  Reopening within a window remembers a surviving unlocked former pane. The host uses the Rust-only
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
watcher rescan requests. The same process worker refreshes worktree discovery and
presence at the two-second session fallback cadence, using core's scan cache;
directory presence uses a process-shared two-second cache with serialized
probes, including explicit snapshot requests. No rendering engine polls. Session metadata changes invalidate and wake the
shared sampler. Tab inventory reuses the engine's process-wide session cache.
Unchanged values do not wake subscribers. A temporarily unavailable
session backend does not block Store; session subscriptions wait for their first
successful snapshot while the shared sampler retries. `Sessions.list()` rejects
on backend errors, and `ProcessService::last_error()` exposes background errors
to host diagnostics. Failed store/watcher startup can be retried.

Every listener has an `async_channel` queue of capacity 64. Overflow clears that
queue and enqueues an authoritative snapshot with `resync: true`. Revisions are
monotonic within a topic. Delivery drains intermediate snapshots and uses
`Callback::post`. One requested frame gates the next delivery to a later engine
turn; quiet listeners await the channel without requesting frames. `CallCx`
binds cancellation to the actual JS owner, even when JS retains a handle.

`StoreBackend`, `SessionsBackend` and `TabsBackend` are injectable synchronous traits. The
blocking `ProcessService::with_backends` constructor plus
`service::install_process_service` allow tests to register fakes with the same
native module specifier. Installing a second process service is rejected.
`with_all_backends` additionally injects the shared tab inventory for fully
isolated fixtures.

Headless verification uses isolated fixtures and actual NSH QuickJS engines:

```sh
NSH_NATIVE_TEST="$(mktemp -d /tmp/riwork-nsh-native-test.XXXXXX)"
RIWORK_HOME="$NSH_NATIVE_TEST/home" CARGO_TARGET_DIR="$NSH_NATIVE_TEST/target" \
  cargo test --locked -p riwork-nsh-native --jobs 4
```

Tests cover external core store writes pushed to two engines, async read calls,
owner/handle/engine teardown, idle frame behavior, queue overflow/resync,
process-wide session fallback and watcher wakes, DTO boundaries, unavailable
session recovery, and blocking-thread boundaries. The two-engine test imports
Store, Sessions and Layouts from the one module, then verifies layout edits and
host snapshots use the injected process home and persist through LayoutStore.
`tests/panel_crud.rs` also reopens real store files after project/folder and task
lifecycle operations, checks cross-service watcher delivery, and exercises real
tab files plus create/close session adapters. Fixtures inject session data;
they never start tmux, launch app windows or touch live RiWork data. Chat and the
other contract modules are left for their port phases; no chat feed is touched.
