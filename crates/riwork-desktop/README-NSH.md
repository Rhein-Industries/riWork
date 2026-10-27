# Integrated AppKit NSH host

The single `riwork` executable still calls `riwork_cli::cli::run_cli` before any
GUI initialization. CLI, MCP and `chat serve` use the unchanged CLI/engine code,
including the original output, errors and exit status. `src/main.rs` and the
older UI modules are uncompiled parity references; GPUI is no longer a dependency
of this binary. This is the host foundation, not the completed RiWork UI port.

## Frozen seams

- `docs/nsh-contracts.md` §0, §2 and §3 are the source of truth. The host reuses the
  AppKit/AppView topology proved in `spikes/host-multiwindow`.
- One `NSWindow`, `AppView`, UUID v4 and `HostChannel` per window. All views load
  the same source bundle, distinguished only by schema-1 `host.params`.
- `set_ui_sets([nsh_base::registry(), riwork_nsh_native::registry()])` runs once,
  before any AppView. The real native crate composes Store, Sessions, Tabs,
  Appearance, Subscription, Layouts and LayoutSession into the sole
  `riwork/native` module.
  The native process service initializes on the startup worker; its fixed home
  also supplies workspace layout persistence.
- The shared `Components` registry registers `riwork.terminal` before the first
  AppView. Local shell tabs use the pinned Ghostty C library and existing engine
  sessions. See [terminal integration](../riwork-nsh-terminal/README.md) and its
  [private build preparation](../riwork-nsh-terminal/BUILD.md).
  The app supports local New Shell, Detach, guarded Exit and explicit reopen of
  the same session, with live terminal palettes and URL/file/folder link opening.
  Paste/drop, subshells, remote tabs and Vim tabs remain next-slice seams.
- The real `apps/riwork` bundle is required. Source is
  `apps/riwork/App.tsx`; `apps/riwork/nsh.json` points its Rust side at
  `crates/riwork-nsh-native`, so NSH type generation, tests and the desktop host
  all use the same registry. `apps/riwork` is a root Cargo workspace member;
  the root `Cargo.lock` is its only lockfile.
- A checkout build defaults to `apps/riwork/.nsh/out/bundle.js`, resolved from
  the desktop crate's compile-time source path, independently of the launch cwd.
  Generate it with the pinned NSH **0.3.0** CLI (see `docs/nsh-toolchain.md`):
  `"$NSH" build apps/riwork --out apps/riwork/.nsh/out`.
  This is the dev-source workflow: edit TSX, rebuild the bundle, restart the
  fixture. The host does not compile TSX or provide hot reload.
- For a built bundle independent of the checkout, run
  `"$NSH" build apps/riwork --out /absolute/output/riwork`, then set
  `RIWORK_NSH_BUNDLE=/absolute/output/riwork/bundle.js`. Keep generated assets,
  source maps and split chunks alongside it. The containing directory supplies
  asset and chunk resolution. Missing bundles fail with the build recipe;
  there is no embedded shell fallback. No package/install changes are included.
- Both bundle paths grant only `window.*`, `app.*`, `system.*` host capabilities.
  No filesystem/process/network capabilities are granted. `nsh/storage` is
  transient per VM; NSH caches stay below `RIWORK_HOME/nsh`.
- Engine-thread host callbacks only queue main-thread messages. Store reads,
  runtime registry writes and forwarding run on the service/socket threads.
  AppKit uses its event loop without a frame-polling loop.

## Implemented host behavior

`window.open`, `route`, `placement`, `list`, `focus`, `close` and `setDockInfo`;
project windows and a global singleton; per-window `route`, `project.open`,
`global.show`, `command`, `window.state`, and `window.willClose` events. A window
is stopped/released on native or channel close, including NSH's own Window.close.
The last window closes the process after all terminal clients detach, their PTY
children exit and the main thread frees their surfaces. AppKit keeps pumping
wakeups through that drain; a parked terminal never blocks the main thread.

The host owns RiWork/Edit/View menus and the Dock menu using the public
`nsh/desktop` Rust model. The menu accelerators dispatch commands to the key
window. Cmd-W emits `closeTab`, Cmd-Shift-N opens a project window, standard Edit
selectors offer native responders first, and font keys offer native responder
key equivalents before UI text commands. The terminal implements native key
equivalents and standard responder editing; paste remains deferred.
Window-local title/chrome comes through `nsh/desktop`; the host supplies full-size
transparent content, minimum 640×400, cascade and traffic-light placement.

A per-home `run/desktop.lock` plus `run/desktop.sock` forwards later GUI launches
before bundle loading. A restoring replacement has a separate `nsh/desktop.handoff.lock` gate so
ordinary launches cannot take ownership during replacement handoff. Shutdown
stops forwarding but retains ownership until AppViews stop. Runtime registration,
window publication, heartbeat, engine restore-ticket validation/watchdog, and a
per-window `app.restoreReady` barrier are implemented. Existing runtime formats
are unchanged, including display-local top-left saved geometry (converted at
the AppKit boundary).

## Deliberate integration points

The host looks up P1c's native `host_layouts` handles by WindowId. Runtime
publication reads `project_layout()` from Rust, including heartbeat publications
when the UI has not moved or resized. Close and quit stop accepting layout edits,
retain handles, and call `flush_blocking()` on the service worker before AppView
teardown. Failure cancels close/quit and keeps the windows available. Global
window state uses the existing core store. Drafts remain deferred.

The real app passes `host.params.restore.layout` to `Layouts.openSession` and
posts `app.restoreReady` once after installing the DockArea projection. Global
restores confirm immediately; the host verifies project handles exist before
accepting readiness. Full restart/CLI reload handoff is still explicitly
unavailable in this integration subset and keeps the old process alive.

`system.openWith` and `system.revealInFinder` validate terminal link targets and
open them through the system. Local session mutations, shared shell membership
and watched terminal appearance are available through `riwork/native`.
System panels, pasteboard, notifications, Quick Look, sharing, fonts/permissions,
remote project resolution and forced application appearance remain future
integration points. Their named host calls reject as unavailable;
unknown/reserved names are not registered. The URL hook only allows the system to
open http/https/mailto/x-apple.systempreferences schemes. Full workspace shortcuts
and product UI behavior remain with the TSX port.

## Isolated verification only

Do not launch this branch against the user's RIWORK_HOME or existing windows.
The supplied runners create disposable state and run only this checkout's binary:

```sh
python3 crates/riwork-desktop/verification/run_headless.py
python3 crates/riwork-desktop/verification/gui_acceptance.py \
  --binary target/debug/riwork --linger-seconds 120
```

The headless runner clears an inherited blocked signal mask (otherwise the
existing SIGTERM tests cannot terminate their fixture hosts). It also keeps its
isolated HOME outside `/tmp`: the existing trust-safety test deliberately refuses
HOME writes while permitting temporary fixture paths. GUI verification always
sets separate HOME/RIWORK_HOME/RIWORK_RUNTIME_DIR and `RIWORK_NSH_BACKGROUND=1`;
that option suppresses AppKit activation and make-key during background tests.
The fixture prints its PID for **RiWork's cua-driver MCP** inspection. Use only
that PID; never test in a live RiWork window.

The integration runner verifies the real bundle (the older GUI runner above is
only the historical P1b bridge fixture):

```sh
# Evidence/build root must be inside /tmp; no installed binary is used.
python3 crates/riwork-desktop/verification/phase1_integration.py headless --root /tmp/riwork-integration
python3 crates/riwork-desktop/verification/phase1_integration.py build --root /tmp/riwork-integration
python3 crates/riwork-desktop/verification/phase1_integration.py parity --root /tmp/riwork-integration
python3 crates/riwork-desktop/verification/phase1_integration.py gui --root /tmp/riwork-integration --linger-seconds 120
```

The runner defaults to the previously verified isolated NSH 0.3 CLI and TypeScript
compiler; override `--nsh` and `--tsc` with equivalent existing tools if needed.
It records exact argv, fixture environment, stdout/stderr and checksums for each
command. GUI mode prints the only PID authorized for read-only cua-driver MCP
inspection. It stops its own process via SIGTERM after inspection; close/quit
barriers are verified in Rust tests, not inferred from that cleanup signal.
For the current local terminal slice, use the separate
[Phase 2 terminal runner](../riwork-nsh-terminal/README.md#reproduce-isolated-verification),
which exercises production tab mount/unmount and host close/quit paths under
output pressure and records the detach/child-exit/free sequence.

The root lockfile is regenerated for integration. GPUI parity source and tests
remain uncompiled and undeleted under the acceptance §4.3 embargo. Passing this
subset does not claim M5 or completion of the remaining terminal slices.
