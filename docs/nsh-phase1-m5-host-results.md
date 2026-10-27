# Phase 1 M5 host acceptance execution

`verification_environment = macOS arm64; isolated fixture HOME/RIWORK_HOME/runtime/config/tmux; pure host/core tests and real main-thread AppKit objects under activation policy Prohibited; no visible windows, Cua calls, live state or desktop input by this agent`

This is the host/core/AppKit portion of M5. Final 101-item acceptance and fixture/Cua/CLI-diff outcomes are in `docs/nsh-phase1-m5-results.md`; passing a host route alone does not pass an item requiring workspace/native/fixture evidence. No task status, commit, merge, push or install was performed by this worker.

## Reproduction and evidence

```sh
python3 crates/riwork-desktop/verification/phase1_m5_host.py --root /tmp/riwork-p1-m5-host --target-dir /tmp/riwork-p1-integration-mkzwamt7/target
```

The runner records exact argv, cwd, isolated environment, exit status, stdout/stderr and SHA-256. It rejects a zero-test Cargo success as missing proof. Missing AppKit named checks deliberately exit 101 instead of accepting zero checks. It inherits the integration runner’s isolated environment; CARGO_HOME/RUSTUP_HOME only locate the installed offline toolchain.

Final named route run: **36 passed, 19 failed/missing proof** across **55 route checks**. These are route counts, not full acceptance item counts. Core `global_window::tests::` runs three tests. The commands and each exact test count follow below; machine-readable equivalents are in `nsh-phase1-m5-host-results.json`. Archived logs and manifests are in `crates/riwork-desktop/verification/m5-host-evidence/`.

## Fixes and regression evidence

- Deferred View actions were enabled and rendering ignored the model state. Initial real AppKit run: 6 passed, 1 failed. The renderer now applies disabled state, disables AppKit automatic re-enabling, and guards direct deferred dispatch while preserving rows and shortcuts.
- Native chrome now uses window-relative geometry and labels Close/Minimize/Zoom; AppKit layout resets required an idempotent windowDidUpdate correction. Root owns the final visible fixture/Cua reading. The unshown AppKit test verifies native (10,8); AppKit expands the accessibility bounds by one point.
- Busy runtime registry skips no longer advance the two-second publication deadline; the fake-clock successor proves the next worker turn stays due.
- New startup tests exposed a lock lifetime failure under concurrent subprocess creation. File close alone left inherited file descriptions holding the handoff gate. LockedFile explicitly unlocks on every Drop path. The deterministic duplicate-descriptor test and existing replacement/abandonment tests pass.
- Environment scrub is tested through the exact production function in a separate single-thread process; no parallel test mutates process-wide environment.

Before-menu-fix and before-lock-fix failures are retained. The full host suite after the lock guard passed 26 host tests, 7 AppKit checks, 5 pre-existing CLI contract tests and 1 startup check. Final frozen-command/direct native-update correction rerun: `cargo test -p riwork-desktop --locked --offline` exited 0 with **40 passed, 0 failed** (27 host, 7 AppKit, 5 CLI contract, 1 startup). Its exact environment and logs are archived under `m5-host-evidence/final-all/`. Root records the later all-crate validation and historical CLI-byte fix separately.

## Item findings

| Item | Host result | Finding |
|---|---|---|
| P1-PROC-01 | PASS | CLI startup parser passes. Review of nsh_main.rs proves run_cli precedes any host/AppKit/NSH initialization. Root owns five-command historical cli-diff. |
| P1-PROC-06 | PASS | Existing runtime registry test passes; root owns the live fixture two-window count. |
| P1-RLD-02 | FAIL | Four engine choreography tests plus host readiness/idempotence/order checks pass. Required 20-second host confirmation test is absent; production restart is explicitly unavailable and no draft-flush hook is integrated. |
| P1-RLD-03 | PASS | Existing nonblocking/change-only engine publishing test and fake-clock host due/retry successor pass. Busy registry skips now leave publication due; successful publication advances by two seconds. |
| P1-RLD-04 | PASS | Host successors read current Rust LayoutSession on runtime publication and preserve display-local top-left geometry; engine validation test passes. Workspace owns restore payload precedence/readiness tests. |
| P1-WIN-02 | FAIL | Core opening/cascade test passes. Named host non-tooltip cascade test is absent, and ordinary opening_frame still selects fixed 1220x780 rather than settings-gated remembered size. Root verified only ordinary cascade. |
| P1-WIN-04 | FAIL | Existing core legacy/invalid-size test passes; debounce/window-size bridge and settings wiring belong to workspace/native and remain absent. |
| P1-GLB-02 | FAIL | All three core global-window persistence/size tests pass. Host still uses generic 1220x780 opening size rather than unconditional global 1100x760/remembered size; no restored global fixture. |
| P1-GLB-04 | FAIL | Existing engine global-kind restore test passes; host runtime-global test is absent and restart unavailable. |
| P1-PROC-02 | PASS | Exact production scrub function passes in a dedicated single-thread process, avoiding mutation of the parallel Rust test environment. |
| P1-PROC-04 | FAIL | All three host successors pass (canonical project reuse, vanished root error, cwd fallback). Root fixture proves missing-directory invocation is rejected by CLI, so the required fallback notice is not accepted; HOME/dot fallback is absent. |
| P1-PROC-05 | PASS | All host ownership/forward/stale-lock/replacement successors pass. Explicit unlock-on-drop guard releases duplicated/inherited file descriptions; root owns real forward/instances proof. |
| P1-WIN-03 | FAIL | Both named zoom/fullscreen host tests are absent; real restored fixture is blocked by unimplemented restart. |
| P1-WIN-07 | FAIL | Named last-close/tooltip host checks and no-NSWindow tooltip AppKit check are absent. No substitute inferred pass is claimed. |
| P1-WIN-08 | FAIL | Both real Rust close/quit flush successors pass (last accepted edit, every project and global state). Required RLD-01 end-to-end reload remains unavailable. |
| P1-WIN-09 | FAIL | Named state-change-only host check is absent. Source has structural state comparison, but review does not replace the host-unit route. |
| P1-GLB-01 | FAIL | Named singleton/no-window host test is absent; no substitute pass from source reading. |
| P1-GLB-08 | FAIL | Named Dock New Window host test is absent; project routing/fallback UI remains unverified. |
| P1-GLB-09 | FAIL | Named frontmost/minimized/open-by-id host route test is absent. |
| P1-DRAG-03 | FAIL | Reserved transfer/receive handler grep has no hits (exit1), but named refusal host-unit filter runs zero tests. Review is retained, not substituted for the route. |
| P1-MENU-01 | PASS | Real NSMenu application tree, labels, separators, shortcut modifiers and tags pass; frozen v1 command-ID set/uniqueness successor passes. |
| P1-MENU-03 | FAIL | Real View menu passes after fixing deferred rows to remain present and disabled through NSMenu.update; direct deferred dispatch is guarded. Named key-window/no-window host routing tests are absent, and no-window text-size behavior is unimplemented. |
| P1-MENU-04 | PASS | Pure Dock uniqueness/order/last-key test plus actual fake-registry NSMenu/caching/actions pass. Workspace owns change-only setDockInfo posts. |
| P1-KEYS-05 | FAIL | Required routing host-unit and no-window text-size dispatch AppKit checks are absent; text-size handlers cannot apply settings without a window. |
| P1-THM-05 | FAIL | Forced-appearance host test is absent and source opens AppView with Appearance::System. Other owners cover Settings/Appearance bridge and TSX/manual routes. |
| P1-WIN-01 | PASS | Real prohibited-activation AppKit objects pass full-size-content/transparent/minimum/chrome checks. Root owns final read-only Cua geometry proof. Standard controls now carry explicit AX labels; chrome coordinates are corrected after native titlebar updates. |
| P1-WIN-06 | FAIL | AppKit titlebar double-click preference check is absent (the harness refuses zero matching checks). Theme/chrome TSX/manual routes are owned by workspace/root. |
| P1-MENU-02 | PASS | Actual NSMenu edit labels and standard cut:/copy:/paste:/selectAll: selectors pass. Workspace owns no-selection clipboard command behavior. |
| P1-KEYS-03 | PASS | Actual native cmd-alt-G/cmd-shift-O/cmd-Q menu equivalents pass. Workspace owns full TSX chord table and routing. |
| P1-TXT-02 | PASS | Actual View menu Bigger/Smaller/Actual Size shortcut text/modifiers pass. Native/TSX Settings effects are owned by other agents. |
| P1-PROC-07 | PASS | One process set_ui_sets before opening windows; one service worker. Per-topic native watcher/hub proof is separate. |
| P1-RLD-01 | FAIL | RESTART_PENDING explicitly rejects app.restart and fails pending CLI reload requests while retaining the old process. |
| P1-GLB-03 | FAIL | Three global-window core tests pass, but no Dashboard native state module is registered and required view/filter/sort UI persistence route is absent. |

## Exact named commands

| Item / route | Exact command | Exit / executed tests | Result |
|---|---|---|---|
| P1-PROC-01 / core | `cargo test -p riwork-cli --lib only_gui_startup_forms_open_the_workspace --locked --offline` | 0 / 1 passed, 0 failed | PASS |
| P1-PROC-06 / core | `cargo test -p riwork-engine --lib live_gui_pids_lists_running_guis_and_changes_nothing --locked --offline` | 0 / 1 passed, 0 failed | PASS |
| P1-RLD-02 / core | `cargo test -p riwork-engine --lib queuing_and_wait_timeout_leave_old_gui_alive_and_do_not_launch_anything --locked --offline` | 0 / 1 passed, 0 failed | PASS |
| P1-RLD-02 / core | `cargo test -p riwork-engine --lib readiness_requires_correct_child_and_matching_registered_windows --locked --offline` | 0 / 1 passed, 0 failed | PASS |
| P1-RLD-02 / core | `cargo test -p riwork-engine --lib failed_or_hung_replacement_keeps_old_alive_and_allows_retry --locked --offline` | 0 / 1 passed, 0 failed | PASS |
| P1-RLD-02 / core | `cargo test -p riwork-engine --lib a_watchdog_that_fired_refuses_to_let_the_restore_confirm --locked --offline` | 0 / 1 passed, 0 failed | PASS |
| P1-RLD-03 / core | `cargo test -p riwork-engine --lib publishing_writes_only_changes_and_never_waits_for_the_registry_lock --locked --offline` | 0 / 1 passed, 0 failed | PASS |
| P1-RLD-04 / core | `cargo test -p riwork-engine --lib identifiers_geometry_and_private_file_read_are_validated --locked --offline` | 0 / 1 passed, 0 failed | PASS |
| P1-WIN-02 / core | `cargo test -p riwork-core --lib opening_windows_cascade_inside_the_display_and_clamp_after_cascading --locked --offline` | 0 / 1 passed, 0 failed | PASS |
| P1-WIN-04 / core | `cargo test -p riwork-core --lib legacy_and_invalid_window_sizes_do_not_break_layout_restoration --locked --offline` | 0 / 1 passed, 0 failed | PASS |
| P1-GLB-02 / core | `cargo test -p riwork-core --lib global_window::tests:: --locked --offline` | 0 / 3 passed, 0 failed | PASS |
| P1-GLB-04 / core | `cargo test -p riwork-engine --lib the_global_window_restores_as_itself_and_older_snapshots_hold_project_windows --locked --offline` | 0 / 1 passed, 0 failed | PASS |
| P1-PROC-02 / host-unit | `cargo test -p riwork-desktop --test startup_environment --locked --offline` | 0 / 1 passed, 0 failed | PASS |
| P1-PROC-04 / host-unit | `cargo test -p riwork-desktop --bin riwork nsh_host::tests::folder_arg_reuses_existing_project --locked --offline` | 0 / 1 passed, 0 failed | PASS |
| P1-PROC-04 / host-unit | `cargo test -p riwork-desktop --bin riwork nsh_host::tests::no_active_project_registers_the_cwd_fallback --locked --offline` | 0 / 1 passed, 0 failed | PASS |
| P1-PROC-04 / host-unit | `cargo test -p riwork-desktop --bin riwork nsh_host::tests::a_vanished_project_root_is_an_error_for_the_caller_not_a_panic --locked --offline` | 0 / 1 passed, 0 failed | PASS |
| P1-PROC-05 / host-unit | `cargo test -p riwork-desktop --bin riwork nsh_runtime::tests::forwards_path_and_reopen_without_creating_a_second_primary --locked --offline` | 0 / 1 passed, 0 failed | PASS |
| P1-PROC-05 / host-unit | `cargo test -p riwork-desktop --bin riwork nsh_runtime::tests::reclaims_stale_socket_but_never_deletes_a_regular_file --locked --offline` | 0 / 1 passed, 0 failed | PASS |
| P1-PROC-05 / host-unit | `cargo test -p riwork-desktop --bin riwork nsh_runtime::tests::replacement_keeps_old_socket_and_prevents_handoff_race --locked --offline` | 0 / 1 passed, 0 failed | PASS |
| P1-RLD-02 / host-unit | `cargo test -p riwork-desktop --bin riwork nsh_runtime::tests::restore_acknowledgements_are_per_window_and_idempotent --locked --offline` | 0 / 1 passed, 0 failed | PASS |
| P1-RLD-02 / host-unit | `cargo test -p riwork-desktop --bin riwork nsh_runtime::tests::restore_matches_targets_even_if_window_order_changes --locked --offline` | 0 / 1 passed, 0 failed | PASS |
| P1-RLD-02 / host-unit | `cargo test -p riwork-desktop --bin riwork restore::tests::confirmation_gives_up_after_20_s --locked --offline` | 0 / 0 passed, 0 failed | FAIL — missing named check |
| P1-RLD-03 / host-unit | `cargo test -p riwork-desktop --bin riwork nsh_host::tests::a_busy_registry_lock_stays_due --locked --offline` | 0 / 1 passed, 0 failed | PASS |
| P1-RLD-04 / host-unit | `cargo test -p riwork-desktop --bin riwork nsh_host::tests::runtime_publication_reads_current_layout_without_an_appkit_state_event --locked --offline` | 0 / 1 passed, 0 failed | PASS |
| P1-RLD-04 / host-unit | `cargo test -p riwork-desktop --bin riwork nsh_host::tests::runtime_frames_keep_the_existing_display_local_top_left_coordinates --locked --offline` | 0 / 1 passed, 0 failed | PASS |
| P1-WIN-02 / host-unit | `cargo test -p riwork-desktop --bin riwork placement::tests::the_cascade_counts_open_non_tooltip_windows --locked --offline` | 0 / 0 passed, 0 failed | FAIL — missing named check |
| P1-WIN-03 / host-unit | `cargo test -p riwork-desktop --bin riwork placement::tests::zoomed_counts_as_maximized_and_keeps_its_normal_frame --locked --offline` | 0 / 0 passed, 0 failed | FAIL — missing named check |
| P1-WIN-03 / host-unit | `cargo test -p riwork-desktop --bin riwork placement::tests::fullscreen_counts_as_fullscreen --locked --offline` | 0 / 0 passed, 0 failed | FAIL — missing named check |
| P1-WIN-07 / host-unit | `cargo test -p riwork-desktop --bin riwork window_registry::tests::the_last_close_quits --locked --offline` | 0 / 0 passed, 0 failed | FAIL — missing named check |
| P1-WIN-07 / host-unit | `cargo test -p riwork-desktop --bin riwork window_registry::tests::tooltips_do_not_count --locked --offline` | 0 / 0 passed, 0 failed | FAIL — missing named check |
| P1-WIN-08 / host-unit | `cargo test -p riwork-desktop --bin riwork nsh_host::tests::quit_flushes_every_layout_session_and_the_global_state_from_rust --locked --offline` | 0 / 1 passed, 0 failed | PASS |
| P1-WIN-08 / host-unit | `cargo test -p riwork-desktop --bin riwork nsh_host::tests::close_flushes_the_last_accepted_edit_before_the_window_goes --locked --offline` | 0 / 1 passed, 0 failed | PASS |
| P1-WIN-09 / host-unit | `cargo test -p riwork-desktop --bin riwork window_state::tests::events_only_on_change --locked --offline` | 0 / 0 passed, 0 failed | FAIL — missing named check |
| P1-GLB-01 / host-unit | `cargo test -p riwork-desktop --bin riwork window_registry::tests::the_global_window_is_a_singleton_and_opens_with_no_window --locked --offline` | 0 / 0 passed, 0 failed | FAIL — missing named check |
| P1-GLB-04 / host-unit | `cargo test -p riwork-desktop --bin riwork runtime_window::tests::the_global_window_is_recorded_as_global --locked --offline` | 0 / 0 passed, 0 failed | FAIL — missing named check |
| P1-GLB-08 / host-unit | `cargo test -p riwork-desktop --bin riwork menu::tests::dock_new_window --locked --offline` | 0 / 0 passed, 0 failed | FAIL — missing named check |
| P1-GLB-09 / host-unit | `cargo test -p riwork-desktop --bin riwork window_registry::tests::route_prefers_frontmost_then_minimised_then_opens --locked --offline` | 0 / 0 passed, 0 failed | FAIL — missing named check |
| P1-DRAG-03 / host-unit | `cargo test -p riwork-desktop --bin riwork host_names::tests::tab_transfer_is_refused --locked --offline` | 0 / 0 passed, 0 failed | FAIL — missing named check |
| P1-MENU-01 / host-unit | `cargo test -p riwork-desktop --bin riwork nsh_menus::tests::command_ids_are_the_v1_set --locked --offline` | 0 / 1 passed, 0 failed | PASS |
| P1-MENU-03 / host-unit | `cargo test -p riwork-desktop --bin riwork menu::tests::window_commands_go_to_the_key_window --locked --offline` | 0 / 0 passed, 0 failed | FAIL — missing named check |
| P1-MENU-03 / host-unit | `cargo test -p riwork-desktop --bin riwork menu::tests::app_commands_work_with_no_window --locked --offline` | 0 / 0 passed, 0 failed | FAIL — missing named check |
| P1-MENU-03 / host-unit | `cargo test -p riwork-desktop --bin riwork nsh_menus::tests::deferred_menu_commands_remain_present_disabled_and_keep_their_chords --locked --offline` | 0 / 1 passed, 0 failed | PASS |
| P1-MENU-04 / host-unit | `cargo test -p riwork-desktop --bin riwork nsh_menus::tests::dock_labels_are_stable_unique_and_keep_the_last_key_window_marked --locked --offline` | 0 / 1 passed, 0 failed | PASS |
| P1-KEYS-05 / host-unit | `cargo test -p riwork-desktop --bin riwork menu::tests::routing --locked --offline` | 0 / 0 passed, 0 failed | FAIL — missing named check |
| P1-THM-05 / host-unit | `cargo test -p riwork-desktop --bin riwork startup::tests::forced_appearance_from_env --locked --offline` | 0 / 0 passed, 0 failed | FAIL — missing named check |
| P1-WIN-01 / appkit | `cargo test -p riwork-desktop --test appkit_shell --locked --offline -- window_chrome --test-threads=1` | 0 / 1 passed, 0 failed | PASS |
| P1-WIN-06 / appkit | `cargo test -p riwork-desktop --test appkit_shell --locked --offline -- titlebar_double_click_reads_the_system_preference --test-threads=1` | 101 / 0 passed, 0 failed | FAIL — missing named check |
| P1-WIN-07 / appkit | `cargo test -p riwork-desktop --test appkit_shell --locked --offline -- tooltips_create_no_nswindow --test-threads=1` | 101 / 0 passed, 0 failed | FAIL — missing named check |
| P1-MENU-01 / appkit | `cargo test -p riwork-desktop --test appkit_shell --locked --offline -- app_menu_matches_gpui --test-threads=1` | 0 / 1 passed, 0 failed | PASS |
| P1-MENU-02 / appkit | `cargo test -p riwork-desktop --test appkit_shell --locked --offline -- edit_menu_uses_standard_selectors --test-threads=1` | 0 / 1 passed, 0 failed | PASS |
| P1-MENU-03 / appkit | `cargo test -p riwork-desktop --test appkit_shell --locked --offline -- view_menu_matches_gpui --test-threads=1` | 0 / 1 passed, 0 failed | PASS |
| P1-MENU-04 / appkit | `cargo test -p riwork-desktop --test appkit_shell --locked --offline -- dock_menu_from_a_fake_registry --test-threads=1` | 0 / 1 passed, 0 failed | PASS |
| P1-KEYS-03 / appkit | `cargo test -p riwork-desktop --test appkit_shell --locked --offline -- app_level_key_equivalents --test-threads=1` | 0 / 1 passed, 0 failed | PASS |
| P1-KEYS-05 / appkit | `cargo test -p riwork-desktop --test appkit_shell --locked --offline -- text_size_key_equivalents_dispatch_without_a_window --test-threads=1` | 101 / 0 passed, 0 failed | FAIL — missing named check |
| P1-TXT-02 / appkit | `cargo test -p riwork-desktop --test appkit_shell --locked --offline -- view_menu_text_size_items --test-threads=1` | 0 / 1 passed, 0 failed | PASS |

## Review commands and deletion embargo

| Command | Exit / finding |
|---|---|
| `sed -n 1,70p crates/riwork-desktop/src/nsh_main.rs` | 0 — CLI dispatch precedes host startup; environment scrub is GUI-only. |
| `rg -n 'set_ui_sets|AppView::new|RESTART_PENDING|app.restart|runtime_tick|system.fonts|Appearance::System' crates/riwork-desktop/src/nsh_host.rs` | 0 — Single UI-set registration, explicit unavailable restart, system-font and appearance gaps are visible. |
| `rg -n 'tab.transfer|tab.receive' crates/riwork-desktop/src/nsh_host.rs` | 1 — No tab.transfer/tab.receive handler registered; named host proof remains missing. |
| `git diff 17e89d9 -- crates/riwork-desktop/src/main.rs crates/riwork-desktop/src/dock_menu.rs` | 0 — Protected legacy main.rs/dock_menu.rs unchanged from 17e89d9. Root audits the full 142-file carry-over set. |

The acceptance §4.3 deletion embargo remains in force. All modifications here are NSH host files or new harness/report files; no GPUI source was deleted or edited. D-PERF-1 remains report-only; no wall-clock performance assertion was added.
