# Phase 6b — Automations and paired Macs

Assignment `fa930048-78c0-4ca5-b44b-86d7de598b68`, slice of `6ef6a636`.
Branch: `nsh-spec-terminal-layout`. Original base: `4df3822dee9f4018be1670823ad49c2987cd6579`.
The original merge was a conflict-free fast-forward; the integration follow-up
below merges `nsh` at `82f5a12215793596d81d420f1045071980ecf9b9`.
This report does not close the umbrella task or the open parity entries below.

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
The integration follow-up also used only the main session, with that same tier,
model, effort and saved account unchanged.

## Integration follow-up on 3fb88b8

Merged `nsh` `82f5a12215793596d81d420f1045071980ecf9b9` into this branch
in `42f2a7dacdf2009f649ed6652fae968589613b54`. Semantic conflicts were limited
to Workspace composition and the native registry. Remote/Schedules coexist with
6a Settings/ProjectSettings and 6c Cua/Previews; the accepted AppSettings,
SettingsFocus, ProjectSettings, CuaSetup and AppPreviewTab implementations have
no Phase 6b edits relative to the merged `nsh`.

RemoteSettings now occupies AppSettings' existing `remote` slot (section 08),
using its page scroll and SettingsFocus scope. It no longer sits beside a
DeferredPanel. Remote's Button/Input/View imports now use the accepted tracked
wrappers. The wrappers operate normally without a SettingsFocus provider for
the other remote surfaces. No schedule engine, remote engine or protocol change
was made in this integration.

The new maintained Rust/QuickJS regression
`delayed_cua_check_preserves_remote_settings_focus` observed **red first**:
with raw controls, all six combinations of Add/Pair/Remove × Cua ready/not ready
issued a competing Focus command on delayed completion. The no-focus controls
correctly focused Cua or the Native theme. After switching the imports, all six
Remote cases emitted no competing Focus command; both pre-existing PANEL-138
tests also passed. These are native mount-command observations with synthetic
focus events, not a live-window focus claim. The JS headless driver's button
focus limitation is why the maintained regression uses the Rust fixture.

`SettingsRouting.test.tsx` tests ordinary ⌘, then host route, and the reverse
route order. Each requires Remote underneath `settings-section-08`, exactly one
Settings tab, and Cua still mounted; the existing ProjectSettings/API-key routes
remain exercised. The existing global singleton command-routing test and native
window-registry tests cover the upstream route that the Workspace consumes.

Every shared-file edit in this follow-up (beyond unchanged files inherited by
the merge):

| File | Integration edit |
|---|---|
| `apps/riwork/Workspace.tsx` | Resolve Remote/Schedules alongside 6a/6c; compose `remote={<RemoteSettings ... />}` in AppSettings, remove the DeferredPanel sibling. Retain stable project/remote computed identities, ProjectSettings, Cua focus control, previews and existing route handlers. |
| `crates/riwork-nsh-native/src/lib.rs` | Resolve the union of Remote/Schedules and 6a/6c module/class registrations and the alphabetical expected-module list. |
| `apps/riwork/remote/Remote.tsx` | Import tracked Button/Input/View from SettingsFocus; clarify slot ownership. |
| `apps/riwork/tests/SettingsCuaFocus.fixture.tsx` | Add a closed in-memory Remote adapter and the actual RemoteSettings slot to the existing combined fixture. No transport/credential/clipboard calls. |
| `apps/riwork/tests/SettingsRouting.test.tsx` | Assert section-08 containment on ordinary and singleton host routes, reuse, and retained Cua/ProjectSettings routes. |
| `crates/riwork-nsh-native/tests/settings_cua_focus.rs` | Add delayed-completion regression for all RemoteSettings controls plus no-focus controls, preserving the two existing focus regressions. |
| `crates/riwork-desktop/tests/support/appkit_shell_macos.rs` | Test-only correction: the inherited menu test expected implemented 6c App preview to remain disabled. Assert row 4 enabled before/after AppKit validation; all genuinely deferred rows still must be disabled. Production menu source is unchanged. |
| `crates/riwork-desktop/verification/phase6b_panels.py` | Add isolated focus-bundle/Rust regression execution, 6a's invented account catalog (no auth files) and an explicit missing-Cua JSON seed mode. Each selected group uses a fresh invocation/store. Preserve private HOME/store, closed backends, IP denial, offline/cache/jobs/lock discipline. |
| `docs/nsh-phase6b-report.md` | Integration evidence, corrected baseline limits, Native-only decision and explicit follow-up tasks. |

Automatic merge results in `apps/riwork/commands.ts` retain both the remote-agent
hook and 6c app-preview command; `apps/riwork/src/lib.rs` retains both remote
dispatch and 6c preview layout support. Those shared files required no further
manual edit. New integration receipts are under
`docs/verification/nsh-phase6b/integration/`; original receipts remain intact.

Intermediate failures remain in the integration command/log receipts. They
include the intended focus red, the stale AppKit menu expectation, a broad
`Settings` filter accidentally including the separately seeded CuaMissing case,
two missing invented-account fixture rows, and rejected test-file/runner-profile
invocations. No isolation was relaxed. An attempted multi-group runner reused a
mutable fake inventory across native processes: Workspace rewrote it to its
compact snake-case projection, while the next native backend requires the full
camel-case DTO. This produced a global-routing fixture failure. That runner
experiment was removed; final groups each start from a newly seeded store.
No global routing or Settings/account production source was changed for these
fixture failures.

Integration reruns (all under the same private/offline/network-denied `run.py`):

| Command label | Result |
|---|---|
| `integration-focus-red` | Exit 101: new remote focus regression failed in six cases; the two existing PANEL-138 regressions passed. Retained raw red log. |
| `integration-ui-p6` | Exit 0: type/native check, compiled combined focus fixture, all **3 native focus regressions** and **68 Phase 6 screen/model tests** passed. Includes ordinary and singleton Settings routes, section-08 containment, 6a ProjectSettings and 6c Cua/preview behavior. |
| `integration-ui-global` | Exit 0: all 10 global command/routing tests passed, including singleton Settings dispatch to the frontmost local project. |
| `integration-ui-memo`, `integration-ui-chat`, `integration-ui-p5` | Exit 0: 6 memo receipt tests, 14 chat model/screen tests (including draft identity), and 25 P5 dashboard/global/usage tests passed. |
| `integration-ui-native`, `integration-ui-mount`, `integration-ui-missing` | Exit 0: Native light first paint, retained native terminal nodes, and missing-Cua Settings startup each passed. Missing Cua used its own status JSON/process. |
| `integration-suites-final` | Exit 0: **87 desktop + 214 native + 172 workspace = 473 tests** passed, plus one nested settings-writer fixture invocation. Five ordinary ignored cases include the 3 focus tests separately selected above. Includes W2/host ownership, remote/schedule fixtures, revision/exactly-once dispatch, and layout persistence. |
| `integration-pending-open` | Exit 0: rebuild of the production App fixture and the selected ignored pending-open/failed-quit/restart recovery regression passed headlessly. Nested commands/logs retained with its manifest. |
| `integration-cli` | Exit 0: 175 passed, 0 failed, 39 ignored. Real tmux/host cases remain ignored. |
| `integration-debug-isolation` | Exit 0: plain-debug Settings auxiliary isolation test and its child passed without the test-support feature. No live API-key service or Orca process. |
| `integration-fmt-check`, `integration-polling`, `integration-diff-check`, `integration-staged-final`, `integration-runner-syntax` | Exit 0: workspace formatting, no TSX polling, clean source/staged whitespace and runner syntax. |
| `integration-accepted-sources` | Exit 0, empty diff against merged `82f5a12` for core/engine/CLI/remote protocol and the accepted SettingsFocus/AppSettings/ProjectSettings/CuaSetup/AppPreviewTab implementations. This verifies unchanged source, not absence of regression by a same-tree rerun. |

The selected UI groups total **126 distinct tests**, all passed. This is scoped
integration coverage, not a claim of a new unfiltered screen-suite run. Each
group ran the native/TypeScript check, used a fresh store, and reported no tmux
files/sockets. The original engine baseline failure is not reclassified as a
pass: its reported/suspected cause and evidence limits remain documented below.

The [integration command receipt](verification/nsh-phase6b/integration/commands.jsonl)
retains expanded argv, timings, every failed attempt and final results;
[results and raw-log hashes](verification/nsh-phase6b/integration/results.json)
link byte-for-byte retained logs, including both original failed engine logs.
The two engine failure logs remain plain files. Other captures are lossless
`.gz` archives; their decoded bytes are checked against the untouched source
logs and hashed. The first staged whitespace check flagged original capture
whitespace only; lossless archival preserves that evidence without editing it.
Original command receipts/results have not been rewritten. The performed UI and
suite commands can be expressed as follows (use new roots/labels to rerun):

```sh
R=/tmp/riwork-p6b-fa930048.vn_exbl9
C=/Users/dominik/.cargo/bin/cargo
N=/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh
T=/Users/dominik/Library/Caches/nsh/typescript-7.0.2-darwin-arm64/package/lib/tsc
python3 "$R/run.py" integration-suites-final "$C" test --locked --offline --jobs 2 -p riwork-nsh-native -p riwork-nsh-workspace -p riwork-desktop --features riwork-nsh-native/test-support -- --test-threads=1
panel_check() {
  label="$1"; filter="$2"; shift 2
  python3 "$R/run.py" "$label" python3 crates/riwork-desktop/verification/phase6b_panels.py --root "$R/$label" --target-dir "$R/target" --nsh "$N" --tsc "$T" --command-log "$R/commands.jsonl" --filter "$filter" "$@"
}
panel_check integration-ui-p6 P6 --focus
panel_check integration-ui-global P1-GLB
panel_check integration-ui-memo P3b
panel_check integration-ui-chat P4
panel_check integration-ui-p5 P5
panel_check integration-ui-native 'P1-THM-04: Native'
panel_check integration-ui-mount 'terminal native nodes'
panel_check integration-ui-missing CuaMissing --cua-missing
python3 "$R/run.py" integration-pending-open python3 crates/riwork-desktop/verification/phase1_pending_open.py ordering --root "$R/integration-pending-open" --target-dir "$R/target" --jobs 2 --ghostty-artifacts /Users/dominik/orca/projects/riWork-nsh-p2-terminals/crates/riwork-nsh-terminal/.runtime/pinned/install
python3 "$R/run.py" integration-cli "$C" test --locked --offline --jobs 2 -p riwork-cli
python3 "$R/run.py" integration-debug-isolation "$C" test --locked --offline --jobs 2 -p riwork-nsh-native --test settings_aux_isolation -- --test-threads=1
python3 "$R/run.py" integration-fmt-check "$C" fmt --all -- --check
python3 "$R/run.py" integration-polling sh apps/riwork/check-polling.sh
python3 "$R/run.py" integration-diff-check git diff --check
python3 "$R/run.py" integration-staged-final git diff --cached --check
python3 "$R/run.py" integration-accepted-sources git diff --exit-code 82f5a12215793596d81d420f1045071980ecf9b9 -- crates/riwork-core crates/riwork-engine crates/riwork-cli remote apps/riwork/SettingsFocus.tsx apps/riwork/settings/AppSettings.tsx apps/riwork/settings/ProjectSettings.tsx apps/riwork/CuaSetup.tsx apps/riwork/AppPreviewTab.tsx
python3 "$R/run.py" integration-runner-syntax python3 -c "import ast,pathlib; ast.parse(pathlib.Path('crates/riwork-desktop/verification/phase6b_panels.py').read_text()); print('runner syntax valid')"
```

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
Remote Settings uses AppSettings' page scroll; Projects has a bounded scroll region. Window, Dock and
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

The user has dropped the six non-Native ThemeChoice styles from migration:
Ghostty, RiWork, Catppuccin, TokyoNight, GruvboxLight and Hermes
(`crates/riwork-core/src/theme.rs:7`).
For theme variants of the surfaces above, **Native remains the target;
non-Native variants are dropped by decision**, rather than open migration
requirements. Existing style code remains. TERM-112's terminal override/config
lifecycle in Native and TERM-113's bridge/protocol behavior remain **OPEN**;
"native config" in TERM-112 does not refer to an abandoned UI style.
The other remote/schedules gaps above stay open under follow-ups `2cb2bb2b` and
`42978307` (todo); this integration does not begin those tasks.

## Every shared-file edit

| Existing file | Change |
|---|---|
| `Cargo.lock` | Add already-locked `chrono` to native crate dependency membership; no version changes. |
| `crates/riwork-nsh-native/Cargo.toml` | Direct chrono dependency for explicit-offset validation/local display. |
| `crates/riwork-nsh-native/src/lib.rs` | Add Remote/Schedules modules and registry entries; update expected module list. |
| `crates/riwork-nsh-native/tests/services.rs` | Test-only settling of startup filesystem notifications before measuring a full presence interval. Still requires presence to advance and session/tab sample counts to remain unchanged during that interval; no service behavior changed. |
| `apps/riwork/Workspace.tsx` | Add panel/remote-terminal wiring, visible wants, prompt integration, narrow remote actions/guards, coherent labels and stable project/remote computed values. Integration composes RemoteSettings through the accepted AppSettings remote slot; retains 6a ProjectSettings and 6c preview/Cua/focus wiring. |
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

## Failure diagnoses and original verification (3fb88b8)

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
failed in the baseline and exact same-tree rerun with a missing process-group
field. A sandbox `/bin/ps` EPERM cause was reported and is suspected; the retained
baseline/rerun logs do not establish that cause. No retained source-equivalent
negative control proves it, and these same-tree reruns do not establish absence
of regression. Original failed logs are preserved byte-for-byte; isolation was
not weakened and the test/source was not patched. The unchanged native
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
| `baseline-core-engine` / `baseline-runtime-rerun` | Core: 292 passed, 20 ignored. Engine: 1,214 passed, 127 ignored, one missing-process-group-field failure; exact same-tree rerun reproduced it. `/bin/ps` sandbox EPERM is reported/suspected, not proved by those logs. No engine patch. |

CLI suite (`cli-suite`) exited 0: **175 passed, 0 failed, 39 ignored**.
The ignored cases include real tmux/host fixtures; they were not enabled under
this assignment. Native’s single ignored case also remains ignored. The pending-open
regression is the one explicitly selected ignored test and ran only headlessly.
[Environment](verification/nsh-phase6b/environment.json),
[final log hashes/results](verification/nsh-phase6b/results.json) and
[pending-open receipt](verification/nsh-phase6b/pending-open.json) accompany the
command receipt. No new full engine run was claimed after its disclosed baseline
failure; engine/protocol source remained unchanged in Phase 6b.

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
