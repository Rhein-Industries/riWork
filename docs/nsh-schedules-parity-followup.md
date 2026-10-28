# Schedules parity follow-up — 42978307

Task `42978307-a1b4-4726-8dbf-bab9ec4fc2a2`, branch
`nsh-spec-terminal-layout`, base `162444b76d7f8496293cbe3a2a4e48a8b47b29e0`.
The requested merge was a fast-forward from `9dcf245`; accepted fixes and prior
evidence were retained. This document adds evidence; earlier reports, receipts,
results, logs and STOPPED records are not rewritten.

**Verdict:** fresh ordinary project-chat dispatch is fixture-proved through the
native adapter and unchanged schedule engine, including Codex, Claude and Muse.
Two rejected-save focus defects are corrected. **PANEL-113 remains OPEN:** pinned
NSH cannot programmatically focus action controls or reproduce GPUI's atomic
native IME traversal gate. No custom traversal interface is invented here.
The umbrella/cutover remain open/held. Remote task `2cb2bb2b` remains staged todo
and was not started; it is not a dependency of this work.

`verification_environment`: private HOME/RIWORK_HOME, disposable ledgers and
project roots, in-process fake providers and private Unix sockets, windowless
NSH fixtures; offline Cargo; sandbox-exec denies IP networking. Builds are
serialized, at most two Cargo jobs, reusing only this worktree's private target
cache `/tmp/riwork-p6b-fa930048.vn_exbl9/target`. No GUI (including AppKit shell
acceptance), Docker, production schedule/chat/shell, real provider/account/
credential, system service/session mutation, network, installation or reload.

`live_targets_touched`: none beyond own shell/worktree and disposable fixture
processes. In-process fake chat state/log mutations are the explicitly requested
dispatch fixture, not a live chat service. No merge to `nsh` or push.

`effective_service_tier`: Service tier default, Fast OFF, user-verified;
GPT-6.1-Sol ultra, effort and saved `dominik@rhein.industries` account unchanged.
One inherited-configuration descendant performed read-only source review; no
descendant built, tested, edited, operated a GUI or changed configuration.

Only Native control behavior is targeted. The other six ThemeChoice migration
styles remain dropped by user decision; their source is not deleted. The
interaction fixture selects the Native control/typography branch; these tests
make no screenshot, color or visual-equivalence claim.

## Fresh-chat proof design and outcomes

`crates/riwork-nsh-native/src/schedules/fresh_dispatch_tests.rs` creates each
schedule with the real `EngineSchedules::create(request, true)`, whose existing
implementation resolves project scope/options, calls `Target::bind_new_chat`
and saves the ordinary ledger (`schedules.rs:494`). It does not construct an
existing orchestrator-chat target. Delivery invokes the **public**
`ScheduleStore::tick_tracked`, reopening the same disposable home each time.
The engine dispatches fresh targets to `schedule_chat::deliver_new`
(`crates/riwork-engine/src/schedules.rs:743`). Engine/CLI/protocol source is
unchanged relative to `162444b`.

A Unix socket observer sits between that scheduler and the actual engine
`TestHost`, whose provider/config/key-store seams are fake
(`crates/riwork-engine/src/chat/testing.rs:367`, `:408`, `:484`). The actual
host's socket is temporarily renamed within its own disposable home. A retained
listener at the ordinary socket handles `system_ensure`'s connect-only probe,
so its launch fallback cannot be entered while a fixture runs
(`crates/riwork-engine/src/orchestrators.rs:89`). No installed CLI, provider
process, external socket or credential is used. The observer is removed and the
host's socket restored before that fake host is dropped.

For each actual wire request the observer reads the ledger **before** forwarding
it. At Create and Send the assertions require a persisted `dispatching` run,
the same preassigned created-chat id as the request, consumed
`new-chat:<id>` token, one attempt and advanced `next_run`
(`fresh_dispatch_tests.rs:252`). These observations exercise the engine's real
claim write (`crates/riwork-engine/src/schedules.rs:853–903`), not a mocked claim
closure. The positive control also requires the fake provider to receive Send
and the actual host's event log to contain the exact inert user prompt. The
unchanged engine checks capability before claiming and sends only after
identified creation/identity/readiness checks
(`crates/riwork-engine/src/schedule_chat.rs:188–370`).

| Fixture | Observed outcome / negative control |
|---|---|
| `p6b_fresh_01` (Codex, Claude, Muse) | Capability → durable claim → one identified Create → one Send → `submitted`. Ordinary project chat, no worktree/orchestrator; canonical project root, provider, model, effort, permission and account binding preserved; Fast false. Muse contributor mode Off. Exact prompt appears in the real host log. Repeated/reopened ticks produce no additional Create/Send and leave the receipt unchanged. |
| `p6b_fresh_02` | Explicit capability denial defers twice without a created id, consumed token, attempt, Create or Send. Due time stays pending; after the unchanged 300-second grace it becomes `missed`, still without creating a chat. |
| `p6b_fresh_03` | Four barrier-synchronized schedulers contend for the same occurrence/ledger. All return; only one capability/Create/Send and one durable claim occur. |
| `p6b_fresh_04` | A disposable ledger already at four attempts in its current window permits the capability probe but denies the claim. `deferred`, no created id/token/Create/Send. The existing counter is not incremented. |
| `p6b_fresh_05`, Create refused | `failed`, paused/review required, preassigned id retained; no actual chat or Send; later ticks do not retry. |
| `p6b_fresh_05`, Create reply lost | Actual fake-host Create occurs, its reply is dropped; `uncertain`, created id retained, paused/review required, no Send/retry. |
| `p6b_fresh_05`, Send refused | Actual chat exists; explicit Send refusal produces `failed`, zero fake-provider Sends, paused/review required, no retry. |
| `p6b_fresh_05`, Send reply lost | Actual Send is forwarded and the fake provider receives it once, then the reply is dropped. `uncertain`, paused/review required; later ticks never replay it even though an effect occurred. |
| `p6b_fresh_06` | Actual fake provider startup fails after Create. Recorded chat is Failed; schedule fails/pauses with its created id; no prompt and no automatic second Create. |

Six tests cover eleven parameterized scenarios. The `fresh-audit-final` raw log and
extracted [wire audits](verification/nsh-schedules-parity-42978307/fresh-wire-audits.json)
retain actual requests, pre-request disk claims and final native DTOs.
This proves the previously missing fresh-chat path, including PANEL-108's Muse
offering: `P6B-SCHED-PARITY-03` independently selects Muse in the actual screen
and requires a fresh-chat request with Fast false and no Codex account.
DOCS-203's historical Codex/Claude-only description is not complete fresh-chat
parity; that prior matrix/archive is preserved rather than silently edited.
Real providers, completion of an agent's work and live dispatch are not claimed.

| Parity entry | Status in this follow-up |
|---|---|
| PANEL-114 fresh-chat dispatch proof | Covered in isolated fixtures, including durable claims, contention and failure/uncertain controls. |
| PANEL-108 Muse offering for fresh chats | Covered by screen selection and actual fake-host dispatch. |
| PANEL-113 exact custom traversal / IME | OPEN; two rejected-save input-focus defects fixed, full cycle and native composition blocked as detailed below. |
| Non-native ThemeChoice migration | Dropped by user decision; no implementation or verification claim. |

The first socket run failed before dispatch because macOS rejects setting a
read timeout on a connect-only peer that already closed (EINVAL). The fixture
now handles exactly that case, as the existing engine socket fixture does
(`crates/riwork-engine/src/schedule_chat/fresh_tests.rs:38–41`). No expectation,
engine timeout, claim, grace, rate, uncertain or preflight rule was weakened.
The original failed run is retained with subsequent passing runs.

## Exact traversal and focus: concrete upstream blocker

GPUI's owner cycles its complete control list with wrap in both directions,
only when not pending (`crates/riwork-desktop/src/schedule_panel.rs:495–545`).
The Native list is:

1. New; then, while editing, fresh/existing destinations, Title, Prompt.
2. Fresh: Codex/Claude/Muse, Model, Effort, Supervised/Auto edit/Full/Plan, Fast.
   Existing: App/Project/Workspace, visible worktrees for Workspace, matching
   live targets and applicable legacy chat targets.
3. First-run 10/30/60 minutes/1 day, Exact; date only when Exact is shown.
4. Once/hourly/daily/weekly/custom repeat; minutes only for custom; Save, Cancel.
5. Each schedule's Edit, Pause/Resume, Delete and optional created-chat action;
   then project-chat actions; wrap back to New.

List construction evidence: `schedule_panel.rs:991–1033`, `:1101–1165`,
`:1204–1384`, `:1525–1585`. Rendering preserves active control identity
(`:965–982`, `:1604–1607`). The following transitions are part of the parity
requirement, not assertions that default NSH traversal implements them:

| Transition | GPUI source / current evidence |
|---|---|
| New/Edit → Title; Cancel/Escape → New, reset delete confirmation | `schedule_panel.rs:527–542`, `:585–590`, `:700–718`. Opening an input is supported; exact action-control return/cycle remains blocked. |
| Destination/provider/scope/worktree/target changes retain invoking control | `:557–608`, `:714–718`; active identity retained through refresh/re-render. Full action focus transitions remain OPEN. |
| Quick first-run, Exact and Repeat retain their action focus as conditional fields appear | `:609–622`, `:707–718`. Both forward/backward conditional traversal and full edit-state control sets remain OPEN. |
| Validation and async save rejection preserve current focus | `:791–796`, `:841–864`. **Corrected and headlessly proved** by PARITY-01/02: invalid minutes and delayed revision conflict retain their input, draft and typed continuation. Both were observed red before the UI change. |
| Successful save conditionally transfers a focused editor input to the panel | `:843–858`; exact NSH panel/action-focus behavior remains OPEN. |
| Pending suppresses traversal; composition keeps native ownership | `:504–513`; full native IME/pending traversal remains OPEN. |
| Pause/delete/refresh | `:678–684`, `:721–732`; no unconditional focus move. Exact controls and wrap remain OPEN. |

The pinned APIs cannot implement that complete cycle:

* `nsh-js-0.3.0/src/dom.rs:330–344` documents `YNode.focus/blur` for Input.
  `nsh-core-0.3.0/src/ops.rs:1441–1442` also defines Focus for an input.
  macOS forwards the command to `text_input::focus`
  (`nsh-platform-macos-0.3.0/src/mount.rs:407–410`), which does nothing without
  `view.state().input` (`src/text_input.rs:1903–1909`). Headless applies it only
  when `input(id).is_some()` (`nsh-testing-0.3.0/src/screen.rs:536`).
* `P6B-SCHED-PARITY-04` is a maintained runtime negative control: after requesting
  Button, Radio and focusable View ref focus, typing still edits the original
  Input (`axxx`). The same call on another Input moves typing there (`y`).
  Recorded Focus commands alone would not prove actual action-control focus.
* Tab can be bound, but the JS callback receives a keystroke after native
  matching/consumption, not a synchronous cancellable event
  (`nsh-js-0.3.0/src/keymap.rs:240–263`, `:279–319`). Native matching reads
  editing/repeat, not marked-text state
  (`nsh-platform-macos-0.3.0/src/keymap.rs:101–140`). Default input Tab has no
  marked-text gate (`src/text_input.rs:713–724`); blur ends composition
  (`:506–520`). Async `CompositionChange` is not the missing atomic gate.
* GPUI explicitly checks native marked text before owner Escape/Tab
  (`schedule_panel.rs:504–509`; `form_input.rs:57–60`). Its input consumes marked
  Indent/Outdent without changing text/range/focus and Escape unmarks/consumes
  (`crates/riwork-desktop/src/text_input.rs:97–112`, `:131–147`). Existing proof
  `behavior_controls/tests.rs:672–746` was read, not run through a GUI.
  NSH's headless driver has no native composition command (`nsh-testing-0.3.0/
  src/driver.rs:19–87`); default headless Tab uses global stops
  (`src/headless/keyboard.rs:28–50`, `:91–101`). Synthetic composing flags
  therefore cannot establish real marked-text parity.

These registry sources are from
`/Users/dominik/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`.
No accepted override supplies the capability: `.cargo/config.toml:3–4` patches
only nsh-base; its Button changes forward Focus/Blur events, and Radio exposes
Change (`apps/riwork/vendor/nsh-base/src/button.rs:30–41`, `:61–62`;
`radio.rs:30–40`). The task needs an upstream/approved native-host extension for
action focus and atomic scoped marked-text-aware traversal. None is present;
no invented method, default-traversal substitute or native IME success is claimed.
Pinned source hashes and excerpts are retained in the new evidence directory.

## Scoped diff and verification

Existing-file edits are only:

* `apps/riwork/schedules/Automations.tsx:84`: preserve focus on rejected saves;
  existing successful-save behavior remains subject to the blocker above.
* `apps/riwork/schedules/Automations.fixture.tsx:62`: select Native control style
  and typography for injected in-memory interactions.
* `crates/riwork-nsh-native/src/schedules.rs`: cfg(test)-only registration of the
  new `fresh_dispatch_tests` module; no production adapter behavior changed.

New files are the fresh dispatch tests, `schedules/Parity.test.tsx`,
`schedules/TraversalCapability.fixture.tsx`, this report and the append-only
`docs/verification/nsh-schedules-parity-42978307/` evidence directory. Engines,
protocol, host/registry/Workspace wiring and remote implementation are unchanged.

Exact commands, results, raw logs and prior-evidence hash checks accompany this
report in the new evidence directory. The original engine spawn-replacement
failure/cause limitations and all older evidence remain unchanged; they are not
reclassified by any same-tree rerun in this task.

| Command label | Result |
|---|---|
| `fresh-compile` | Exit 0; new fixture compiled with offline/locked Cargo. |
| `fresh-proof-first` | Exit 101; all six fixtures hit the observer's closed-probe macOS timeout setup error before the intended exchange. Retained, fixture corrected as described above. |
| `fresh-proof-socket-fixed`, `fresh-audit`, `fresh-audit-final` | Exit 0; all 6 new tests / 11 scenarios passed. Final audit retains actual wire/claim observations after test-only cleanup was hardened to join every contender before unwrapping its result. |
| `schedules-ui-red` | Exit 1; both focus-preservation tests failed before correction; Muse offering and API-blocker reproducer passed. Type/native check passed. |
| `schedules-ui-green` | Exit 0; native/TypeScript check and all **15 schedules screen/model tests** passed: 4 existing screen + 7 model + 4 new parity regressions. |
| `native-workspace` | Exit 0; **220 native + 172 workspace tests passed**, plus one nested settings-writer invocation. 4 native tests remain ordinarily ignored; no ignored tests were enabled here. |
| `desktop-headless` | Exit 0; **69 headless desktop unit tests passed**, 1 ignored. AppKit shell/GUI acceptance was not run. |
| `engine-fresh-existing` | Exit 0; **10 existing fresh-chat engine tests passed**, 1,332 unit tests filtered out. This is not a new full engine-suite result. |
| `fmt-check`, `fmt-final`, `polling`, `whitespace`, `final-source-whitespace` | Exit 0; workspace format check, no TSX polling and whitespace checks passed. |
| `unchanged-engine-and-remote` | Exit 0, empty diff against `162444b` for core/engine/CLI/protocol, remote UI/native helper and host wiring. Source equality is the claim. |
| `prior-evidence-check` | Exit 0; all **1,912** recorded prior files byte-identical after merge/build/test work, including first-stage archives and prior STOPPED/results/log records. |
| `staged-final` | Exit 0; staged source, report and evidence passed `git diff --cached --check`. Final scope/hash receipts are appended separately below. |

New evidence: [commands/results](verification/nsh-schedules-parity-42978307/commands.jsonl),
[environment and log hashes](verification/nsh-schedules-parity-42978307/results.json),
[final staging receipts](verification/nsh-schedules-parity-42978307/final-checks.jsonl),
[prior-evidence check](verification/nsh-schedules-parity-42978307/prior-evidence-result.json),
[pinned source hashes](verification/nsh-schedules-parity-42978307/sources.json).
Raw captures are lossless `.gz` files with hashes of the original decoded bytes;
failures are retained alongside passing runs. Existing evidence directories are
untouched. The scratch root is `/tmp/riwork-schedules-42978307.ewa5a_9y`.

The performed substantive checks were (the receipt retains expanded argv,
timings, results and child seed/check/test commands; use new roots/labels when
rerunning):

```sh
R=/tmp/riwork-schedules-42978307.ewa5a_9y
C=/Users/dominik/.cargo/bin/cargo
T=/tmp/riwork-p6b-fa930048.vn_exbl9/target
N=/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh
TS=/Users/dominik/Library/Caches/nsh/typescript-7.0.2-darwin-arm64/package/lib/tsc
python3 "$R/run.py" fresh-audit-final "$C" test --locked --offline --jobs 2 -p riwork-nsh-native --features test-support p6b_fresh -- --nocapture --test-threads=1
python3 "$R/run.py" schedules-ui-red python3 crates/riwork-desktop/verification/phase6b_panels.py --root "$R/ui-red" --target-dir "$T" --nsh "$N" --tsc "$TS" --command-log "$R/commands.jsonl" --filter P6B-SCHED-PARITY
python3 "$R/run.py" schedules-ui-green python3 crates/riwork-desktop/verification/phase6b_panels.py --root "$R/ui-green" --target-dir "$T" --nsh "$N" --tsc "$TS" --command-log "$R/commands.jsonl" --filter P6B-SCHED
python3 "$R/run.py" native-workspace "$C" test --locked --offline --jobs 2 -p riwork-nsh-native -p riwork-nsh-workspace --features riwork-nsh-native/test-support -- --test-threads=1
python3 "$R/run.py" desktop-headless "$C" test --locked --offline --jobs 2 -p riwork-desktop --bin riwork -- --test-threads=1
python3 "$R/run.py" engine-fresh-existing "$C" test --locked --offline --jobs 2 -p riwork-engine schedule_chat::fresh_tests -- --nocapture --test-threads=1
python3 "$R/run.py" fmt-check "$C" fmt --all -- --check
python3 "$R/run.py" polling sh apps/riwork/check-polling.sh
python3 "$R/run.py" whitespace git diff --check
python3 "$R/run.py" unchanged-engine-and-remote git diff --exit-code 162444b76d7f8496293cbe3a2a4e48a8b47b29e0 -- crates/riwork-core crates/riwork-engine crates/riwork-cli remote apps/riwork/remote crates/riwork-nsh-native/src/remote.rs crates/riwork-desktop/src/nsh_host.rs
python3 "$R/run.py" prior-evidence-check python3 "$R/check_prior_evidence.py"
```
