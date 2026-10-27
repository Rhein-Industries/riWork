# riwork/native (phase 1a)

NSH 0.3.0 bridge for the read side of the frozen contract in
[`docs/nsh-contracts.md` §1](../../docs/nsh-contracts.md). The root workspace's
`crates/*` member pattern includes this crate; `Cargo.lock` pins its dependencies.
It does not depend on the desktop crate or an AppKit host.

Implemented JavaScript surface:

- `Store.snapshot()`, `Store.search(query)`, `Store.inspectProject(root)` return
  Promises and run blocking core calls through `spawn_blocking`.
- `Store.subscribe(listener)` pushes `StoreEvent` snapshots.
- `Sessions.list()` returns a Promise of `{ sessions }`, using
  `SessionManager::list_with_activity` for the inventory. Activity timestamps are
  reserved for a later port; both reads and subscription payloads contain the
  same inventory fields.
- `Sessions.subscribe(listener)` pushes `SessionsEvent` snapshots from the
  shared session sampler.
- `Subscription.close()` is idempotent. JS owner disposal, VM shutdown and
  garbage collection also cancel delivery and close the listener queue.

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
Store reads happen on filesystem changes, including atomic replacements and
watcher rescan requests. Session metadata changes invalidate and wake the shared
sampler. Only tmux liveness has a fallback timer, two seconds after the last
sample. Unchanged values do not wake subscribers. A temporarily unavailable
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

`StoreBackend` and `SessionsBackend` are injectable synchronous traits. The
blocking `ProcessService::with_backends` constructor plus
`service::install_process_service` allow tests to register fakes with the same
native module specifier. Installing a second process service is rejected.

Headless verification uses isolated fixtures and actual NSH QuickJS engines:

```sh
NSH_NATIVE_TEST="$(mktemp -d /tmp/riwork-nsh-native-test.XXXXXX)"
RIWORK_HOME="$NSH_NATIVE_TEST/home" CARGO_TARGET_DIR="$NSH_NATIVE_TEST/target" \
  cargo test --locked -p riwork-nsh-native --jobs 4
```

Tests cover external core store writes pushed to two engines, async read calls,
owner/handle/engine teardown, idle frame behavior, queue overflow/resync,
process-wide session fallback and watcher wakes, DTO boundaries, unavailable
session recovery, and blocking-thread boundaries. Fixtures inject session data;
they never start tmux, launch app windows or touch live RiWork data. Chat and the
other contract modules are left for their port phases; no chat feed is touched.
