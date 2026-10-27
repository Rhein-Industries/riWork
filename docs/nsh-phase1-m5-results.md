# NSH Phase 1 M5 acceptance results

**Phase 1 is not accepted.** All 101 section-5 items have a disposition: **13 PASS, 87 FAIL, 1 REPORT-ONLY**. Missing required implementations, missing test targets, zero-test filters and unavailable mandatory routes count as failures. Passing component suites do not close their whole acceptance items. The substantial missing Phase 1 features remain unfixed; this is an acceptance execution and bounded-fix handoff, not an exit sign-off.

```yaml
assignment: f6975181-1a9a-41ea-a500-87e47abc1ad6
branch: nsh-p1-m5
base_commit: 17e89d9
verification_environment: isolated fixtures only; macOS 26.2 arm64; headless Rust/NSH, real AppKit, read-only Cua on fixture PIDs
live_windows_or_state_touched: none
task_status: unchanged; owned by orchestrator
merge_push_install: none
commit_sha: supplied in the final handoff (the commit containing this report)
```

The [machine record](nsh-phase1-m5-results.json) holds exact argv, cwd, environment, exit code, output paths/hashes, Cua arguments/results and every item. The command catalog below is part of this report. Raw command output and screenshots are retained under [m5-evidence](../crates/riwork-desktop/verification/m5-evidence/); [artifact-index.json](../crates/riwork-desktop/verification/m5-evidence/artifact-index.json) maps temporary paths to committed copies. Supplemental owner analyses: [host](nsh-phase1-m5-host-results.md), [native](nsh-phase1-m5-native-results.md), [workspace](nsh-phase1-m5-workspace-results.md).

## Counts by acceptance section

| Section | PASS | FAIL | Report-only | Total |
|---|---:|---:|---:|---:|
| 5.1 Process and CLI entry (P1b) | 4 | 3 | 0 | 7 |
| 5.2 Restart, reload and restore (P1b with P1c) | 2 | 2 | 0 | 4 |
| 5.3 Windows and chrome | 1 | 9 | 0 | 10 |
| 5.4 Global window | 0 | 10 | 0 | 10 |
| 5.5 Panes, splits, locks, main pane and focus (P1c) | 0 | 12 | 0 | 12 |
| 5.6 Tabs, strip, menus and the close dialog (P1c, with P1a `Tabs`/`Sessions`) | 0 | 10 | 0 | 10 |
| 5.7 Drag and panel identity (P1c) | 0 | 4 | 0 | 4 |
| 5.8 Layout persistence, restore and reopening GPUI layouts (P1c) | 1 | 9 | 0 | 10 |
| 5.9 Menus and keys | 2 | 7 | 0 | 9 |
| 5.10 Themes, appearance and interface text | 1 | 9 | 0 | 10 |
| 5.11 Status bar (P1c, with P1a `Settings`) | 0 | 5 | 0 | 5 |
| 5.12 Polling, idle work and performance | 2 | 2 | 1 | 5 |
| 5.13 `riwork/native` contract items (P1a) | 0 | 5 | 0 | 5 |

## Changes and reruns

- Restored frozen CLI JSON insertion order lost with the GPUI dependency graph; checked the five prescribed commands against a real ea0f2f5 GPUI desktop build, and added a byte golden that does not share the candidate graph.
- Kept deferred native menu rows disabled through NSMenu validation and guarded dispatch; declared inert deferred chords and tab-cycle aliases. Full R-DEFER menu/key parity remains failed where absent.
- Stopped empty session inventories from waking every two seconds; watcher discovery arms the fallback, removing the last shell disarms it. A regression failed before the fix; 60 actual quiet seconds now produce zero deliveries.
- Kept DockArea mounted across layout updates. The former condition remounted panels on selection; mount/visibility diagnostics now prove identity for selection, cross-group drag and resize. Other required identity scenarios remain unimplemented.
- Corrected native titlebar coordinates after AppKit layout, added control AX labels, retained runtime publication deadlines on skipped locks, and explicitly unlocked all desktop/handoff ownership paths before duplicated descriptors can outlive their owner.

The first CLI reference attempt failed because PATH selected Zig 0.17; the already installed Zig 0.16 executable was reused, without installation. A combined suite overlapped the preserve_order rebuild and compared old/new feature graphs in the MCP byte test; that failed run is retained. Final frozen-source checks supersede it, as recorded below.

## Final verification

Final formatting and staged whitespace checks exited 0. Captured stdout/stderr remain byte-exact; scoped .gitattributes disable whitespace checks only for these raw output files. The final deletion-embargo comparison confirmed all 142 protected source files unchanged. All fixture GUI children were terminated; explicit-PID Cua cleanup verified zero remaining windows.

The final frozen-source six-crate Cargo suite exited 0: **1,740 passed, 0 failed, 187 ignored** across 35 result groups. Final NSH type-check exited 0 and its JUnit result was **18 passed, 0 failures/errors/skips**. These counts describe the available component suite, not the 101-item acceptance matrix. The final integrated binary and NSH bundle built successfully; dependency-tree checks found no GPUI or Ghostty path. Historical CLI parity matched all five prescribed stdout/stderr/exit triples. Final suite commands and raw output are in the catalog and machine record; [NSH JUnit](../crates/riwork-desktop/verification/m5-evidence/headless-ph_295w9-nsh.xml) is retained.

## Verification environment and reproducible entry points

Pinned NSH: `/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh`, version `0.3.0 (76fa8a7)`, SHA-256 `90b7633c80306407fc97b2051be00af868d4577281f918ff655c650a8a5a2e60`. Rust/Cargo 1.99.1; TypeScript `/Users/dominik/Library/Caches/nsh/typescript-7.0.2-darwin-arm64/package/lib/tsc`. Existing Zig: `/Users/dominik/.local/share/riwork/toolchains/zig-0.16.0/zig`. No tools installed.

Every command ran from this worktree unless its recorded cwd says otherwise. Build/test HOME is a disposable ignored `target/nsh-integration-home-*` directory (outside /tmp for folder-trust tests). RIWORK_HOME/runtime/config/tmux are private. GUI launches use a clean environment and `RIWORK_NSH_BACKGROUND=1`; every Cua call names its exact fixture PID. GUI cleanup is SIGTERM, never claimed as graceful quit proof.

```sh
ROOT=/tmp/riwork-p1-m5-f6975181
TARGET=/private/tmp/riwork-p1-integration-mkzwamt7/target
python3 crates/riwork-desktop/verification/phase1_m5.py probes --root "$ROOT" --target-dir "$TARGET"
python3 crates/riwork-desktop/verification/phase1_m5.py reference --root "$ROOT" --target-dir "$TARGET" --zig /Users/dominik/.local/share/riwork/toolchains/zig-0.16.0/zig
python3 crates/riwork-desktop/verification/phase1_integration.py build --root "$ROOT" --target-dir "$TARGET"
python3 crates/riwork-desktop/verification/phase1_integration.py parity --root "$ROOT" --target-dir "$TARGET" --reference "$ROOT/reference-target/debug/riwork"
python3 crates/riwork-desktop/verification/phase1_integration.py headless --root "$ROOT" --target-dir "$TARGET"
python3 crates/riwork-desktop/verification/phase1_m5.py fixture --root "$ROOT" --target-dir "$TARGET" --existing-appearance --linger-seconds 120
python3 crates/riwork-desktop/verification/phase1_m5.py ledger --root "$ROOT" --target-dir "$TARGET"
cargo fmt -p riwork-desktop -p riwork-nsh-native -p riwork-nsh-workspace --check
git diff --check
```

The prescribed p1-basic, p1-layouts, p1-restore and p1-theme generators all exit 101: the declared test-support feature/example is absent. The real GUI uses an explicitly mapped **two-project integration fixture**, generated through core writers. It is sufficient for the recorded startup/forwarding/chrome/write checks, not the missing restore/theme/layout scenario coverage.

## Write allow-list, carry-over ledger and decisions

Four real fixture launches were audited with SHA-256 **and mtime**, including removal detection. Changed files were only `nsh/desktop.handoff.lock` and `run/desktop.lock`; no disallowed writes, sessions.json, chats/ or tmux entry appeared. Three seeded appearance.json through the core publisher before the audit; it remained byte/mtime identical. The unseeded run never created it. No live RiWork application PID/window/state was read. A final Cua query for fixture PID 81359 returned zero windows after cleanup.

All **142 protected legacy desktop source files** remain byte-for-byte identical to ea0f2f5. No GPUI deletion occurred. Section 4.3 successors are **not complete**: the preserved source does not count as executed successor coverage. Native theme, strip, global, pane and key successors that are missing are failed in the item table. The deletion embargo remains active.

D-PERF-1 is applied as adopted: P1-PERF-01 is report-only and introduces no wall-clock failure. P1-PERF-02 structural guards still fail for missing gesture counters. D-THEME-1, D-PROC-1 and D-COPY-1 remain open; no decision was invented. The separate pinned-runtime teardown gate still controls Phase 2. Section 9 checklists are not signed off.

## Performance report (D-PERF-1)

| Measurement | Result | Limit |
|---|---|---|
| Archived GPUI release Busy frame | median 11.646792 ms; p90 12.014917 ms; 41 frames; exit 0 | Exact archive and RIWORK_FRAME_TABS=24; under build contention, not a matched quiet five-run sample |
| Integrated NSH release Busy median / p90 | Not measured | Required settled monitor and Busy/120-project/Native fixture absent |
| Supplemental NSH debug two-window idle, 20.012 s | ps CPU time 0:00.50 to 0:00.50; 0.0% at ps resolution | Concurrent builds; not a release or matched GPUI comparison |
| Historical GPUI idle reference | 0.65% of one core (acceptance plan) | Historical context only; no comparison/speedup claim |

## Every section-5 item

Each command ID resolves to its exact invocation and result below. Multiple routes are conjunctive: a failed/missing route keeps the whole item failed. For a missing file/feature, the recorded setup/probe is the failure; no test body is claimed to have executed.

### 5.1 Process and CLI entry (P1b)

| Item | Result | Commands | Evidence and remaining failure |
|---|---|---|---|
| P1-PROC-01 | PASS | `C136`, `C256`, `C257`, `C258`, `C259`, `C260`, `C261`, `C262`, `C263`, `C264`, `C265`, `C266`, `C267`, `C268`, `C269`, `C270`, `C271`, `C272`, `C273`, `C280`, `C281`, `C283`, `C284`, `C285` | Host: CLI startup parser passes. Review of nsh_main.rs proves run_cli precedes any host/AppKit/NSH initialization. Root owns five-command historical cli-diff. Historical ea0f2f5 GPUI executable built with existing Zig 0.16. Initial settings JSON byte-order failure fixed by preserving serde_json insertion order. Final five exact command comparisons match stdout, stderr and exit code; regression golden is from the historical executable. |
| P1-PROC-02 | PASS | `C156` | Host: Exact production scrub function passes in a dedicated single-thread process, avoiding mutation of the parallel Rust test environment. |
| P1-PROC-03 | PASS | `C209`, `C216`, `C223`, `C230`, `C288`, `C289` | Final invalid-home command exits 2 with riwork: prefix. Immediate pgrep -f on only the fixture path exits 1: no orphan fixture process. |
| P1-PROC-04 | FAIL | `C157`, `C158`, `C159`, `C210`, `C217`, `C224`, `C231`, `C274`, `C290`, `C291`, `C292`, `C293`, `C294`, `C295`, `C296`, `C297`, `C298`, `C299`, `C300`, `C301`, `C302`, `C303`, `C304`, `C305`, `C306`, `C307`, `C308`, `C309`, `C310`, `C311`, `C312`, `C313`, `C314`, `C315`, `C316`, `C317`, `C318` | Host: All three host successors pass (canonical project reuse, vanished root error, cwd fallback). Root fixture proves missing-directory invocation is rejected by CLI, so the required fallback notice is not accepted; HOME/dot fallback is absent. |
| P1-PROC-05 | FAIL | `C160`, `C161`, `C162`, `C211`, `C212`, `C218`, `C219`, `C225`, `C226`, `C232`, `C233`, `C290`, `C291`, `C292`, `C293`, `C294`, `C295`, `C296`, `C297`, `C298`, `C299`, `C300`, `C301`, `C302`, `C303`, `C304`, `C305`, `C306`, `C307`, `C308`, `C309`, `C310`, `C311`, `C312`, `C313`, `C314`, `C315`, `C316`, `C317`, `C318` | Host: All host ownership/forward/stale-lock/replacement successors pass. Explicit unlock-on-drop guard releases duplicated/inherited file descriptions; root owns real forward/instances proof. FAIL whole item: forwarding and one-PID/two-window checks pass, but output behavior depends on D-PROC-1, which remains explicitly open. Acceptance is withheld pending that decision; this run does not choose a new CLI contract. Final real fixture PID 81359: two distinct Alpha/Beta runtime windows, one registered PID; allowed lock writes only; no tmux entries; preexisting appearance.json SHA-256 and mtime unchanged. Cleanup SIGTERM exit -15. |
| P1-PROC-06 | PASS | `C137`, `C212`, `C219`, `C226`, `C233`, `C290`, `C291`, `C292`, `C293`, `C294`, `C295`, `C296`, `C297`, `C298`, `C299`, `C300`, `C301`, `C302`, `C303`, `C304`, `C305`, `C306`, `C307`, `C308`, `C309`, `C310`, `C311`, `C312`, `C313`, `C314`, `C315`, `C316`, `C317`, `C318` | Host: Existing runtime registry test passes; root owns the live fixture two-window count. Final real fixture PID 81359: two distinct Alpha/Beta runtime windows, one registered PID; allowed lock writes only; no tmux entries; preexisting appearance.json SHA-256 and mtime unchanged. Cleanup SIGTERM exit -15. |
| P1-PROC-07 | FAIL | `C191`, `C278` | Host: One process set_ui_sets before opening windows; one service worker. Per-topic native watcher/hub proof is separate. Required native hubs route exits 101 (test-support/hubs absent); host registration review alone is insufficient. |

### 5.2 Restart, reload and restore (P1b with P1c)

| Item | Result | Commands | Evidence and remaining failure |
|---|---|---|---|
| P1-RLD-01 | FAIL | `C191`, `C213`, `C214`, `C220`, `C221`, `C227`, `C228`, `C234`, `C235`, `C276` | Host: RESTART_PENDING explicitly rejects app.restart and fails pending CLI reload requests while retaining the old process. |
| P1-RLD-02 | FAIL | `C138`, `C139`, `C140`, `C141`, `C142`, `C143`, `C144` | Host: Four engine choreography tests plus host readiness/idempotence/order checks pass. Required 20-second host confirmation test is absent; production restart is explicitly unavailable and no draft-flush hook is integrated. |
| P1-RLD-03 | PASS | `C145`, `C146` | Host: Existing nonblocking/change-only engine publishing test and fake-clock host due/retry successor pass. Busy registry skips now leave publication due; successful publication advances by two seconds. |
| P1-RLD-04 | PASS | `C125`, `C126`, `C051`, `C127`, `C040`, `C052`, `C147`, `C148`, `C149`, `C243`, `C026` | Workspace: Mapped App.test.tsx production restore confirms native payload installation before exactly one app.restoreReady; nonrestore test confirms none. layout_roundtrip verifies saved restore wins. Workspace component PASS; coordinator combines host runtime-window routes. Host: Host successors read current Rust LayoutSession on runtime publication and preserve display-local top-left geometry; engine validation test passes. Workspace owns restore payload precedence/readiness tests. Successor proof: crates/riwork-nsh-native/tests/native_headless.rs native_pushes_one_service_to_two_engines_and_stops_on_close_owner_and_engine_drop sends an untouched restore payload, proves restored_host.snapshot equality, calls the native host flush barrier and verifies LayoutStore.load equality before dropping the app; native-after-fix and final six-crate suite execute this real engine path. The workspace-only open test does not itself prove a disk save. |

### 5.3 Windows and chrome

| Item | Result | Commands | Evidence and remaining failure |
|---|---|---|---|
| P1-WIN-01 | PASS | `C186`, `C290`, `C291`, `C292`, `C293`, `C294`, `C295`, `C296`, `C297`, `C298`, `C299`, `C300`, `C301`, `C302`, `C303`, `C304`, `C305`, `C306`, `C307`, `C308`, `C309`, `C310`, `C311`, `C312`, `C313`, `C314`, `C315`, `C316`, `C317`, `C318` | Host: Real prohibited-activation AppKit objects pass full-size-content/transparent/minimum/chrome checks. Root owns final read-only Cua geometry proof. Standard controls now carry explicit AX labels; chrome coordinates are corrected after native titlebar updates. Initial Cua AX origin was (11,12), failed. After settled titlebar correction, final Close AX frame minus window origin is (9,7) for both windows, within +/-1 of (10,8); AppKit control frame itself is (10,8). Exact Cua Close query succeeds. |
| P1-WIN-02 | FAIL | `C150`, `C151`, `C211`, `C218`, `C225`, `C232`, `C291`, `C292`, `C293`, `C294`, `C295`, `C296`, `C297`, `C298`, `C300`, `C301`, `C302`, `C303`, `C304`, `C306`, `C307`, `C308`, `C309`, `C310`, `C312`, `C313`, `C314`, `C315`, `C316`, `C317`, `C318` | Host: Core opening/cascade test passes. Named host non-tooltip cascade test is absent, and ordinary opening_frame still selects fixed 1220x780 rather than settings-gated remembered size. Root verified only ordinary cascade. |
| P1-WIN-03 | FAIL | `C163`, `C164`, `C276` | Host: Both named zoom/fullscreen host tests are absent; real restored fixture is blocked by unimplemented restart. |
| P1-WIN-04 | FAIL | `C119`, `C120`, `C040`, `C152` | Workspace: Session has no remembered-size API; core invalid-size test passes. Full window_size bridge target absent. Host: Existing core legacy/invalid-size test passes; debounce/window-size bridge and settings wiring belong to workspace/native and remain absent. |
| P1-WIN-05 | FAIL | `C121`, `C040`, `C291`, `C292`, `C293`, `C294`, `C295`, `C296`, `C297`, `C298`, `C300`, `C301`, `C302`, `C303`, `C304`, `C306`, `C307`, `C308`, `C309`, `C310`, `C312`, `C313`, `C314`, `C315`, `C316`, `C317`, `C318` | Workspace: Mapped model title/dedup test passes; project title derives path basename, lacks Store-name/remote metadata and memoized reactive updates. |
| P1-WIN-06 | FAIL | `C122`, `C075`, `C040`, `C187`, `C277` | Workspace: No theme/fullscreen-controlled window-controls-island or metrics tests; separate 28 pt title row. Host: AppKit titlebar double-click preference check is absent (the harness refuses zero matching checks). Theme/chrome TSX/manual routes are owned by workspace/root. |
| P1-WIN-07 | FAIL | `C165`, `C166`, `C167` | Host: Named last-close/tooltip host checks and no-NSWindow tooltip AppKit check are absent. No substitute inferred pass is claimed. |
| P1-WIN-08 | FAIL | `C168`, `C169`, `C213`, `C214`, `C220`, `C221`, `C227`, `C228`, `C234`, `C235` | Host: Both real Rust close/quit flush successors pass (last accepted edit, every project and global state). Required RLD-01 end-to-end reload remains unavailable. |
| P1-WIN-09 | FAIL | `C123`, `C040`, `C170` | Workspace: No window.state listener. Existing default ignoring is inert, but named TSX event check absent. Host: Named state-change-only host check is absent. Source has structural state comparison, but review does not replace the host-unit route. |
| P1-WIN-10 | FAIL | `C124`, `C040` | Workspace: Cmd-Shift-N is not bound and no window.open is emitted. |

### 5.4 Global window

| Item | Result | Commands | Evidence and remaining failure |
|---|---|---|---|
| P1-GLB-01 | FAIL | `C039`, `C040`, `C171` | Workspace: Global entry only renders Global workspace; global.show has no subscription or selected chat tab. Host: Named singleton/no-window host test is absent; no substitute pass from source reading. |
| P1-GLB-02 | FAIL | `C041`, `C040`, `C153`, `C276` | Workspace: Core global size validation passes; global TSX never posts window.setDockInfo. Host/Cua title/size delegated. Host: All three core global-window persistence/size tests pass. Host still uses generic 1220x780 opening size rather than unconditional global 1100x760/remembered size; no restored global fixture. |
| P1-GLB-03 | FAIL | `C041`, `C040`, `C153` | Workspace: Global state adapter and view/filter/sort controls absent; no Dashboard persistence subscription. Host: Three global-window core tests pass, but no Dashboard native state module is registered and required view/filter/sort UI persistence route is absent. |
| P1-GLB-04 | FAIL | `C042`, `C043`, `C040`, `C154`, `C155`, `C276` | Workspace: Restore readiness exists, but restore.global is ignored and global chat selection is absent. Host: Existing engine global-kind restore test passes; host runtime-global test is absent and restart unavailable. |
| P1-GLB-05 | FAIL | `C044`, `C040`, `C276` | Workspace: Production global test proves no DockArea and no project session. Global-only disk audit remains required in coordinator fixture route. |
| P1-GLB-06 | FAIL | `C045`, `C040` | Workspace: Required segmented bar, labels, metrics and persistent switch are absent. |
| P1-GLB-07 | FAIL | `C046`, `C040` | Workspace: Global root does not bind Cmd-W or send window.close. |
| P1-GLB-08 | FAIL | `C047`, `C040`, `C172` | Workspace: Global key routing, window.list lookup and fallback notice absent. Host: Named Dock New Window host test is absent; project routing/fallback UI remains unverified. |
| P1-GLB-09 | FAIL | `C048`, `C040`, `C173` | Workspace: No TSX route event handler or Settings selection. Host: Named frontmost/minimized/open-by-id host route test is absent. |
| P1-GLB-10 | FAIL | `C049`, `C040` | Workspace: Global path replaces startup notice with Global workspace; no notice dismiss button. |

### 5.5 Panes, splits, locks, main pane and focus (P1c)

| Item | Result | Commands | Evidence and remaining failure |
|---|---|---|---|
| P1-PANE-01 | FAIL | `C050`, `C051`, `C040`, `C052` | Workspace: Default five-panel model exists, but Sessions.create is never called; no-layout shell and panels_initialized reconstruction absent. |
| P1-PANE-02 | FAIL | `C051`, `C040`, `C052` | Workspace: No split keyboard or create/rollback implementation. |
| P1-PANE-03 | FAIL | `C053`, `C054`, `C055`, `C040`, `C052` | Workspace: Core ratio tests and PaneTree clamp pass; actual Headless divider/persist route absent. Divider selector repaired; NSH overlay still 1 pt, not core 5 pt gap. |
| P1-PANE-04 | FAIL | `C056`, `C051`, `C040`, `C052` | Workspace: Core locked geometry and model resizing pass; 2 s guard and focus-mode invariance absent; named Headless/extents matrix missing. |
| P1-PANE-05 | FAIL | `C057`, `C058`, `C040`, `C052` | Workspace: Lock toggle exists; close refusal notices and width-dependent lock control absent. Ledger test not moved. |
| P1-PANE-06 | FAIL | `C059`, `C060`, `C061`, `C062`, `C040`, `C052` | Workspace: Main toggle exists; new-tab routing and focus behavior absent; ledger routing tests not moved. |
| P1-PANE-07 | FAIL | `C063`, `C064`, `C065`, `C055`, `C040`, `C052` | Workspace: Sentinel retention model tests pass; exact empty copy fixed; close-pane command and inherited-tab flow absent. |
| P1-PANE-08 | FAIL | `C066`, `C067`, `C040`, `C052` | Workspace: Gather model/menu exists; Cmd-Shift-M, pane-menu entry and required notices absent; ledger tests not moved. |
| P1-PANE-09 | FAIL | `C068`, `C069`, `C070`, `C071`, `C072`, `C040`, `C052` | Workspace: No Default Layout command/menu action or five moved ledger tests. |
| P1-PANE-10 | FAIL | `C051`, `C040`, `C052` | Workspace: No focus-mode presentation, centered mode, restore button, or state fields. |
| P1-PANE-11 | FAIL | `C073`, `C040`, `C052` | Workspace: Existing click test proves activation but required pane-* selectors and Cmd-T route absent. |
| P1-PANE-12 | FAIL | `C074`, `C075`, `C040`, `C052` | Workspace: Pane context menu has only lock/main; required full rows, disabled deferred entries, selectors and metrics absent. |

### 5.6 Tabs, strip, menus and the close dialog (P1c, with P1a `Tabs`/`Sessions`)

| Item | Result | Commands | Evidence and remaining failure |
|---|---|---|---|
| P1-TAB-01 | FAIL | `C076`, `C077`, `C051`, `C040`, `C052` | Workspace: Core persisted remote/chat shape tests pass; pending-chat identity and no-save test/implementation absent. |
| P1-TAB-02 | FAIL | `C078`, `C051`, `C040`, `C052` | Workspace: No shell creation, placeholder panel routing, Dashboard redirect, casing or tooltip route. |
| P1-TAB-03 | FAIL | `C055`, `C040`, `C052` | Workspace: Mapped native active-index and TSX tab selection/visibility tests execute; named full focus/scroll/persistence route not present. |
| P1-TAB-04 | FAIL | `C079`, `C040`, `C052` | Workspace: No close dialog, Tabs update, session close, lock refusal or close-key flow. |
| P1-TAB-05 | FAIL | `C051`, `C040`, `C052` | Workspace: No session subscription/adoption route, caps or lock filtering. |
| P1-STRIP-01 | FAIL | `C080`, `C040` | Workspace: Shared membership unavailable; frontend always renders pane tab groups. |
| P1-STRIP-02 | FAIL | `C075`, `C040` | Workspace: No shared strip status mapping, status accessibility label, close button or metrics route. |
| P1-STRIP-03 | FAIL | `C081`, `C040` | Workspace: Right-click opens pane lock/main only; rename/move/close shared-tab context menu absent. |
| P1-STRIP-04 | FAIL | `C082`, `C040` | Workspace: No All tabs overflow menu, selection reveal or keyboard menu focus flow. |
| P1-STRIP-05 | FAIL | `C083`, `C084`, `C085`, `C086`, `C087`, `C088`, `C089`, `C090`, `C091`, `C092`, `C093`, `C040` | Workspace: Core inventory tests pass; no Tabs subscription/revision gate or two-window hide delivery. |

### 5.7 Drag and panel identity (P1c)

| Item | Result | Commands | Evidence and remaining failure |
|---|---|---|---|
| P1-DRAG-01 | FAIL | `C094`, `C055`, `C040`, `C052` | Workspace: Existing real DockArea move into empty pane passes; shared Move publish, ghost styling and required full L-NESTED/second-open route absent. |
| P1-DRAG-02 | FAIL | `C055`, `C040`, `C052` | Workspace: Four-direction PaneTree move test exists, but no real Headless edge-drop test or measured drop fraction. |
| P1-DRAG-03 | FAIL | `C095`, `C052`, `C174` | Workspace: Host-unit and no-handler review delegated to host report. Host: Reserved transfer/receive handler grep has no hits (exit1), but named refusal host-unit filter runs zero tests. Review is retained, not substituted for the route. |
| P1-DRAG-04 | FAIL | `C096`, `C040`, `C052` | Workspace: Actual mount/visibility diagnostics exposed DockArea remounting on every projection. Replaced dock() JSX gate with one-time dockReady. Regression now retains mount 1 through selection, cross-group drag and resize. Edge/focus/project-switch identity proof remains absent; full item fails. |

### 5.8 Layout persistence, restore and reopening GPUI layouts (P1c)

| Item | Result | Commands | Evidence and remaining failure |
|---|---|---|---|
| P1-LAY-01 | FAIL | `C097`, `C040`, `C052` | Workspace: Existing oracle covers geometry and several fixtures, not one named test per full §4.7 and L-* matrix. |
| P1-LAY-02 | FAIL | `C055`, `C040`, `C052` | Workspace: Existing PaneTree operations and one UI drag pass; prescribed real Headless output per gesture and counter test absent. |
| P1-LAY-03 | FAIL | `C098`, `C099`, `C040`, `C052`, `C275` | Workspace: 10 roundtrip tests cover storage subsets; complete named L-* manifest, byte/mtime no-edit and Reopen UI matrix absent. |
| P1-LAY-04 | FAIL | `C100`, `C101`, `C051`, `C040`, `C052` | Workspace: Mapped damaged/newer recovery Rust tests pass; required default notice suffix frontend test absent. |
| P1-LAY-05 | FAIL | `C102`, `C103`, `C051`, `C040`, `C052` | Workspace: Core legacy migration passes; global-tab pruning/MOVED_NOTICE bridge and TSX paths absent. |
| P1-LAY-06 | FAIL | `C051`, `C040`, `C052` | Workspace: No Sessions reconciliation, dead/detached/adoption retry implementation. |
| P1-LAY-07 | FAIL | `C051`, `C040`, `C052`, `C213`, `C214`, `C220`, `C221`, `C227`, `C228`, `C234`, `C235` | Workspace: 250 ms worker and flush/drop tests pass; full trigger/ManualClock matrix and restart/project-switch UI missing. |
| P1-LAY-08 | FAIL | `C051`, `C040`, `C052` | Workspace: Safe aliases partly pass but unknown-tab aliases intentionally persist original unsafe disk ids; pending-chat mapping absent. |
| P1-LAY-09 | FAIL | `C104`, `C105`, `C106`, `C040`, `C052` | Workspace: Core carry tests and persistence switch pass; no project.open TSX subscription or carried component identity. |
| P1-LAY-10 | PASS | `C052`, `C107`, `C040` | Workspace: Mapped layout_roundtrip oracle persists through LayoutStore and loads equal ProjectLayout; writer review confirms no alternate production layouts.json writer. |

### 5.9 Menus and keys

| Item | Result | Commands | Evidence and remaining failure |
|---|---|---|---|
| P1-MENU-01 | PASS | `C175`, `C176` | Host: Real NSMenu application tree, labels, separators, shortcut modifiers and tags pass; frozen v1 command-ID set/uniqueness successor passes. |
| P1-MENU-02 | PASS | `C188`, `C245`, `C246`, `C254`, `C255` | Host: Actual NSMenu edit labels and standard cut:/copy:/paste:/selectAll: selectors pass. Workspace owns no-selection clipboard command behavior. Added and executed named NSH host Copy route on an empty selection with an isolated clipboard sentinel; unchanged. Native standard-selector AppKit check also passes. |
| P1-MENU-03 | FAIL | `C177`, `C178`, `C179`, `C180` | Host: Real View menu passes after fixing deferred rows to remain present and disabled through NSMenu.update; direct deferred dispatch is guarded. Named key-window/no-window host routing tests are absent, and no-window text-size behavior is unimplemented. |
| P1-MENU-04 | FAIL | `C181`, `C182` | Host: Pure Dock uniqueness/order/last-key test plus actual fake-registry NSMenu/caching/actions pass. Workspace owns change-only setDockInfo posts. FAIL whole item: required NSH change-only window.setDockInfo posts are absent from production App/Workspace, despite passing native Dock model/AppKit checks. |
| P1-KEYS-01 | FAIL | `C108`, `C040` | Workspace: Added exact deferred seams and inert chord test; full GPUI active table and cross-host uniqueness successor remain absent. |
| P1-KEYS-02 | FAIL | `C109`, `C040` | Workspace: Number/model tests and canonical bracket-cycle aliases pass; shared-order/modal route and manual AppKit Ctrl-Tab check missing. Ctrl-Tab manual AppKit behavior NOT EXECUTED: read-only Cua authorization forbids the input. No screenshot proves key dispatch; this gap is not counted as a pass. |
| P1-KEYS-03 | FAIL | `C108`, `C040`, `C189` | Workspace: Deferred chords now declared/inert; required panel/new-window active key routes absent. Host: Actual native cmd-alt-G/cmd-shift-O/cmd-Q menu equivalents pass. Workspace owns full TSX chord table and routing. |
| P1-KEYS-04 | FAIL | `C110`, `C040` | Workspace: No modal or strip menu implementation; corresponding focus/escape tests absent. |
| P1-KEYS-05 | FAIL | `C111`, `C183`, `C184` | Workspace: Host-unit/appkit-only route delegated to host report; no TSX text-size owner exists. Host: Required routing host-unit and no-window text-size dispatch AppKit checks are absent; text-size handlers cannot apply settings without a window. |

### 5.10 Themes, appearance and interface text

| Item | Result | Commands | Evidence and remaining failure |
|---|---|---|---|
| P1-THM-01 | FAIL | `C001`, `C002` | Native/core: The named moved test runs zero tests. Core ThemeChoice already has seven snake_case choices, Ghostty default and labels, but Settings is absent from the native registry and no UI theme selection is wired. |
| P1-THM-02 | FAIL | `C003`, `C004`, `C005` | Native/core: Moved palette tests are not in riwork-core (zero tests); Appearance module, appearance integration test target and test-support feature are absent. Preserved legacy GPUI theme source has no compiled successor. |
| P1-THM-03 | FAIL | `C006`, `C007`, `C008` | Native/core: All three named core successors run zero tests; Settings terminal_colors_forced exists but the required Appearance override field is absent. |
| P1-THM-04 | FAIL | `C009`, `C002`, `C010`, `C128`, `C040` | Native/core: Native review finds no Appearance module or startup resolved palette. Workspace owner supplies the Theme.test.tsx route result; review is not substituted for that route. Workspace: Frontend hard-codes colors and never subscribes Appearance; first Native light paint fails implementation review. |
| P1-THM-05 | FAIL | `C011`, `C010`, `C129`, `C040`, `C185`, `C277` | Native/core: The bridge named route exits 101 because test-support is absent; Settings/Appearance subscriptions are unimplemented. Host uses Appearance::System and rejects app.setAppearance. Other owners supply host/TSX/fixture routes. Workspace: No Appearance/Settings subscriptions or per-window color-scheme update; native/host portions delegated. Host: Forced-appearance host test is absent and source opens AppView with Appearance::System. Other owners cover Settings/Appearance bridge and TSX/manual routes. |
| P1-THM-06 | PASS | `C012`, `C208`, `C215`, `C222`, `C229`, `C253`, `C290`, `C299`, `C305`, `C311` | Native/core: Review passed (no reachable appearance writer). Root fixture PID 25211 exited -15 after read-only inspection; pre-existing appearance.json hash e28fad78729878a34119070e89efe095dc8e270ab1344d5963723da4a6cc652f and mtime_ns 1793137071953824496 stayed identical. Only nsh/desktop.handoff.lock and run/desktop.lock changed; no removals, disallowed writes, sessions/chats or tmux entries. Final real fixture PID 81359: two distinct Alpha/Beta runtime windows, one registered PID; allowed lock writes only; no tmux entries; preexisting appearance.json SHA-256 and mtime unchanged. Cleanup SIGTERM exit -15. |
| P1-THM-07 | FAIL | `C013` | Native/core: Named bridge route exits 101; no Appearance module/fallback diagnostic. D-THEME-1 remains open in the acceptance decision record; no decision is invented here. |
| P1-TXT-01 | FAIL | `C014`, `C015`, `C016`, `C017` | Native/core: Three named core tests each pass (3 total); native Settings is absent, so two-handle Settings race route exits 101. This is a failed whole acceptance item despite passing core coverage. |
| P1-TXT-02 | FAIL | `C130`, `C040`, `C190` | Workspace: No command listener or Settings text sizing; hard-coded text sizes. Host: Actual View menu Bigger/Smaller/Actual Size shortcut text/modifiers pass. Native/TSX Settings effects are owned by other agents. |
| P1-TXT-03 | FAIL | `C131`, `C132`, `C040` | Workspace: No font choice/offset/space/casing implementation; moved/new sentence-case test absent. |

### 5.11 Status bar (P1c, with P1a `Settings`)

| Item | Result | Commands | Evidence and remaining failure |
|---|---|---|---|
| P1-STAT-01 | FAIL | `C112`, `C113`, `C114`, `C075`, `C040` | Workspace: Core configuration tests pass; bar hard-coded and not Settings-owned; full defaults/order/metrics missing. |
| P1-STAT-02 | FAIL | `C115`, `C040` | Workspace: No Project Alpha selector, worktree/live/session or required no-data items. |
| P1-STAT-03 | FAIL | `C116`, `C040` | Workspace: Current popup has Gather tabs into main pane, separator and summary; Default layout, toggle/geometry and required selector absent. |
| P1-STAT-04 | FAIL | `C117`, `C040` | Workspace: Sticky dismissible bottom notice exists; top-right toast, replacement/focus/Escape proof absent. |
| P1-STAT-05 | FAIL | `C118`, `C040` | Workspace: top-orchestrator global menu absent. |

### 5.12 Polling, idle work and performance

| Item | Result | Commands | Evidence and remaining failure |
|---|---|---|---|
| P1-POLL-01 | FAIL | `C018`, `C019`, `C020`, `C021`, `C022`, `C023`, `C024`, `C025`, `C026` | Native/core: Every declared test-support route exits 101. Existing native tests prove snapshot/dedup/overflow/close and two real QuickJS engines sharing Store/Sessions; missing Tabs/Settings/Appearance hubs and project-switch/streaming coverage prevent acceptance. Streaming topics remain deferred, without fabricated coverage. |
| P1-POLL-02 | PASS | `C027`, `C028`, `C026`, `C133`, `C040`, `C243` | Native/core: Fixed empty-inventory 2s polling. Regression failed before fix (2 reads vs 1); after fix 11 native tests pass, including 60 actual quiet seconds with zero Store/Sessions deliveries and only initial sample, plus watcher-triggered arm/disarm. Explicit bridge successor uses real time instead of missing ManualClock. Final workspace Idle.test.tsx passes its 60,000ms virtual-clock unchanged-host-calls check within a clean NSH suite (nsh-reh5ez2m). Workspace: Named Idle.test.tsx passes 60,000 ms virtual time with unchanged host calls AND posts. Native agent separately passes the 60 s quiet-home bridge and sampler arm/disarm regression; coordinator combines native/host evidence. |
| P1-POLL-03 | PASS | `C134`, `C135`, `C040`, `C252` | Workspace: Forbidden polling review command returns no source hits; check included in this recorded acceptance execution. |
| P1-PERF-01 | REPORT_ONLY | `C279`, `C280`, `C281`, `C282`, `C283`, `C284`, `C285`, `C286`, `C319` | REPORT-ONLY, incomplete measurement: archived GPUI release frame command passes with median 11.646792 ms, p90 12.014917 ms over 41 frames; logs retained. Integrated NSH host has no settled monitor/Busy+120-project/Native benchmark fixture; no median/p90 comparison is invented. Supplemental debug two-window 20.012s ps CPU delta was 0.00s (0.0% at ps resolution), under concurrent builds; historical 0.65% is not a matched rerun. D-PERF-1 creates no Phase 1 wall-clock failure. |
| P1-PERF-02 | FAIL | `C055` | Workspace: Required per-gesture bridge counters absent. Partial no-fallback model tests do not prove hard structural guards. |

### 5.13 `riwork/native` contract items (P1a)

| Item | Result | Commands | Evidence and remaining failure |
|---|---|---|---|
| P1-NAT-01 | FAIL | `C029`, `C030`, `C009` | Native/core: Named probe path does not exist (exit 1). The registry and camelCase DTO generation work for Store/Sessions/Layouts only; Tabs/Settings/Appearance/Dashboard are absent. Core/engine diff from ea0f2f5 is empty, so persistence serde formats remain unchanged. |
| P1-NAT-02 | FAIL | `C009`, `C031`, `C032`, `C033`, `C026` | Native/core: Store/Sessions async work uses spawn_blocking and existing error tests pass. Sync LayoutSession edits take HostLayout.closing/project mutexes shared with the host, violating the frozen no-cross-thread-lock sync rule; <1ms is unmeasured. Named exhaustive frozen-error test route exits 101. |
| P1-NAT-03 | FAIL | `C031`, `C010`, `C034` | Native/core: Store/Sessions have production/fake backend traits; host data_dir is home/nsh; storage grep has no hits. Layouts directly uses concrete LayoutPersistence (no backend trait/fake), and the other mandatory P1 modules are absent. Whole item fails. |
| P1-NAT-04 | FAIL | `C035`, `C026` | Native/core: Declared TSX probe does not exist (exit 1); Store.createFolder is not exported. Supplemental Rust test native_pushes_one_service_to_two_engines_and_stops_on_close_owner_and_engine_drop passes, but its folder write is direct core Store, so it does not prove the requested UI write through the bridge. |
| P1-NAT-05 | FAIL | `C036`, `C037`, `C038` | Native/core: Both named routes exit 101; Tabs.update and Store ID mutation methods do not exist, so the required valid_key/UUID rejection surface is absent. Existing inspectProject only rejects an empty root and is not an ID validator. |

## Exact command catalog

Command environments and stdout/stderr hashes are in the machine record; root manifests are copied beside raw output. `exit 0; 0 tests` is not a pass. `rg` exit 1 means no matches; file/target absence is explicitly failed where the route requires it. Cua calls have structured results rather than shell exit codes.

| ID | Exact command or MCP call | Exit / observed result |
|---|---|---|
| C001 | `cargo test -p riwork-core --lib --locked --offline hermes_settings_round_trip_preserves_every_existing_theme_name` | exit 0 |
| C002 | `rg -n 'ThemeChoice\|Follow Ghostty\|Catppuccin\|Tokyo Night\|Gruvbox\|Hermes\|Settings\|Appearance' crates/riwork-core/src/theme.rs apps/riwork/App.tsx crates/riwork-nsh-native/src/lib.rs` | exit 0 |
| C003 | `cargo test -p riwork-core --lib --locked --offline only_native_goes_stale_when_macos_switches_light_and_dark` | exit 0 |
| C004 | `cargo test -p riwork-core --lib --locked --offline published_terminal_follows_what_the_desktop_shows_while_following_ghostty` | exit 0 |
| C005 | `cargo test -p riwork-nsh-native --features test-support --test appearance --locked --offline -- every_preset_token_is_exact` | exit 101 |
| C006 | `cargo test -p riwork-core --lib --locked --offline terminal_override_is_what_terminals_are_forced_to` | exit 0 |
| C007 | `cargo test -p riwork-core --lib --locked --offline native_terminals_match_the_theme_only_while_the_option_is_on` | exit 0 |
| C008 | `cargo test -p riwork-core --lib --locked --offline selected_theme_controls_terminals_and_keeps_legacy_preferences` | exit 0 |
| C009 | `rg -n 'pub (async )?fn\|#\[module\]\|#\[class\]' crates/riwork-nsh-native/src/lib.rs apps/riwork/src/lib.rs` | exit 0 |
| C010 | `rg -n 'AppOptions\|data_dir\|appearance:\|app.setAppearance' crates/riwork-desktop/src/nsh_host.rs` | exit 0 |
| C011 | `cargo test -p riwork-nsh-native --features test-support --test settings --locked --offline -- a_write_by_another_process_reaches_every_subscriber_once` | exit 101 |
| C012 | `rg -n 'appearance_file\|publish_appearance\|write_appearance\|appearance.json' crates/riwork-nsh-native/src apps/riwork/src crates/riwork-desktop/src/nsh_main.rs crates/riwork-desktop/src/nsh_host.rs crates/riwork-desktop/src/nsh_runtime.rs` | exit 1 |
| C013 | `cargo test -p riwork-nsh-native --features test-support --test appearance --locked --offline -- follow_ghostty_without_a_parser_uses_the_riwork_palette_and_keeps_the_error` | exit 101 |
| C014 | `cargo test -p riwork-core --lib --locked --offline saved_sizes_round_to_tenths_clamp_and_write_plain_numbers` | exit 0 |
| C015 | `cargo test -p riwork-core --lib --locked --offline a_step_starts_from_the_stored_size_not_the_cached_one` | exit 0 |
| C016 | `cargo test -p riwork-core --lib --locked --offline ui_text_size_defaults_to_today_clamps_and_keeps_odd_values_until_changed` | exit 0 |
| C017 | `cargo test -p riwork-nsh-native --features test-support --test settings --locked --offline -- change_text_size_is_a_read_modify_write_under_the_lock` | exit 101 |
| C018 | `cargo test -p riwork-nsh-native --features test-support --test subscriptions --locked --offline -- the_first_delivery_is_a_snapshot` | exit 101 |
| C019 | `cargo test -p riwork-nsh-native --features test-support --test subscriptions --locked --offline -- rev_is_monotonic_per_topic_per_process` | exit 101 |
| C020 | `cargo test -p riwork-nsh-native --features test-support --test subscriptions --locked --offline -- overflow_clears_the_queue_and_sends_one_resync_snapshot` | exit 101 |
| C021 | `cargo test -p riwork-nsh-native --features test-support --test subscriptions --locked --offline -- one_delivery_per_topic_per_engine_turn` | exit 101 |
| C022 | `cargo test -p riwork-nsh-native --features test-support --test subscriptions --locked --offline -- streaming_topics_coalesce_to_16_ms_trailing` | exit 101 |
| C023 | `cargo test -p riwork-nsh-native --features test-support --test subscriptions --locked --offline -- closing_or_dropping_the_owner_ends_the_task` | exit 101 |
| C024 | `cargo test -p riwork-nsh-native --features test-support --test subscriptions --locked --offline -- a_project_switch_resubscribes_and_drops_late_deliveries` | exit 101 |
| C025 | `cargo test -p riwork-nsh-native --features test-support --test hubs --locked --offline -- one_watcher_and_one_sampler_per_home_per_process` | exit 101 |
| C026 | `cargo test -p riwork-nsh-native --locked --offline` | exit 0 |
| C027 | `cargo test -p riwork-nsh-native --features test-support --test services --locked --offline -- a_quiet_home_produces_no_deliveries_for_60_s` | exit 101 |
| C028 | `cargo test -p riwork-nsh-native --test services --locked --offline -- an_empty_inventory_sleeps_until_a_watcher_event_and_disarms_when_empty_again --exact` | exit 101 |
| C029 | `/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh check crates/riwork-nsh-native/tests/app --tsc /Users/dominik/Library/Caches/nsh/typescript-7.0.2-darwin-arm64/package/lib/tsc` | exit 1 |
| C030 | `git diff ea0f2f5 -- crates/riwork-core crates/riwork-engine` | exit 0 |
| C031 | `rg -n 'trait \|impl .*Backend\|spawn_blocking\|START.lock\|PROCESS\|get_or_init' crates/riwork-nsh-native/src/backend.rs crates/riwork-nsh-native/src/service.rs` | exit 0 |
| C032 | `sed -n 100,180p apps/riwork/src/host_layouts.rs` | exit 0 |
| C033 | `cargo test -p riwork-nsh-native --features test-support --test services --locked --offline -- errors_use_only_the_frozen_codes` | exit 101 |
| C034 | `rg -n nsh/storage apps/riwork crates/riwork-nsh-native` | exit 1 |
| C035 | `/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh test crates/riwork-nsh-native/tests/app --junit /private/tmp/riwork-p1-m5-f6975181/native/named-routes-a0u78b8b/native-probe.xml` | exit 1 |
| C036 | `cargo test -p riwork-nsh-native --features test-support --test services --locked --offline -- tabs_update_rejects_invalid_keys` | exit 101 |
| C037 | `cargo test -p riwork-nsh-native --features test-support --test services --locked --offline -- store_rejects_non_uuid_ids` | exit 101 |
| C038 | `rg -n 'valid_key\|Uuid\|uuid\|invalid:\|create_folder\|createFolder' crates/riwork-nsh-native/src` | exit 0 |
| C039 | `test -f apps/riwork/tests/Global.test.tsx` | exit 1 |
| C040 | `/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh test apps/riwork --junit /private/tmp/riwork-p1-m5-workspace/nsh-reh5ez2m/nsh.xml` | exit 0 |
| C041 | `cargo test -p riwork-core --lib global_window::tests --locked --offline` | exit 0; test counts [[3, 0, 0]] |
| C042 | `cargo test -p riwork-core --lib the_global_window_restores_as_itself_and_older_snapshots_hold_project_windows --locked --offline` | exit 0; test counts [[0, 0, 0]] |
| C043 | `cargo test -p riwork-engine --lib the_global_window_restores_as_itself_and_older_snapshots_hold_project_windows --locked --offline` | exit 0; test counts [[1, 0, 0]] |
| C044 | `rg -n P1-GLB-05 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | exit 1 |
| C045 | `rg -n P1-GLB-06 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | exit 1 |
| C046 | `rg -n P1-GLB-07 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | exit 1 |
| C047 | `rg -n P1-GLB-08 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | exit 1 |
| C048 | `rg -n P1-GLB-09 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | exit 1 |
| C049 | `rg -n P1-GLB-10 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | exit 1 |
| C050 | `test -f apps/riwork/tests/Panes.test.tsx` | exit 1 |
| C051 | `cargo test -p riwork-nsh-native --features test-support --test layout_session --locked --offline -- --list` | exit 101 |
| C052 | `cargo test -p riwork-nsh-workspace --locked --offline` | exit 0; test counts [[26, 0, 0], [10, 0, 0], [0, 0, 0]] |
| C053 | `cargo test -p riwork-core --lib split_ratios_round_trip_and_survive_insertions_and_collapses --locked --offline` | exit 0; test counts [[1, 0, 0]] |
| C054 | `cargo test -p riwork-core --lib invalid_ratios_are_repaired_before_persistence --locked --offline` | exit 0; test counts [[1, 0, 0]] |
| C055 | `cargo test -p riwork-nsh-native --features test-support --test layout_gestures --locked --offline -- --list` | exit 101 |
| C056 | `cargo test -p riwork-core --lib preserve_locked --locked --offline` | exit 0; test counts [[8, 0, 0]] |
| C057 | `cargo test -p riwork-core --lib lock_state_follows_the_users_choice_and_defaults_to_the_navigation_pane --locked --offline` | exit 0; test counts [[0, 0, 0]] |
| C058 | `cargo test -p riwork-core --lib stale_lock_ids_are_removed_without_reenabling_auto_lock --locked --offline` | exit 0; test counts [[1, 0, 0]] |
| C059 | `cargo test -p riwork-core --lib only_a_main_pane_that_exists_outside_focus_mode_takes_what_is_opened --locked --offline` | exit 0; test counts [[0, 0, 0]] |
| C060 | `cargo test -p riwork-core --lib a_new_tab_opens_in_the_main_pane_whichever_pane_is_selected --locked --offline` | exit 0; test counts [[0, 0, 0]] |
| C061 | `cargo test -p riwork-core --lib an_open_chat_orchestrator_tab_comes_to_the_front_by_the_main_pane_rules --locked --offline` | exit 0; test counts [[0, 0, 0]] |
| C062 | `cargo test -p riwork-core --lib a_main_pane_that_is_not_in_the_layout_is_dropped_and_there_is_never_more_than_one --locked --offline` | exit 0; test counts [[1, 0, 0]] |
| C063 | `cargo test -p riwork-core --lib closing_a_pane_focuses_the_pane_that_takes_its_space --locked --offline` | exit 0; test counts [[0, 0, 0]] |
| C064 | `cargo test -p riwork-core --lib a_closed_pane_hands_its_tabs_to_the_main_pane_after_its_own --locked --offline` | exit 0; test counts [[0, 0, 0]] |
| C065 | `cargo test -p riwork-core --lib closing_the_main_pane_or_a_locked_one_or_having_no_main_pane_closes_the_tabs_as_before --locked --offline` | exit 0; test counts [[0, 0, 0]] |
| C066 | `cargo test -p riwork-core --lib a_gather_moves_every_unlocked_tab_into_the_main_pane_and_leaves_the_locked_pane_alone --locked --offline` | exit 0; test counts [[0, 0, 0]] |
| C067 | `cargo test -p riwork-core --lib a_gather_never_touches_any_locked_pane_among_several --locked --offline` | exit 0; test counts [[0, 0, 0]] |
| C068 | `cargo test -p riwork-core --lib a_messy_window_becomes_a_locked_navigation_pane_beside_the_main_pane --locked --offline` | exit 0; test counts [[0, 0, 0]] |
| C069 | `cargo test -p riwork-core --lib the_default_layout_closes_nothing_and_a_locked_panes_tabs_are_moved_not_dropped --locked --offline` | exit 0; test counts [[0, 0, 0]] |
| C070 | `cargo test -p riwork-core --lib a_window_that_is_laid_out_already_is_left_exactly_as_it_is --locked --offline` | exit 0; test counts [[0, 0, 0]] |
| C071 | `cargo test -p riwork-core --lib panes_keep_their_ids_and_a_new_one_is_made_only_when_there_is_none_to_keep --locked --offline` | exit 0; test counts [[0, 0, 0]] |
| C072 | `cargo test -p riwork-core --lib the_default_layout_is_saved_as_the_projects_layout_and_reads_back_as_it_was --locked --offline` | exit 0; test counts [[0, 0, 0]] |
| C073 | `rg -n P1-PANE-11 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | exit 1 |
| C074 | `test -f apps/riwork/tests/PaneMenu.test.tsx` | exit 1 |
| C075 | `test -f apps/riwork/tests/metrics.test.ts` | exit 1 |
| C076 | `cargo test -p riwork-core --lib a_remote_shell_tab_has_a_fixed_json_shape_and_key --locked --offline` | exit 0; test counts [[1, 0, 0]] |
| C077 | `cargo test -p riwork-core --lib a_chat_tab_has_a_fixed_json_shape_and_key --locked --offline` | exit 0; test counts [[1, 0, 0]] |
| C078 | `test -f apps/riwork/tests/Tabs.test.tsx` | exit 1 |
| C079 | `test -f apps/riwork/tests/CloseDialog.test.tsx` | exit 1 |
| C080 | `test -f apps/riwork/tests/Strip.test.tsx` | exit 1 |
| C081 | `rg -n P1-STRIP-03 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | exit 1 |
| C082 | `rg -n P1-STRIP-04 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | exit 1 |
| C083 | `cargo test -p riwork-core --lib stale_inventory_keeps_hide_and_rename --locked --offline` | exit 0; test counts [[0, 0, 0]] |
| C084 | `cargo test -p riwork-core --lib one_refresh_transaction_ends_where_separate_transactions_did --locked --offline` | exit 0; test counts [[0, 0, 0]] |
| C085 | `cargo test -p riwork-core --lib titles_come_from_a_rename_then_the_provider_then_the_first_message_then_the_kind --locked --offline` | exit 0; test counts [[0, 0, 0]] |
| C086 | `cargo test -p riwork-core --lib a_title_is_set_once_and_only_a_higher_source_or_a_new_rename_replaces_it --locked --offline` | exit 0; test counts [[0, 0, 0]] |
| C087 | `cargo test -p riwork-core --lib new_sessions_append_and_any_tab_hides --locked --offline` | exit 0; test counts [[0, 0, 0]] |
| C088 | `cargo test -p riwork-nsh-native --features test-support --test tabs --locked --offline -- --list` | exit 101 |
| C089 | `cargo test -p riwork-engine --lib stale_inventory_keeps_hide_and_rename --locked --offline` | exit 0; test counts [[1, 0, 0]] |
| C090 | `cargo test -p riwork-engine --lib one_refresh_transaction_ends_where_separate_transactions_did --locked --offline` | exit 0; test counts [[1, 0, 0]] |
| C091 | `cargo test -p riwork-engine --lib titles_come_from_a_rename_then_the_provider_then_the_first_message_then_the_kind --locked --offline` | exit 0; test counts [[1, 0, 0]] |
| C092 | `cargo test -p riwork-engine --lib a_title_is_set_once_and_only_a_higher_source_or_a_new_rename_replaces_it --locked --offline` | exit 0; test counts [[1, 0, 0]] |
| C093 | `cargo test -p riwork-engine --lib new_sessions_append_and_any_tab_hides --locked --offline` | exit 0; test counts [[1, 0, 0]] |
| C094 | `test -f apps/riwork/tests/Drag.test.tsx` | exit 1 |
| C095 | `rg -n P1-DRAG-03 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | exit 1 |
| C096 | `test -f apps/riwork/tests/Identity.test.tsx` | exit 1 |
| C097 | `cargo test -p riwork-nsh-native --features test-support --test layout_oracle --locked --offline -- --list` | exit 101 |
| C098 | `test -f apps/riwork/tests/Reopen.test.tsx` | exit 1 |
| C099 | `cargo test -p riwork-nsh-native --features test-support --test layout_reopen --locked --offline -- --list` | exit 101 |
| C100 | `cargo test -p riwork-core --lib corrupt_file_is_set_aside_on_save_instead_of_crashing_or_being_deleted --locked --offline` | exit 0; test counts [[1, 0, 0]] |
| C101 | `cargo test -p riwork-core --lib newer_schema_is_neither_loaded_nor_overwritten --locked --offline` | exit 0; test counts [[1, 0, 0]] |
| C102 | `cargo test -p riwork-core --lib legacy_shell_layout_migrates_without_changing_tab_order_or_selection --locked --offline` | exit 0; test counts [[1, 0, 0]] |
| C103 | `cargo test -p riwork-core --lib mixed_tabs_preserve_order_and_selection_while_migrating_bottom_strips --locked --offline` | exit 0; test counts [[1, 0, 0]] |
| C104 | `cargo test -p riwork-core --lib carry_preserves_locked_tabs_ratios_and_maps_colliding_destination_ids --locked --offline` | exit 0; test counts [[1, 0, 0]] |
| C105 | `cargo test -p riwork-core --lib destination_locked_pane_sessions_are_recovered_into_unlocked_region --locked --offline` | exit 0; test counts [[1, 0, 0]] |
| C106 | `cargo test -p riwork-core --lib a_locked_main_pane_stays_the_main_pane_when_the_project_changes --locked --offline` | exit 0; test counts [[1, 0, 0]] |
| C107 | `rg -n 'layouts\.json' crates/riwork-nsh-native crates/riwork-desktop/src/nsh_main.rs crates/riwork-desktop/src/nsh_host.rs apps/riwork -g '!**/target/**' -g '!**/.nsh/**'` | exit 0 |
| C108 | `test -f apps/riwork/tests/keymap.test.ts` | exit 1 |
| C109 | `cargo test -p riwork-core --lib command_numbers_count_session_tabs_and_nine_is_the_last --locked --offline` | exit 0; test counts [[0, 0, 0]] |
| C110 | `rg -n P1-KEYS-04 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | exit 1 |
| C111 | `rg -n P1-KEYS-05 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | exit 1 |
| C112 | `cargo test -p riwork-core --lib status_bar::tests --locked --offline` | exit 0; test counts [[4, 0, 0]] |
| C113 | `cargo test -p riwork-core --lib routing_and_reordering_preserve_other_side_and_round_trip --locked --offline` | exit 0; test counts [[1, 0, 0]] |
| C114 | `test -f apps/riwork/tests/StatusBar.test.tsx` | exit 1 |
| C115 | `rg -n P1-STAT-02 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | exit 1 |
| C116 | `rg -n P1-STAT-03 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | exit 1 |
| C117 | `rg -n P1-STAT-04 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | exit 1 |
| C118 | `rg -n P1-STAT-05 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | exit 1 |
| C119 | `cargo test -p riwork-core --lib legacy_and_invalid_window_sizes_do_not_break_layout_restoration --locked --offline` | exit 0; test counts [[1, 0, 0]] |
| C120 | `cargo test -p riwork-nsh-native --features test-support --test window_size --locked --offline -- --list` | exit 101 |
| C121 | `test -f apps/riwork/tests/Window.test.tsx` | exit 1 |
| C122 | `test -f apps/riwork/tests/Chrome.test.tsx` | exit 1 |
| C123 | `rg -n P1-WIN-09 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | exit 1 |
| C124 | `rg -n P1-WIN-10 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | exit 1 |
| C125 | `cargo test -p riwork-core --lib identifiers_geometry_and_private_file_read_are_validated --locked --offline` | exit 0; test counts [[0, 0, 0]] |
| C126 | `test -f apps/riwork/tests/Restore.test.tsx` | exit 1 |
| C127 | `cargo test -p riwork-engine --lib identifiers_geometry_and_private_file_read_are_validated --locked --offline` | exit 0; test counts [[1, 0, 0]] |
| C128 | `test -f apps/riwork/tests/Theme.test.tsx` | exit 1 |
| C129 | `rg -n P1-THM-05 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | exit 1 |
| C130 | `rg -n P1-TXT-02 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | exit 1 |
| C131 | `cargo test -p riwork-core --lib sentence_case_keeps_proper_nouns_and_acronyms --locked --offline` | exit 0; test counts [[0, 0, 0]] |
| C132 | `test -f apps/riwork/tests/TextSize.test.tsx` | exit 1 |
| C133 | `test -f apps/riwork/tests/Idle.test.tsx` | exit 0 |
| C134 | `rg -n 'setInterval\(\|requestAnimationFrame\(\|\.refetch\(' apps/riwork -g '!**/*.test.ts*' -g '!**/target/**' -g '!**/.nsh/**'` | exit 1 |
| C135 | `sh apps/riwork/check-polling.sh` | exit 0 |
| C136 | `cargo test -p riwork-cli --lib only_gui_startup_forms_open_the_workspace --locked --offline` | exit 0 |
| C137 | `cargo test -p riwork-engine --lib live_gui_pids_lists_running_guis_and_changes_nothing --locked --offline` | exit 0 |
| C138 | `cargo test -p riwork-engine --lib queuing_and_wait_timeout_leave_old_gui_alive_and_do_not_launch_anything --locked --offline` | exit 0 |
| C139 | `cargo test -p riwork-engine --lib readiness_requires_correct_child_and_matching_registered_windows --locked --offline` | exit 0 |
| C140 | `cargo test -p riwork-engine --lib failed_or_hung_replacement_keeps_old_alive_and_allows_retry --locked --offline` | exit 0 |
| C141 | `cargo test -p riwork-engine --lib a_watchdog_that_fired_refuses_to_let_the_restore_confirm --locked --offline` | exit 0 |
| C142 | `cargo test -p riwork-desktop --bin riwork nsh_runtime::tests::restore_acknowledgements_are_per_window_and_idempotent --locked --offline` | exit 0 |
| C143 | `cargo test -p riwork-desktop --bin riwork nsh_runtime::tests::restore_matches_targets_even_if_window_order_changes --locked --offline` | exit 0 |
| C144 | `cargo test -p riwork-desktop --bin riwork restore::tests::confirmation_gives_up_after_20_s --locked --offline` | exit 0 |
| C145 | `cargo test -p riwork-engine --lib publishing_writes_only_changes_and_never_waits_for_the_registry_lock --locked --offline` | exit 0 |
| C146 | `cargo test -p riwork-desktop --bin riwork nsh_host::tests::a_busy_registry_lock_stays_due --locked --offline` | exit 0 |
| C147 | `cargo test -p riwork-engine --lib identifiers_geometry_and_private_file_read_are_validated --locked --offline` | exit 0 |
| C148 | `cargo test -p riwork-desktop --bin riwork nsh_host::tests::runtime_publication_reads_current_layout_without_an_appkit_state_event --locked --offline` | exit 0 |
| C149 | `cargo test -p riwork-desktop --bin riwork nsh_host::tests::runtime_frames_keep_the_existing_display_local_top_left_coordinates --locked --offline` | exit 0 |
| C150 | `cargo test -p riwork-core --lib opening_windows_cascade_inside_the_display_and_clamp_after_cascading --locked --offline` | exit 0 |
| C151 | `cargo test -p riwork-desktop --bin riwork placement::tests::the_cascade_counts_open_non_tooltip_windows --locked --offline` | exit 0 |
| C152 | `cargo test -p riwork-core --lib legacy_and_invalid_window_sizes_do_not_break_layout_restoration --locked --offline` | exit 0 |
| C153 | `cargo test -p riwork-core --lib global_window::tests:: --locked --offline` | exit 0 |
| C154 | `cargo test -p riwork-engine --lib the_global_window_restores_as_itself_and_older_snapshots_hold_project_windows --locked --offline` | exit 0 |
| C155 | `cargo test -p riwork-desktop --bin riwork runtime_window::tests::the_global_window_is_recorded_as_global --locked --offline` | exit 0 |
| C156 | `cargo test -p riwork-desktop --test startup_environment --locked --offline` | exit 0 |
| C157 | `cargo test -p riwork-desktop --bin riwork nsh_host::tests::folder_arg_reuses_existing_project --locked --offline` | exit 0 |
| C158 | `cargo test -p riwork-desktop --bin riwork nsh_host::tests::no_active_project_registers_the_cwd_fallback --locked --offline` | exit 0 |
| C159 | `cargo test -p riwork-desktop --bin riwork nsh_host::tests::a_vanished_project_root_is_an_error_for_the_caller_not_a_panic --locked --offline` | exit 0 |
| C160 | `cargo test -p riwork-desktop --bin riwork nsh_runtime::tests::forwards_path_and_reopen_without_creating_a_second_primary --locked --offline` | exit 0 |
| C161 | `cargo test -p riwork-desktop --bin riwork nsh_runtime::tests::reclaims_stale_socket_but_never_deletes_a_regular_file --locked --offline` | exit 0 |
| C162 | `cargo test -p riwork-desktop --bin riwork nsh_runtime::tests::replacement_keeps_old_socket_and_prevents_handoff_race --locked --offline` | exit 0 |
| C163 | `cargo test -p riwork-desktop --bin riwork placement::tests::zoomed_counts_as_maximized_and_keeps_its_normal_frame --locked --offline` | exit 0 |
| C164 | `cargo test -p riwork-desktop --bin riwork placement::tests::fullscreen_counts_as_fullscreen --locked --offline` | exit 0 |
| C165 | `cargo test -p riwork-desktop --bin riwork window_registry::tests::the_last_close_quits --locked --offline` | exit 0 |
| C166 | `cargo test -p riwork-desktop --bin riwork window_registry::tests::tooltips_do_not_count --locked --offline` | exit 0 |
| C167 | `cargo test -p riwork-desktop --test appkit_shell --locked --offline -- tooltips_create_no_nswindow --test-threads=1` | exit 101 |
| C168 | `cargo test -p riwork-desktop --bin riwork nsh_host::tests::quit_flushes_every_layout_session_and_the_global_state_from_rust --locked --offline` | exit 0 |
| C169 | `cargo test -p riwork-desktop --bin riwork nsh_host::tests::close_flushes_the_last_accepted_edit_before_the_window_goes --locked --offline` | exit 0 |
| C170 | `cargo test -p riwork-desktop --bin riwork window_state::tests::events_only_on_change --locked --offline` | exit 0 |
| C171 | `cargo test -p riwork-desktop --bin riwork window_registry::tests::the_global_window_is_a_singleton_and_opens_with_no_window --locked --offline` | exit 0 |
| C172 | `cargo test -p riwork-desktop --bin riwork menu::tests::dock_new_window --locked --offline` | exit 0 |
| C173 | `cargo test -p riwork-desktop --bin riwork window_registry::tests::route_prefers_frontmost_then_minimised_then_opens --locked --offline` | exit 0 |
| C174 | `cargo test -p riwork-desktop --bin riwork host_names::tests::tab_transfer_is_refused --locked --offline` | exit 0 |
| C175 | `cargo test -p riwork-desktop --bin riwork nsh_menus::tests::command_ids_are_the_v1_set --locked --offline` | exit 0 |
| C176 | `cargo test -p riwork-desktop --test appkit_shell --locked --offline -- app_menu_matches_gpui --test-threads=1` | exit 0 |
| C177 | `cargo test -p riwork-desktop --bin riwork menu::tests::window_commands_go_to_the_key_window --locked --offline` | exit 0 |
| C178 | `cargo test -p riwork-desktop --bin riwork menu::tests::app_commands_work_with_no_window --locked --offline` | exit 0 |
| C179 | `cargo test -p riwork-desktop --bin riwork nsh_menus::tests::deferred_menu_commands_remain_present_disabled_and_keep_their_chords --locked --offline` | exit 0 |
| C180 | `cargo test -p riwork-desktop --test appkit_shell --locked --offline -- view_menu_matches_gpui --test-threads=1` | exit 0 |
| C181 | `cargo test -p riwork-desktop --bin riwork nsh_menus::tests::dock_labels_are_stable_unique_and_keep_the_last_key_window_marked --locked --offline` | exit 0 |
| C182 | `cargo test -p riwork-desktop --test appkit_shell --locked --offline -- dock_menu_from_a_fake_registry --test-threads=1` | exit 0 |
| C183 | `cargo test -p riwork-desktop --bin riwork menu::tests::routing --locked --offline` | exit 0 |
| C184 | `cargo test -p riwork-desktop --test appkit_shell --locked --offline -- text_size_key_equivalents_dispatch_without_a_window --test-threads=1` | exit 101 |
| C185 | `cargo test -p riwork-desktop --bin riwork startup::tests::forced_appearance_from_env --locked --offline` | exit 0 |
| C186 | `cargo test -p riwork-desktop --test appkit_shell --locked --offline -- window_chrome --test-threads=1` | exit 0 |
| C187 | `cargo test -p riwork-desktop --test appkit_shell --locked --offline -- titlebar_double_click_reads_the_system_preference --test-threads=1` | exit 101 |
| C188 | `cargo test -p riwork-desktop --test appkit_shell --locked --offline -- edit_menu_uses_standard_selectors --test-threads=1` | exit 0 |
| C189 | `cargo test -p riwork-desktop --test appkit_shell --locked --offline -- app_level_key_equivalents --test-threads=1` | exit 0 |
| C190 | `cargo test -p riwork-desktop --test appkit_shell --locked --offline -- view_menu_text_size_items --test-threads=1` | exit 0 |
| C191 | `rg -n 'set_ui_sets\|AppView::new\|RESTART_PENDING\|app.restart\|runtime_tick\|system.fonts\|Appearance::System' crates/riwork-desktop/src/nsh_host.rs` | exit 0 |
| C192 | `/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh --version` | exit 0 |
| C193 | `rustc --version` | exit 0 |
| C194 | `cargo --version` | exit 0 |
| C195 | `cargo build -p riwork-desktop -p riwork-cli --locked --offline` | exit 0 |
| C196 | `/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh build apps/riwork --out /private/tmp/riwork-p1-m5-f6975181/bundle` | exit 0 |
| C197 | `cargo tree -p riwork-desktop --target aarch64-apple-darwin --edges normal,build --locked --offline` | exit 0 |
| C198 | `cargo tree -p riwork-desktop --locked --offline` | exit 0 |
| C199 | `cargo fmt -p riwork-desktop -p riwork-nsh-native -p riwork-nsh-workspace --check` | exit 0 |
| C200 | `git diff --check` | exit 0 |
| C201 | `git ls-tree -r --name-only ea0f2f5 -- crates/riwork-desktop/src` | exit 0 |
| C202 | `git diff ea0f2f5 -- crates/riwork-desktop/src/account_status.rs crates/riwork-desktop/src/account_status/tests.rs crates/riwork-desktop/src/api_key.rs crates/riwork-desktop/src/app_preview.rs crates/riwork-desktop/src/app_preview/fixture.rs crates/riwork-desktop/src/app_preview/tests.rs crates/riwork-desktop/src/appearance_sync.rs crates/riwork-desktop/src/behavior_controls.rs crates/riwork-desktop/src/behavior_controls/tests.rs crates/riwork-desktop/src/chat.rs crates/riwork-desktop/src/chat_choice.rs crates/riwork-desktop/src/chat_drafts.rs crates/riwork-desktop/src/chat_drafts/references.rs crates/riwork-desktop/src/chat_tabs.rs crates/riwork-desktop/src/chat_view.rs crates/riwork-desktop/src/chat_view/activity_summary.rs crates/riwork-desktop/src/chat_view/approval.rs crates/riwork-desktop/src/chat_view/attachment_draft.rs crates/riwork-desktop/src/chat_view/attachment_ui.rs crates/riwork-desktop/src/chat_view/attachment_ui/native_paste.rs crates/riwork-desktop/src/chat_view/attachment_ui/quick_look.rs crates/riwork-desktop/src/chat_view/attachment_ui/sent.rs crates/riwork-desktop/src/chat_view/attachment_ui/sent_tests.rs crates/riwork-desktop/src/chat_view/attachment_ui/tests.rs crates/riwork-desktop/src/chat_view/cards.rs crates/riwork-desktop/src/chat_view/code_highlight.rs crates/riwork-desktop/src/chat_view/command_output.rs crates/riwork-desktop/src/chat_view/composer.rs crates/riwork-desktop/src/chat_view/diagram.rs crates/riwork-desktop/src/chat_view/diagram/tests.rs crates/riwork-desktop/src/chat_view/dictate.rs crates/riwork-desktop/src/chat_view/diff.rs crates/riwork-desktop/src/chat_view/display.rs crates/riwork-desktop/src/chat_view/display_tests.rs crates/riwork-desktop/src/chat_view/draft_tests.rs crates/riwork-desktop/src/chat_view/editor_tests.rs crates/riwork-desktop/src/chat_view/editors.rs crates/riwork-desktop/src/chat_view/feed.rs crates/riwork-desktop/src/chat_view/feedback.rs crates/riwork-desktop/src/chat_view/file_actions.rs crates/riwork-desktop/src/chat_view/file_actions/tests.rs crates/riwork-desktop/src/chat_view/file_review.rs crates/riwork-desktop/src/chat_view/file_review/tests.rs crates/riwork-desktop/src/chat_view/find.rs crates/riwork-desktop/src/chat_view/find_tests.rs crates/riwork-desktop/src/chat_view/header_line.rs crates/riwork-desktop/src/chat_view/host.rs crates/riwork-desktop/src/chat_view/image_scope.rs crates/riwork-desktop/src/chat_view/image_scope/tests.rs crates/riwork-desktop/src/chat_view/links.rs crates/riwork-desktop/src/chat_view/markdown.rs crates/riwork-desktop/src/chat_view/media.rs crates/riwork-desktop/src/chat_view/media/actions.rs crates/riwork-desktop/src/chat_view/media/actions/tests.rs crates/riwork-desktop/src/chat_view/memo.rs crates/riwork-desktop/src/chat_view/memo_tests.rs crates/riwork-desktop/src/chat_view/message_parts.rs crates/riwork-desktop/src/chat_view/notice_tests.rs crates/riwork-desktop/src/chat_view/notice_ui.rs crates/riwork-desktop/src/chat_view/notices.rs crates/riwork-desktop/src/chat_view/output_actions.rs crates/riwork-desktop/src/chat_view/output_actions/tests.rs crates/riwork-desktop/src/chat_view/output_table.rs crates/riwork-desktop/src/chat_view/panels.rs crates/riwork-desktop/src/chat_view/perf.rs crates/riwork-desktop/src/chat_view/plan_document.rs crates/riwork-desktop/src/chat_view/plan_document/tests.rs crates/riwork-desktop/src/chat_view/prose.rs crates/riwork-desktop/src/chat_view/references.rs crates/riwork-desktop/src/chat_view/references/tests.rs crates/riwork-desktop/src/chat_view/rows.rs crates/riwork-desktop/src/chat_view/select.rs crates/riwork-desktop/src/chat_view/selection_document.rs crates/riwork-desktop/src/chat_view/selection_tests.rs crates/riwork-desktop/src/chat_view/state.rs crates/riwork-desktop/src/chat_view/subshells.rs crates/riwork-desktop/src/chat_view/subshells_tests.rs crates/riwork-desktop/src/chat_view/suggest_tests.rs crates/riwork-desktop/src/chat_view/suggest_ui.rs crates/riwork-desktop/src/chat_view/testing.rs crates/riwork-desktop/src/chat_view/tool_views.rs crates/riwork-desktop/src/chat_view/toolbar.rs crates/riwork-desktop/src/chat_view/turn_navigation.rs crates/riwork-desktop/src/chat_view/turn_review.rs crates/riwork-desktop/src/chat_view/usage_chip.rs crates/riwork-desktop/src/chat_view/viewer_tests.rs crates/riwork-desktop/src/chat_view/widgets.rs crates/riwork-desktop/src/chat_view/worker_shelf.rs crates/riwork-desktop/src/chat_view/worker_shelf_tests.rs crates/riwork-desktop/src/chat_view/worker_summary.rs crates/riwork-desktop/src/controls.rs crates/riwork-desktop/src/dashboard.rs crates/riwork-desktop/src/dashboard/decision_review.rs crates/riwork-desktop/src/dashboard/tests.rs crates/riwork-desktop/src/delegation_choice.rs crates/riwork-desktop/src/dictation.rs crates/riwork-desktop/src/dock_menu.rs crates/riwork-desktop/src/file_explorer.rs crates/riwork-desktop/src/file_preview.rs crates/riwork-desktop/src/form_input.rs crates/riwork-desktop/src/global_nav.rs crates/riwork-desktop/src/global_nav/tests.rs crates/riwork-desktop/src/global_window.rs crates/riwork-desktop/src/global_window/tests.rs crates/riwork-desktop/src/handoff_dialog.rs crates/riwork-desktop/src/icons.rs crates/riwork-desktop/src/main.rs crates/riwork-desktop/src/memo_editor.rs crates/riwork-desktop/src/metal_layer.rs crates/riwork-desktop/src/notifications.rs crates/riwork-desktop/src/nuke_shells.rs crates/riwork-desktop/src/orchestrator_choice.rs crates/riwork-desktop/src/orchestrator_tab_tests.rs crates/riwork-desktop/src/orchestrator_wakes.rs crates/riwork-desktop/src/orchestrators.rs crates/riwork-desktop/src/pane_layout.rs crates/riwork-desktop/src/panels.rs crates/riwork-desktop/src/preview_host.rs crates/riwork-desktop/src/project_creator.rs crates/riwork-desktop/src/project_settings.rs crates/riwork-desktop/src/remote_prompt.rs crates/riwork-desktop/src/remote_service.rs crates/riwork-desktop/src/schedule_panel.rs crates/riwork-desktop/src/schedules.rs crates/riwork-desktop/src/settings.rs crates/riwork-desktop/src/status_bar.rs crates/riwork-desktop/src/subshells.rs crates/riwork-desktop/src/symbols.rs crates/riwork-desktop/src/tab_close_dialog.rs crates/riwork-desktop/src/tab_menu.rs crates/riwork-desktop/src/tab_strip.rs crates/riwork-desktop/src/terminal_drop.rs crates/riwork-desktop/src/terminal_lifecycle.rs crates/riwork-desktop/src/terminal_links.rs crates/riwork-desktop/src/terminal_links/overlay.rs crates/riwork-desktop/src/terminal_links/tests.rs crates/riwork-desktop/src/terminal_links/ui.rs crates/riwork-desktop/src/text_input.rs crates/riwork-desktop/src/text_input/tests.rs crates/riwork-desktop/src/theme.rs crates/riwork-desktop/src/tooltip.rs crates/riwork-desktop/src/ui_text.rs` | exit 0 |
| C203 | `sw_vers` | exit 0 |
| C204 | `uname -m` | exit 0 |
| C205 | `git rev-parse HEAD` | exit 0 |
| C206 | `rg -n 'setInterval\(\|requestAnimationFrame\(\|\.refetch\(' apps/riwork -g '!**/*.test.ts*'` | exit 1 |
| C207 | `rg -n 'nsh/storage\|appearance_file\|sessions.json\|chats/' apps/riwork crates/riwork-nsh-native crates/riwork-desktop/src/nsh_host.rs` | exit 0 |
| C208 | `cargo run -p riwork-nsh-workspace --example integration_fixture --locked --offline -- /private/tmp/rwm5-ki8420zw` | exit 0 |
| C209 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork /private/tmp/rwm5-ki8420zw/repos/Alpha` | exit 2 |
| C210 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork /private/tmp/rwm5-ki8420zw/missing` | exit 2 |
| C211 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork open /private/tmp/rwm5-ki8420zw/repos/Beta --json` | exit 0 |
| C212 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork instances --json` | exit 0 |
| C213 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork restart --json` | exit 2 |
| C214 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork instances --json` | exit 0 |
| C215 | `cargo run -p riwork-nsh-workspace --example integration_fixture --locked --offline -- /private/tmp/rwm5-u3qi2ooy --appearance` | exit 0 |
| C216 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork /private/tmp/rwm5-u3qi2ooy/repos/Alpha` | exit 2 |
| C217 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork /private/tmp/rwm5-u3qi2ooy/missing` | exit 2 |
| C218 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork open /private/tmp/rwm5-u3qi2ooy/repos/Beta --json` | exit 0 |
| C219 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork instances --json` | exit 0 |
| C220 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork restart --json` | exit 2 |
| C221 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork instances --json` | exit 0 |
| C222 | `cargo run -p riwork-nsh-workspace --example integration_fixture --locked --offline -- /private/tmp/rwm5-ikpfjuzy --appearance` | exit 0 |
| C223 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork /private/tmp/rwm5-ikpfjuzy/repos/Alpha` | exit 2 |
| C224 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork /private/tmp/rwm5-ikpfjuzy/missing` | exit 2 |
| C225 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork open /private/tmp/rwm5-ikpfjuzy/repos/Beta --json` | exit 0 |
| C226 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork instances --json` | exit 0 |
| C227 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork restart --json` | exit 2 |
| C228 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork instances --json` | exit 0 |
| C229 | `cargo run -p riwork-nsh-workspace --example integration_fixture --locked --offline -- /private/tmp/rwm5-eo6nq7gc --appearance` | exit 0 |
| C230 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork /private/tmp/rwm5-eo6nq7gc/repos/Alpha` | exit 2 |
| C231 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork /private/tmp/rwm5-eo6nq7gc/missing` | exit 2 |
| C232 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork open /private/tmp/rwm5-eo6nq7gc/repos/Beta --json` | exit 0 |
| C233 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork instances --json` | exit 0 |
| C234 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork restart --json` | exit 2 |
| C235 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork instances --json` | exit 0 |
| C236 | `/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh --version` | exit 0 |
| C237 | `rustc --version` | exit 0 |
| C238 | `cargo --version` | exit 0 |
| C239 | `cargo test -p riwork-core -p riwork-engine -p riwork-cli -p riwork-desktop -p riwork-nsh-native -p riwork-nsh-workspace --locked --offline` | exit 101 |
| C240 | `/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh --version` | exit 0 |
| C241 | `rustc --version` | exit 0 |
| C242 | `cargo --version` | exit 0 |
| C243 | `cargo test -p riwork-core -p riwork-engine -p riwork-cli -p riwork-desktop -p riwork-nsh-native -p riwork-nsh-workspace --locked --offline` | exit 0 |
| C244 | `cargo run -p riwork-nsh-workspace --example seed_fixture --locked --offline -- /private/tmp/riwork-p1-m5-f6975181/headless-ph_295w9/nsh-home` | exit 0 |
| C245 | `/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh check apps/riwork --tsc /Users/dominik/Library/Caches/nsh/typescript-7.0.2-darwin-arm64/package/lib/tsc` | exit 0 |
| C246 | `/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh test apps/riwork --junit /private/tmp/riwork-p1-m5-f6975181/headless-ph_295w9/nsh.xml` | exit 0 |
| C247 | `git ls-tree -r --name-only ea0f2f5 -- crates/riwork-desktop/src` | exit 0 |
| C248 | `git diff ea0f2f5 -- crates/riwork-desktop/src/account_status.rs crates/riwork-desktop/src/account_status/tests.rs crates/riwork-desktop/src/api_key.rs crates/riwork-desktop/src/app_preview.rs crates/riwork-desktop/src/app_preview/fixture.rs crates/riwork-desktop/src/app_preview/tests.rs crates/riwork-desktop/src/appearance_sync.rs crates/riwork-desktop/src/behavior_controls.rs crates/riwork-desktop/src/behavior_controls/tests.rs crates/riwork-desktop/src/chat.rs crates/riwork-desktop/src/chat_choice.rs crates/riwork-desktop/src/chat_drafts.rs crates/riwork-desktop/src/chat_drafts/references.rs crates/riwork-desktop/src/chat_tabs.rs crates/riwork-desktop/src/chat_view.rs crates/riwork-desktop/src/chat_view/activity_summary.rs crates/riwork-desktop/src/chat_view/approval.rs crates/riwork-desktop/src/chat_view/attachment_draft.rs crates/riwork-desktop/src/chat_view/attachment_ui.rs crates/riwork-desktop/src/chat_view/attachment_ui/native_paste.rs crates/riwork-desktop/src/chat_view/attachment_ui/quick_look.rs crates/riwork-desktop/src/chat_view/attachment_ui/sent.rs crates/riwork-desktop/src/chat_view/attachment_ui/sent_tests.rs crates/riwork-desktop/src/chat_view/attachment_ui/tests.rs crates/riwork-desktop/src/chat_view/cards.rs crates/riwork-desktop/src/chat_view/code_highlight.rs crates/riwork-desktop/src/chat_view/command_output.rs crates/riwork-desktop/src/chat_view/composer.rs crates/riwork-desktop/src/chat_view/diagram.rs crates/riwork-desktop/src/chat_view/diagram/tests.rs crates/riwork-desktop/src/chat_view/dictate.rs crates/riwork-desktop/src/chat_view/diff.rs crates/riwork-desktop/src/chat_view/display.rs crates/riwork-desktop/src/chat_view/display_tests.rs crates/riwork-desktop/src/chat_view/draft_tests.rs crates/riwork-desktop/src/chat_view/editor_tests.rs crates/riwork-desktop/src/chat_view/editors.rs crates/riwork-desktop/src/chat_view/feed.rs crates/riwork-desktop/src/chat_view/feedback.rs crates/riwork-desktop/src/chat_view/file_actions.rs crates/riwork-desktop/src/chat_view/file_actions/tests.rs crates/riwork-desktop/src/chat_view/file_review.rs crates/riwork-desktop/src/chat_view/file_review/tests.rs crates/riwork-desktop/src/chat_view/find.rs crates/riwork-desktop/src/chat_view/find_tests.rs crates/riwork-desktop/src/chat_view/header_line.rs crates/riwork-desktop/src/chat_view/host.rs crates/riwork-desktop/src/chat_view/image_scope.rs crates/riwork-desktop/src/chat_view/image_scope/tests.rs crates/riwork-desktop/src/chat_view/links.rs crates/riwork-desktop/src/chat_view/markdown.rs crates/riwork-desktop/src/chat_view/media.rs crates/riwork-desktop/src/chat_view/media/actions.rs crates/riwork-desktop/src/chat_view/media/actions/tests.rs crates/riwork-desktop/src/chat_view/memo.rs crates/riwork-desktop/src/chat_view/memo_tests.rs crates/riwork-desktop/src/chat_view/message_parts.rs crates/riwork-desktop/src/chat_view/notice_tests.rs crates/riwork-desktop/src/chat_view/notice_ui.rs crates/riwork-desktop/src/chat_view/notices.rs crates/riwork-desktop/src/chat_view/output_actions.rs crates/riwork-desktop/src/chat_view/output_actions/tests.rs crates/riwork-desktop/src/chat_view/output_table.rs crates/riwork-desktop/src/chat_view/panels.rs crates/riwork-desktop/src/chat_view/perf.rs crates/riwork-desktop/src/chat_view/plan_document.rs crates/riwork-desktop/src/chat_view/plan_document/tests.rs crates/riwork-desktop/src/chat_view/prose.rs crates/riwork-desktop/src/chat_view/references.rs crates/riwork-desktop/src/chat_view/references/tests.rs crates/riwork-desktop/src/chat_view/rows.rs crates/riwork-desktop/src/chat_view/select.rs crates/riwork-desktop/src/chat_view/selection_document.rs crates/riwork-desktop/src/chat_view/selection_tests.rs crates/riwork-desktop/src/chat_view/state.rs crates/riwork-desktop/src/chat_view/subshells.rs crates/riwork-desktop/src/chat_view/subshells_tests.rs crates/riwork-desktop/src/chat_view/suggest_tests.rs crates/riwork-desktop/src/chat_view/suggest_ui.rs crates/riwork-desktop/src/chat_view/testing.rs crates/riwork-desktop/src/chat_view/tool_views.rs crates/riwork-desktop/src/chat_view/toolbar.rs crates/riwork-desktop/src/chat_view/turn_navigation.rs crates/riwork-desktop/src/chat_view/turn_review.rs crates/riwork-desktop/src/chat_view/usage_chip.rs crates/riwork-desktop/src/chat_view/viewer_tests.rs crates/riwork-desktop/src/chat_view/widgets.rs crates/riwork-desktop/src/chat_view/worker_shelf.rs crates/riwork-desktop/src/chat_view/worker_shelf_tests.rs crates/riwork-desktop/src/chat_view/worker_summary.rs crates/riwork-desktop/src/controls.rs crates/riwork-desktop/src/dashboard.rs crates/riwork-desktop/src/dashboard/decision_review.rs crates/riwork-desktop/src/dashboard/tests.rs crates/riwork-desktop/src/delegation_choice.rs crates/riwork-desktop/src/dictation.rs crates/riwork-desktop/src/dock_menu.rs crates/riwork-desktop/src/file_explorer.rs crates/riwork-desktop/src/file_preview.rs crates/riwork-desktop/src/form_input.rs crates/riwork-desktop/src/global_nav.rs crates/riwork-desktop/src/global_nav/tests.rs crates/riwork-desktop/src/global_window.rs crates/riwork-desktop/src/global_window/tests.rs crates/riwork-desktop/src/handoff_dialog.rs crates/riwork-desktop/src/icons.rs crates/riwork-desktop/src/main.rs crates/riwork-desktop/src/memo_editor.rs crates/riwork-desktop/src/metal_layer.rs crates/riwork-desktop/src/notifications.rs crates/riwork-desktop/src/nuke_shells.rs crates/riwork-desktop/src/orchestrator_choice.rs crates/riwork-desktop/src/orchestrator_tab_tests.rs crates/riwork-desktop/src/orchestrator_wakes.rs crates/riwork-desktop/src/orchestrators.rs crates/riwork-desktop/src/pane_layout.rs crates/riwork-desktop/src/panels.rs crates/riwork-desktop/src/preview_host.rs crates/riwork-desktop/src/project_creator.rs crates/riwork-desktop/src/project_settings.rs crates/riwork-desktop/src/remote_prompt.rs crates/riwork-desktop/src/remote_service.rs crates/riwork-desktop/src/schedule_panel.rs crates/riwork-desktop/src/schedules.rs crates/riwork-desktop/src/settings.rs crates/riwork-desktop/src/status_bar.rs crates/riwork-desktop/src/subshells.rs crates/riwork-desktop/src/symbols.rs crates/riwork-desktop/src/tab_close_dialog.rs crates/riwork-desktop/src/tab_menu.rs crates/riwork-desktop/src/tab_strip.rs crates/riwork-desktop/src/terminal_drop.rs crates/riwork-desktop/src/terminal_lifecycle.rs crates/riwork-desktop/src/terminal_links.rs crates/riwork-desktop/src/terminal_links/overlay.rs crates/riwork-desktop/src/terminal_links/tests.rs crates/riwork-desktop/src/terminal_links/ui.rs crates/riwork-desktop/src/text_input.rs crates/riwork-desktop/src/text_input/tests.rs crates/riwork-desktop/src/theme.rs crates/riwork-desktop/src/tooltip.rs crates/riwork-desktop/src/ui_text.rs` | exit 0 |
| C249 | `sw_vers` | exit 0 |
| C250 | `uname -m` | exit 0 |
| C251 | `git rev-parse HEAD` | exit 0 |
| C252 | `rg -n 'setInterval\(\|requestAnimationFrame\(\|\.refetch\(' apps/riwork -g '!**/*.test.ts*'` | exit 1 |
| C253 | `rg -n 'nsh/storage\|appearance_file\|sessions.json\|chats/' apps/riwork crates/riwork-nsh-native crates/riwork-desktop/src/nsh_host.rs` | exit 0 |
| C254 | `/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh check apps/riwork --tsc /Users/dominik/Library/Caches/nsh/typescript-7.0.2-darwin-arm64/package/lib/tsc` | exit 0 |
| C255 | `/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh test apps/riwork --junit /private/tmp/riwork-p1-m5-f6975181/nsh-yu1ieoe3/nsh.xml` | exit 0 |
| C256 | `git diff ea0f2f5 -- crates/riwork-cli` | exit 0 |
| C257 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork project add /private/tmp/riwork-p1-m5-f6975181/parity-f_vatnks/cli-fixture/project --name Alpha --json` | exit 0 |
| C258 | `/private/tmp/riwork-p1-m5-f6975181/reference-target/debug/riwork project list --json` | exit 0 |
| C259 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork project list --json` | exit 0 |
| C260 | `/private/tmp/riwork-p1-m5-f6975181/reference-target/debug/riwork settings list --json` | exit 0 |
| C261 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork settings list --json` | exit 0 |
| C262 | `git diff ea0f2f5 -- crates/riwork-cli` | exit 0 |
| C263 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork project add /private/tmp/riwork-p1-m5-f6975181/parity-zh7mtux5/cli-fixture/project --name Alpha --json` | exit 0 |
| C264 | `/private/tmp/riwork-p1-m5-f6975181/reference-target/debug/riwork project list --json` | exit 0 |
| C265 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork project list --json` | exit 0 |
| C266 | `/private/tmp/riwork-p1-m5-f6975181/reference-target/debug/riwork settings list --json` | exit 0 |
| C267 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork settings list --json` | exit 0 |
| C268 | `/private/tmp/riwork-p1-m5-f6975181/reference-target/debug/riwork instances --json` | exit 0 |
| C269 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork instances --json` | exit 0 |
| C270 | `/private/tmp/riwork-p1-m5-f6975181/reference-target/debug/riwork appearance --json` | exit 2 |
| C271 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork appearance --json` | exit 2 |
| C272 | `/private/tmp/riwork-p1-m5-f6975181/reference-target/debug/riwork nosuchcommand` | exit 2 |
| C273 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork nosuchcommand` | exit 2 |
| C274 | `cargo run -p riwork-nsh-native --features test-support --example p1_fixture_home --locked --offline -- --scenario p1-basic --out /private/tmp/riwork-p1-m5-f6975181/probes-sdm1zgnm/p1-basic/home` | exit 101 |
| C275 | `cargo run -p riwork-nsh-native --features test-support --example p1_fixture_home --locked --offline -- --scenario p1-layouts --out /private/tmp/riwork-p1-m5-f6975181/probes-sdm1zgnm/p1-layouts/home` | exit 101 |
| C276 | `cargo run -p riwork-nsh-native --features test-support --example p1_fixture_home --locked --offline -- --scenario p1-restore --out /private/tmp/riwork-p1-m5-f6975181/probes-sdm1zgnm/p1-restore/home` | exit 101 |
| C277 | `cargo run -p riwork-nsh-native --features test-support --example p1_fixture_home --locked --offline -- --scenario p1-theme --out /private/tmp/riwork-p1-m5-f6975181/probes-sdm1zgnm/p1-theme/home` | exit 101 |
| C278 | `cargo test -p riwork-nsh-native --features test-support --test hubs --locked --offline -- two_engines_share_one_home_watcher_and_one_hub_per_topic` | exit 101 |
| C279 | `rg -n 'perfSettled\|surface.*monitor\|performance_snapshot\|performanceSnapshot' crates/riwork-desktop/src/nsh_host.rs apps/riwork` | exit 1 |
| C280 | `git archive --format=tar --output /private/tmp/riwork-p1-m5-f6975181/reference-_12ls4e5/ea0f2f5.tar ea0f2f5` | exit 0 |
| C281 | `cargo build --manifest-path /private/tmp/riwork-p1-m5-f6975181/reference-_12ls4e5/reference/Cargo.toml --target-dir /private/tmp/riwork-p1-m5-f6975181/reference-target -p riwork-desktop --locked --offline` | exit 101 |
| C282 | `cargo test --manifest-path /private/tmp/riwork-p1-m5-f6975181/reference-_12ls4e5/reference/Cargo.toml --target-dir /private/tmp/riwork-p1-m5-f6975181/reference-target -p riwork-desktop --release --locked --offline measure_a_busy_workspace_frame -- --ignored --nocapture` | exit 101 |
| C283 | `/Users/dominik/.local/share/riwork/toolchains/zig-0.16.0/zig version` | exit 0 |
| C284 | `git archive --format=tar --output /private/tmp/riwork-p1-m5-f6975181/reference-phz1dufl/ea0f2f5.tar ea0f2f5` | exit 0 |
| C285 | `cargo build --manifest-path /private/tmp/riwork-p1-m5-f6975181/reference-phz1dufl/reference/Cargo.toml --target-dir /private/tmp/riwork-p1-m5-f6975181/reference-target -p riwork-desktop --locked --offline` | exit 0 |
| C286 | `cargo test --manifest-path /private/tmp/riwork-p1-m5-f6975181/reference-phz1dufl/reference/Cargo.toml --target-dir /private/tmp/riwork-p1-m5-f6975181/reference-target -p riwork-desktop --release --locked --offline measure_a_busy_workspace_frame -- --ignored --nocapture` | exit 0 |
| C287 | `git diff --cached --check` | exit 0 |
| C288 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork /private/tmp/rwm5-ikpfjuzy/repos/Alpha` | exit 2 |
| C289 | `pgrep -f /private/tmp/rwm5-ikpfjuzy` | exit 1 |
| C290 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork /private/tmp/rwm5-ki8420zw/repos/Alpha` | exit -15 |
| C291 | `cua-driver MCP list_windows({"pid":97664,"session":"p1-acceptance"})` | recorded structured Cua result |
| C292 | `cua-driver MCP get_window_state({"pid":97664,"window_id":6375,"session":"p1-acceptance","query":"","tree_format":"elements","include_screenshot":false})` | recorded structured Cua result |
| C293 | `cua-driver MCP get_window_state({"pid":97664,"window_id":6375,"session":"p1-acceptance","include_accessibility_tree":false,"screenshot_out_file":"/private/tmp/rwm5-ki8420zw/evidence/alpha.png"})` | recorded structured Cua result |
| C294 | `cua-driver MCP get_window_state({"pid":97664,"window_id":6376,"session":"p1-acceptance","query":"","tree_format":"elements","include_screenshot":false})` | recorded structured Cua result |
| C295 | `cua-driver MCP get_window_state({"pid":97664,"window_id":6376,"session":"p1-acceptance","include_accessibility_tree":false,"screenshot_out_file":"/private/tmp/rwm5-ki8420zw/evidence/beta.png"})` | recorded structured Cua result |
| C296 | `cua-driver MCP get_window_state({"pid":97664,"window_id":6375,"session":"p1-acceptance","query":"close","tree_format":"elements","include_screenshot":false})` | recorded structured Cua result |
| C297 | `cua-driver MCP verify_state({"pid":97664,"window_id":6375,"session":"p1-acceptance","expect":[{"window":{"exists":true,"bounds":{"x":1070,"y":334,"width":1220,"height":780,"tolerance_px":1}}},{"element":{"selector":{"label_contains":"RiWork \u00b7 Alpha"},"exists":true}}],"timeout_ms":5000,"stable_samples":2})` | recorded structured Cua result |
| C298 | `cua-driver MCP verify_state({"pid":97664,"window_id":6376,"session":"p1-acceptance","expect":[{"window":{"exists":true,"bounds":{"x":1092,"y":356,"width":1220,"height":780,"tolerance_px":1}}},{"element":{"selector":{"label_contains":"RiWork \u00b7 Beta"},"exists":true}}],"timeout_ms":5000,"stable_samples":2})` | recorded structured Cua result |
| C299 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork /private/tmp/rwm5-u3qi2ooy/repos/Alpha` | exit -15 |
| C300 | `cua-driver MCP list_windows({"pid":25211,"session":"p1-acceptance"})` | recorded structured Cua result |
| C301 | `cua-driver MCP get_window_state({"pid":25211,"window_id":6387,"session":"p1-acceptance","query":"close","tree_format":"elements","include_screenshot":false})` | recorded structured Cua result |
| C302 | `cua-driver MCP get_window_state({"pid":25211,"window_id":6387,"session":"p1-acceptance","include_accessibility_tree":false,"screenshot_out_file":"/private/tmp/rwm5-u3qi2ooy/evidence/alpha.png"})` | recorded structured Cua result |
| C303 | `cua-driver MCP get_window_state({"pid":25211,"window_id":6388,"session":"p1-acceptance","query":"close","tree_format":"elements","include_screenshot":false})` | recorded structured Cua result |
| C304 | `cua-driver MCP get_window_state({"pid":25211,"window_id":6388,"session":"p1-acceptance","include_accessibility_tree":false,"screenshot_out_file":"/private/tmp/rwm5-u3qi2ooy/evidence/beta.png"})` | recorded structured Cua result |
| C305 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork /private/tmp/rwm5-ikpfjuzy/repos/Alpha` | exit -15 |
| C306 | `cua-driver MCP list_windows({"pid":81359,"session":"p1-acceptance"})` | recorded structured Cua result |
| C307 | `cua-driver MCP get_window_state({"pid":81359,"window_id":6413,"session":"p1-acceptance","query":"close","tree_format":"elements","include_screenshot":false})` | recorded structured Cua result |
| C308 | `cua-driver MCP get_window_state({"pid":81359,"window_id":6413,"session":"p1-acceptance","include_accessibility_tree":false,"screenshot_out_file":"/private/tmp/rwm5-ikpfjuzy/evidence/alpha.png"})` | recorded structured Cua result |
| C309 | `cua-driver MCP get_window_state({"pid":81359,"window_id":6414,"session":"p1-acceptance","query":"close","tree_format":"elements","include_screenshot":false})` | recorded structured Cua result |
| C310 | `cua-driver MCP get_window_state({"pid":81359,"window_id":6414,"session":"p1-acceptance","include_accessibility_tree":false,"screenshot_out_file":"/private/tmp/rwm5-ikpfjuzy/evidence/beta.png"})` | recorded structured Cua result |
| C311 | `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork /private/tmp/rwm5-eo6nq7gc/repos/Alpha` | exit -15 |
| C312 | `cua-driver MCP list_windows({"pid":46330,"session":"p1-acceptance"})` | recorded structured Cua result |
| C313 | `cua-driver MCP get_window_state({"pid":46330,"window_id":6400,"session":"p1-acceptance","query":"close","tree_format":"elements","include_screenshot":false})` | recorded structured Cua result |
| C314 | `cua-driver MCP verify_state({"pid":46330,"window_id":6400,"session":"p1-acceptance","expect":[{"window":{"exists":true,"bounds":{"x":1070,"y":334,"width":1220,"height":780,"tolerance_px":1}}},{"element":{"selector":{"label_contains":"Close"},"exists":true}}],"timeout_ms":5000,"stable_samples":2})` | recorded structured Cua result |
| C315 | `cua-driver MCP get_window_state({"pid":46330,"window_id":6400,"session":"p1-acceptance","include_accessibility_tree":false,"screenshot_out_file":"/private/tmp/rwm5-eo6nq7gc/evidence/alpha.png"})` | recorded structured Cua result |
| C316 | `cua-driver MCP get_window_state({"pid":46330,"window_id":6401,"session":"p1-acceptance","query":"close","tree_format":"elements","include_screenshot":false})` | recorded structured Cua result |
| C317 | `cua-driver MCP verify_state({"pid":46330,"window_id":6401,"session":"p1-acceptance","expect":[{"window":{"exists":true,"bounds":{"x":1092,"y":356,"width":1220,"height":780,"tolerance_px":1}}},{"element":{"selector":{"label_contains":"Close"},"exists":true}}],"timeout_ms":5000,"stable_samples":2})` | recorded structured Cua result |
| C318 | `cua-driver MCP get_window_state({"pid":46330,"window_id":6401,"session":"p1-acceptance","include_accessibility_tree":false,"screenshot_out_file":"/private/tmp/rwm5-eo6nq7gc/evidence/beta.png"})` | recorded structured Cua result |
| C319 | `ps -o time= -p 97664` | see route log |
