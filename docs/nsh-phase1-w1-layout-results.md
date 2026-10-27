# W1 layout persistence and fixture evidence

Task `71ac437e-38de-4861-807a-bf81e4e44e47`, branch `nsh-p1-fix-workspace`.
Run from `/Users/dominik/orca/projects/riWork-nsh-p1-fix-workspace` on 2026-10-27.
This records the persistence portion of sections 5.8 and the shared fixture generator.
The W1 aggregate report owns final acceptance dispositions, NSH/JUnit results,
Sessions reconciliation, real Headless gestures and host lifecycle integration.

All test homes are explicit TempDirs below `/tmp`. Generator outputs are below
`/private/tmp/riwork-w1-layout-persistence`; no live home, GUI, tmux, installation,
merge, push, or task-status operation was used. No GPUI source or test was deleted.

| Check | Evidence |
|---|---|
| P1-LAY-01 | Named `oracle_l_*` cases cover all 13 fixture variants (L-CARRY has source/destination). Named oracles cover every listed §4.7 test, including eight `preserve_locked` resize cases. The pure mapper checks unchanged projection and exact core pane extents at 1280×800, 1920×1080 and 640×400. |
| P1-LAY-03 | Thirteen `reopen_*_renders_golden_and_writes_nothing` cases assert group membership, ordering, selection, identical layout bytes and identical `modified()` timestamp. Thirteen `edit_*_saves_what_riwork_core_saves` cases compare an actual selection edit against a separate home saved through the core writer, including the untouched sibling project. Raw opaque fields, escapes and unreadable entries have a dedicated regression. |
| P1-LAY-04 | `open_session_on_d_{newer,corrupt,entry}` verify the exact `; using a default layout` suffix and unchanged bytes. Corrupt-file set-aside and refusal to overwrite newer/unreadable state remain covered. |
| P1-LAY-05 | The two named GPUI global-nav migration successors test dashboard/global-chat/live-orchestrator pruning, exact MOVED_NOTICE and saving; remote/preview/dead-orchestrator identities remain. Legacy shell order/selection and sidebar reconstruction are covered. |
| P1-LAY-07 | Worker subchecks pass; full item remains blocked by host restart. The real persistence worker has a debug-only manually advanced clock. `each_trigger_saves_once_after_250_ms` covers 17 snapshot mutations, including split/close/select/move/lock/main/gather/default/resize/panel/worktree/task. Repeated schedule resets the trailing deadline; 249 ms does not write, 250 ms does, and subsequent idle/flush does not. `flush_points_write_synchronously` exercises the barrier for all five labels; `nothing_is_saved_without_an_edit` advances a day without writes. This does not prove an actual restart, whose host route still rejects. |
| P1-LAY-08 | Unsafe IDs and opaque tabs persist at the safe ID on an edit. A safe reload snapshot still migrates opaque data from the original unsafe disk generation. Default Layout relocates opaque tabs to main, preserving raw JSON text and avoiding duplicates over subsequent saves. |
| P1-LAY-09 | Existing outgoing-save/core carry test remains passing; same-project switch is inert and carry failure carries the required notice. Root owns the TSX project-switch and carried component identity route. |
| P1-LAY-10 | Both save modes still delegate to `LayoutStore`; each named edit reloads to equal `ProjectLayout` through core. No alternate production `layouts.json` writer was introduced. |

`LayoutStore` gained additive remapped-save methods and reuses unchanged raw JSON
subtrees while retaining its existing lock and atomic rename. Serialized schemas
and serde field definitions are unchanged. `HostLayout.edit_with_remap` publishes
the project and opaque-tab remapping together. No JSON filesystem work runs in a
TSX mutation or the NSH engine thread.

The feature-gated `p1_fixture_home` writes valid state through Store, LayoutStore,
TabStore, SettingsStore, GlobalWindowStore and RuntimeManager. Legacy/future/damaged
layout literals are copied from the core fixture forms. `p1-layouts` includes the
13 L-* variants, three separately isolated D-* homes, and Alpha/Beta/Gamma/Busy for
the combined NSH suite. Shell/chat IDs in the shared fixtures are deterministic
canonical UUIDs; `fixtures.json.aliases` maps their friendly names. Explicit shared
Move operations fix the declared tab order. `expected` is the rendered migration
expectation, while `saved` retains the pre-migration snapshot. `fixture-project`
remains available for the existing workspace harness.

`p1-restore` uses the acceptance §3.2 fallback: RuntimeManager cannot issue a ticket
without a live requester and trusted replacement launch, so the generator records
its two publicly published project windows in `run/p1-restore-windows.json` and the
manifest. Alpha is windowed at 100,100,1000×700; Beta is maximized and focused. The
saved global state is Chat / Needs you. No synthetic valid-looking restore ticket
is fabricated; host-unit and NSH routes cover the fallback's restore behavior.

Exact commands and outcomes:

| Command | Result |
|---|---|
| `cargo test -p riwork-core --lib layouts:: --offline --locked` | exit 0; 77 passed, 228 filtered. Existing GPUI core persistence/geometry tests remain passing. |
| `cargo test -p riwork-nsh-workspace --test layout_roundtrip --offline` | exit 0; 79 passed. This run also refreshed Cargo.lock for the pinned local `nsh-base` patch. |
| `cargo test -p riwork-nsh-workspace --test layout_roundtrip --offline --locked` | Prior final-feature run: exit 0; 78 passed. The additional safe-reload opaque-ID regression raised this to 79 in the command above. |
| `cargo run -p riwork-nsh-native --features test-support --example p1_fixture_home --offline --locked -- --scenario p1-layouts --out /tmp/riwork-w1-layout-persistence/ordered-layouts/home` | exit 0; final 17 project fixtures and three damaged homes generated, with explicit shared tab order and migrated golden metadata. |
| `cargo run -p riwork-nsh-native --features test-support --example p1_fixture_home --offline --locked -- --scenario p1-restore --out /tmp/riwork-w1-layout-persistence/restore/home` | exit 0; four projects, two RuntimeManager-published restore windows and global state generated. |
| `target/debug/examples/p1_fixture_home --scenario p1-basic --out /tmp/riwork-w1-layout-persistence/basic/home` | exit 0; final basic fixture generated from the binary built by the ordered-layouts command. |
| `target/debug/examples/p1_fixture_home --scenario p1-restore --out /tmp/riwork-w1-layout-persistence/ordered-restore/home` | exit 0; final restore fixture generated from the same binary. |
| `cargo test -p riwork-nsh-native --test layout_session --offline --locked` | exit 0; 18 passed after the bridge review. Covers source-project generations for create/close completion, switch serialization and failed-switch recovery, single-flight initial-shell retry, recovered notices, live chat statuses and worker parent identity. |
| `cargo test -p riwork-nsh-workspace --lib --offline --locked` | exit 0; 29 passed, including no-sleep first-2-second resize coverage, stale-revision/new-epoch handling, and latest-revision delivery after a frozen interaction. |
| `cargo run -p riwork-nsh-native --features test-support --example p1_fixture_home --offline --locked -- --scenario p1-layouts --out /tmp/riwork-w1-layout-persistence/final-layouts/home` | exit 0; final combined fixture includes explicit chat inventory, raw worker parent UUIDs, Busy titles S01–S24 and the original Busy-Sxx aliases. |
| `cargo run -p riwork-nsh-native --features test-support --example p1_fixture_home --offline --locked -- --scenario p1-layouts --out /tmp/riwork-w1-layout-persistence/acceptance-layouts/home` | exit 0; supersedes final-layouts with STRIP-02's exact Alpha chat title `Chat 2` and status `Waiting` in both seeded Session and fake runtime inventory. Stable alias/UUID unchanged. |
| `cargo run -p riwork-nsh-native --features test-support --example p1_fixture_home --offline --locked -- --scenario p1-basic --theme native --out /tmp/riwork-w1-layout-persistence/theme-native/home` | exit 0; optional theme persisted through SettingsStore. The same built CLI was also run with `--theme ghostty`, `ri_work`, `catppuccin`, `tokyo_night`, `gruvbox_light`, and `hermes`, each to `/tmp/riwork-w1-layout-persistence/theme-<value>/home`; each stored the exact serialized theme. Invalid theme refused before creating the home. Omission preserves the prior default. |

Development reruns exposed and corrected a stale unsafe-ID expectation, a concrete
String/PathBuf type-inference typo, a legacy chat-dismissal expectation in the
manual-clock default-layout case, and the unknown-tab retention sentinel in the
golden group assertion. The final 79-test result includes those corrections.

The bridge review found and corrected asynchronous results applying to a different
project after a switch, simultaneous first-shell retries, stale errors after a
successful session delivery, and an older revision replacing a newer queued
snapshot during a drag/menu/dialog. Switches now reject overlapping switches and
edits until completion; failure releases the guard. Session errors are separate
from persistent layout/migration notices. Fixture status hydration includes chats,
and fixture reads share the fake backend's write lock. The final UI run must use
`acceptance-layouts/home` and its `fake-sessions.json`, superseding the earlier homes.

D-COPY-1 remains open. This persistence work does not change the existing close
dialog wording; the W1 aggregate report must retain that explicit flag.

P1-LAY-07 cannot be marked PASS while `nsh_host.rs` rejects `app.restart` with
`RESTART_PENDING` and answers pending CLI reload requests with `fail_reload`.
The P1c Rust snapshot/flush API and host close/quit barriers exist, but the required
RLD-01 end-to-end replacement launch is not wired. `HostRuntime` already exposes
engine-backed `launch_reload` / `reload_ready` / `fail_reload`; enabling the
initiating host state machine belongs to P1b under D-OWN-1. A flush-only hook that
still rejects restart would not close this acceptance item. No host restart
changes were made during this review.
