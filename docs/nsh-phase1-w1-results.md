# M5 W1 workspace fix results

Task `71ac437e-38de-4861-807a-bf81e4e44e47`; branch `nsh-p1-fix-workspace`, based on `ed1b456`. Sources are `nsh-phase1-m5-results.md`, `nsh-phase1-m5-workspace-results.md` and acceptance §§5.5–5.8. This report records new W1 evidence without rewriting the historical M5 failures.

**34 of the 35 previous failures are closed.** Sections 5.5–5.8 now have **35 PASS / 1 FAIL**, including the retained P1-LAY-10 pass. All **13 baseline PASS items remain passing**. The assignment is not fully closed because P1-LAY-07 requires the host restart integration described below.

The workspace implements pane split/close/lock/main/focus, shared tabs and menus, detach/exit choice, native drag with stable panel identity, persistence, restore reconciliation and reopening GPUI-written layouts. `compact={false}` remains explicit. Pane locks never use `DockArea.locked`. No GPUI source or test was deleted. No merge, push, install or task-status change was performed.

**P1-LAY-07 remains FAIL as a complete acceptance item.** The workspace's debounce and flush barriers pass, including project switching and session drop, and the host's close/quit barriers pass. The existing host still rejects `app.restart` and CLI reload requests with `RESTART_PENDING`. The required RLD-01 replacement-process run therefore cannot pass in this workspace wave. Adding an unused or flush-only restart hook would not prove it. P1b must connect its existing `HostRuntime::launch_reload`/`reload_ready` APIs to the host restart state machine and run the replacement/failure-recovery routes.

**D-COPY-1 remains open.** The close dialog retains the existing wording. This change does not approve or substitute new copy.

## Acceptance evidence

The detailed reports contain individual test names and exact commands:

- [Pane/model/native gesture evidence](nsh-phase1-w1-pane-results.md).
- [Persistence, bridge and fixture evidence](nsh-phase1-w1-layout-results.md).
- [UI routes, 46/46 tests, strict TypeScript and exact commands](nsh-phase1-w1-ui-results.md).

Every row below is **PASS** unless explicitly marked **FAIL**.

| Item | W1 evidence |
|---|---|
| P1-PANE-01 | Real LayoutSession default initialization, legacy navigation initialization, Gamma UI creates one fake shell; restored empty main creates none. |
| P1-PANE-02 | Fake create/split/rollback; right/down second-half placement; native UI split and failed-create route. |
| P1-PANE-03 | Real Headless native resize output; five-point measured divider on both axes; ratio clamp; UI save/reopen. |
| P1-PANE-04 | Core locked-extent matrix, measured pane extents within 0.5 px, explicit first-two-seconds guard and focus/resize regressions. |
| P1-PANE-05 | Core moved lock successor, exact close refusals/internal removal bypass, width-dependent lock control. |
| P1-PANE-06 | Three moved routing successors, main validity, main/active routing through fake shell creation and focus mode. |
| P1-PANE-07 | Three moved close successors, native sentinel gestures, last-pane/main retention and ordered inheritance. |
| P1-PANE-08 | Two moved gather successors plus UI command/menu and exact notices. |
| P1-PANE-09 | Five moved Default Layout successors plus UI repeat/host-command routes and opaque-tab relocation. |
| P1-PANE-10 | Persistent DockArea, transient focus state in host snapshots, centered presentation and unchanged saved geometry on entering/leaving/resizing. |
| P1-PANE-11 | Body activation and subsequent shell routing with no main pane; selected surface focus. |
| P1-PANE-12 | Native fixed suffix keeps controls reachable during overflow; exact menu rows/shortcuts, disabled deferred rows, thresholds and metrics. |
| P1-TAB-01 | Core tab-key shapes; unsaved pending chat omitted from disk and resolved without changing the mounted panel key. |
| P1-TAB-02 | Selected-worktree/root cwd, existing panel selection, Dashboard host redirect, theme casing and natural tooltip. |
| P1-TAB-03 | Native active-index gesture; selection/visibility/save; shared and classic selected-tab reveal. |
| P1-TAB-04 | Ask/Detach/Exit, initial Detach focus, cancel/backdrop/width, worker detach, lock refusal; two windows observe hide, fake Exit records close. D-COPY-1 remains open. |
| P1-TAB-05 | Live project-only adoption with cap 16 and lock routing; selection unchanged; delayed fake delivery. |
| P1-STRIP-01 | Panels then shared order then carried views; Alpha shared strip and Beta unreadable-store classic fallback. |
| P1-STRIP-02 | Status/title hydration, role/name, dot/close/panel-cell metrics, waiting-chat fixture and read-only desktop evidence. |
| P1-STRIP-03 | Right-click, Shift-F10 and dedicated Menu key host route; persistent rename and adjacent parent-group moves. |
| P1-STRIP-04 | Overflow-only control, stable open-menu ownership, resize/selection/rename reveal and keyboard wrapping. |
| P1-STRIP-05 | Epoch/revision gate, monotonic frozen queue, shared Hide in a peer, owner-store updates and title priority. |
| P1-DRAG-01 | Real native group/reorder output plus shared-strip pan, ghost, shared Move persistence and selected identity. |
| P1-DRAG-02 | Four native edge drags and measured 34%/36% boundaries confirm accepted NSH 35% zones; no root/top-window targets. |
| P1-DRAG-03 | Actual host registration allowlist excludes tab.transfer/tab.receive; frontend route test. |
| P1-DRAG-04 | Mounted placeholder identity/visibility through group move, edge split, focus, resize and carried project switch. |
| P1-LAY-01 | Named core/fixture oracles, all 13 L fixtures, three extents, normalized projection and ratio tolerances. |
| P1-LAY-02 | Eight required gesture routes use real Headless onLayoutChange, one projection per gesture, fallback diagnostic and sentinel cleanup. |
| P1-LAY-03 | All named reopen/edit cases, unchanged bytes/mtime without edit, core writer equality, sibling/opaque raw preservation; UI golden matrix. |
| P1-LAY-04 | All three damaged homes, exact default notice suffix, no open-time overwrite, corrupt backup/newer refusal. |
| P1-LAY-05 | Legacy order/selection, Dashboard/global-chat/live-global-shell pruning, exact migration notice, no global-window open. |
| P1-LAY-06 | Dead/detached filtering, adoption, failure/retry preserving positions; single-flight initial shell creation and recovery. |
| P1-LAY-07 | **FAIL overall:** workspace ManualClock/flush tests and host close/quit pass; host restart/RLD-01 integration remains unavailable. |
| P1-LAY-08 | Safe ID remap, opaque remap composition across reload/default, counter adoption and stable pending-chat resolution. |
| P1-LAY-09 | Outgoing flush, locked carry/collision recovery, per-project reset, same-project no-op, async generation/switch guards; UI carried mount identity. |
| P1-LAY-10 | **Existing PASS retained:** only LayoutStore writes production layouts.json; core reload equals the saved ProjectLayout. |

## Verification and isolation

All application runs use generated temporary homes, explicit HOME/RIWORK_HOME/runtime/tmux directories and the debug-only fake-session adapter. No tmux socket, live user home, real shell or chat session is used by the new workspace routes. Existing CLI/core/engine suites retain their isolated test backends and existing ignored tests.

The pinned NSH 0.3.0 source patch is local to this repository. It adds opt-in five-point divider geometry, a fixed pane-control suffix and classic-tab reveal. Published source and tests remain present, with provenance and exact patch in [vendor/RIWORK.md](../apps/riwork/vendor/nsh-base/RIWORK.md). No global Cargo cache or installed toolchain was changed. A typed alias accounts for the installed CLI's older declarations without a second registration or remounting wrapper.

Exact native, build, CLI, GUI commands, environment variables, exits and log hashes are in the adjacent JSON record. The UI and component reports record their isolated commands separately. Principal native commands:

```sh
cargo test -p riwork-core -p riwork-engine -p riwork-cli -p riwork-desktop -p riwork-nsh-native -p riwork-nsh-workspace --features riwork-nsh-native/test-support --locked --offline
cargo test -p riwork-desktop -p riwork-nsh-native -p riwork-nsh-workspace --features riwork-nsh-native/test-support --locked --offline -- --test-threads=1
sh apps/riwork/check-polling.sh
python3 crates/riwork-desktop/verification/phase1_integration.py build --root /tmp/riwork-w1-final --target-dir /Users/dominik/orca/projects/riWork-nsh-p1-fix-workspace/target
python3 crates/riwork-desktop/verification/phase1_integration.py parity --root /tmp/riwork-w1-final --target-dir /Users/dominik/orca/projects/riWork-nsh-p1-fix-workspace/target
python3 crates/riwork-desktop/verification/phase1_integration.py gui --root /tmp/riwork-w1-final --target-dir /Users/dominik/orca/projects/riWork-nsh-p1-fix-workspace/target --workspace-fixture --linger-seconds 300
```

The first six-crate run passed CLI **175**, core **285** and engine **1206** tests, then caught a duplicate DockArea registration in the experimental declaration workaround. That workaround was removed. The corrected integration run passed **208/208**, including the actual 60-second quiet-home check, native restore test, AppKit tests and all workspace/persistence tests. An intermediate rerun caught an existing CLI chat-socket cleanup timing failure; the serial rerun passed all six CLI contract tests. The earlier failures remain recorded rather than counted as successful runs.

After the final classic-tab reveal change, the native gesture suite passed **13/13**, including the new measured scroll regression; the preserved upstream dock/resizable suites passed **22/22** and **11/11**. The final UI suite passed **46/46** in 32.944 seconds, and delayed adoption passed separately in a fresh process. Strict TypeScript, Rust formatting, polling lint and whitespace checks passed. Counts describe their individual runs and are not summed as disjoint tests.

The final build is `/private/tmp/riwork-w1-final/build-s7gnaac7/manifest.json`. Its two-window Native-theme launch and cleanup audit passed at `/private/tmp/riwork-w1-complete/gui-zgxi5mnx/manifest.json`; its exact final command was:

```sh
python3 crates/riwork-desktop/verification/phase1_integration.py gui --root /tmp/riwork-w1-complete --target-dir /Users/dominik/orca/projects/riWork-nsh-p1-fix-workspace/target --bundle /tmp/riwork-w1-final/bundle/bundle.js --workspace-fixture --theme native --linger-seconds 60
```

The runner stopped only its owned fixture process (PID 6939) after the read-only captures. It recorded no disallowed writes or tmux files/sockets. Graceful close/quit persistence is covered by the native tests; GUI cleanup uses SIGTERM and is not counted as a graceful lifecycle test.

## Existing PASS items

| Baseline item | Regression evidence |
|---|---|
| P1-PROC-01 | Unchanged CLI sources; five-command byte/exit parity and CLI contracts. |
| P1-PROC-02 | Startup-environment test. |
| P1-PROC-03 | Invalid-home/dispatcher exit and no GUI initialization contract tests. |
| P1-PROC-06 | Two fixture windows registered under one PID; allowed-write/tmux audit. |
| P1-RLD-03 | Runtime publish pacing and busy-lock tests. |
| P1-RLD-04 | Native restored payload durability; app restoreReady remains exactly once. |
| P1-WIN-01 | Real AppKit chrome/style/minimum-size test and read-only Cua screenshots. |
| P1-LAY-10 | Bidirectional LayoutStore compatibility. |
| P1-MENU-01 | Host menu model and real AppKit menu tests. |
| P1-MENU-02 | Standard Edit selectors and NSH Copy-without-selection test. |
| P1-THM-06 | Isolated GUI audit creates no appearance.json or Ghostty configuration; startup environment remains scrubbed. |
| P1-POLL-02 | Native real 60-second quiet-home test and NSH virtual 60-second unchanged-host-work test. |
| P1-POLL-03 | Executed forbidden-polling lint, zero matches. |

The accepted drag-geometry difference remains 35% native edge zones instead of deferred 23% pane/12-point window-edge zones. Middle-click remains the documented upstream gap. Full theme, global-window and restart acceptance outside W1 is not promoted by this report.

## Read-only desktop evidence

RiWork's **cua-driver MCP** was the only desktop provider. Accessibility and Screen Recording were granted. No desktop input calls were made. Every read names the isolated fixture PID and window. Seven generated theme configurations were launched with `--workspace-fixture --theme <theme>`: `native`, `ri_work`, `ghostty`, `catppuccin`, `tokyo_night`, `gruvbox_light`, `hermes`. All seven launch/write/tmux audits passed and each registered Alpha and Beta under one PID. Exact process environments, arguments and cleanup outcomes are in the JSON record.

For each Alpha window the call was `get_window_state({pid, window_id, session:"w1-workspace", query:"waiting", max_elements:300, screenshot_out_file:"/tmp/riwork-w1-final/<theme>.png"})`. The explicit Native-window `verify_state` predicate `element.exists=true, selector.label_contains=", waiting"` returned **satisfied**. These results and exact targets are retained in [the Cua record](nsh-phase1-w1-cua.json).

The screenshots show the waiting dot, selected close mark, flat shared cells, pane controls and divider without clipping. All seven settings configurations produce the same current W1 strip rendering; the identical screenshot hashes are recorded, not presented as seven distinct palettes. Full theme palette integration remains outside W1. Final-build captures: [shared strip](nsh-phase1-w1-strip.png) and [classic tabs](nsh-phase1-w1-classic.png).
