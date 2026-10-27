# M5 native/core acceptance evidence

`verification_environment = macOS arm64, isolated fixture RIWORK_HOME and HOME, isolated runtime/config/tmux, real headless QuickJS engines with fake Sessions; no desktop/Cua calls and no live state.`

Task `f6975181-1a9a-41ea-a500-87e47abc1ad6`, branch `nsh-p1-m5`. This is the native/core portion of the consolidated [M5 results](nsh-phase1-m5-results.md). Raw evidence is `/private/tmp/riwork-p1-m5-f6975181/native`. The [machine-readable record](nsh-phase1-m5-native-results.json) contains every exact argv, cwd, isolated environment, exit code, log path, SHA-256 and test-result count.

The full native suite passed **11 tests, 0 failed** after the fix. The three named text-size core tests each passed. These suite totals do not imply acceptance of missing features: zero-test filters and missing test targets are failures, never passes. For these fifteen acceptance items: **2 PASS, 13 FAIL**. THM-06 passes the fixture audit; POLL-02 passes the combined bridge and final TSX routes.

## Fixed failure and verification

The service sampled the empty session inventory every two seconds. The new regression failed before the fix with `left: 2; right: 1` samples after 2.15 seconds. The service now waits for the single home watcher while empty, arms its two-second liveness fallback after a shell appears, and disarms it when the last shell disappears. A temporary read failure retains the last successful inventory state.

The post-fix regression proves watcher discovery, enabled liveness sampling and last-shell removal. `a_quiet_home_produces_no_deliveries_for_60_s` runs for 60 actual seconds, proves no Store/Sessions delivery and exactly one initial session sample, and completes without a redraw timer. This is an explicit real-time successor to the planned ManualClock test; no ManualClock implementation is claimed. The existing two-engine bridge test still passes. The fake fixture event writes are test setup, not P1 application writes.

Exact before/after commands (with the recorded isolated environments):

```sh
cargo test -p riwork-nsh-native --test services --locked --offline -- an_empty_inventory_sleeps_until_a_watcher_event_and_disarms_when_empty_again --exact
cargo test -p riwork-nsh-native --locked --offline
cargo fmt -p riwork-nsh-native --check
```

## Item results

The command names in this table identify exact invocations in the command catalog below and JSON record. Cross-owner routes are explicitly retained; native review does not replace an NSH or fixture route.

| Item | Result | Route | Command records | Evidence / remaining failure |
|---|---|---|---|---|
| P1-THM-01 | FAIL | core + review | `hermes_settings_round_trip_preserves_every_existing_theme_name`, `themes-label-review` | The named moved test runs zero tests. Core ThemeChoice already has seven snake_case choices, Ghostty default and labels, but Settings is absent from the native registry and no UI theme selection is wired. |
| P1-THM-02 | FAIL | core + bridge | `only_native_goes_stale_when_macos_switches_light_and_dark`, `published_terminal_follows_what_the_desktop_shows_while_following_ghostty`, `every_preset_token_is_exact` | Moved palette tests are not in riwork-core (zero tests); Appearance module, appearance integration test target and test-support feature are absent. Preserved legacy GPUI theme source has no compiled successor. |
| P1-THM-03 | FAIL | core | `terminal_override_is_what_terminals_are_forced_to`, `native_terminals_match_the_theme_only_while_the_option_is_on`, `selected_theme_controls_terminals_and_keeps_legacy_preferences` | All three named core successors run zero tests; Settings terminal_colors_forced exists but the required Appearance override field is absent. |
| P1-THM-04 | FAIL | nsh-test (workspace) + native review | `native-surface`, `themes-label-review`, `app-options` | Native review finds no Appearance module or startup resolved palette. Workspace owner supplies the Theme.test.tsx route result; review is not substituted for that route. |
| P1-THM-05 | FAIL | bridge + host-unit/nsh-test/manual (other owners) | `a_write_by_another_process_reaches_every_subscriber_once`, `app-options` | The bridge named route exits 101 because test-support is absent; Settings/Appearance subscriptions are unimplemented. Host uses Appearance::System and rejects app.setAppearance. Other owners supply host/TSX/fixture routes. |
| P1-THM-06 | PASS | review + fixture+cua | `appearance-writer-review`; root fixture manifest `fixture-5gf4wtii/manifest.json` | Review passed (no reachable appearance writer). Root fixture PID 25211 exited -15 after read-only inspection; pre-existing appearance.json hash e28fad78729878a34119070e89efe095dc8e270ab1344d5963723da4a6cc652f and mtime_ns 1793137071953824496 stayed identical. Only nsh/desktop.handoff.lock and run/desktop.lock changed; no removals, disallowed writes, sessions/chats or tmux entries. |
| P1-THM-07 | FAIL | bridge | `follow_ghostty_without_a_parser_uses_the_riwork_palette_and_keeps_the_error` | Named bridge route exits 101; no Appearance module/fallback diagnostic. D-THEME-1 remains open in the acceptance decision record; no decision is invented here. |
| P1-TXT-01 | FAIL | core + bridge | `saved_sizes_round_to_tenths_clamp_and_write_plain_numbers`, `a_step_starts_from_the_stored_size_not_the_cached_one`, `ui_text_size_defaults_to_today_clamps_and_keeps_odd_values_until_changed`, `change_text_size_is_a_read_modify_write_under_the_lock` | Three named core tests each pass (3 total); native Settings is absent, so two-handle Settings race route exits 101. This is a failed whole acceptance item despite passing core coverage. |
| P1-POLL-01 | FAIL | bridge | `the_first_delivery_is_a_snapshot`, `rev_is_monotonic_per_topic_per_process`, `overflow_clears_the_queue_and_sends_one_resync_snapshot`, `one_delivery_per_topic_per_engine_turn`, `streaming_topics_coalesce_to_16_ms_trailing`, `closing_or_dropping_the_owner_ends_the_task`, `a_project_switch_resubscribes_and_drops_late_deliveries`, `one_watcher_and_one_sampler_per_home_per_process`, `native-after-fix` | Every declared test-support route exits 101. Existing native tests prove snapshot/dedup/overflow/close and two real QuickJS engines sharing Store/Sessions; missing Tabs/Settings/Appearance hubs and project-switch/streaming coverage prevent acceptance. Streaming topics remain deferred, without fabricated coverage. |
| P1-POLL-02 | PASS | bridge + nsh-test | `a_quiet_home_produces_no_deliveries_for_60_s`, `empty-inventory-before-fix`, `native-after-fix`, `workspace-final-nsh-test` | Fixed empty-inventory 2s polling. Regression failed before fix (2 reads vs 1); after fix 11 native tests pass, including 60 actual quiet seconds with zero Store/Sessions deliveries and only initial sample, plus watcher-triggered arm/disarm. Explicit bridge successor uses real time instead of missing ManualClock. Final workspace Idle.test.tsx passes its 60,000ms virtual-clock unchanged-host-calls check within a clean NSH suite (nsh-reh5ez2m). |
| P1-NAT-01 | FAIL | nsh-test/check + review | `nat01-declared-probe`, `core-engine-format-diff`, `native-surface` | Named probe path does not exist (exit 1). The registry and camelCase DTO generation work for Store/Sessions/Layouts only; Tabs/Settings/Appearance/Dashboard are absent. Core/engine diff from ea0f2f5 is empty, so persistence serde formats remain unchanged. |
| P1-NAT-02 | FAIL | review + bridge | `native-surface`, `native-backends`, `layout-blocking-review`, `errors_use_only_the_frozen_codes`, `native-after-fix` | Store/Sessions async work uses spawn_blocking and existing error tests pass. Sync LayoutSession edits take HostLayout.closing/project mutexes shared with the host, violating the frozen no-cross-thread-lock sync rule; <1ms is unmeasured. Named exhaustive frozen-error test route exits 101. |
| P1-NAT-03 | FAIL | review | `native-backends`, `app-options`, `no-native-storage` | Store/Sessions have production/fake backend traits; host data_dir is home/nsh; storage grep has no hits. Layouts directly uses concrete LayoutPersistence (no backend trait/fake), and the other mandatory P1 modules are absent. Whole item fails. |
| P1-NAT-04 | FAIL | nsh-test | `nat04-declared-probe`, `native-after-fix` | Declared TSX probe does not exist (exit 1); Store.createFolder is not exported. Supplemental Rust test native_pushes_one_service_to_two_engines_and_stops_on_close_owner_and_engine_drop passes, but its folder write is direct core Store, so it does not prove the requested UI write through the bridge. |
| P1-NAT-05 | FAIL | bridge + review | `tabs_update_rejects_invalid_keys`, `store_rejects_non_uuid_ids`, `native-ids-review` | Both named routes exit 101; Tabs.update and Store ID mutation methods do not exist, so the required valid_key/UUID rejection surface is absent. Existing inspectProject only rejects an empty root and is not an ID validator. |

## Frozen call-model review

Every exported module/class call in the integrated registry was read using `native-surface`, `native-backends` and `layout-blocking-review` below. No sub-millisecond benchmark is claimed.

| Export | Kind | Domain work and thread boundary |
|---|---|---|
| `Store.snapshot`, `Store.search`, `Store.inspectProject` | async | Initialization and core Store IO run through the native blocking helper / `spawn_blocking`. |
| `Store.subscribe`, `Sessions.subscribe` | sync | Read initialized process Arc and start owner-bound tasks; hub registration and its mutex run on the blocking pool. |
| `Sessions.list` | async | SessionManager read/socket work is on the blocking pool. |
| `Layouts.openSession` | async | Parses input, then performs home resolution, load, worker registration and initial save scheduling inside `spawn_blocking`. |
| `LayoutSession.dock`, `applyDock`, `setExtent`, `selectPanel`, `selectPane`, `toggleLock`, `toggleMain`, `gather` | sync | Call `HostLayout.edit`, which takes the cross-thread `closing` mutex, and may publish through the `project` mutex. This violates the frozen sync rule even though filesystem saves are queued. |
| `LayoutSession.snapshot`, `LayoutSession.panel` | sync | Traverse the owned in-memory model; no domain IO. Size-dependent cost is not measured. |
| `LayoutSession.flush` | async | Clones the host handle, then waits for disk flush inside `spawn_blocking`. |
| `Subscription.close` | sync | Cancels the owner task and releases cleanup. No domain IO. |

Store/Sessions error paths use only frozen `invalid`, `unavailable`, `conflict`, `broken`, `io` prefixes. Existing tests exercise async-thread separation and error coding, but the planned exhaustive seven-code bridge test is absent. Missing Settings, Appearance, Tabs and Dashboard exports have no call-model implementation to accept.

## Carry-over and scope

No protected GPUI file was deleted or modified by this work. `git diff ea0f2f5 -- crates/riwork-core crates/riwork-engine` is empty. The legacy theme tests remain preserved in the uncompiled GPUI source; their zero-test core filters are explicitly not accepted as successors. The §4.3 deletion embargo remains in force. There are no new shell/session/chat creation APIs or appearance publishing. No merge, push, install or task-status change was performed.

## Exact command catalog

All commands used cwd `/Users/dominik/orca/projects/riWork-nsh-p1-m5`. Each run gets a different disposable HOME and RIWORK_HOME from the existing integration runner; the full environment is recorded in JSON. Shared build artifacts are `/private/tmp/riwork-p1-integration-mkzwamt7/target`, tool caches are the existing pinned toolchain, and native NSH cache is `/private/tmp/riwork-p1-m5-f6975181/native/nsh-cache`.

### after-fix

| Command record | Exact command | Exit | Test result |
|---|---|---:|---|
| `native-after-fix` | `cargo test -p riwork-nsh-native --locked --offline` | 0 | test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s; test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.65s; test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 60.05s; test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s |

### before-fix

| Command record | Exact command | Exit | Test result |
|---|---|---:|---|
| `empty-inventory-before-fix` | `cargo test -p riwork-nsh-native --test services --locked --offline -- an_empty_inventory_sleeps_until_a_watcher_event_and_disarms_when_empty_again --exact` | 101 | test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 4 filtered out; finished in 2.19s |

### named-routes

| Command record | Exact command | Exit | Test result |
|---|---|---:|---|
| `nsh-version` | `/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh --version` | 0 | No Rust tests |
| `rustc-version` | `rustc --version` | 0 | No Rust tests |
| `cargo-version` | `cargo --version` | 0 | No Rust tests |
| `declared-fixture-generator` | `cargo run -p riwork-nsh-native --features test-support --example p1_fixture_home --locked --offline -- --scenario p1-theme --out /private/tmp/riwork-p1-m5-f6975181/native/named-routes-a0u78b8b/fixture-home` | 101 | No Rust tests |
| `nat01-declared-probe` | `/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh check crates/riwork-nsh-native/tests/app --tsc /Users/dominik/Library/Caches/nsh/typescript-7.0.2-darwin-arm64/package/lib/tsc` | 1 | No Rust tests |
| `nat04-declared-probe` | `/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh test crates/riwork-nsh-native/tests/app --junit /private/tmp/riwork-p1-m5-f6975181/native/named-routes-a0u78b8b/native-probe.xml` | 1 | No Rust tests |
| `every_preset_token_is_exact` | `cargo test -p riwork-nsh-native --features test-support --test appearance --locked --offline -- every_preset_token_is_exact` | 101 | No Rust tests |
| `follow_ghostty_without_a_parser_uses_the_riwork_palette_and_keeps_the_error` | `cargo test -p riwork-nsh-native --features test-support --test appearance --locked --offline -- follow_ghostty_without_a_parser_uses_the_riwork_palette_and_keeps_the_error` | 101 | No Rust tests |
| `a_write_by_another_process_reaches_every_subscriber_once` | `cargo test -p riwork-nsh-native --features test-support --test settings --locked --offline -- a_write_by_another_process_reaches_every_subscriber_once` | 101 | No Rust tests |
| `change_text_size_is_a_read_modify_write_under_the_lock` | `cargo test -p riwork-nsh-native --features test-support --test settings --locked --offline -- change_text_size_is_a_read_modify_write_under_the_lock` | 101 | No Rust tests |
| `the_first_delivery_is_a_snapshot` | `cargo test -p riwork-nsh-native --features test-support --test subscriptions --locked --offline -- the_first_delivery_is_a_snapshot` | 101 | No Rust tests |
| `rev_is_monotonic_per_topic_per_process` | `cargo test -p riwork-nsh-native --features test-support --test subscriptions --locked --offline -- rev_is_monotonic_per_topic_per_process` | 101 | No Rust tests |
| `overflow_clears_the_queue_and_sends_one_resync_snapshot` | `cargo test -p riwork-nsh-native --features test-support --test subscriptions --locked --offline -- overflow_clears_the_queue_and_sends_one_resync_snapshot` | 101 | No Rust tests |
| `one_delivery_per_topic_per_engine_turn` | `cargo test -p riwork-nsh-native --features test-support --test subscriptions --locked --offline -- one_delivery_per_topic_per_engine_turn` | 101 | No Rust tests |
| `streaming_topics_coalesce_to_16_ms_trailing` | `cargo test -p riwork-nsh-native --features test-support --test subscriptions --locked --offline -- streaming_topics_coalesce_to_16_ms_trailing` | 101 | No Rust tests |
| `closing_or_dropping_the_owner_ends_the_task` | `cargo test -p riwork-nsh-native --features test-support --test subscriptions --locked --offline -- closing_or_dropping_the_owner_ends_the_task` | 101 | No Rust tests |
| `a_project_switch_resubscribes_and_drops_late_deliveries` | `cargo test -p riwork-nsh-native --features test-support --test subscriptions --locked --offline -- a_project_switch_resubscribes_and_drops_late_deliveries` | 101 | No Rust tests |
| `one_watcher_and_one_sampler_per_home_per_process` | `cargo test -p riwork-nsh-native --features test-support --test hubs --locked --offline -- one_watcher_and_one_sampler_per_home_per_process` | 101 | No Rust tests |
| `a_quiet_home_produces_no_deliveries_for_60_s` | `cargo test -p riwork-nsh-native --features test-support --test services --locked --offline -- a_quiet_home_produces_no_deliveries_for_60_s` | 101 | No Rust tests |
| `errors_use_only_the_frozen_codes` | `cargo test -p riwork-nsh-native --features test-support --test services --locked --offline -- errors_use_only_the_frozen_codes` | 101 | No Rust tests |
| `tabs_update_rejects_invalid_keys` | `cargo test -p riwork-nsh-native --features test-support --test services --locked --offline -- tabs_update_rejects_invalid_keys` | 101 | No Rust tests |
| `store_rejects_non_uuid_ids` | `cargo test -p riwork-nsh-native --features test-support --test services --locked --offline -- store_rejects_non_uuid_ids` | 101 | No Rust tests |
| `hermes_settings_round_trip_preserves_every_existing_theme_name` | `cargo test -p riwork-core --lib --locked --offline hermes_settings_round_trip_preserves_every_existing_theme_name` | 0 | test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 291 filtered out; finished in 0.00s |
| `only_native_goes_stale_when_macos_switches_light_and_dark` | `cargo test -p riwork-core --lib --locked --offline only_native_goes_stale_when_macos_switches_light_and_dark` | 0 | test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 291 filtered out; finished in 0.00s |
| `published_terminal_follows_what_the_desktop_shows_while_following_ghostty` | `cargo test -p riwork-core --lib --locked --offline published_terminal_follows_what_the_desktop_shows_while_following_ghostty` | 0 | test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 291 filtered out; finished in 0.00s |
| `terminal_override_is_what_terminals_are_forced_to` | `cargo test -p riwork-core --lib --locked --offline terminal_override_is_what_terminals_are_forced_to` | 0 | test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 291 filtered out; finished in 0.00s |
| `native_terminals_match_the_theme_only_while_the_option_is_on` | `cargo test -p riwork-core --lib --locked --offline native_terminals_match_the_theme_only_while_the_option_is_on` | 0 | test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 291 filtered out; finished in 0.00s |
| `selected_theme_controls_terminals_and_keeps_legacy_preferences` | `cargo test -p riwork-core --lib --locked --offline selected_theme_controls_terminals_and_keeps_legacy_preferences` | 0 | test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 291 filtered out; finished in 0.00s |
| `saved_sizes_round_to_tenths_clamp_and_write_plain_numbers` | `cargo test -p riwork-core --lib --locked --offline saved_sizes_round_to_tenths_clamp_and_write_plain_numbers` | 0 | test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 290 filtered out; finished in 0.00s |
| `a_step_starts_from_the_stored_size_not_the_cached_one` | `cargo test -p riwork-core --lib --locked --offline a_step_starts_from_the_stored_size_not_the_cached_one` | 0 | test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 290 filtered out; finished in 0.08s |
| `ui_text_size_defaults_to_today_clamps_and_keeps_odd_values_until_changed` | `cargo test -p riwork-core --lib --locked --offline ui_text_size_defaults_to_today_clamps_and_keeps_odd_values_until_changed` | 0 | test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 290 filtered out; finished in 0.10s |

### review

| Command record | Exact command | Exit | Test result |
|---|---|---:|---|
| `native-surface` | `rg -n 'pub (async )?fn\|#\[module\]\|#\[class\]' crates/riwork-nsh-native/src/lib.rs apps/riwork/src/lib.rs` | 0 | No Rust tests |
| `core-engine-format-diff` | `git diff ea0f2f5 -- crates/riwork-core crates/riwork-engine` | 0 | No Rust tests |
| `no-native-storage` | `rg -n nsh/storage apps/riwork crates/riwork-nsh-native` | 1 | No matches |
| `native-backends` | `rg -n 'trait \|impl .*Backend\|spawn_blocking\|START.lock\|PROCESS\|get_or_init' crates/riwork-nsh-native/src/backend.rs crates/riwork-nsh-native/src/service.rs` | 0 | No Rust tests |
| `layout-blocking-review` | `sed -n 100,180p apps/riwork/src/host_layouts.rs` | 0 | No Rust tests |
| `appearance-writer-review` | `rg -n 'appearance_file\|publish_appearance\|write_appearance\|appearance.json' crates/riwork-nsh-native/src apps/riwork/src crates/riwork-desktop/src/nsh_main.rs crates/riwork-desktop/src/nsh_host.rs crates/riwork-desktop/src/nsh_runtime.rs` | 1 | No matches |
| `app-options` | `rg -n 'AppOptions\|data_dir\|appearance:\|app.setAppearance' crates/riwork-desktop/src/nsh_host.rs` | 0 | No Rust tests |
| `themes-label-review` | `rg -n 'ThemeChoice\|Follow Ghostty\|Catppuccin\|Tokyo Night\|Gruvbox\|Hermes\|Settings\|Appearance' crates/riwork-core/src/theme.rs apps/riwork/App.tsx crates/riwork-nsh-native/src/lib.rs` | 0 | No Rust tests |
| `native-ids-review` | `rg -n 'valid_key\|Uuid\|uuid\|invalid:\|create_folder\|createFolder' crates/riwork-nsh-native/src` | 0 | No Rust tests |
| `cargo-fmt-native` | `cargo fmt -p riwork-nsh-native --check` | 0 | No Rust tests |


## Cross-owner appearance fixture

The root runner's completed `/private/tmp/riwork-p1-m5-f6975181/fixture-5gf4wtii/manifest.json` proves the existing appearance file unchanged, including its mtime. Exact fixture generation and GUI launch (using the manifest's clean fixture environment):

```sh
cargo run -p riwork-nsh-workspace --example integration_fixture --locked --offline -- /private/tmp/rwm5-u3qi2ooy --appearance
/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork /private/tmp/rwm5-u3qi2ooy/repos/Alpha
```

## Cross-owner idle route

The final clean workspace run `/private/tmp/riwork-p1-m5-workspace/nsh-reh5ez2m/manifest.json` exits 0. Its JUnit explicitly passes `P1-POLL-02: a settled project sends no host work during 60 seconds of idle time`. Exact invocation (isolated environment recorded in the linked JSON):

```sh
/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh test apps/riwork --junit /private/tmp/riwork-p1-m5-workspace/nsh-reh5ez2m/nsh.xml
```
