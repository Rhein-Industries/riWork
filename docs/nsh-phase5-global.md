# NSH Phase 5: global window, dashboard, orchestrators

```yaml
assignment: 9a9d3423-c34a-4244-ae9d-8aa41448f6b8
branch: nsh-p5-global (off nsh 7718e60; nsh fd58edc and 0d4e1a6 merged in)
verification_environment: isolated_tmp_homes_headless_and_rust_tests_only
live_targets_touched: none
app_windows_launched: none
task_status: not_marked_done
```

This phase fills the global window that Phase 1 left as a placeholder, and adds
the bridge and host services behind it. Parity references: SHELL-034..068,
SHELL-217..237, SHELL-221..224, SHELL-269..273, PANEL-064..066, PANEL-078..099,
PANEL-143..149, PANEL-180..190 and DOCS-209..213 in
[`docs/nsh-parity.md`](nsh-parity.md). Phase 1 acceptance
[§5.4](nsh-phase1-acceptance.md#54-global-window) (P1-GLB-01..10, all FAIL in
M5) is rerun below.

## What this slice adds

| Part | Where | Notes |
|---|---|---|
| Dashboard read service | `crates/riwork-nsh-native/src/overview.rs` | One process thread for every window. It makes the same engine calls, in the same order, as `riwork dashboard --json` (store snapshot, session sample, `activity::states_once`, a running host's chats else the saved ones, `read_extras`, `read_backends`, `global_overview::build`, `observed::read`, `wire_with_observations`). It publishes the engine's own wire v1 (the phone's `dashboard.get` shape) plus `global_overview::backend_lines`. It starts no host, chat, shell, orchestrator or tmux server. |
| Haiku summary seam | same | `dashboard_intelligence::enrich_saved` only: the saved summary is shown. No Keychain read, no API request, and no `--refresh-summary` process. DOCS-212 keeps the request path in the engine and CLI. |
| `Dashboard.subscribe` / `refresh` | `crates/riwork-nsh-native/src/dashboard.rs` | Subscription-driven. With no subscriber the reader blocks on a channel and reads nothing. With one, a home change (store, sessions, chats, settings, `schedules.json`, `usage/`) wakes a read at most every 2 s; extras and the age heartbeat are at most every 10 s. Both intervals match GPUI. Refresh forces a read, and one made during a read runs after it. A failed read keeps the last figures and says why. |
| Change broadcast | `crates/riwork-nsh-native/src/service.rs`, `hub.rs` | `ProcessService::observe_changes`: the process worker wakes observers when a store or session snapshot actually changed, or when the one home watcher saw a relevant path. It uses non-blocking `try_send`. No new watcher or timer was added. |
| Codex completion recorder | `crates/riwork-nsh-native/src/backend.rs` | `EngineSessionsBackend::sample` keeps one `ActivityTracker` per process and records finished Codex turns with `notifications::record_completion_outcome`, as every GPUI window's sample did (SHELL-050, SHELL-273). The ledger dedupes across processes. |
| `Orchestrators.find` / `start` / `plan` | `crates/riwork-nsh-native/src/orchestrators.rs` | `find` and `plan` start nothing. `start` is called only by an explicit Start, ⌘⇧O or ⌘⌥O. It runs `orchestrators::plan` then `create` with the engine's scope lock, and returns `fallback` when Automatic ran its second choice. |
| `Usage` / `Accounts` | `crates/riwork-nsh-native/src/usage.rs` | See [Usage and accounts](#usage-and-accounts). |
| `LayoutSession.openChat(chatId)` | `apps/riwork/src/{layout,lib}.rs` | Places or selects an existing chat by the main-pane rules (SHELL-215, SHELL-219). |
| Global window | `apps/riwork/global/GlobalWindow.tsx` | The bar (glyph and `Global` in magenta, a Dashboard \| Chat segmented control with ids `global-view-dashboard` and `global-view-chat`, a drag region, `This Mac · every project`, 34 pt, inset 92/10). `global.show`, `restore.global` and `global-window.json` (view, filter, sort, `showQuiet`) are saved on change, and search is never saved. ⌘W, ⌘⇧N, Dock New Window (`command newWindow`), ⌘, and ⌘⇧S go to the most recent local project window, else the active project, else `Open one of this Mac's projects first.`. The bottom notice bar has gold text and `global-notice-dismiss`. The Dashboard subscription is held only while the window is not occluded. |
| Dashboard | `apps/riwork/dashboard/{Dashboard.tsx,model.ts}` | A port of `dashboard.rs`. It has the toolbar freshness line (Live, Last read N ago, Showing the read from…), the summary strip with the Global chat block, filters and orders with live counts, the search, the empty-table copy with "Show all", and project rows with their accessible names. The Activity rail has Decisions coverage and cards, Overview (Haiku), Needs you, Working now and Finished. The Work rail has Tasks in progress, Automations and Backends. The layout follows the window's own width (Wide, Normal, Compact). Row routing and its gone-target notices follow `global_window::route`. |
| Global orchestrator view | `apps/riwork/global/GlobalChat.tsx` | Chat form: the Phase 4a `ChatTab` bound to the orchestrator chat. Terminal form: `riwork.terminal` (`LocalTerminal`) under the fixed label and note. With none, the Start card uses `chat_copy` wording, id `global-chat-start`. An Unknown read changes nothing; None lets go of what was shown. |
| Project orchestrator | `apps/riwork/Workspace.tsx`, `commands.ts`, `keyboard.ts` | ⌘⌥O, the pane menu's New tab ▸ Project orchestrator, and P·ORCH: a chat orchestrator opens as a chat tab (`openChat`), a terminal one through the existing `openShell` path (unhide, then place). New tab ▸ Global orchestrator opens the global window's Chat with start. `openProjectOrchestrator` and `openGlobalOrchestrator` leave `DEFERRED_COMMANDS`. |
| Route steps | `Workspace.tsx` | `route` accepts the contract's `{ panel }` shorthand, plus `chat {chatId}` and `task {taskId}` (selects the task, opens Tasks). |
| Host | `crates/riwork-desktop/src/nsh_host.rs`, `nsh_notifications.rs` | Pure window decisions with the named host-unit tests: global singleton, route search order (on-screen front to back, then minimized and other Spaces), Dock New Window, and the global runtime record. TSX owns view, filter and sort; the host owns only `window_size` and merges it into the file on flush and publish. `window.list` gains `focusSerial`. `system.notify` is served. There is one non-recursive home watcher for the alert and wake ledgers. Alerts are claimed at startup and on each ledger change. Wakes tick at most once a second, only while a wake is pending. |

## Usage and accounts

The usage slice adds two `riwork/native` modules, both `schema: 1`:

| JS | Kind | Rule |
|---|---|---|
| `Accounts.subscribe(listener)` | sync handle | `AccountsDto` from the saved `codex-accounts.json` cache and Settings only (`cached_accounts`): saved accounts with their PANEL-146 detail lines, the selection, balancing, `fromCache`. It never starts discovery and never runs on a timer. |
| `Accounts.refresh()` | async | Runs `codex_accounts::discover_in` once (it starts Orca's CLI and writes the cache). Concurrent callers share one discovery. |
| `Accounts.selectDefault(id \| null)` | async | `null` or the system default saves `selected_codex_account = null`. An unavailable or unknown id is `refused: This account is unavailable. Refresh accounts or choose another.` |
| `Usage.subscribe(listener)` | sync handle | `UsageDto` from one process service. Codex: one entry per account home, read with `read_codex_usage_at` (starts `codex app-server`), one read per home at a time, `codex_refresh_interval` backoff, results fed to `codex_balancing::record_result`, and stale after 15 min or a failed refresh. Claude: file-only `<home>/usage/<shell>.json` for live Claude shells. Grok: not read (`[]`). It reads only while subscribed; it is woken by `observe_changes` and one Rust-side deadline. |
| `Usage.refresh(force)` | async | `force` ignores the backoff but never starts a second read for a home already being read. |

Status bar: the `usage` item shows the PANEL-180..184 readouts (`apps/riwork/usage.ts`, ported from `account_status.rs`, with tests). Its aria label is `Usage {segments}`, and a click opens the Usage panel. The `codex_account` item shows the PANEL-186 label and opens a menu of the PANEL-188 rows. P·ORCH is a button that opens the project orchestrator.

Debug and `test-support` builds never start codex, grok or Orca for a fixture home (`RIWORK_NSH_FAKE_SESSIONS` set, or a home under `/tmp`): Codex figures come from `<home>/usage-fixture.json` if present, otherwise "Account usage is unavailable". The bridge tests use fakes that count every read.

## Acceptance §5.4 rerun (P1-GLB-01..10)

Route commands are recorded under [Runs](#runs). Every `nsh test` ran on a freshly generated `p1-layouts` home under `/tmp/rw-p5g.EikL` with fake sessions and the fixture chat host.

| ID | M5 | Now | Evidence |
|---|---|---|---|
| P1-GLB-01 | FAIL | **PASS** | host-unit `window_registry::tests::the_global_window_is_a_singleton_and_opens_with_no_window` (C171: 1 passed); nsh-test `P1-GLB-01`: `global.show {view:"chat"}` → `label:Chat, shown`. |
| P1-GLB-02 | FAIL | **PASS** | core `global_window::tests` (C041: 3 passed); host-unit `the_global_window_opens_at_its_own_size` (1100×760, the saved size, an impossible size dropped, title `RiWork · Global`); nsh-test `P1-GLB-02`: exactly one `window.setDockInfo {project:"Global", branch:""}`. |
| P1-GLB-03 | FAIL | **PASS** | core C041; host-unit `a_view_tsx_saved_survives_a_host_flush_and_the_runtime_publish`; nsh-test `P1-GLB-03` (press Chat, wait 300 ms, a second global app opens on Chat, and its search is empty) and `P5-GLOBAL-02` (filter and order survive a reopen). |
| P1-GLB-04 | FAIL | **PASS** | engine `the_global_window_restores_as_itself_and_older_snapshots_hold_project_windows` (C043: 1 passed); host-unit `runtime_window::tests::the_global_window_is_recorded_as_global` (C155: 1 passed); nsh-test `P1-GLB-04`: `restore.global.view = "chat"` wins over the file, and `app.restoreReady` is posted once. The `p1-restore` generator (C276, exit 101 in M5) now exits 0 and records the global window. |
| P1-GLB-05 | FAIL | **PASS** | nsh-test `P1-GLB-05`: no DockArea, no `window.route`/`open`/`close`, no `app.restoreReady`. A global-only run on its own fresh home: `state.json` sha256 unchanged (before and after), no tmux files. The only file written is the empty `schedules.lock`, which the engine's `ScheduleStore::list` takes during the dashboard read, as `riwork dashboard` does. **Accepted 2026-10-28** (acceptance §1.1, contracts §8.5): R-DEFER 5 was written for the Phase 1 placeholder; an empty `schedules.lock` is now an allowed write on dashboard and global paths. The three generic audit scripts (`phase1_{w3,m5,integration}.py`) now allow an empty `schedules.lock`, but they still allow `state.json` writes, which broader workflows make. The `state.json` byte-identity for P1-GLB-05 therefore rests on the dedicated before/after inventory of the global-only run (13 files before, 14 after), not on those scripts. A dashboard read can also write the engine's private `dashboard-observations/` cache when chats exist; that is not part of this acceptance and did not occur in the global-only run. |
| P1-GLB-06 | FAIL | **PASS** | nsh-test `P1-GLB-06`: `Global` (cased per theme), `This Mac · every project`, both segment ids, drag region `global-drag`, `GLOBAL_BAR = {height 34, insetWithControls 92, inset 10}`; switching saves (GLB-03). |
| P1-GLB-07 | FAIL | **PASS** | nsh-test `P1-GLB-07`: ⌘W right after open → `window.close {}`. The menu's `command closeTab` takes the same path. |
| P1-GLB-08 | FAIL | **PASS** | host-unit `menu::tests::dock_new_window` (C172: 1 passed); nsh-test `P1-GLB-08`: ⌘⇧N and `command newWindow` open the window with the highest `focusSerial`'s project, ⌘, and ⌘⇧S → `window.route {step:{kind:"panel", panel:"settings"/"schedules"}}`, and with no project window the store's active project. The no-project notice is covered by the pure `commandProject([], null) === null` and the exact notice constant. |
| P1-GLB-09 | FAIL | **PASS** | host-unit `window_registry::tests::route_prefers_frontmost_then_minimised_then_opens` (C173: 1 passed); nsh-test `P1-GLB-09`: `route {step:{panel:"settings"}}` selects Settings and its pane. |
| P1-GLB-10 | FAIL | **PASS** | nsh-test `P1-GLB-10`: `params.notice` is shown as an alert in gold; `global-notice-dismiss` removes the bar. |

## Phase 5 checks

| Requirement | Evidence | Result |
|---|---|---|
| Dashboard from store facts, backends, agents | nsh-test `P5-GLOBAL-01` (fixture projects L-NAV, L-NESTED and the rest under All, the summary strip, Backends rows, the terminal orchestrator); bridge `nothing_is_read_without_a_window_and_one_read_serves_every_window` (the engine wire v1 for a TempDir store) | pass |
| Freshness ages without a TSX clock | Every age is measured against the snapshot's `now`. Model tests `P5-DASH-05` (Live, Last read 1m ago, Showing the read from 1m ago · error, Reading sessions…); bridge `a_failed_read_keeps_the_figures_it_had_and_says_why` | pass |
| Filters, orders, search, empty states, layout by width, row words and routing | `apps/riwork/dashboard/model.test.ts` `P5-DASH-01..09`; nsh-test `P5-GLOBAL-02` and `P5-GLOBAL-03` | pass |
| Haiku summary is display only | `P5-DASH-10` (hidden when disabled or without evidence; a saved summary, highlights and status); the bridge wire carries `intelligence.state = "disabled"` from `enrich_saved` with no request | pass |
| Subscription-driven, zero polling | `check-polling.sh` passes. Bridge: no read without a subscriber, one read serves two windows, a change is read at the change pace, a refresh at once, a quiet window gets heartbeats only, and no reads after the last window goes. A refresh with no window left does not spin the reader: `a_refresh_is_read_at_once_for_a_window_and_with_no_window_the_reader_blocks` (see [the idle-loop fix](#reader-idle-loop-fix-task-0ff5e68a)). `services::home_changes_wake_change_observers_and_a_quiet_home_wakes_none`. Host: `wakes_block_without_a_pending_wake_and_tick_once_a_second_with_one`, `only_the_two_ledgers_wake_their_services` | pass |
| Global orchestrator presentation | nsh-test `P5-GLOBAL-04`: a live terminal orchestrator is shown as itself (label, `riwork.terminal`) and no Start is offered; `P5-DASH-08`: Start card wording for none, terminal, chat and unknown | pass |
| Project orchestrator presentation (chat form = Phase 4a chat tab) | nsh-test `P5-ORCH-01`: ⌘⌥O makes the project's orchestrator chat on the fixture host, opens it as a chat tab (`ChatTab`, `chat-input`), and a second ⌘⌥O reuses it; `P5-ORCH-02`: New tab ▸ Global/Project orchestrator enabled; bridge `orchestrators_start_only_on_request_validate_ids_and_name_the_fallback` | pass |
| Orchestrator wakes (engine semantics unchanged) | `Runner::tick` unchanged; the host service blocks until the wake ledger changes, then ticks once a second only while a wake is pending (`nsh_notifications::tests::wakes_block_without_a_pending_wake_and_tick_once_a_second_with_one`) | pass |
| Finished-agent notifications (fixture-verified) | Host `notifications::tests` (7): a pending opted-in completion is posted once with the engine presentation and never again; the tag maps back and other actions are ignored; the agent must still belong to the project; the fixture poster writes `nsh/fixture-notifications.jsonl` only under a `/tmp` home; `system.notify` validation. The Codex recorder runs once per process in the session sampler. | pass (fixture only) |
| Account status and usage surfaces | bridge `usage_bridge` 9 passed; nsh-test `P5-USAGE-*` and `P1-STAT-*` in the full run; `apps/riwork/usage.test.ts` | pass |

## Runs

```sh
source <scratchpad>/env.sh   # RIWORK_HOME, RIWORK_RUNTIME_DIR, TMUX_TMPDIR, TMPDIR, NSH_CACHE under /tmp/rw-p5g.EikL; HOME=target/nsh-integration-home-p5g
cargo test --locked --offline -p riwork-nsh-native --features test-support      # lib 98 + 1 ignored; dashboard_bridge 4; services 17; usage_bridge 9; chat_bridge 11; layout_session 18; panel_crud 4; others unchanged
cargo test --locked --offline -p riwork-nsh-workspace                           # 44, 1, 13, 82, 18, 7: all pass
cargo test --locked --offline -p riwork-desktop                                 # bin 60, appkit_shell 11, cli_contract 6, startup_environment 1 (pinned Ghostty install via RIWORK_GHOSTTY_INSTALL_DIR, read-only)
"$NSH_CLI" check apps/riwork --tsc "$NSH_TSC"                                    # pass
PATH=<ripgrep 15.2.0>:$PATH sh apps/riwork/check-polling.sh                     # exit 0
nsh-run.sh full1 ; nsh-run.sh full2   # fresh p1-layouts home each, RIWORK_NSH_FAKE_SESSIONS + RIWORK_NSH_CHAT_FIXTURE=1, nsh test apps/riwork
                                      # 138 passed, 0 failed (twice); no tmux files
nsh-run.sh glb05 --filter P1-GLB-05   # home audit above
cargo fmt --all --check                                                          # clean
```

The M5 §5.4 command catalog, rerun on this branch:

| ID | Command | Result |
|---|---|---|
| C039 | `test -f apps/riwork/tests/Global.test.tsx` | exit 0;  |
| C041 | `cargo test -p riwork-core --lib global_window::tests --locked --offline` | exit 0; ok. 3 passed; 0 failed; |
| C043 | `cargo test -p riwork-engine --lib the_global_window_restores_as_itself_and_older_snapshots_hold_project_windows --locked --offline` | exit 0; ok. 1 passed; 0 failed; |
| rg-GLB-01 | `rg -c P1-GLB-01 apps/riwork -g *.test.ts -g *.test.tsx` | exit 0; apps/riwork/tests/Global.test.tsx:1 |
| rg-GLB-02 | `rg -c P1-GLB-02 apps/riwork -g *.test.ts -g *.test.tsx` | exit 0; apps/riwork/tests/Global.test.tsx:1 |
| rg-GLB-03 | `rg -c P1-GLB-03 apps/riwork -g *.test.ts -g *.test.tsx` | exit 0; apps/riwork/tests/Global.test.tsx:1 |
| rg-GLB-04 | `rg -c P1-GLB-04 apps/riwork -g *.test.ts -g *.test.tsx` | exit 0; apps/riwork/tests/Global.test.tsx:1 |
| rg-GLB-05 | `rg -c P1-GLB-05 apps/riwork -g *.test.ts -g *.test.tsx` | exit 0; apps/riwork/tests/Global.test.tsx:1 |
| rg-GLB-06 | `rg -c P1-GLB-06 apps/riwork -g *.test.ts -g *.test.tsx` | exit 0; apps/riwork/tests/Global.test.tsx:1 |
| rg-GLB-07 | `rg -c P1-GLB-07 apps/riwork -g *.test.ts -g *.test.tsx` | exit 0; apps/riwork/tests/Global.test.tsx:1 |
| rg-GLB-08 | `rg -c P1-GLB-08 apps/riwork -g *.test.ts -g *.test.tsx` | exit 0; apps/riwork/tests/Global.test.tsx:1 |
| rg-GLB-09 | `rg -c P1-GLB-09 apps/riwork -g *.test.ts -g *.test.tsx` | exit 0; apps/riwork/tests/Global.test.tsx:1 |
| rg-GLB-10 | `rg -c P1-GLB-10 apps/riwork -g *.test.ts -g *.test.tsx` | exit 0; apps/riwork/tests/Global.test.tsx:1 |
| C155 | `cargo test -p riwork-desktop --bin riwork runtime_window::tests::the_global_window_is_recorded_as_global --locked --offline` | exit 0; ok. 1 passed; 0 failed; |
| C171 | `cargo test -p riwork-desktop --bin riwork window_registry::tests::the_global_window_is_a_singleton_and_opens_with_no_window --locked --offline` | exit 0; ok. 1 passed; 0 failed; |
| C172 | `cargo test -p riwork-desktop --bin riwork menu::tests::dock_new_window --locked --offline` | exit 0; ok. 1 passed; 0 failed; |
| C173 | `cargo test -p riwork-desktop --bin riwork window_registry::tests::route_prefers_frontmost_then_minimised_then_opens --locked --offline` | exit 0; ok. 1 passed; 0 failed; |
| GLB-02-host | `cargo test -p riwork-desktop --bin riwork window_registry::tests::the_global_window_opens_at_its_own_size --locked --offline` | exit 0; ok. 1 passed; 0 failed; |
| GLB-03-host | `cargo test -p riwork-desktop --bin riwork a_view_tsx_saved_survives_a_host_flush_and_the_runtime_publish --locked --offline` | exit 0; ok. 1 passed; 0 failed; |

Environment: Rust 1.99.1, `--locked --offline`; pinned NSH CLI 0.3.0 (`76fa8a7`) at
`/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV`. Scratch root `/tmp/rw-p5g.EikL`. No app window was
launched, Cua was not used, and no live RiWork home, window, chat, orchestrator or notification
center was read or written. Fixture runs read the machine's provider CLIs on `PATH` only as the
Backends section does (paths and versions, nothing executed).

### After merging nsh fd58edc (P3b Files)

The merge conflicted in two files only:

- `crates/riwork-nsh-native/src/lib.rs` now has one registry: P3b's `memo` and `Files`, plus
  Phase 5's `Accounts`, `Orchestrators` and `Usage`, for 12 modules. The registry test lists all 12.
- `apps/riwork/tests/StatusBar.test.tsx` keeps P3b's check that labels are scoped to the status
  bar, using the Phase 5 labels.

P3b's Files tests need `RIWORK_NSH_FILES_FIXTURE=1`, so `nsh-run.sh` now exports it.

```sh
cargo fmt --all --check                                                          # clean
cargo test --locked --offline -p riwork-nsh-native --features test-support      # lib 108 + 1 ignored; files_bridge 3; dashboard_bridge 4; services 17; usage_bridge 9; chat_bridge 11; layout_session 18; panel_crud 4; appearance 3; native_headless 1; settings 1 + 7
cargo test --locked --offline -p riwork-nsh-workspace                           # 47, 1, 13, 82, 18, 7: all pass
cargo test --locked --offline -p riwork-desktop                                 # bin 60, appkit_shell 11, cli_contract 6, startup_environment 1
PATH=<ripgrep 15.2.0>:$PATH sh apps/riwork/check-polling.sh                     # exit 0
nsh-run.sh merge2                     # 146 passed, 0 failed; no tmux files
"$NSH_CLI" check apps/riwork --tsc "$NSH_TSC"                                    # exit 0
cargo test -p riwork-engine --lib the_global_window_restores_as_itself_and_older_snapshots_hold_project_windows --locked --offline   # C043: exit 0, 1 passed
nsh-run.sh fglb --filter P1-GLB       # 10 passed, 0 failed; no tmux files
nsh-run.sh fglb05 --filter P1-GLB-05  # 1 passed; home audit below
```

GLB-05 home audit, a global-only run on a fresh home. This is the dedicated before/after sha256
inventory of every home file that `nsh-run.sh` saves (`out/before.sha` and `out/after.sha`: 13
files before, 14 after), not one of the generic Phase 1 audit scripts:

- The only file written is `schedules.lock`, at 0 bytes.
- `state.json` has sha256 `fe56e0b0…d2f0` before and after.
- Nothing was removed.
- No `schedules.json`, `sessions.json`, `chats/` or `appearance.json` was created, and there are
  no tmux files.

C043 had exited 101 on the first post-merge attempt (03:08), while compiling `riwork-engine`. Only
the first line of its output was kept, so the cause is not proven. The suspected cause is disk
exhaustion. What supports it:

- The system log (`deleted`, `CD_VERYLOWDISK_EVENT`) shows the data volume at 190 MB free at 03:08:00.
- The engine test binary had to be rebuilt, and it alone is 86 MB.
- No engine source has changed since then, and the same command on the same tree now exits 0.

None of this shows the actual error. `nsh check` and the two filtered `nsh test` runs also failed
in that minute, and their output was not kept either. The same cause is suspected for them, and
it is unproven too. All three pass now, on the same tree.

### Reader idle loop fix (task 0ff5e68a)

**The bug** was in the dashboard reader loop, `overview::run`:

- `Wake::Refresh` set `force`. With no subscriber left, the loop turned again without clearing it.
- `force` made the next deadline `Some(now)`, so the loop never blocked on its inbox. It spun at
  full CPU until a window subscribed again.
- It made no reads while spinning, so a reads-only check could not see it.

**The fix:** `force` is now local to one loop turn. It is set by a `Refresh` drained in that turn
and used by that turn's read only, so it never reaches the deadline.

- With nobody subscribed, the turn drops the refresh, and the next turn blocks on `recv()`.
- A subscribed refresh is still read at once.
- A refresh that arrives during a read stays queued in the inbox and is read on the next turn.

**The test:** `OverviewService` counts loop turns with one relaxed atomic add per turn, read
through `turns()`, and the regression measures them. The test runs as follows:

1. Pacing is set to a 5 s change gap and a 30 s heartbeat, so only a forced read can arrive
   within a second.
2. A subscribed refresh must be read in under 1 s.
3. The test drops the last subscription, sends a refresh and lets the loop settle for 200 ms.
4. It then counts loop turns over 500 ms, with no wake sent. It asserts the reads did not grow,
   then asserts at most 1 turn.
5. A new subscriber's refresh must again be read in under 1 s.

| Run | Logic | Result |
|---|---|---|
| Old-fail | the merged `overview.rs` with only the turn counter added | `dashboard reader: 8520317 turns in 500 ms with no window after a refresh`. The reads assertion passed (reads stayed at 2), and the test panicked at `dashboard_bridge.rs:201` on the turn bound. Result: 4 passed, 1 failed, exit 101. Two earlier runs, before the test file was formatted, measured 8,465,327 and 8,518,220 turns. |
| Fixed-pass | per-turn `force` | `dashboard reader: 0 turns in 500 ms with no window after a refresh`. Result: 5 passed, exit 0. The regression alone passed 10 of 10 repeat runs, with 0 turns in each. |

```sh
cargo test --locked --offline -p riwork-nsh-native --features test-support --test dashboard_bridge -- --nocapture
```

## Known gaps and divergences

- **No GUI run.** No app window was launched and Cua was not used, so the fixture+cua halves of the GLB rows (titles and frames in RLD-01, the Dock menu) were not re-read. Their host-unit and nsh-test halves pass.
- **The real notification poster has never run.** UNUserNotificationCenter is only created inside an app bundle. It compiles but is unexercised; delivery, claim-once and click routing are verified through the fixture poster and unit tests only.
- **The Haiku refresh is not triggered from NSH.** The dashboard shows the saved summary only; `riwork dashboard --json` (the phone connector) still spawns its refresh as before.
- **Not ported yet:**
  - the decision review dialog (PANEL-100..103): cards are listed and a click routes to the chat;
  - the Recent activity list;
  - the exact order of the single (Normal) rail;
  - `Chat.claimTab` for the global chat (the Phase 4a seam);
  - the Global menu's detail lines in project windows.
- **The project windows' Global menu checkmark can lag.** In-window view switches are saved by the global window but not reported to the host, so the checkmark follows the last view the host opened.
- **Usage:** Grok is not read (`GROK · —`). The Codex account menu is read-only, because `riwork/native` has no `Store.setProjectCodexAccount` yet. Accounts are discovered when the menu opens with no list, not at app start.
- **Fixture orchestrators are fixture-host chats.** They use the `turn` recording in the default approval mode. The production path is the engine's `plan` + `create`, covered by the engine's own tests and a bridge fake. The orchestrator chat tab shows the chat's title, not the fixed `P·ORCH · PROJECT`.
- **`DEFERRED_COMMANDS`** no longer lists `openProjectOrchestrator` or `openGlobalOrchestrator` (Phase 1 acceptance §8); ⌘⌥O moved to the window keymap.
- **Fixture home:** the `p1-layouts` scenario gains one project, `Phase5` (a copy of L-MAIN's layout), for tests that change tabs. Golden `L-` fixtures stay untouched.
- **A possible flake:** P1-WIN-09 failed once in a helper agent's full run (an extra `window.setDockInfo` post). It passed in that agent's rerun and in both full runs above.
