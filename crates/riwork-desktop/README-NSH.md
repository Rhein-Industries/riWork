# Phase 1b: AppKit NSH host

The single `riwork` executable still calls `riwork_cli::cli::run_cli` before any
GUI initialization. CLI, MCP and `chat serve` use the unchanged CLI/engine code,
including the original output, errors and exit status. `src/main.rs` and the
older UI modules are uncompiled parity references; GPUI is no longer a dependency
of this binary. This is the host foundation, not the completed RiWork UI port.

## Frozen seams

- `docs/nsh-contracts.md` §0 and §2 are the source of truth. The host reuses the
  AppKit/AppView topology proved in `spikes/host-multiwindow`.
- One `NSWindow`, `AppView`, UUID v4 and `HostChannel` per window. All views load
  the same source bundle, distinguished only by schema-1 `host.params`.
- `set_ui_sets([nsh_base::registry(), native::registry()])` runs once. **P1a is
  absent at this branch's base (`ea0f2f5`)**: `nsh_native_stub.rs` registers the
  frozen `riwork/native` specifier without domain exports. When P1a lands, add
  `riwork-nsh-native = { path = "../riwork-nsh-native" }` to the macOS dependencies
  and replace the stub alias with `use riwork_nsh_native as native`.
- The shared `Components` registry is created once. P1c can register
  `riwork.terminal` there before the first AppView. This host has no Ghostty
  component, terminal attachment or terminal persistence yet.
- `RIWORK_NSH_BUNDLE=/absolute/path/bundle.js` loads a built NSH source bundle;
  its directory supplies assets and split chunks. With no override, the embedded
  small shell renders each window's identity. Both paths declare only
  `window.*`, `app.*`, `system.*` host capabilities. No filesystem/process/network
  capabilities are granted. `nsh/storage` is transient per VM; NSH caches stay
  below `RIWORK_HOME/nsh`.
- Engine-thread host callbacks only queue main-thread messages. Store reads,
  runtime registry writes and forwarding run on the service/socket threads.
  AppKit uses its event loop without a frame-polling loop.

## Implemented host behavior

`window.open`, `route`, `placement`, `list`, `focus`, `close` and `setDockInfo`;
project windows and a global singleton; per-window `route`, `project.open`,
`global.show`, `command`, `window.state`, and `window.willClose` events. A window
is stopped/released on native or channel close, including NSH's own Window.close.
The last window closes the process.

The host owns RiWork/Edit/View menus and the Dock menu using the public
`nsh/desktop` Rust model. The menu accelerators dispatch commands to the key
window. Cmd-W emits `closeTab`, Cmd-Shift-N opens a project window, standard Edit
selectors offer native responders first, and font keys offer native responder
key equivalents before UI text commands. The future native terminal must supply
its Ghostty key-equivalent handler; that path is not claimed as verified here.
Window-local title/chrome comes through `nsh/desktop`; the host supplies full-size
transparent content, minimum 640×400, cascade and traffic-light placement.

A per-home `run/desktop.lock` plus `run/desktop.sock` forwards later GUI launches
before bundle loading. A restoring replacement has a separate handoff gate so
ordinary launches cannot take ownership during replacement handoff. Shutdown
stops forwarding but retains ownership until AppViews stop. Runtime registration,
window publication, heartbeat, engine restore-ticket validation/watchdog, and a
per-window `app.restoreReady` barrier are implemented. Existing runtime formats
are unchanged, including display-local top-left saved geometry (converted at
the AppKit boundary).

## Deliberate integration points

P1a must provide Rust LayoutSession/current-window snapshots and draft/layout
flush hooks before restart can be enabled. `app.restart` and external runtime
reload requests currently fail explicitly with `unavailable`, preserving the old
process. The embedded shell refuses restore startup because it cannot restore
layouts. A supplied real application bundle receives the restore payload and is
responsible for posting `app.restoreReady` only after its native session restores.
This is the requested handshake skeleton, not a complete reload implementation.

System panels, pasteboard, notifications, Quick Look, sharing, fonts/permissions,
remote project resolution, forced appearance and process domain services are
explicit future integration points. Their named host calls reject as unavailable;
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

The assignment restricts the commit to this host crate. Cargo resolves the new
NSH dependencies while testing, but the repository-root Cargo.lock is left to
the toolchain/integration owner and is not part of this commit. Regenerate that
lock in the integration branch before using `--locked` builds.
