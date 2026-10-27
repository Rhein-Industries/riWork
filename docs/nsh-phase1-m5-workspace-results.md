# Phase 1 M5 workspace-route execution

Assignment `f6975181-1a9a-41ea-a500-87e47abc1ad6`; worktree `/Users/dominik/orca/projects/riWork-nsh-p1-m5`.

`verification_environment = macOS arm64; isolated RIWORK_HOME, HOME, runtime, tmux and cache directories; NSH 0.3.0 headless engines; no desktop calls or live state.`

This report covers the workspace routes delegated by the coordinator. The package is `riwork-nsh-workspace` at `apps/riwork`; its registry is integrated through `riwork-nsh-native`. The final M5 report combines overlapping native, host, AppKit and fixture/Cua routes. A missing test target or an exit-zero filter with zero tests is a **failure of the required route**, never a pass. Existing tests are mapped only to assertions they actually execute.

Final verification: **36 Rust workspace tests passed** (26 unit + 10 layout roundtrip), **17 NSH tests passed**, and NSH TypeScript/native checks passed. These suite totals do not substitute for the missing acceptance coverage.

Changes made here:

- Fixed a real panel identity bug: the JSX condition depended on every `dock()` projection, so selecting a tab remounted DockArea and the placeholder content. A separate one-time readiness signal retains the area; actual per-engine component counters now stay `mount 1` through selection, cross-group drag and resize.
- Added title/mount/visibility placeholder diagnostics without starting deferred surfaces. Restored the exact empty-pane guidance and `Pane divider` accessibility label.
- Declared the deferred command/chord seams explicitly, kept those chords inert, and added the missing canonical Cmd-Shift-bracket tab-cycle bindings (the physical Cmd-brace chords). Active commands not implemented by M4 remain acceptance failures.
- Added the named 60-second virtual idle check and an executable no-polling lint script.

No legacy GPUI source/test was deleted or modified here. Section 4.3 ledger successors that are still absent stay absent in the table; the deletion embargo remains in force. No merge, push, install, commit or task-status action was performed by this delegated worker. The coordinator owns the final commit.

Exact run commands (cwd is the worktree above):

```sh
python3 crates/riwork-desktop/verification/phase1_integration.py nsh --root /tmp/riwork-p1-m5-workspace --target-dir /private/tmp/riwork-p1-integration-mkzwamt7/target
sh apps/riwork/check-polling.sh
```

Final manifest: `/private/tmp/riwork-p1-m5-workspace/nsh-reh5ez2m/manifest.json`; JUnit: `/private/tmp/riwork-p1-m5-workspace/nsh-reh5ez2m/nsh.xml`. The manifest records exact argv, environment, output paths and SHA-256 hashes. The seeded fixture write audit reported no deferred state or tmux files/sockets. The committed [JSON record](nsh-phase1-m5-workspace-results.json) retains the command/environment mapping for every item.

Two earlier runs remain recorded: `nsh-mulsume0` failed after an exploratory alias used literal `mod+}`/`mod+{`, which NSH rejects (it requires the unshifted physical key plus Shift); `nsh-r7hw44bu` passed 16/17 and exposed the real remount regression with the new counter. Both were repaired before the final 17/17 run.

| Section / route scope | Pass | Fail |
|---|---:|---:|
| GLB | 0 | 10 |
| PANE | 0 | 12 |
| TAB | 0 | 5 |
| STRIP | 0 | 5 |
| DRAG | 0 | 4 |
| LAY | 1 | 9 |
| KEYS | 0 | 5 |
| STAT | 0 | 5 |
| WIN | 0 | 5 |
| RLD | 1 | 0 |
| THM | 0 | 2 |
| TXT | 0 | 2 |
| POLL | 2 | 0 |
| PERF | 0 | 1 |

The four pass entries below are workspace-route results; `RLD-04` and `POLL-02` still require the coordinator to combine the other owners’ evidence. `PERF-01` is coordinator-owned and report-only under adopted D-PERF-1. `PERF-02` remains a hard failed structural guard.

| Item | Workspace route | Exact command references | Result / missing criteria |
|---|---|---|---|
| P1-GLB-01 | FAIL | W002, W103 | Global entry only renders Global workspace; global.show has no subscription or selected chat tab. |
| P1-GLB-02 | FAIL | W003, W103 | Core global size validation passes; global TSX never posts window.setDockInfo. Host/Cua title/size delegated. |
| P1-GLB-03 | FAIL | W003, W103 | Global state adapter and view/filter/sort controls absent; no Dashboard persistence subscription. |
| P1-GLB-04 | FAIL | W004, W090, W103 | Restore readiness exists, but restore.global is ignored and global chat selection is absent. |
| P1-GLB-05 | FAIL | W005, W103 | Production global test proves no DockArea and no project session. Global-only disk audit remains required in coordinator fixture route. |
| P1-GLB-06 | FAIL | W006, W103 | Required segmented bar, labels, metrics and persistent switch are absent. |
| P1-GLB-07 | FAIL | W007, W103 | Global root does not bind Cmd-W or send window.close. |
| P1-GLB-08 | FAIL | W008, W103 | Global key routing, window.list lookup and fallback notice absent. |
| P1-GLB-09 | FAIL | W009, W103 | No TSX route event handler or Settings selection. |
| P1-GLB-10 | FAIL | W010, W103 | Global path replaces startup notice with Global workspace; no notice dismiss button. |
| P1-PANE-01 | FAIL | W011, W012, W103, W001 | Default five-panel model exists, but Sessions.create is never called; no-layout shell and panels_initialized reconstruction absent. |
| P1-PANE-02 | FAIL | W012, W103, W001 | No split keyboard or create/rollback implementation. |
| P1-PANE-03 | FAIL | W013, W014, W015, W103, W001 | Core ratio tests and PaneTree clamp pass; actual Headless divider/persist route absent. Divider selector repaired; NSH overlay still 1 pt, not core 5 pt gap. |
| P1-PANE-04 | FAIL | W016, W012, W103, W001 | Core locked geometry and model resizing pass; 2 s guard and focus-mode invariance absent; named Headless/extents matrix missing. |
| P1-PANE-05 | FAIL | W017, W018, W103, W001 | Lock toggle exists; close refusal notices and width-dependent lock control absent. Ledger test not moved. |
| P1-PANE-06 | FAIL | W019, W020, W021, W022, W103, W001 | Main toggle exists; new-tab routing and focus behavior absent; ledger routing tests not moved. |
| P1-PANE-07 | FAIL | W023, W024, W025, W015, W103, W001 | Sentinel retention model tests pass; exact empty copy fixed; close-pane command and inherited-tab flow absent. |
| P1-PANE-08 | FAIL | W026, W027, W103, W001 | Gather model/menu exists; Cmd-Shift-M, pane-menu entry and required notices absent; ledger tests not moved. |
| P1-PANE-09 | FAIL | W028, W029, W030, W031, W032, W103, W001 | No Default Layout command/menu action or five moved ledger tests. |
| P1-PANE-10 | FAIL | W012, W103, W001 | No focus-mode presentation, centered mode, restore button, or state fields. |
| P1-PANE-11 | FAIL | W033, W103, W001 | Existing click test proves activation but required pane-* selectors and Cmd-T route absent. |
| P1-PANE-12 | FAIL | W034, W035, W103, W001 | Pane context menu has only lock/main; required full rows, disabled deferred entries, selectors and metrics absent. |
| P1-TAB-01 | FAIL | W036, W037, W012, W103, W001 | Core persisted remote/chat shape tests pass; pending-chat identity and no-save test/implementation absent. |
| P1-TAB-02 | FAIL | W038, W012, W103, W001 | No shell creation, placeholder panel routing, Dashboard redirect, casing or tooltip route. |
| P1-TAB-03 | FAIL | W015, W103, W001 | Mapped native active-index and TSX tab selection/visibility tests execute; named full focus/scroll/persistence route not present. |
| P1-TAB-04 | FAIL | W039, W103, W001 | No close dialog, Tabs update, session close, lock refusal or close-key flow. |
| P1-TAB-05 | FAIL | W012, W103, W001 | No session subscription/adoption route, caps or lock filtering. |
| P1-STRIP-01 | FAIL | W040, W103 | Shared membership unavailable; frontend always renders pane tab groups. |
| P1-STRIP-02 | FAIL | W035, W103 | No shared strip status mapping, status accessibility label, close button or metrics route. |
| P1-STRIP-03 | FAIL | W041, W103 | Right-click opens pane lock/main only; rename/move/close shared-tab context menu absent. |
| P1-STRIP-04 | FAIL | W042, W103 | No All tabs overflow menu, selection reveal or keyboard menu focus flow. |
| P1-STRIP-05 | FAIL | W043, W044, W045, W046, W047, W048, W091, W092, W093, W094, W095, W103 | Core inventory tests pass; no Tabs subscription/revision gate or two-window hide delivery. |
| P1-DRAG-01 | FAIL | W049, W015, W103, W001 | Existing real DockArea move into empty pane passes; shared Move publish, ghost styling and required full L-NESTED/second-open route absent. |
| P1-DRAG-02 | FAIL | W015, W103, W001 | Four-direction PaneTree move test exists, but no real Headless edge-drop test or measured drop fraction. |
| P1-DRAG-03 | FAIL | W050, W001 | Host-unit and no-handler review delegated to host report. |
| P1-DRAG-04 | FAIL | W051, W103, W001 | Actual mount/visibility diagnostics exposed DockArea remounting on every projection. Replaced dock() JSX gate with one-time dockReady. Regression now retains mount 1 through selection, cross-group drag and resize. Edge/focus/project-switch identity proof remains absent; full item fails. |
| P1-LAY-01 | FAIL | W052, W103, W001 | Existing oracle covers geometry and several fixtures, not one named test per full §4.7 and L-* matrix. |
| P1-LAY-02 | FAIL | W015, W103, W001 | Existing PaneTree operations and one UI drag pass; prescribed real Headless output per gesture and counter test absent. |
| P1-LAY-03 | FAIL | W053, W054, W103, W001 | 10 roundtrip tests cover storage subsets; complete named L-* manifest, byte/mtime no-edit and Reopen UI matrix absent. |
| P1-LAY-04 | FAIL | W055, W056, W012, W103, W001 | Mapped damaged/newer recovery Rust tests pass; required default notice suffix frontend test absent. |
| P1-LAY-05 | FAIL | W057, W058, W012, W103, W001 | Core legacy migration passes; global-tab pruning/MOVED_NOTICE bridge and TSX paths absent. |
| P1-LAY-06 | FAIL | W012, W103, W001 | No Sessions reconciliation, dead/detached/adoption retry implementation. |
| P1-LAY-07 | FAIL | W012, W103, W001 | 250 ms worker and flush/drop tests pass; full trigger/ManualClock matrix and restart/project-switch UI missing. |
| P1-LAY-08 | FAIL | W012, W103, W001 | Safe aliases partly pass but unknown-tab aliases intentionally persist original unsafe disk ids; pending-chat mapping absent. |
| P1-LAY-09 | FAIL | W059, W060, W061, W103, W001 | Core carry tests and persistence switch pass; no project.open TSX subscription or carried component identity. |
| P1-LAY-10 | PASS | W001, W062, W103 | Mapped layout_roundtrip oracle persists through LayoutStore and loads equal ProjectLayout; writer review confirms no alternate production layouts.json writer. |
| P1-KEYS-01 | FAIL | W063, W103 | Added exact deferred seams and inert chord test; full GPUI active table and cross-host uniqueness successor remain absent. |
| P1-KEYS-02 | FAIL | W064, W103 | Number/model tests and canonical bracket-cycle aliases pass; shared-order/modal route and manual AppKit Ctrl-Tab check missing. |
| P1-KEYS-03 | FAIL | W063, W103 | Deferred chords now declared/inert; required panel/new-window active key routes absent. |
| P1-KEYS-04 | FAIL | W065, W103 | No modal or strip menu implementation; corresponding focus/escape tests absent. |
| P1-KEYS-05 | FAIL | W066 | Host-unit/appkit-only route delegated to host report; no TSX text-size owner exists. |
| P1-STAT-01 | FAIL | W067, W068, W069, W035, W103 | Core configuration tests pass; bar hard-coded and not Settings-owned; full defaults/order/metrics missing. |
| P1-STAT-02 | FAIL | W070, W103 | No Project Alpha selector, worktree/live/session or required no-data items. |
| P1-STAT-03 | FAIL | W071, W103 | Current popup has Gather tabs into main pane, separator and summary; Default layout, toggle/geometry and required selector absent. |
| P1-STAT-04 | FAIL | W072, W103 | Sticky dismissible bottom notice exists; top-right toast, replacement/focus/Escape proof absent. |
| P1-STAT-05 | FAIL | W073, W103 | top-orchestrator global menu absent. |
| P1-WIN-04 | FAIL | W074, W075, W103 | Session has no remembered-size API; core invalid-size test passes. Full window_size bridge target absent. |
| P1-WIN-05 | FAIL | W076, W103 | Mapped model title/dedup test passes; project title derives path basename, lacks Store-name/remote metadata and memoized reactive updates. |
| P1-WIN-06 | FAIL | W077, W035, W103 | No theme/fullscreen-controlled window-controls-island or metrics tests; separate 28 pt title row. |
| P1-WIN-09 | FAIL | W078, W103 | No window.state listener. Existing default ignoring is inert, but named TSX event check absent. |
| P1-WIN-10 | FAIL | W079, W103 | Cmd-Shift-N is not bound and no window.open is emitted. |
| P1-RLD-04 | PASS | W080, W081, W012, W096, W103, W001 | Mapped App.test.tsx production restore confirms native payload installation before exactly one app.restoreReady; nonrestore test confirms none. layout_roundtrip verifies saved restore wins. Workspace component PASS; coordinator combines host runtime-window routes. |
| P1-THM-04 | FAIL | W082, W103 | Frontend hard-codes colors and never subscribes Appearance; first Native light paint fails implementation review. |
| P1-THM-05 | FAIL | W083, W103 | No Appearance/Settings subscriptions or per-window color-scheme update; native/host portions delegated. |
| P1-TXT-02 | FAIL | W084, W103 | No command listener or Settings text sizing; hard-coded text sizes. |
| P1-TXT-03 | FAIL | W085, W086, W103 | No font choice/offset/space/casing implementation; moved/new sentence-case test absent. |
| P1-POLL-02 | PASS | W087, W103 | Named Idle.test.tsx passes 60,000 ms virtual time with unchanged host calls AND posts. Native agent separately passes the 60 s quiet-home bridge and sampler arm/disarm regression; coordinator combines native/host evidence. |
| P1-POLL-03 | PASS | W089, W097, W103 | Forbidden polling review command returns no source hits; check included in this recorded acceptance execution. |
| P1-PERF-02 | FAIL | W015 | Required per-gesture bridge counters absent. Partial no-fallback model tests do not prove hard structural guards. |

Each `Wnnn` below is the literal command executed, under its referenced isolated environment. `test -f` is a required-route availability probe: exit 1 records the missing named test instead of inventing test coverage. Full Cargo missing-target invocations returned 101. Core/engine filters that report zero executed tests are called out explicitly.

| Command | Exact argv as shell | Exit | Tests passed / failed / ignored | Environment |
|---|---|---:|---|---|
| W001 | `cargo test -p riwork-nsh-workspace --locked --offline` | 0 | 26/0/0; 10/0/0; 0/0/0 | E1 |
| W002 | `test -f apps/riwork/tests/Global.test.tsx` | 1 | — | E1 |
| W003 | `cargo test -p riwork-core --lib global_window::tests --locked --offline` | 0 | 3/0/0 | E1 |
| W004 | `cargo test -p riwork-core --lib the_global_window_restores_as_itself_and_older_snapshots_hold_project_windows --locked --offline` | 0 | 0/0/0 | E1 |
| W005 | `rg -n P1-GLB-05 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | 1 | — | E1 |
| W006 | `rg -n P1-GLB-06 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | 1 | — | E1 |
| W007 | `rg -n P1-GLB-07 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | 1 | — | E1 |
| W008 | `rg -n P1-GLB-08 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | 1 | — | E1 |
| W009 | `rg -n P1-GLB-09 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | 1 | — | E1 |
| W010 | `rg -n P1-GLB-10 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | 1 | — | E1 |
| W011 | `test -f apps/riwork/tests/Panes.test.tsx` | 1 | — | E1 |
| W012 | `cargo test -p riwork-nsh-native --features test-support --test layout_session --locked --offline -- --list` | 101 | — | E1 |
| W013 | `cargo test -p riwork-core --lib split_ratios_round_trip_and_survive_insertions_and_collapses --locked --offline` | 0 | 1/0/0 | E1 |
| W014 | `cargo test -p riwork-core --lib invalid_ratios_are_repaired_before_persistence --locked --offline` | 0 | 1/0/0 | E1 |
| W015 | `cargo test -p riwork-nsh-native --features test-support --test layout_gestures --locked --offline -- --list` | 101 | — | E1 |
| W016 | `cargo test -p riwork-core --lib preserve_locked --locked --offline` | 0 | 8/0/0 | E1 |
| W017 | `cargo test -p riwork-core --lib lock_state_follows_the_users_choice_and_defaults_to_the_navigation_pane --locked --offline` | 0 | 0/0/0 | E1 |
| W018 | `cargo test -p riwork-core --lib stale_lock_ids_are_removed_without_reenabling_auto_lock --locked --offline` | 0 | 1/0/0 | E1 |
| W019 | `cargo test -p riwork-core --lib only_a_main_pane_that_exists_outside_focus_mode_takes_what_is_opened --locked --offline` | 0 | 0/0/0 | E1 |
| W020 | `cargo test -p riwork-core --lib a_new_tab_opens_in_the_main_pane_whichever_pane_is_selected --locked --offline` | 0 | 0/0/0 | E1 |
| W021 | `cargo test -p riwork-core --lib an_open_chat_orchestrator_tab_comes_to_the_front_by_the_main_pane_rules --locked --offline` | 0 | 0/0/0 | E1 |
| W022 | `cargo test -p riwork-core --lib a_main_pane_that_is_not_in_the_layout_is_dropped_and_there_is_never_more_than_one --locked --offline` | 0 | 1/0/0 | E1 |
| W023 | `cargo test -p riwork-core --lib closing_a_pane_focuses_the_pane_that_takes_its_space --locked --offline` | 0 | 0/0/0 | E1 |
| W024 | `cargo test -p riwork-core --lib a_closed_pane_hands_its_tabs_to_the_main_pane_after_its_own --locked --offline` | 0 | 0/0/0 | E1 |
| W025 | `cargo test -p riwork-core --lib closing_the_main_pane_or_a_locked_one_or_having_no_main_pane_closes_the_tabs_as_before --locked --offline` | 0 | 0/0/0 | E1 |
| W026 | `cargo test -p riwork-core --lib a_gather_moves_every_unlocked_tab_into_the_main_pane_and_leaves_the_locked_pane_alone --locked --offline` | 0 | 0/0/0 | E1 |
| W027 | `cargo test -p riwork-core --lib a_gather_never_touches_any_locked_pane_among_several --locked --offline` | 0 | 0/0/0 | E1 |
| W028 | `cargo test -p riwork-core --lib a_messy_window_becomes_a_locked_navigation_pane_beside_the_main_pane --locked --offline` | 0 | 0/0/0 | E1 |
| W029 | `cargo test -p riwork-core --lib the_default_layout_closes_nothing_and_a_locked_panes_tabs_are_moved_not_dropped --locked --offline` | 0 | 0/0/0 | E1 |
| W030 | `cargo test -p riwork-core --lib a_window_that_is_laid_out_already_is_left_exactly_as_it_is --locked --offline` | 0 | 0/0/0 | E1 |
| W031 | `cargo test -p riwork-core --lib panes_keep_their_ids_and_a_new_one_is_made_only_when_there_is_none_to_keep --locked --offline` | 0 | 0/0/0 | E1 |
| W032 | `cargo test -p riwork-core --lib the_default_layout_is_saved_as_the_projects_layout_and_reads_back_as_it_was --locked --offline` | 0 | 0/0/0 | E1 |
| W033 | `rg -n P1-PANE-11 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | 1 | — | E1 |
| W034 | `test -f apps/riwork/tests/PaneMenu.test.tsx` | 1 | — | E1 |
| W035 | `test -f apps/riwork/tests/metrics.test.ts` | 1 | — | E1 |
| W036 | `cargo test -p riwork-core --lib a_remote_shell_tab_has_a_fixed_json_shape_and_key --locked --offline` | 0 | 1/0/0 | E1 |
| W037 | `cargo test -p riwork-core --lib a_chat_tab_has_a_fixed_json_shape_and_key --locked --offline` | 0 | 1/0/0 | E1 |
| W038 | `test -f apps/riwork/tests/Tabs.test.tsx` | 1 | — | E1 |
| W039 | `test -f apps/riwork/tests/CloseDialog.test.tsx` | 1 | — | E1 |
| W040 | `test -f apps/riwork/tests/Strip.test.tsx` | 1 | — | E1 |
| W041 | `rg -n P1-STRIP-03 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | 1 | — | E1 |
| W042 | `rg -n P1-STRIP-04 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | 1 | — | E1 |
| W043 | `cargo test -p riwork-core --lib stale_inventory_keeps_hide_and_rename --locked --offline` | 0 | 0/0/0 | E1 |
| W044 | `cargo test -p riwork-core --lib one_refresh_transaction_ends_where_separate_transactions_did --locked --offline` | 0 | 0/0/0 | E1 |
| W045 | `cargo test -p riwork-core --lib titles_come_from_a_rename_then_the_provider_then_the_first_message_then_the_kind --locked --offline` | 0 | 0/0/0 | E1 |
| W046 | `cargo test -p riwork-core --lib a_title_is_set_once_and_only_a_higher_source_or_a_new_rename_replaces_it --locked --offline` | 0 | 0/0/0 | E1 |
| W047 | `cargo test -p riwork-core --lib new_sessions_append_and_any_tab_hides --locked --offline` | 0 | 0/0/0 | E1 |
| W048 | `cargo test -p riwork-nsh-native --features test-support --test tabs --locked --offline -- --list` | 101 | — | E1 |
| W049 | `test -f apps/riwork/tests/Drag.test.tsx` | 1 | — | E1 |
| W050 | `rg -n P1-DRAG-03 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | 1 | — | E1 |
| W051 | `test -f apps/riwork/tests/Identity.test.tsx` | 1 | — | E1 |
| W052 | `cargo test -p riwork-nsh-native --features test-support --test layout_oracle --locked --offline -- --list` | 101 | — | E1 |
| W053 | `test -f apps/riwork/tests/Reopen.test.tsx` | 1 | — | E1 |
| W054 | `cargo test -p riwork-nsh-native --features test-support --test layout_reopen --locked --offline -- --list` | 101 | — | E1 |
| W055 | `cargo test -p riwork-core --lib corrupt_file_is_set_aside_on_save_instead_of_crashing_or_being_deleted --locked --offline` | 0 | 1/0/0 | E1 |
| W056 | `cargo test -p riwork-core --lib newer_schema_is_neither_loaded_nor_overwritten --locked --offline` | 0 | 1/0/0 | E1 |
| W057 | `cargo test -p riwork-core --lib legacy_shell_layout_migrates_without_changing_tab_order_or_selection --locked --offline` | 0 | 1/0/0 | E1 |
| W058 | `cargo test -p riwork-core --lib mixed_tabs_preserve_order_and_selection_while_migrating_bottom_strips --locked --offline` | 0 | 1/0/0 | E1 |
| W059 | `cargo test -p riwork-core --lib carry_preserves_locked_tabs_ratios_and_maps_colliding_destination_ids --locked --offline` | 0 | 1/0/0 | E1 |
| W060 | `cargo test -p riwork-core --lib destination_locked_pane_sessions_are_recovered_into_unlocked_region --locked --offline` | 0 | 1/0/0 | E1 |
| W061 | `cargo test -p riwork-core --lib a_locked_main_pane_stays_the_main_pane_when_the_project_changes --locked --offline` | 0 | 1/0/0 | E1 |
| W062 | `rg -n 'layouts\.json' crates/riwork-nsh-native crates/riwork-desktop/src/nsh_main.rs crates/riwork-desktop/src/nsh_host.rs apps/riwork -g '!**/target/**' -g '!**/.nsh/**'` | 0 | — | E1 |
| W063 | `test -f apps/riwork/tests/keymap.test.ts` | 1 | — | E1 |
| W064 | `cargo test -p riwork-core --lib command_numbers_count_session_tabs_and_nine_is_the_last --locked --offline` | 0 | 0/0/0 | E1 |
| W065 | `rg -n P1-KEYS-04 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | 1 | — | E1 |
| W066 | `rg -n P1-KEYS-05 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | 1 | — | E1 |
| W067 | `cargo test -p riwork-core --lib status_bar::tests --locked --offline` | 0 | 4/0/0 | E1 |
| W068 | `cargo test -p riwork-core --lib routing_and_reordering_preserve_other_side_and_round_trip --locked --offline` | 0 | 1/0/0 | E1 |
| W069 | `test -f apps/riwork/tests/StatusBar.test.tsx` | 1 | — | E1 |
| W070 | `rg -n P1-STAT-02 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | 1 | — | E1 |
| W071 | `rg -n P1-STAT-03 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | 1 | — | E1 |
| W072 | `rg -n P1-STAT-04 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | 1 | — | E1 |
| W073 | `rg -n P1-STAT-05 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | 1 | — | E1 |
| W074 | `cargo test -p riwork-core --lib legacy_and_invalid_window_sizes_do_not_break_layout_restoration --locked --offline` | 0 | 1/0/0 | E1 |
| W075 | `cargo test -p riwork-nsh-native --features test-support --test window_size --locked --offline -- --list` | 101 | — | E1 |
| W076 | `test -f apps/riwork/tests/Window.test.tsx` | 1 | — | E1 |
| W077 | `test -f apps/riwork/tests/Chrome.test.tsx` | 1 | — | E1 |
| W078 | `rg -n P1-WIN-09 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | 1 | — | E1 |
| W079 | `rg -n P1-WIN-10 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | 1 | — | E1 |
| W080 | `cargo test -p riwork-core --lib identifiers_geometry_and_private_file_read_are_validated --locked --offline` | 0 | 0/0/0 | E1 |
| W081 | `test -f apps/riwork/tests/Restore.test.tsx` | 1 | — | E1 |
| W082 | `test -f apps/riwork/tests/Theme.test.tsx` | 1 | — | E1 |
| W083 | `rg -n P1-THM-05 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | 1 | — | E1 |
| W084 | `rg -n P1-TXT-02 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | 1 | — | E1 |
| W085 | `cargo test -p riwork-core --lib sentence_case_keeps_proper_nouns_and_acronyms --locked --offline` | 0 | 0/0/0 | E1 |
| W086 | `test -f apps/riwork/tests/TextSize.test.tsx` | 1 | — | E1 |
| W087 | `test -f apps/riwork/tests/Idle.test.tsx` | 0 | — | E1 |
| W088 | `rg -n P1-POLL-03 apps/riwork -g '*.test.ts' -g '*.test.tsx'` | 1 | — | E1 |
| W089 | `rg -n 'setInterval\(&#124;requestAnimationFrame\(&#124;\.refetch\(' apps/riwork -g '!**/*.test.ts*' -g '!**/target/**' -g '!**/.nsh/**'` | 1 | — | E1 |
| W090 | `cargo test -p riwork-engine --lib the_global_window_restores_as_itself_and_older_snapshots_hold_project_windows --locked --offline` | 0 | 1/0/0 | E2 |
| W091 | `cargo test -p riwork-engine --lib stale_inventory_keeps_hide_and_rename --locked --offline` | 0 | 1/0/0 | E2 |
| W092 | `cargo test -p riwork-engine --lib one_refresh_transaction_ends_where_separate_transactions_did --locked --offline` | 0 | 1/0/0 | E2 |
| W093 | `cargo test -p riwork-engine --lib titles_come_from_a_rename_then_the_provider_then_the_first_message_then_the_kind --locked --offline` | 0 | 1/0/0 | E2 |
| W094 | `cargo test -p riwork-engine --lib a_title_is_set_once_and_only_a_higher_source_or_a_new_rename_replaces_it --locked --offline` | 0 | 1/0/0 | E2 |
| W095 | `cargo test -p riwork-engine --lib new_sessions_append_and_any_tab_hides --locked --offline` | 0 | 1/0/0 | E2 |
| W096 | `cargo test -p riwork-engine --lib identifiers_geometry_and_private_file_read_are_validated --locked --offline` | 0 | 1/0/0 | E2 |
| W097 | `sh apps/riwork/check-polling.sh` | 0 | — | E2 |
| W098 | `/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh --version` | 0 | — | E3 |
| W099 | `rustc --version` | 0 | — | E3 |
| W100 | `cargo --version` | 0 | — | E3 |
| W101 | `cargo run -p riwork-nsh-workspace --example seed_fixture --locked --offline -- /private/tmp/riwork-p1-m5-workspace/nsh-reh5ez2m/nsh-home` | 0 | — | E3 |
| W102 | `/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh check apps/riwork --tsc /Users/dominik/Library/Caches/nsh/typescript-7.0.2-darwin-arm64/package/lib/tsc` | 0 | — | E4 |
| W103 | `/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh test apps/riwork --junit /private/tmp/riwork-p1-m5-workspace/nsh-reh5ez2m/nsh.xml` | 0 | — | E4 |

Environment values are exact; `CARGO_HOME` and `RUSTUP_HOME` only read the installed toolchains. All writes from test/application state use disposable paths.

`E1`:

```json
{
  "HOME": "/Users/dominik/orca/projects/riWork-nsh-p1-m5/target/nsh-integration-home-v9i28oxr",
  "PATH": "/opt/homebrew/Cellar/openjdk@17/17.0.17/libexec/openjdk.jdk/Contents/Home/bin:/opt/homebrew/Caskroom/codex/0.162.1/codex-path:/Users/dominik/.okc/bin:/Users/dominik/.grok/bin:/Users/dominik/.bun/bin:/Users/dominik/.bun/bin:/Users/dominik/.opencode/bin:/opt/homebrew/opt/openjdk@17/bin:/Users/dominik/.bun/bin:/Users/dominik/.pyenv/shims:/Users/dominik/.avm/bin:/Users/dominik/.bun/bin:/Users/dominik/.local/share/solana/install/active_release/bin:/Users/dominik/.local/share/solana/install/active_release/bin:/opt/homebrew/Cellar/openjdk@17/17.0.17/libexec/openjdk.jdk/Contents/Home/bin:/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/System/Cryptexes/App/usr/bin:/usr/bin:/bin:/usr/sbin:/sbin:/var/run/com.apple.security.cryptexd/codex.system/bootstrap/usr/local/bin:/var/run/com.apple.security.cryptexd/codex.system/bootstrap/usr/bin:/var/run/com.apple.security.cryptexd/codex.system/bootstrap/usr/appleinternal/bin:/opt/pmk/env/global/bin:/Library/Apple/usr/bin:/usr/local/share/dotnet:~/.dotnet/tools:/opt/homebrew/Cellar/openjdk@17/17.0.17/libexec/openjdk.jdk/Contents/Home/bin:/Users/dominik/.codex/tmp/arg0/codex-arg03l3W4Z:/opt/homebrew/Caskroom/codex/0.162.1/codex-path:/Users/dominik/.local/share/riwork/cua/harness-bin:/Users/dominik/.okc/bin:/Users/dominik/Library/pnpm:/Users/dominik/.grok/bin:/Users/dominik/.bun/bin:/Users/dominik/.opencode/bin:/opt/homebrew/opt/openjdk@17/bin:/Users/dominik/.avm/bin:/Users/dominik/.local/share/solana/install/active_release/bin:/opt/homebrew/bin:/opt/homebrew/sbin:/Users/dominik/.starkli/bin:/Users/dominik/Library/Application Support/org.dfinity.dfx/bin:/Users/dominik/.cargo/bin:/Users/dominik/.local/bin:/Users/dominik/.docker/bin:/Users/dominik/.cache/lm-studio/bin:/Users/dominik/.nargo/bin:/opt/local/bin:/Users/dominik/.docker/bin:/Users/dominik/.cache/lm-studio/bin:/Users/dominik/.nargo/bin:/Users/dominik/.local/bin",
  "RIWORK_HOME": "/private/tmp/riwork-p1-m5-workspace/routes-6kmsa8vp/state",
  "RIWORK_RUNTIME_DIR": "/private/tmp/riwork-p1-m5-workspace/routes-6kmsa8vp/runtime",
  "TMPDIR": "/tmp",
  "TMUX_TMPDIR": "/private/tmp/riwork-p1-m5-workspace/routes-6kmsa8vp/tmux",
  "XDG_CONFIG_HOME": "/Users/dominik/orca/projects/riWork-nsh-p1-m5/target/nsh-integration-home-v9i28oxr/.config",
  "CARGO_HOME": "/Users/dominik/.cargo",
  "RUSTUP_HOME": "/Users/dominik/.rustup",
  "CARGO_TARGET_DIR": "/private/tmp/riwork-p1-integration-mkzwamt7/target",
  "CARGO_BUILD_JOBS": "4",
  "CARGO_NET_OFFLINE": "true",
  "NSH_CACHE": "/private/tmp/riwork-p1-m5-workspace/nsh-cache",
  "NSH_TSC": "/Users/dominik/Library/Caches/nsh/typescript-7.0.2-darwin-arm64/package/lib/tsc"
}
```

`E2`:

```json
{
  "HOME": "/Users/dominik/orca/projects/riWork-nsh-p1-m5/target/nsh-integration-home-ts8ci963",
  "PATH": "/opt/homebrew/Cellar/openjdk@17/17.0.17/libexec/openjdk.jdk/Contents/Home/bin:/opt/homebrew/Caskroom/codex/0.162.1/codex-path:/Users/dominik/.okc/bin:/Users/dominik/.grok/bin:/Users/dominik/.bun/bin:/Users/dominik/.bun/bin:/Users/dominik/.opencode/bin:/opt/homebrew/opt/openjdk@17/bin:/Users/dominik/.bun/bin:/Users/dominik/.pyenv/shims:/Users/dominik/.avm/bin:/Users/dominik/.bun/bin:/Users/dominik/.local/share/solana/install/active_release/bin:/Users/dominik/.local/share/solana/install/active_release/bin:/opt/homebrew/Cellar/openjdk@17/17.0.17/libexec/openjdk.jdk/Contents/Home/bin:/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/System/Cryptexes/App/usr/bin:/usr/bin:/bin:/usr/sbin:/sbin:/var/run/com.apple.security.cryptexd/codex.system/bootstrap/usr/local/bin:/var/run/com.apple.security.cryptexd/codex.system/bootstrap/usr/bin:/var/run/com.apple.security.cryptexd/codex.system/bootstrap/usr/appleinternal/bin:/opt/pmk/env/global/bin:/Library/Apple/usr/bin:/usr/local/share/dotnet:~/.dotnet/tools:/opt/homebrew/Cellar/openjdk@17/17.0.17/libexec/openjdk.jdk/Contents/Home/bin:/Users/dominik/.codex/tmp/arg0/codex-arg03l3W4Z:/opt/homebrew/Caskroom/codex/0.162.1/codex-path:/Users/dominik/.local/share/riwork/cua/harness-bin:/Users/dominik/.okc/bin:/Users/dominik/Library/pnpm:/Users/dominik/.grok/bin:/Users/dominik/.bun/bin:/Users/dominik/.opencode/bin:/opt/homebrew/opt/openjdk@17/bin:/Users/dominik/.avm/bin:/Users/dominik/.local/share/solana/install/active_release/bin:/opt/homebrew/bin:/opt/homebrew/sbin:/Users/dominik/.starkli/bin:/Users/dominik/Library/Application Support/org.dfinity.dfx/bin:/Users/dominik/.cargo/bin:/Users/dominik/.local/bin:/Users/dominik/.docker/bin:/Users/dominik/.cache/lm-studio/bin:/Users/dominik/.nargo/bin:/opt/local/bin:/Users/dominik/.docker/bin:/Users/dominik/.cache/lm-studio/bin:/Users/dominik/.nargo/bin:/Users/dominik/.local/bin",
  "RIWORK_HOME": "/private/tmp/riwork-p1-m5-workspace/supplement-ofs9mbu_/state",
  "RIWORK_RUNTIME_DIR": "/private/tmp/riwork-p1-m5-workspace/supplement-ofs9mbu_/runtime",
  "TMPDIR": "/tmp",
  "TMUX_TMPDIR": "/private/tmp/riwork-p1-m5-workspace/supplement-ofs9mbu_/tmux",
  "XDG_CONFIG_HOME": "/Users/dominik/orca/projects/riWork-nsh-p1-m5/target/nsh-integration-home-ts8ci963/.config",
  "CARGO_HOME": "/Users/dominik/.cargo",
  "RUSTUP_HOME": "/Users/dominik/.rustup",
  "CARGO_TARGET_DIR": "/private/tmp/riwork-p1-integration-mkzwamt7/target",
  "CARGO_BUILD_JOBS": "4",
  "CARGO_NET_OFFLINE": "true",
  "NSH_CACHE": "/private/tmp/riwork-p1-m5-workspace/nsh-cache",
  "NSH_TSC": "/Users/dominik/Library/Caches/nsh/typescript-7.0.2-darwin-arm64/package/lib/tsc"
}
```

`E3`:

```json
{
  "HOME": "/Users/dominik/orca/projects/riWork-nsh-p1-m5/target/nsh-integration-home-lnudlcme",
  "PATH": "/opt/homebrew/Cellar/openjdk@17/17.0.17/libexec/openjdk.jdk/Contents/Home/bin:/opt/homebrew/Caskroom/codex/0.162.1/codex-path:/Users/dominik/.okc/bin:/Users/dominik/.grok/bin:/Users/dominik/.bun/bin:/Users/dominik/.bun/bin:/Users/dominik/.opencode/bin:/opt/homebrew/opt/openjdk@17/bin:/Users/dominik/.bun/bin:/Users/dominik/.pyenv/shims:/Users/dominik/.avm/bin:/Users/dominik/.bun/bin:/Users/dominik/.local/share/solana/install/active_release/bin:/Users/dominik/.local/share/solana/install/active_release/bin:/opt/homebrew/Cellar/openjdk@17/17.0.17/libexec/openjdk.jdk/Contents/Home/bin:/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/System/Cryptexes/App/usr/bin:/usr/bin:/bin:/usr/sbin:/sbin:/var/run/com.apple.security.cryptexd/codex.system/bootstrap/usr/local/bin:/var/run/com.apple.security.cryptexd/codex.system/bootstrap/usr/bin:/var/run/com.apple.security.cryptexd/codex.system/bootstrap/usr/appleinternal/bin:/opt/pmk/env/global/bin:/Library/Apple/usr/bin:/usr/local/share/dotnet:~/.dotnet/tools:/opt/homebrew/Cellar/openjdk@17/17.0.17/libexec/openjdk.jdk/Contents/Home/bin:/Users/dominik/.codex/tmp/arg0/codex-arg03l3W4Z:/opt/homebrew/Caskroom/codex/0.162.1/codex-path:/Users/dominik/.local/share/riwork/cua/harness-bin:/Users/dominik/.okc/bin:/Users/dominik/Library/pnpm:/Users/dominik/.grok/bin:/Users/dominik/.bun/bin:/Users/dominik/.opencode/bin:/opt/homebrew/opt/openjdk@17/bin:/Users/dominik/.avm/bin:/Users/dominik/.local/share/solana/install/active_release/bin:/opt/homebrew/bin:/opt/homebrew/sbin:/Users/dominik/.starkli/bin:/Users/dominik/Library/Application Support/org.dfinity.dfx/bin:/Users/dominik/.cargo/bin:/Users/dominik/.local/bin:/Users/dominik/.docker/bin:/Users/dominik/.cache/lm-studio/bin:/Users/dominik/.nargo/bin:/opt/local/bin:/Users/dominik/.docker/bin:/Users/dominik/.cache/lm-studio/bin:/Users/dominik/.nargo/bin:/Users/dominik/.local/bin",
  "RIWORK_HOME": "/private/tmp/riwork-p1-m5-workspace/nsh-reh5ez2m/state",
  "RIWORK_RUNTIME_DIR": "/private/tmp/riwork-p1-m5-workspace/nsh-reh5ez2m/runtime",
  "TMPDIR": "/tmp",
  "TMUX_TMPDIR": "/private/tmp/riwork-p1-m5-workspace/nsh-reh5ez2m/tmux",
  "XDG_CONFIG_HOME": "/Users/dominik/orca/projects/riWork-nsh-p1-m5/target/nsh-integration-home-lnudlcme/.config",
  "CARGO_HOME": "/Users/dominik/.cargo",
  "RUSTUP_HOME": "/Users/dominik/.rustup",
  "CARGO_TARGET_DIR": "/private/tmp/riwork-p1-integration-mkzwamt7/target",
  "CARGO_BUILD_JOBS": "4",
  "CARGO_NET_OFFLINE": "true",
  "NSH_CACHE": "/private/tmp/riwork-p1-m5-workspace/nsh-cache",
  "NSH_TSC": "/Users/dominik/Library/Caches/nsh/typescript-7.0.2-darwin-arm64/package/lib/tsc"
}
```

`E4`:

```json
{
  "HOME": "/Users/dominik/orca/projects/riWork-nsh-p1-m5/target/nsh-integration-home-lnudlcme",
  "PATH": "/opt/homebrew/Cellar/openjdk@17/17.0.17/libexec/openjdk.jdk/Contents/Home/bin:/opt/homebrew/Caskroom/codex/0.162.1/codex-path:/Users/dominik/.okc/bin:/Users/dominik/.grok/bin:/Users/dominik/.bun/bin:/Users/dominik/.bun/bin:/Users/dominik/.opencode/bin:/opt/homebrew/opt/openjdk@17/bin:/Users/dominik/.bun/bin:/Users/dominik/.pyenv/shims:/Users/dominik/.avm/bin:/Users/dominik/.bun/bin:/Users/dominik/.local/share/solana/install/active_release/bin:/Users/dominik/.local/share/solana/install/active_release/bin:/opt/homebrew/Cellar/openjdk@17/17.0.17/libexec/openjdk.jdk/Contents/Home/bin:/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/System/Cryptexes/App/usr/bin:/usr/bin:/bin:/usr/sbin:/sbin:/var/run/com.apple.security.cryptexd/codex.system/bootstrap/usr/local/bin:/var/run/com.apple.security.cryptexd/codex.system/bootstrap/usr/bin:/var/run/com.apple.security.cryptexd/codex.system/bootstrap/usr/appleinternal/bin:/opt/pmk/env/global/bin:/Library/Apple/usr/bin:/usr/local/share/dotnet:~/.dotnet/tools:/opt/homebrew/Cellar/openjdk@17/17.0.17/libexec/openjdk.jdk/Contents/Home/bin:/Users/dominik/.codex/tmp/arg0/codex-arg03l3W4Z:/opt/homebrew/Caskroom/codex/0.162.1/codex-path:/Users/dominik/.local/share/riwork/cua/harness-bin:/Users/dominik/.okc/bin:/Users/dominik/Library/pnpm:/Users/dominik/.grok/bin:/Users/dominik/.bun/bin:/Users/dominik/.opencode/bin:/opt/homebrew/opt/openjdk@17/bin:/Users/dominik/.avm/bin:/Users/dominik/.local/share/solana/install/active_release/bin:/opt/homebrew/bin:/opt/homebrew/sbin:/Users/dominik/.starkli/bin:/Users/dominik/Library/Application Support/org.dfinity.dfx/bin:/Users/dominik/.cargo/bin:/Users/dominik/.local/bin:/Users/dominik/.docker/bin:/Users/dominik/.cache/lm-studio/bin:/Users/dominik/.nargo/bin:/opt/local/bin:/Users/dominik/.docker/bin:/Users/dominik/.cache/lm-studio/bin:/Users/dominik/.nargo/bin:/Users/dominik/.local/bin",
  "RIWORK_HOME": "/private/tmp/riwork-p1-m5-workspace/nsh-reh5ez2m/nsh-home",
  "RIWORK_RUNTIME_DIR": "/private/tmp/riwork-p1-m5-workspace/nsh-reh5ez2m/runtime",
  "TMPDIR": "/tmp",
  "TMUX_TMPDIR": "/private/tmp/riwork-p1-m5-workspace/nsh-reh5ez2m/nsh-tmux",
  "XDG_CONFIG_HOME": "/Users/dominik/orca/projects/riWork-nsh-p1-m5/target/nsh-integration-home-lnudlcme/.config",
  "CARGO_HOME": "/Users/dominik/.cargo",
  "RUSTUP_HOME": "/Users/dominik/.rustup",
  "CARGO_TARGET_DIR": "/private/tmp/riwork-p1-integration-mkzwamt7/target",
  "CARGO_BUILD_JOBS": "4",
  "CARGO_NET_OFFLINE": "true",
  "NSH_CACHE": "/private/tmp/riwork-p1-m5-workspace/nsh-cache",
  "NSH_TSC": "/Users/dominik/Library/Caches/nsh/typescript-7.0.2-darwin-arm64/package/lib/tsc"
}
```
