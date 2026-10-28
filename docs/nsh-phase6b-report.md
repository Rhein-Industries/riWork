# Phase 6b — Automations and paired Macs

Assignment `fa930048-78c0-4ca5-b44b-86d7de598b68`, slice of `6ef6a636`.
Branch: `nsh-spec-terminal-layout`. Base: `4df3822dee9f4018be1670823ad49c2987cd6579`.
The requested merge from `nsh` was a conflict-free fast-forward. This report does
not close the umbrella task or the open parity entries below.

`verification_environment`: private HOME and RIWORK_HOME, disposable stores,
windowless NSH engines and non-visible AppKit test objects; offline Cargo;
dedicated `/tmp/riwork-p6b-fa930048.vn_exbl9/target`; serialized builds, at most
two Cargo jobs. IP networking denied by sandbox-exec. The exactly-once fixture
uses the existing in-process fake chat host through its private Unix socket.

`live_targets_touched`: none beyond own shell/worktree and disposable test
processes. No production schedule store, pairing, credential, host, application
window, network, installation, reload, push, or merge to `nsh` was used.

`effective_service_tier`: Service tier default, Fast OFF, user re-verified;
gpt-6.1-sol, effort and saved `dominik@rhein.industries` account unchanged.
Earlier rounds preserved lane outputs after two subagent capacity failures;
Continuation 2 used the main session only, with no descendants or model changes.

## Result and boundaries

The new Automations screen exposes create/edit/pause/resume/delete, explicit
scope/target selection, pinned chat options, elapsed/exact first runs, recurrence,
runtime errors and created/earlier chat links. Native schema 1 projects the
existing engine ledger; no engine or CLI persistence/dispatch contract changed.
The host starts one native ticker before AppViews, with the existing
`ScheduleStore::tick_tracked` and process watcher. Subscriptions do not start
dispatch; fixture/test-support builds disable the process dispatcher.

The new Remote screens expose paired-host status, two-step removal, add/pair
prompts, project creation, cached project folders and project lists, chat/dead
shell refusals, local-only unavailable surfaces, and remote terminal chrome.
The new bridge delegates to the existing RemoteTree/RemoteCli/Daemons and cache.
Test-support uses a closed injected backend and cannot discover or invoke a
remote CLI. Fake pairing links are inert literals, never real credentials.

The approved narrow dispatch accepts the engine remote-project namespace at
workspace/host validation points. Remote layout data has no local root, shell
inventory or shared tab inventory; display preferences remain local Settings.
Remote splits create empty panes. Remote shell placement uses the existing
SavedTab host/shell identity and deduplicates it. Local creation, adoption,
membership and exit paths are guarded in remote projects. Existing W2
prepare/reserve/commit and host-layout ownership remain in place.

The remote prompt now thaws the layout on cancellation and successful closure.
Remote Settings/Projects lists have bounded scroll regions. Window, Dock and
status-bar labels use the same remote project/host attribution. Stable computed
project/remote identities preserve retained local terminal nodes across layout
deliveries. Automation inputs retain their nodes while their values change;
Escape is panel-scoped, composition-aware and disabled during a pending save.

## Parity evidence

“Fixture covered” describes isolated evidence, not live-host acceptance or a
claim that an entire grouped row is closed. The source matrix remains
[nsh-parity.md](nsh-parity.md); native payloads are added in
[nsh-contracts.md](nsh-contracts.md), §§1.8 and 1.10.

| Rows | Coverage and evidence |
|---|---|
| PANEL-104–112, PANEL-115–116; DOCS-200–204, DOCS-207 | Automation model/screen and native adapter implemented. `apps/riwork/schedules/{model.ts,Automations.tsx}`; `crates/riwork-nsh-native/src/schedules.rs`. Tests `P6B-SCHED-01..07`, `P6B-SCHED-UI-01..04`, native `p6b_sched_01..05,08,09`: defaults, scope/type/target validation, offset/cadence, custom repeat zero refusal, revision CRUD, pinned identity/options, draft retention, pending actions and result-chat routing. Menu/chord keeps saved id `schedules`; existing keyboard screen suite also runs. |
| PANEL-113 | Cmd+S, Escape, pending boundaries and draft retention have screen evidence. Exact custom Tab/Shift+Tab traversal, all focus transitions and IME traversal are **OPEN**; current controls use NSH traversal. |
| PANEL-114; DOCS-205–206 | Process ticker and visibility-owned subscription implemented; changed-data equality ignores idle time. Native `p6b_sched_06` calls the real public engine tick twice and after reopening a disposable store: exactly one fake-provider Send, submitted outcome, no next run. `p6b_sched_07` proves an empty tick does not create a ledger. Engine grace/rate/claim/uncertain/preflight behavior is unchanged. Fresh-chat capability/claim/Create/Send end-to-end behavior is not newly proved by this bridge fixture; the exactly-once fixture targets an existing fake orchestrator chat. |
| PANEL-061–062, PANEL-177–178; SHELL-191, SHELL-199; TERM-111, TERM-114–116; DOCS-196–197 | Host/project/list/prompt presentation implemented in `apps/riwork/remote/Remote.tsx` and native `remote.rs`. `P6B-REMOTE-UI-01..05` covers pending removal, masked validation, fake pairing/defaults/copy, stale projects, refusal, strip actions, prompt thaw and subscription stability. Native remote tests cover unobserved silence, shared wants, cache/selection, removed/stale hosts, masked secrets and uncertain single-flight creation. Engine intervals and request/protocol/host-registry handling are reused; invite expiry is not exercised against a host. Remote project sort preference remains **OPEN** below. |
| PANEL-063; SHELL-190; TERM-109 | `P6B-REMOTE-ROUTING-01` opens all five unavailable panels before local editors can mount. Model test checks host-attributed local-only refusal and handoff copy. Workspace guards Vim/subshell actions. Native remote fixtures refuse local APIs and preserve local store/session/membership bytes. Full handoff integration and complete remote status-bar parity remain **OPEN**. |
| SHELL-189, SHELL-196–198; TERM-098–103 | Narrow remote project/open/create/worktree/orchestrator routing implemented. `remote/workspace.test.ts` covers captured destination pane, one request, switch-away-and-back/close stale results, no automatic retry, exact host-tab selection and chat/dead refusal. Native `remote::tests` covers known worktree vs project fallback and pending creation. `apps/riwork/src/remote_tests.rs` proves local→remote→local layout round trips, remote pair deduplication, opaque navigation IDs, empty splits, local guards and unchanged local inventories. Full shortcut/orchestrator scope and host behavior remain **OPEN**. |
| SHELL-192–195; TERM-090–097, TERM-104–108, TERM-112–113; DOCS-195 | Presentation/dispatch only. `RemoteTerminal.tsx` sends the existing native remote target, preserves ended state and invokes same-target reattach/focus; it installs no local file link/drop actions. `P6B-REMOTE-MODEL-03` and fake UI-04 cover strip text/actions, and core layout fixtures preserve SavedTab::RemoteShell identity. Real bridge attach/teardown/transport, title refresh for every carried tab, reconnection, palette transitions and timing remain **OPEN**. |
| TERM-110 | Existing cache selection API and explicit runtime/remote layout restore are fixture-backed. `crates/riwork-desktop/src/nsh_remote_tests.rs` checks prepare/restored keys without reading a corrupted local Store. Cold launch from `remote_cache.selected`, folder-launch clearing, real switching/restoration and cross-window interaction remain **OPEN**. |

Additional open scope: remote folders currently use the unchanged engine default
project order. Honoring every window's selected date/live/name order requires a
broader snapshot projection; it was not added here. Remote connection/status
presentation is fixture-backed, but no live transport or host claim is made.
The original UI offers Muse for fresh chats, as PANEL-108 records; DOCS-203’s
Codex/Claude-only wording is not treated as complete fresh-chat parity.
Visual equivalence, complete remote keyboard behavior and exact custom traversal
are not inferred from type checking or fake interaction tests.

## Every shared-file edit

| Existing file | Change |
|---|---|
| `Cargo.lock` | Add already-locked `chrono` to native crate dependency membership; no version changes. |
| `crates/riwork-nsh-native/Cargo.toml` | Direct chrono dependency for explicit-offset validation/local display. |
| `crates/riwork-nsh-native/src/lib.rs` | Add Remote/Schedules modules and registry entries; update expected module list. |
| `crates/riwork-nsh-native/tests/services.rs` | Test-only settling of startup filesystem notifications before measuring a full presence interval. Still requires presence to advance and session/tab sample counts to remain unchanged during that interval; no service behavior changed. |
| `apps/riwork/Workspace.tsx` | Add panel/remote-terminal wiring, visible wants, prompt integration, narrow remote actions/guards, coherent labels and stable project/remote computed values. Settings editor remains the existing sibling; no 6a editor or 6c preview/Cua implementation. |
| `apps/riwork/Panels.tsx` | Local Store deliveries do not clear remote selections; reset can retain destination navigation; avoid a second caller reset after Workspace already switched. |
| `apps/riwork/commands.ts` | Optional remote-only agent shortcut hook; local deferred commands retain their behavior. |
| `apps/riwork/src/lib.rs` | Remote validation, SavedTab pair placement, remote navigation ids/snapshot flag, empty split and local-operation guards. |
| `apps/riwork/src/session_io.rs` | Dispatch remote loads to the new helper and refuse local creation/membership paths. |
| `crates/riwork-desktop/src/nsh_host.rs` | Remote prepare/restore branches and test module; host-lifetime schedules start/observer hook before AppViews. Quit-gate/flush/recovery ordering is untouched. |
| `docs/nsh-contracts.md` | Add the new schema-1 payload/function tables and remote layout entry point. |

All other implementation/test additions are new files under
`apps/riwork/{remote,schedules}`, `apps/riwork/src/{remote.rs,remote_tests.rs}`,
`crates/riwork-nsh-native/src/{remote.rs,schedules.rs,schedules/tests.rs}`,
`crates/riwork-desktop/src/nsh_remote_tests.rs`, and
`crates/riwork-desktop/verification/phase6b_panels.py`. The latter seeds private
stores, enables closed native test-support in the generated runner, caps Cargo
jobs at two, blocks IP networking, records child commands/results and checks that
no tmux files/sockets were created. `phase3b_files.py` was not run.

## Failure diagnoses and verification

The three preserved native failures were test-side errors:

* Registry expected-module list omitted the new Remote and Schedules entries.
* Pair-mask test expected 17 characters; both implementations deliberately expose
  16. The assertion now checks the exact prefix, ellipsis and character count.
* Worktree test injected a reply before RemoteTree had planned/allocated that
  project's lists. It now observes/plans the visible project first and asserts
  that the reply was accepted. Production engine behavior was unchanged.

Initial screen checks also found unsupported key props, missing fixture
capabilities, an editor value-object remount and the new remote gate's layout
object dependency. These were corrected in the new files/minimal Workspace
wiring. Pending tests now inspect disabled controls and use keyboard attempts;
the headless driver correctly refuses a direct press on a disabled button.

The unchanged engine `spawn_replacement_starts_a_detached_child_with_the_restore_environment`
failed in the baseline and exact rerun because sandbox-exec refuses `/bin/ps`
with EPERM, omitting its process-group field. This is the previously proved
environment limitation; it was not chased or patched. The unchanged native
presence watcher test intermittently counted an extra startup event; a clean-base
run and a fresh branch rerun both passed before its fixture changed, while a full
run reproduced the failure. Its test now waits for setup counts to settle, then
requires a fresh presence refresh across a full interval with no additional
session/tab queries. The production service is unchanged. The final suite
outcome is recorded below rather than hiding the earlier failures.

All command arguments, exit codes, timings and raw-log locations are retained in
`/tmp/riwork-p6b-fa930048.vn_exbl9/commands.jsonl` and the committed
[command receipt](verification/nsh-phase6b/commands.jsonl). The pending-open
script's nested build/test commands are also retained in its fixture manifest.

| Command receipt label | Result |
|---|---|
| `workspace-desktop-tests` | Exit 0: desktop 79 passed / 1 ignored; workspace 171 passed / 0 failed. Includes offscreen AppKit and remote prepare/restore/layout fixtures. |
| `native-final-settled` | Exit 0: 125 native unit tests plus all 78 integration tests passed (203 total, plus one child settings-writer fixture invocation); 1 ignored. Includes nine schedule and seven remote unit fixtures. |
| `phase6b-screens-final` → `p6b-nsh-check`, `p6b-nsh-test` | Exit 0: native/TypeScript check passed; **180 screen/model tests passed, 0 failed**, including all 29 new Phase 6b cases and existing mount, drag, memo receipt, chat draft, panel state, global-window and theme regressions. No tmux files/sockets. |
| `pending-open-final` | Exit 0: final bundle build and the ignored `production_app_recovers_pending_open_after_failed_quit_and_restart` regression passed; lost-ready-ack/failed-quit recovery retained. |
| `format-check`, `polling-check`, `diff-check`, `staged-whitespace-final`, `runner-syntax` | Exit 0: cargo fmt across the workspace, no TSX polling, clean whitespace, Python runner syntax. |
| `unchanged-engines` | Exit 0, empty diff against HEAD for `crates/riwork-core`, `crates/riwork-engine`, `crates/riwork-cli`, `remote`. |
| `baseline-core-engine` / `baseline-runtime-rerun` | Core: 292 passed, 20 ignored. Engine: 1,214 passed, 127 ignored, only the disclosed sandbox `/bin/ps` failure; exact rerun reproduced it. No engine patch. |

CLI suite (`cli-suite`) exited 0: **175 passed, 0 failed, 39 ignored**.
The ignored cases include real tmux/host fixtures; they were not enabled under
this assignment. Native’s single ignored case also remains ignored. The pending-open
regression is the one explicitly selected ignored test and ran only headlessly.
[Environment](verification/nsh-phase6b/environment.json),
[final log hashes/results](verification/nsh-phase6b/results.json) and
[pending-open receipt](verification/nsh-phase6b/pending-open.json) accompany the
command receipt. No new full engine run was claimed after its disclosed baseline
sandbox failure; engine/protocol source remained unchanged.

The final commands were invoked as follows; the receipt retains the expanded
absolute tool paths, fixture roots, intermediate failures and timings:

```sh
R=/tmp/riwork-p6b-fa930048.vn_exbl9
python3 "$R/run.py" workspace-desktop-tests /Users/dominik/.cargo/bin/cargo test --locked --offline --jobs 2 -p riwork-nsh-workspace -p riwork-desktop
python3 "$R/run.py" native-final-settled /Users/dominik/.cargo/bin/cargo test --locked --offline --jobs 2 -p riwork-nsh-native --features test-support -- --test-threads=1
python3 "$R/run.py" phase6b-screens-final python3 crates/riwork-desktop/verification/phase6b_panels.py --root "$R/screens-final" --target-dir "$R/target" --nsh /tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh --tsc /Users/dominik/Library/Caches/nsh/typescript-7.0.2-darwin-arm64/package/lib/tsc --command-log "$R/commands.jsonl"
python3 "$R/run.py" pending-open-final python3 crates/riwork-desktop/verification/phase1_pending_open.py ordering --root "$R/pending-open-final" --target-dir "$R/target" --jobs 2 --ghostty-artifacts /Users/dominik/orca/projects/riWork-nsh-p2-terminals/crates/riwork-nsh-terminal/.runtime/pinned/install
python3 "$R/run.py" format-check /Users/dominik/.cargo/bin/cargo fmt --all -- --check
python3 "$R/run.py" polling-check sh apps/riwork/check-polling.sh
python3 "$R/run.py" diff-check git diff --check
python3 "$R/run.py" unchanged-engines git diff --exit-code HEAD -- crates/riwork-core crates/riwork-engine crates/riwork-cli remote
python3 "$R/run.py" runner-syntax python3 -m py_compile crates/riwork-desktop/verification/phase6b_panels.py
python3 "$R/run.py" cli-suite /Users/dominik/.cargo/bin/cargo test --locked --offline --jobs 2 -p riwork-cli
```
