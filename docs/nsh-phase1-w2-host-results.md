# NSH Phase 1 M5 W2 host results

**The 30 host-lane items finish at 23 PASS and 7 PARTIAL. The assignment is not an all-PASS acceptance sign-off.** W2 implements real-app restart handoff, Rust close/quit flush barriers, remembered geometry, native chrome, menu routing and the full declared keymap. Remaining dependencies are shell creation, shared-strip numbering, D-PROC-1, observed external AX gaps and tooltip lifecycle hiding.

```yaml
assignment: 5741d092-f840-4456-a96f-e301357af991
branch: nsh-p1-fix-host
base_commit: ed1b456
scope: acceptance 5.1, 5.2, 5.3, 5.9; global-window mechanics only
verification_environment: isolated RIWORK_HOME/runtime/tmux/config; fixture PIDs only
input_policy: read-only cua-driver MCP; no desktop input
native_tests: unshown AppKit windows; activation prohibited; local synthetic events only
merge_push_install: none
task_status: unchanged; owned by orchestrator
report_state: final
```

The [machine record](nsh-phase1-w2-host-results.json) contains all 30 dispositions and evidence mappings. The baseline [M5 results](nsh-phase1-m5-results.md), [host-owner results](nsh-phase1-m5-host-results.md), and baseline machine JSON remain unchanged. Requirements are [acceptance](nsh-phase1-acceptance.md) §§5.1–5.3 and 5.9. A narrower host-owner PASS does not override a whole-item dependency: notably PROC-05's output decision and the prior menu/keyboard workspace gaps. Global content, chat/terminal content and the full theme service are outside this host assignment.

| Section | M5 PASS / FAIL | W2 PASS | W2 PARTIAL |
|---|---:|---:|---:|
| 5.1 Process/CLI | 4 / 3 | 5 | 2 |
| 5.2 Restart/restore | 2 / 2 | 3 | 1 |
| 5.3 Windows | 1 / 9 | 8 | 2 |
| 5.9 Menus/keys | 2 / 7 | 7 | 2 |
| Total | 9 / 21 | 23 | 7 |

## Recorded verification

The [artifact index](../crates/riwork-desktop/verification/w2-host-evidence/artifact-index.json) maps source paths to retained byte-exact outputs and hashes. Links below point to durable copies. Manifests preserve exact argv, cwd, isolated environment, exit code, duration and stdout/stderr hashes; Cua JSON and images are alongside their run manifest. The superseded `fail-flush-o1s9j_mi` injection attempt and `missing-3tg3iucm` notice attempt are archived. The injection failure came from a default-feature test build overwriting `target/debug/riwork`; this report does not claim every development attempt was retained. The final runner copies the fixture-enabled executable to `fixture-bin/riwork` before launching it; final lifecycle evidence below uses that immutable copy.

| Evidence | Result |
|---|---|
| [W2-BUILD] | Fixture-enabled desktop, production NSH bundle and automatic close/quit bundles build successfully. Immutable executable SHA-256 `00355577e56ef93b84fba924b2689295277e0fa151d831f4c0c93a1a8200609d`. |
| [W2-RUST] | **120 passed, 0 failed**: 43 host unit, 10 real AppKit, 6 CLI contract, 1 startup environment, 9 native unit, 1 native two-engine, 5 services, 28 workspace unit, 10 layout persistence and 7 window-size. Zero-test doctest groups are not proof. |
| [W2-HOST] | Final focused host-bin rerun **43 passed**, including close/quit preparation guards and failed-restart forwarding recovery. [W2-RESUME] separately reruns the idempotent forwarding-resume successor successfully. These overlap W2-RUST; counts are not added as unique tests. |
| [W2-RUNTIME] | **32 passed, 10 ignored**. [W2-WATCHDOG] subsequently runs **4/4** of those ignored cases successfully; six remain unexecuted. Accelerated watchdog timing is detailed in RLD-02. |
| [W2-NSH-FINAL] | Type-check exit 0; **36 passed, 0 failed**. Includes seven theme islands, geometry, stored titles, restore-ready, accessible notice props, Dock metadata and modal/All tabs successor. [Deferred command review](../crates/riwork-desktop/verification/w2-host-evidence/strip-final-b7qqh2by/deferred-command-review.json): 24 intended IDs, none missing/unexpected; `newTab` is not deferred. |
| [W2-CTRL-TAB] | **1/1 PASS** after the aggregate AppKit run: real NSH AppView/JS keymap through local NSApplication event dispatch; exactly one forward/reverse callback. Explicit unshown fixture window; no Cua/global input. This is automated native coverage, not a manual fixture action. |
| [W2-PARITY] | Final immutable binary (same SHA-256 as W2-BUILD) matches all five prescribed historical GPUI stdout, stderr and exit triples byte-for-byte: project list, settings list, instances, appearance, unknown command. Appearance/unknown correctly match exit 2. |
| [W2-STARTUP-ERROR] | Invalid private RIWORK_HOME exits 2 with `riwork: ` prefix; no remaining fixture process or tmux entries. |
| [W2-RESTORE] | PID **47825 → 50480**, restart exit 0 (1 requested/1 reloaded/0 failed), old exit 0, one new registry entry, all three runtime window payloads equal. Cua titles/bounds equal and six screenshots visually compared. [Visual comparison](../crates/riwork-desktop/verification/w2-host-evidence/restore-l7crkidh/visual-comparison.json) explicitly records the empty AX tab result. |
| [W2-CLOSE] / [W2-QUIT] | Real-app automatic host requests exit 0 in **3.848 s / 3.174 s**, leaving empty registries. These are host-call fixture actions, not native close-button/menu clicks. Rust tests independently prove disk flush correctness. |
| [W2-FAIL-FLUSH] | Injected deferred draft-writer hook failure makes restart exit 2; original PID **52962** survives; Beta forward exits 0 and Cua sees both windows. |
| [W2-BETA] / [W2-MISSING] | Direct Beta startup PID **51601** has the correct title. Missing-path startup PID **36862** falls back to Alpha with visible notice, but the prescribed AX notice query remains incomplete. |
| [W2-TOOLTIP] / [W2-AX-REVIEW] | No-NSWindow AppKit check reruns **1/1 PASS**; pinned-runtime API reviews and source hashes record the remaining tooltip and external AX limitations without inventing a cause or pass. |

Final [cleanup](../crates/riwork-desktop/verification/w2-host-evidence/final-cleanup/cua.json) records no windows for all seven final fixture PIDs; process preflight finds none remaining. Final lifecycle manifests also record empty instance registries after cleanup. No live RiWork PID/state was inspected.

Final restore uses Alpha normal bounds `(100,100,1000,700)`, Beta normal bounds `(140,140,1000,700)` with `maximized`, `focus_mode: true`, `focus_centered: false`, and Global `(180,180,1100,760)`. Global view/filter/sort payload preservation establishes window mechanics only.

## Seven-theme chrome evidence

Each linked run retains `manifest.json`, `cua.json` and the screenshot. All seven show one registered PID, two Alpha/Beta windows at **1220×780**, Beta offset **(22,22)**, and Close AX offset **(9,7)**, within ±1 of `(10,8)`.

| Theme | Run / fixture PID | Screenshot |
|---|---|---|
| ghostty | [basic-558dn1yi](../crates/riwork-desktop/verification/w2-host-evidence/basic-558dn1yi/manifest.json) / 88934 | [win-06-ghostty.png](../crates/riwork-desktop/verification/w2-host-evidence/basic-558dn1yi/win-06-ghostty.png) |
| native | [basic-rgmqshkw](../crates/riwork-desktop/verification/w2-host-evidence/basic-rgmqshkw/manifest.json) / 15584 | [win-06-native.png](../crates/riwork-desktop/verification/w2-host-evidence/basic-rgmqshkw/win-06-native.png) |
| ri_work | [basic-1nf84y9k](../crates/riwork-desktop/verification/w2-host-evidence/basic-1nf84y9k/manifest.json) / 17068 | [win-06-ri_work.png](../crates/riwork-desktop/verification/w2-host-evidence/basic-1nf84y9k/win-06-ri_work.png) |
| catppuccin | [basic-b7_v5s2l](../crates/riwork-desktop/verification/w2-host-evidence/basic-b7_v5s2l/manifest.json) / 21941 | [win-06-catppuccin.png](../crates/riwork-desktop/verification/w2-host-evidence/basic-b7_v5s2l/win-06-catppuccin.png) |
| tokyo_night | [basic-qxirw2fk](../crates/riwork-desktop/verification/w2-host-evidence/basic-qxirw2fk/manifest.json) / 24904 | [win-06-tokyo_night.png](../crates/riwork-desktop/verification/w2-host-evidence/basic-qxirw2fk/win-06-tokyo_night.png) |
| gruvbox_light | [basic-tp9hc7o4](../crates/riwork-desktop/verification/w2-host-evidence/basic-tp9hc7o4/manifest.json) / 27356 | [win-06-gruvbox_light.png](../crates/riwork-desktop/verification/w2-host-evidence/basic-tp9hc7o4/win-06-gruvbox_light.png) |
| hermes | [basic-z0bz3g8p](../crates/riwork-desktop/verification/w2-host-evidence/basic-z0bz3g8p/manifest.json) / 28967 | [win-06-hermes.png](../crates/riwork-desktop/verification/w2-host-evidence/basic-z0bz3g8p/win-06-hermes.png) |

## Acceptance mapping

`PASS` uses the stated W2 and retained baseline evidence; `PARTIAL` retains a concrete implementation, decision or observation gap. Baseline columns are the whole-item M5 results.

### 5.1 Process and CLI

| Item | M5 | W2 | Evidence and remaining work |
|---|---|---|---|
| P1-PROC-01 | PASS | PASS | [W2-PARITY] compares all five prescribed commands against the historical ea0f2f5 GPUI executable: stdout, stderr and exit status are byte-equal. [W2-RUST]'s six CLI contract tests also pass, including the independent `settings_json_keeps_the_gpui_reference_field_order` golden. `preserve_order` remains enabled; CLI dispatch precedes AppKit/NSH initialization. |
| P1-PROC-02 | PASS | PASS | `startup::tests::gui_start_scrubs_child_ids` passes in its dedicated process. No parallel test mutates the shared environment. |
| P1-PROC-03 | PASS | PASS | [W2-STARTUP-ERROR] uses an invalid private RIWORK_HOME: exit 2, `riwork: ` stderr prefix, empty stdout, no fixture process remaining and no tmux entries. CLI error regressions also pass. |
| P1-PROC-04 | FAIL | PARTIAL | Host tests pass canonical reuse, vanished-root error, cwd/HOME fallback and explicit-missing-path recovery. [W2-BETA] directly starts Beta (PID 51601, Cua title `RiWork · Beta`). [W2-MISSING] shows Alpha and a visible missing-folder notice; [W2-NSH-FINAL] proves accessible alert props. The required real-app AX notice query still matches 0 and verification returns `unknown` / `observation_unavailable`; the incomplete tree prevents whole-item PASS. |
| P1-PROC-05 | FAIL | PARTIAL | Host ownership/forward/replacement/stale-lock tests pass, as in the prior narrower host-owner PASS. [W2-FAIL-FLUSH] demonstrates successful forwarding after a cancelled restart. **D-PROC-1 remains open** in acceptance: no new output decision is inferred. [W2-BASIC-THEMES] provides seven one-PID/two-window/Cua records and successful Beta forwards; the output decision still prevents full acceptance. |
| P1-PROC-06 | PASS | PASS | Engine registry query test and [W2-RESTORE] three-window registry pass; all seven [W2-BASIC-THEMES] manifests show exactly one PID and two ordinary project windows after Beta forwarding. |
| P1-PROC-07 | FAIL | PASS | [W2-RUST] runs `native_pushes_one_service_to_two_engines_and_stops_on_close_owner_and_engine_drop`, `one_sampler_serves_multiple_subscribers_and_emits_only_changes`, watcher and quiet-home tests, plus registry composition. Production uses one process service/watcher and one hub per provided topic; host registers the UI sets before AppViews. These are explicit successors to the absent named `hubs.rs::two_engines_share_one_home_watcher_and_one_hub_per_topic`, not a claim that that file/test now exists. |

### 5.2 Restart, reload and restore

| Item | M5 | W2 | Evidence and remaining work |
|---|---|---|---|
| P1-RLD-01 | FAIL | PARTIAL | [W2-RESTORE] functionally passes: PID 47825 → 50480, restart exit 0, old exit 0, one replacement registration, exact three-window RuntimeWindow equality, matching Cua titles/bounds and visually matching Alpha/Beta tab labels. The mandated AX tab-label comparison returns no tabs; screenshots and Rust payload equality do not replace that missing route. |
| P1-RLD-02 | FAIL | PASS | [W2-HOST]/[W2-RUNTIME] pass per-window/idempotent/matching-target acknowledgements, 20 s confirmation cap, failed/hung replacement retry and fired-watchdog refusal. [W2-FAIL-FLUSH] makes restart exit 2, retains PID 52962 and successfully forwards Beta afterward. [W2-WATCHDOG] executes four formerly ignored cases, including a real wedged child that unregisters, exits 75 and ends its helper. That test uses a 400 ms deadline with the production timeout callback; the production 40 s deadline/configuration is source-checked. Six other ignored engine cases remain unexecuted; no full 40 s wall-clock run is claimed. |
| P1-RLD-03 | PASS | PASS | `publishing_writes_only_changes_and_never_waits_for_the_registry_lock` and `a_busy_registry_lock_stays_due` pass. A skipped busy registry lock keeps publication due; successful publication advances the two-second deadline. |
| P1-RLD-04 | PASS | PASS | [W2-RUST] reads current Rust LayoutSession at publication and preserves normal geometry without a JS round trip. Native two-engine and layout-roundtrip tests cover restore installation/save precedence. [W2-NSH-FINAL] actual production App tests verify layout installation before one ready post and none on ordinary startup; restored Global confirms without opening a project session. [W2-RESTORE] preserves focus flags as well as layouts. |

### 5.3 Windows and chrome

| Item | M5 | W2 | Evidence and remaining work |
|---|---|---|---|
| P1-WIN-01 | PASS | PASS | Real unshown AppKit `window_chrome` passes full-size-content, transparent titlebar, 640×400 minimum and native close control `(10,8)`. Every [W2-BASIC-THEMES] Cua record measures Close AX offset `(9,7)`, within ±1. |
| P1-WIN-02 | FAIL | PASS | Production opening size honors remembered project size/settings, otherwise 1220×780; Global uses remembered/default 1100×760 independently. `the_cascade_counts_open_non_tooltip_windows` passes. All seven [W2-BASIC-THEMES] Cua records show ordinary Alpha/Beta size 1220×780 and exact `(22,22)` cascade. Preserved baseline core fit/clamp test remains applicable. |
| P1-WIN-03 | FAIL | PASS | Named `zoomed_counts_as_maximized_and_keeps_its_normal_frame` and `fullscreen_counts_as_fullscreen` pass. Restore uses exact saved normal frames and keeps ordinary geometry while zoomed/fullscreen. [W2-RESTORE] confirms maximized Beta and equal before/after native bounds. |
| P1-WIN-04 | FAIL | PASS | All seven prescribed `window_size.rs` bridge checks execute and pass: 250 ms ordinary resize, ignore zoom/fullscreen, no writes for move/return to settled, range rejection, remember off, turning on captures current size, carry size across project switch. Tests exercise real persistence and mtime/no-write behavior. Existing core legacy/invalid-size test passed in the baseline; it was not rerun by the runtime-only engine filter. |
| P1-WIN-05 | FAIL | PASS | Stored project name/remote metadata and change-only title cases pass in [W2-NSH-FINAL]; production title is set in one memoized effect after async Store bootstrap. Cua [W2-BASIC-THEMES] shows `RiWork · Alpha`/`RiWork · Beta`; restored Global is `RiWork · Global`. |
| P1-WIN-06 | FAIL | PASS | [W2-NSH-FINAL] passes seven themed islands, fullscreen removal, 78×28 island/inset 92 metrics, top-pane-only handles, and empty drag strip measured after tabs. AppKit real NSH drag target double-click tests None/Minimize/Maximize preference routes without changing persisted defaults. Native and Hermes keep an unpainted island; other presets use the chrome palette projection. [W2-BASIC-THEMES] retains `win-06-<theme>.png` for all seven themes (catalog above). Narrow `Appearance.get()` reads existing appearance/settings; this does not implement the full Phase-1 theme service or Ghostty parser/publisher. |
| P1-WIN-07 | FAIL | PARTIAL | Named last-close/tooltip-count tests and actual mounted NSH `tooltips_create_no_nswindow` now pass. [W2-CLOSE] exits normally after last close. Existing tests prove overlays do not contribute NSWindows/cascade/quit count. [W2-TOOLTIP] reruns no-NSWindow successfully and records the exact limitation: pinned NSH0.3 exposes no public AppView dismiss/cancel-pointer API or native move/resign hint observer; its cancellation function is crate-private. The RiWork delegate only emits window.state, with no controlled tooltip owner consuming it. The controlled-open probe cannot establish automatic hiding. A runtime cancellation hook or app-controlled tooltip layer is required for move/deactivation/project-switch parity; no full clause PASS is inferred. |
| P1-WIN-08 | FAIL | PASS | Rust close/quit tests save the last accepted edit, every project layout and Global state; failed flush/cancel and late loading-window guards pass in [W2-HOST]. Final [W2-CLOSE]/[W2-QUIT] exit 0 with empty registries; [W2-RESTORE] closes the prior GUI only after confirmation. SIGTERM cleanup is never counted as graceful quit proof. |
| P1-WIN-09 | FAIL | PARTIAL | Host `events_only_on_change` and NSH occlusion/no-host-call/fullscreen-island test pass. State carries key/visible/occluded/mode/backingScale/frame/appActive, and Dock model keeps last key window. Explicit tooltip hiding on deactivation remains unsupported by the current public runtime integration as recorded in [W2-TOOLTIP]/WIN-07. |
| P1-WIN-10 | FAIL | PASS | [W2-NSH-FINAL] keyboard test verifies exact `window.open {kind: "project", projectId}` from ⌘⇧N; host cascade successor passes. |

### 5.9 Menus and keys

| Item | M5 | W2 | Evidence and remaining work |
|---|---|---|---|
| P1-MENU-01 | PASS | PASS | Real NSMenu application tree, labels/order/separator/modifiers/tags and frozen command ID tests pass in [W2-RUST]. |
| P1-MENU-02 | PASS | PASS | AppKit standard cut:/copy:/paste:/selectAll: selectors pass; [W2-NSH-FINAL] actual App Copy with no selection leaves clipboard unchanged. |
| P1-MENU-03 | FAIL | PARTIAL | Real View tree, present+disabled deferred items, inert direct dispatch, key-window/app-global routing, no Close Window action, no-window text-size persistence and native equivalents pass. NSH exercises panel/layout/close routing. **Residual:** `newTab`/⌘T is declared and is not deferred, but has no `Sessions.create` handler; therefore the requirement that every window-level command have its effect is incomplete. Global content behavior is out of this host assignment. |
| P1-MENU-04 | FAIL | PASS | Host uniqueness/order/last-key and actual Dock NSMenu/cache/action tests pass. [W2-DOCK] fixes the initial lowercase path-basename post: stored project resolution precedes the first post; name/branch changes deduplicate. [W2-NSH]/modal follow-up observes one project post, and Global posts its own metadata without opening a project session. No live Dock interaction was used. |
| P1-KEYS-01 | FAIL | PASS | Full chord/no-duplicate scan and native app-equivalent comparison pass. `DEFERRED_COMMANDS` and contextual deferred chord table cover acceptance §8; all deferred command dispatch is guarded. Declaration coverage does not imply `newTab` works (MENU-03 residual). |
| P1-KEYS-02 | FAIL | PARTIAL | Pure number/cycle rules and actual fallback-pane key aliases/modal behavior pass. [W2-CTRL-TAB] proves native AppKit dispatch reaches a real NSH keymap exactly once in each direction, replacing the previously absent automated successor. It is deliberately **not** the requested manual fixture action because desktop input was prohibited. Production calls `numberedTab(..., false)`: shared strip membership/order is not installed, so Alpha's session-only shared-strip numbering remains incomplete. |
| P1-KEYS-03 | FAIL | PASS | NSH per-panel chord/command routing, navigation-pane reuse and ⌘⇧N pass; host-only Global/Quit chords are absent from TSX and present on real NSMenu items. Deferred keys remain inert. Terminal/chat contents remain deferred. |
| P1-KEYS-04 | FAIL | PASS | [W2-NSH-FINAL] actual App tests cover close-modal shortcut blocking, 60% backdrop/16 pt padding, modifier dismissal before selection, and contextual Up/Down/Left/Right wrapping inside All tabs without changing the selected session. Enter/Space activates; Escape cancels and restores ordinary Ctrl-Tab dispatch. Shared-strip membership remains KEYS-02 residual; this successor concerns the available pane All tabs overlay. |
| P1-KEYS-05 | FAIL | PASS | Host routing tests and real AppKit `text_size_key_equivalents_dispatch_without_a_window` pass for all three keys; settings mutation works with no windows. Terminal-first-responder behavior is explicitly deferred. |

Final [format, syntax and whitespace checks](../crates/riwork-desktop/verification/w2-host-evidence/final-checks-rson1nf4/manifest.json) pass.

## Reproduction and recorded commands

[phase1_w2_host.py](../crates/riwork-desktop/verification/phase1_w2_host.py) creates private fixtures and records exact commands. The following are reproduction entry points, not replacements for the archived run-specific argv. `build` creates the immutable fixture executable; GUI modes pause at checkpoint files for read-only explicit-PID Cua inspection. Never substitute a live home or PID.

```sh
ROOT=/tmp/riwork-w2-reproduce
TARGET=/private/tmp/riwork-p1-integration-mkzwamt7/target
python3 crates/riwork-desktop/verification/phase1_w2_host.py build --root "$ROOT" --target-dir "$TARGET"
python3 crates/riwork-desktop/verification/phase1_w2_host.py tests --root "$ROOT" --target-dir "$TARGET"
python3 crates/riwork-desktop/verification/phase1_w2_host.py restore --root "$ROOT" --target-dir "$TARGET"
python3 crates/riwork-desktop/verification/phase1_w2_host.py auto-close --root "$ROOT" --target-dir "$TARGET"
python3 crates/riwork-desktop/verification/phase1_w2_host.py auto-quit --root "$ROOT" --target-dir "$TARGET"
python3 crates/riwork-desktop/verification/phase1_w2_host.py fail-flush --root "$ROOT" --target-dir "$TARGET"
python3 crates/riwork-desktop/verification/phase1_w2_host.py beta --root "$ROOT" --target-dir "$TARGET"
python3 crates/riwork-desktop/verification/phase1_w2_host.py missing --root "$ROOT" --target-dir "$TARGET"
python3 crates/riwork-desktop/verification/phase1_w2_host.py basic --theme native --root "$ROOT" --target-dir "$TARGET"
```

Repeat the final command with each theme in the catalog. The `tests` mode records these exact suite argv:

```sh
cargo test -p riwork-desktop -p riwork-nsh-native -p riwork-nsh-workspace --features riwork-nsh-native/test-support --locked --offline
cargo test -p riwork-engine --features test-support --lib runtime::tests:: --locked --offline
```

[W2-NSH-FINAL] records the pinned CLI type-check and 36-test invocation with its private fixture environment and JUnit output. [W2-HOST] records `cargo test -p riwork-desktop --bin riwork --locked --offline`. [W2-PARITY] records every reference/candidate argv and byte comparison. [W2-CTRL-TAB] and [W2-WATCHDOG] record direct already-built test-binary argv; the latter uses `--exact runtime::tests::<name> --ignored --test-threads=1` with a 60 s outer timeout for each of:

- `a_watchdog_fires_after_its_deadline_and_leaves_the_last_breadcrumb`
- `a_confirmation_in_flight_holds_the_watchdog_off_for_a_bounded_time`
- `a_stalled_filesystem_cannot_hold_the_watchdog_up`
- `a_wedged_replacement_records_where_it_hung_unregisters_and_exits_with_its_group`

Their measured command durations are 0.453 s, 1.253 s, 0.352 s and 0.444 s. The last case re-executes `watchdog_child_entry`, invokes production timeout cleanup and proves exit 75; the test deadline is 400 ms. The production 40 s constant/configuration is reviewed separately, not presented as a 40 s timed test.

## Residual dependencies

Seven items remain PARTIAL:

- **PROC-04 and RLD-01 — external AX observation:** missing notice verification reports `unknown` / `observation_unavailable`; the retained incomplete 10-element tree exposes native chrome/menu, not the notice. Restore AX tab queries return no tabs. Screenshots support the visible result and headless tests support authored roles, but neither proves the prescribed external query. Pinned NSH accessibility mount hooks are unconditional; no omitted public AppView enable/init API or feature gate was found. The precise upstream/integration/observation cause is not established.
- **PROC-05 — D-PROC-1:** single-instance forwarding works, but the existing open output/PID decision remains open. CLI bytes are preserved; this work does not decide the contract.
- **WIN-07 and WIN-09 — tooltip hiding:** overlay/no-NSWindow/counting checks pass. Move/deactivation/project-switch hiding has no public AppView cancellation hook or native lifecycle observer in the current integration. The host emits `window.state` without a controlled tooltip owner consuming it. A runtime hook or app-controlled tooltip layer is still required.
- **MENU-03 — shell creation:** `newTab`/⌘T is declared and not deferred but lacks the native Sessions creation handler.
- **KEYS-02 — shared-strip numbering:** fallback pane-order cycling/numbering works; the shared membership service is absent and production selects the pane-order branch. Automated native Ctrl-Tab passes; no manual desktop input was authorized or performed.

The bounded KEYS-04 All tabs/modal successor passes, and no remaining dependency is hidden in `DEFERRED_COMMANDS`. No full Phase-1 or all-PASS host objective completion is claimed.

[W2-BUILD]: ../crates/riwork-desktop/verification/w2-host-evidence/build-u6ti7cnv/manifest.json
[W2-RUST]: ../crates/riwork-desktop/verification/w2-host-evidence/tests-vo4a6b9s/manifest.json
[W2-HOST]: ../crates/riwork-desktop/verification/w2-host-evidence/review-host-ivw0x2ik/manifest.json
[W2-RESUME]: ../crates/riwork-desktop/verification/w2-host-evidence/review-resume-vsspqlob/manifest.json
[W2-RUNTIME]: ../crates/riwork-desktop/verification/w2-host-evidence/tests-vo4a6b9s/manifest.json
[W2-NSH-FINAL]: ../crates/riwork-desktop/verification/w2-host-evidence/strip-final-b7qqh2by/manifest.json
[W2-NSH]: ../crates/riwork-desktop/verification/w2-host-evidence/strip-final-b7qqh2by/manifest.json
[W2-WATCHDOG]: ../crates/riwork-desktop/verification/w2-host-evidence/watchdog-2t1puqmx/manifest.json
[W2-CTRL-TAB]: ../crates/riwork-desktop/verification/w2-host-evidence/riwork-w2-window-check/control-tab.json
[W2-PARITY]: ../crates/riwork-desktop/verification/w2-host-evidence/final-parity-yf52jnkn/manifest.json
[W2-STARTUP-ERROR]: ../crates/riwork-desktop/verification/w2-host-evidence/startup-error-ouma1czu/manifest.json
[W2-RESTORE]: ../crates/riwork-desktop/verification/w2-host-evidence/restore-l7crkidh/manifest.json
[W2-CLOSE]: ../crates/riwork-desktop/verification/w2-host-evidence/auto-close-ymoo0zv1/manifest.json
[W2-QUIT]: ../crates/riwork-desktop/verification/w2-host-evidence/auto-quit-mgtc93ks/manifest.json
[W2-FAIL-FLUSH]: ../crates/riwork-desktop/verification/w2-host-evidence/fail-flush-at7__xw8/manifest.json
[W2-BETA]: ../crates/riwork-desktop/verification/w2-host-evidence/beta-xb5rbw78/manifest.json
[W2-MISSING]: ../crates/riwork-desktop/verification/w2-host-evidence/missing-1qyn7drg/manifest.json
[W2-TOOLTIP]: ../crates/riwork-desktop/verification/w2-host-evidence/tooltip-review-amucwz7c/manifest.json
[W2-AX-REVIEW]: ../crates/riwork-desktop/verification/w2-host-evidence/access-review-1cwi6ejk/manifest.json
[W2-BASIC-THEMES]: ../crates/riwork-desktop/verification/w2-host-evidence/basic-558dn1yi/manifest.json
[W2-DOCK]: ../crates/riwork-desktop/verification/w2-host-evidence/dock-2q5az9yi/manifest.json
