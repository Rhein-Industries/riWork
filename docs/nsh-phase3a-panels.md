# NSH Phase 3a panels

Assignment: `500f21fc-0a69-40e6-b158-2df7a8fbf2bc`
Branch: `nsh-p3-panels`, based on `nsh` at `17e89d9`

`verification_environment = isolated /tmp RIWORK_HOME and RIWORK_RUNTIME_DIR fixtures, Rust tests and windowless NSH 0.3.0 engines`

This phase implements the local Projects/folders, Worktrees, Tasks and Shells
panel surfaces in `apps/riwork`. The parity references are the local panel rows
PANEL-001–060 and the corresponding project, task, navigation and shell rows in
`docs/nsh-parity.md`. The frozen contracts and D-OWN-1 continue to put durable
records and shared subscriptions in `riwork-nsh-native`, layout authority in
`riwork-nsh-workspace`, and window-local presentation in TSX.

Every panel receives authoritative Store/Sessions subscription snapshots.
Mutations resolve after the real store write and publish the resulting snapshot
to peer engines. TSX uses no data polling timer or durable NSH storage. Folder
movement uses explicit menus, the documented fallback for NSH's in-app drag gap.
Project switching flushes the outgoing layout and carries locked regions using
the core layout rule. Shell selection records an attach intent and renders a
placeholder. No live terminal component is enabled; the Phase 2 gate remains.

## Bridge additions

The one `riwork/native` registry composes Store, Sessions, Tabs and Layouts.
The implemented Store mutations use the names in contracts §1.5. Project requests
are `{ root, name?, initGit? }`; `initGit` defaults to true. Task mutations are
`addTask(project, title, details)`, `assignTasks(worktree, taskIds)`,
`unassignTasks(taskIds)` and `setTaskStatus(taskId, status)`, where status is
`todo`, `in_progress` or `done`. Folder methods accept optional parent IDs;
removal promotes direct children and projects through the existing core rules.
Notification and orchestrator-wake switches save through
`setProjectNotifications(project, enabled)` and
`setProjectOrchestratorWakes(project, enabled)`.

`Store.defaultProjectDirectory()` is an additive asynchronous form helper for
the existing core default-project-directory rule. Tests supply an explicit
temporary directory to the creator and never invoke that helper against the
user's Documents folder. Worktree DTOs expose sampled `present` state; their
durable format is unchanged.

Sessions adds `create({ projectId, worktreeId?, cwd, command? })` and `close(id)`
adapters behind the injectable SessionsBackend. Panel attachment does not call
either adapter. Tabs supplies `subscribe(projectId, listener)` and
`update(projectId, update)` for hide, unhide, move and rename, using the shared
home watcher and process sampler. All domain I/O runs off engine threads.
The panel controller subscribes to active-project tab membership for shared
shell titles and hidden state; reopening a hidden live shell unhides its entry.
The host reads switched project identity and layout atomically from its registered
layout handle for routing, window lists, runtime publication and close capture.
It resolves the corresponding project root on the service thread.

## Scope boundaries

This is the requested Phase 3a workflow slice, not completion of every local
panel parity row. Project sorting currently offers Name, Date added and Live
sessions per window. Last edited/Recency and Settings-backed ordering remain
for their owning modules. Tree display is a tested, cycle-safe TS projection of
the authoritative native snapshot. Chat history/filter/new-chat/nuke flows,
remote projects, detailed shell activity/CPU/RAM/subagents and terminal lifecycle
remain outside this slice. Existing unrelated panels retain their placeholders.
Full shared terminal membership reconciliation and the documented NSH drag gap
are unchanged.

## Verification

The scratch root for this run is `/tmp/riwork-nsh-p3a-500f21fc.FuPHBM`.
Its `env.sh` records the isolated home, runtime, target, cache and temporary
directories. The pinned CLI is
`/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh`:
`nsh 0.3.0 (76fa8a7)`, SHA-256
`90b7633c80306407fc97b2051be00af868d4577281f918ff655c650a8a5a2e60`.
Host: macOS 26.2 (25C56), arm64; Rust/Cargo 1.99.1.

The final retained home is `$NSH_P3A/home-verified`. UI project directories are
unique `/tmp/riwork-nsh-panel-ui-*` fixtures. Existing dependency caches were
read offline; build output, NSH cache, runtime and temporary files are isolated.
The only root lockfile change adds the already-locked `uuid` dependency to the
workspace app package; no resolved external package version changed.

The real bridge test runs two independent QuickJS engines with the production
CoreStoreBackend and injected session inventories. One engine creates a project,
creates and assigns a task, marks it in progress and done, and unassigns it. The
peer receives each relevant state through its subscription. Reopening the real
core store and reading `state.json` verifies persistence and the unchanged
snake_case disk format. Invalid task status is rejected without changing files.
The plain Rust fixture suite also uses an independent ProcessService observing
the same files to prove watcher delivery, folder promotion/cycle refusal,
cross-project task refusal, tab rename/hide and injected session create/close.
The UI fixture injects shell inventory data for live/exited selection tests;
it never starts a shell process.

Exact final commands, from the assigned worktree (all exited 0):

```sh
source /tmp/riwork-nsh-p3a-500f21fc.FuPHBM/env.sh
cargo test --locked --offline -p riwork-nsh-native -p riwork-nsh-workspace --jobs 4
cargo test --locked --offline -p riwork-desktop --bin riwork nsh_host::tests --jobs 4

export RIWORK_HOME="$NSH_P3A/home-verified"
cargo run --locked --offline -p riwork-nsh-workspace --example seed_fixture -- "$RIWORK_HOME"
CARGO_NET_OFFLINE=true "$NSH_CLI" check apps/riwork --tsc '/Users/dominik/Library/Caches/nsh/typescript-7.0.2-darwin-arm64/package/lib/tsc'
CARGO_NET_OFFLINE=true "$NSH_CLI" test apps/riwork --junit "$NSH_P3A/nsh-headless-verified.xml"

cargo fmt -p riwork-nsh-native -p riwork-nsh-workspace -p riwork-desktop -- --check
git diff --check
```

`env.sh` sets `NSH_P3A` to the scratch root above, `NSH_CLI` to the pinned
absolute CLI path, `RIWORK_HOME=$NSH_P3A/home`,
`RIWORK_RUNTIME_DIR=$NSH_P3A/runtime`, `CARGO_TARGET_DIR=$NSH_P3A/target`,
`NSH_CACHE=$NSH_P3A/nsh-cache`, `TMPDIR=$NSH_P3A/tmp`,
`RUSTUP_TOOLCHAIN=1.99.1`, and `CARGO_BUILD_JOBS=4`.
The seed command refuses to overwrite an existing layout fixture; use a new
home for a replay.

| Check | Final result | Evidence under scratch root |
|---|---|---|
| Native services + workspace Rust | 60 passed, 0 failed: 18 native and 42 workspace tests | `cargo-services-layouts-final.log` |
| Host model/lifecycle Rust | 9 passed, 0 failed; 12 unrelated tests filtered | `cargo-host-final.log` |
| NSH check | Rust check/build and TypeScript passed | `nsh-check-verified.log` |
| NSH headless tests | 23 passed, 0 failed (11.7 s): 8 workspace, 3 panel workflows, 12 model tests | `nsh-test-verified.log`, `nsh-headless-verified.xml` |
| Formatting and diff whitespace | Passed | `fmt-check-verified.log` |
| Retained real store files | Created plain project exists without `.git`; task is done/unassigned with details; folder was removed; snake_case disk fields preserved | `home-verified/state.json`, `persisted-ui-proof.json` |

Source inspection also confirmed that the nine top-level TS/TSX files use no
`setInterval`, `requestAnimationFrame`, repeated resource refetch or durable
`nsh/storage` import. The creator's single 250 ms debounce is not a data poll.
NSH emits an informational warning about native functions without doc comments;
the type check and all tests still pass.

Exploratory failures are retained beside the final logs. They exposed NSH's
Input/List prop names, unbound signal setter callbacks and a drag fixture's
fixed offset after adding the toolbar. These were corrected; drag verification
now measures the source and destination centers. A new bridge test initially
waited for a notification after an unchanged project reselection; it now proves
an actual two-project switch. Fixture readiness waits allow 15 seconds for
asynchronous initialization during concurrent compilation. The final full run
uses a fresh home and all final source changes.

No live RiWork windows or state, task status, merge, push, installation or
desktop automation is part of this verification. Task status is left to the
orchestrator. Headless tests establish behavior and persistence; they do not
claim visual parity with a running AppKit window.
