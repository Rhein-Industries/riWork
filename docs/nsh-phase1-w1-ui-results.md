# W1 workspace UI results

Task `71ac437e-38de-4861-807a-bf81e4e44e47`, branch `nsh-p1-fix-workspace`, 2026-10-27.
Run directory: `/Users/dominik/orca/projects/riWork-nsh-p1-fix-workspace`.
The [aggregate W1 report](nsh-phase1-w1-results.md) owns complete acceptance dispositions, including the remaining host restart limitation in P1-LAY-07. This document records the TSX/headless route evidence.

**Final result: 46 tests passed, 0 failed (32.9 seconds), exit 0.** Strict TypeScript checking and `git diff --check` also exit 0. The original App/model/keyboard/idle/Edit-menu regressions remain passing alongside the new workspace routes.

The workspace uses one persistent native DockArea with `compact={false}`. Pane locks are core state, never `DockArea.locked`. A typed alias preserves the native component value while exposing the opt-in properties of the pinned local nsh-base patch. It introduces no JS wrapper or second component registration.

Pane controls and keyboard commands now route through LayoutSession. Shared membership/status delivery, menu/dialog/drag freezing, per-group moves, title updates, local close choices, default/gather/focus presentation, and project switching install the authoritative returned projection. The fixed suffix keeps pane controls outside scrolling native tabs. Header sizing follows interface text size; menu fallbacks and main-marker thresholds follow the GPUI rules. Shared and classic tab selection reveal their selected header cells.

**D-COPY-1 remains open.** `CloseDialog.tsx` keeps the current shipping body verbatim. No new copy decision is implied.

## Executed UI routes

| Acceptance | Executed route |
|---|---|
| P1-PANE-01/02 | Gamma default navigation and one fake shell; Alpha split/new selection/focus exit; Beta failed create leaves pane contents unchanged and shows the provider error. |
| P1-PANE-03/04 | Actual native divider drag and second-open projection equality; locked pixel resize after the two-second first-draw guard. Core/Headless extent/clamp tests are in the pane report. |
| P1-PANE-05/06 | Exact locked tab/pane close notices; hidden close mark; main routing and focus exception. |
| P1-PANE-07/08/09 | Ordered close-pane inheritance and active main; gather/default notices, repeat default and host command; empty-main fixture copy. |
| P1-PANE-10/11/12 | Focus on/off and centered presentation preserve the saved tree after the startup guard; body activation routes new shells with no main; narrow-pane fallback menu, disabled deferred rows, metrics. |
| P1-TAB-02/03 | Existing Files panel stays singleton; Dashboard calls the global host route without becoming a tab; natural tooltip; tab selection shows the selected surface only. |
| P1-TAB-04, P1-STRIP-05 | Exact Ask body, inert backdrop, Escape cancellation; creating then detaching a fake shell removes its shared tab from two open windows. Native tests prove Exit records and worker/store behavior. |
| P1-TAB-05 | The fake external shell appears through subscription delivery without changing the active pane or selected surfaces. |
| P1-STRIP-01/02 | Alpha panels precede shared session order; Beta unreadable inventory uses classic bars; `Chat 2, waiting` and session accessibility labels; status/close/panel metrics. Unknown activity draws no dot. |
| P1-STRIP-03 | Right click, Shift-F10 and dedicated Menu-key host command expose equivalent menus; boundary move disabled; rename input is persistent and a peer sees the title. Native tests cover the AppKit Menu-key translation. |
| P1-STRIP-04 | Busy overflow at 800, no overflow at 1920 after dismissal, menu retained while open across resize, Up/Enter wrap, number shortcut reveal, focus-mode dismissal; minimal reveal calculations after width/position/rename changes. |
| P1-DRAG-01/02/03 | Real native cross-group/edge drags; shared-strip left/right moves publish order to a second open; no cross-window transfer/receive calls. Native accepted edge zones are 35%; root/top-window drop targets remain the documented upstream deviation. |
| P1-DRAG-04 | Scoped `mount 1` checks through group move, edge split, focus, resize, and each carried locked tab after project switch. Shown carried placeholders also report `visible: true`. |
| P1-LAY-03/05/08/09 | All 13 GPUI-written L fixtures render golden IDs and selections, including unsafe-ID remapping; empty/MOVED_NOTICE copy; no global open during migration; carried tabs retain their component mounts. Native reports own byte/mtime/save/opaque-field guarantees. |

Tab-key serialization/pending chats, damaged-home recovery, save timing and trigger barriers are native checks in the companion reports; UI test success alone is not substituted for those checks.

## Reproduction

All app runs use copies of generated fixture homes and the fake session adapter. No live home, tmux session, installation, merge, push or task-status mutation was used. No GPUI source/test deletion was made.

The final fixture source is `/private/tmp/riwork-w1-layout-persistence/acceptance-layouts/home`, generated by the feature-gated core writer. It has canonical UUID aliases, ordered Alpha sessions, Chat 2 waiting, short Busy S01–S24 titles, and all L fixtures. The exact final setup and command are:

```sh
mkdir -p /tmp/riwork-w1-ui-pass/runtime /tmp/riwork-w1-ui-pass/tmux
cp -R /private/tmp/riwork-w1-layout-persistence/acceptance-layouts/home /tmp/riwork-w1-ui-pass/home
env RIWORK_HOME=/tmp/riwork-w1-ui-pass/home RIWORK_RUNTIME_DIR=/tmp/riwork-w1-ui-pass/runtime RIWORK_NSH_FAKE_SESSIONS=/tmp/riwork-w1-ui-pass/home/fake-sessions.json TMUX_TMPDIR=/tmp/riwork-w1-ui-pass/tmux CARGO_TARGET_DIR=/Users/dominik/orca/projects/riWork-nsh-p1-fix-workspace/target NSH_CACHE=/tmp/riwork-p1-w1-nsh-cache CARGO_BUILD_JOBS=4 CARGO_NET_OFFLINE=true /tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh test apps/riwork --junit /tmp/riwork-w1-ui-pass/all.xml > /tmp/riwork-w1-ui-pass/all.log 2>&1
/Users/dominik/Library/Caches/nsh/typescript-7.0.2-darwin-arm64/package/lib/tsc -p apps/riwork/tsconfig.json --noEmit > /tmp/riwork-w1-ui-pass/tsc.log 2>&1
```

The final directory is a pristine copy for this complete suite. Its source fixture home is retained unchanged for reproduction. Type checking exits 0.

## Development runs

Failed intermediate runs are retained as failure evidence, not counted as successful verification:

| Log | Outcome and correction |
|---|---|
| `/tmp/riwork-w1-ui-run1/all.log` | 34/41 passed. Identified resize startup guard, async subscription callback context, stale fixture inventory ordering/migration expectation, and an incorrect failed-create assertion comparing notice-dependent pixel geometry. |
| `/tmp/riwork-w1-ui-run2/all.log` | Runner initialization failed: exporting the same DockArea from two UI sets registered it twice. Removed the duplicate export and used the native-value typed alias. The orphaned development CLI/runner were terminated by their recorded PIDs. |
| `/tmp/riwork-w1-ui-run3/smoke.log` | 2/2 passed after the alias: pane menu and metrics. |
| `/tmp/riwork-w1-ui-run4/all.log` | 41/46 passed. Identified missing pan-start coordinates, an unbound signal setter in rename, a consuming Popover trigger handler, ambiguous divider selector, and JSON property-order comparison in the test. |
| `/tmp/riwork-w1-ui-debug2/debug.log` | Focused `--filter shared`: 5/5 passed, including two-window hide, persistent shared drag order, rename and waiting-chat accessibility. Temporary diagnostic logging was removed. |
| `/tmp/riwork-w1-ui-final/all.log` | 44/46 passed. Corrected the second window to use a unique window ID for the same project. The remaining native tab hint did not render on the Headless route; an explicit anchored one-shot hover hint now renders and clears on leave/menu/drag/selection, with no duplicate native tab tooltip. |
| `/tmp/riwork-w1-ui-final/tooltip2.log` | Focused `--filter Dashboard`: 1/1 passed, including the natural Projects tooltip after its hover delay. |
| `/tmp/riwork-w1-ui-final/overflow.log` | The old trigger handler still consumed the Popover press. Removing that handler lets the native trigger own opening; initialization moved into onOpenChange. |
| `/tmp/riwork-w1-ui-tests/check-adapter.log` | `nsh check apps/riwork` exits 0 with the local native-value alias. The only warnings are missing Rust API doc comments. |

For each development NSH run the environment command above was used with the corresponding run-directory prefix and fixture copy; the shared probe adds `--filter shared`, the smoke adds `--filter P1-PANE-12`, and the overflow probe adds `--filter overflow`.

Final artifacts:

| File | SHA-256 |
|---|---|
| `/tmp/riwork-w1-ui-pass/all.log` | `c7af7e05fc358af4167391f135cd511741da1361b329b9eb1a6ab92ce5e3a09e` |
| `/tmp/riwork-w1-ui-pass/all.xml` | `03a0e9e1292f8af3599b9e5dcf97c1e95ca81e93a41691a14e1c91cbf2694f7b` |
| `/tmp/riwork-w1-ui-pass/tsc.log` | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` (empty successful output) |

The delayed-delivery route was also rerun alone in a new process/home so its first provider access starts the fake's 300 ms appearance timer: **1/1 passed, exit 0** (test 346 ms, complete command 3.0 seconds).

```sh
mkdir -p /tmp/riwork-w1-ui-adoption/runtime /tmp/riwork-w1-ui-adoption/tmux
cp -R /private/tmp/riwork-w1-layout-persistence/acceptance-layouts/home /tmp/riwork-w1-ui-adoption/home
env RIWORK_HOME=/tmp/riwork-w1-ui-adoption/home RIWORK_RUNTIME_DIR=/tmp/riwork-w1-ui-adoption/runtime RIWORK_NSH_FAKE_SESSIONS=/tmp/riwork-w1-ui-adoption/home/fake-sessions.json TMUX_TMPDIR=/tmp/riwork-w1-ui-adoption/tmux CARGO_TARGET_DIR=/Users/dominik/orca/projects/riWork-nsh-p1-fix-workspace/target NSH_CACHE=/tmp/riwork-p1-w1-nsh-cache CARGO_BUILD_JOBS=4 CARGO_NET_OFFLINE=true /tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh test apps/riwork --filter P1-TAB-05 --junit /tmp/riwork-w1-ui-adoption/adoption.xml > /tmp/riwork-w1-ui-adoption/adoption.log 2>&1
```
