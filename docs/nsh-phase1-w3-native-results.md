# M5 W3 native, appearance/text and status acceptance

Task `c62ea465-bf1e-4ae9-ad41-0f7b86b2965f`; branch `nsh-p1-fix-native`, base
`ed1b456`. **§5.10: 10 PASS; §5.11: 5 PASS; §5.13: 5 PASS.** This closes the
19 failures assigned to W3 and retains the existing THM-06 pass. The original
M5 reports remain historical evidence; this is the W3 rerun.

`verification_environment = macOS arm64; isolated HOME, RIWORK_HOME, runtime,
config and tmux fixtures; real headless NSH engines; read-only Cua.ai Driver
screenshots of an exact fixture PID; no live RiWork input or live data writes.`

The [machine-readable results](nsh-phase1-w3-native-results.json) contain all
40 exact command argv/cwd/environments, exit codes, stdout/stderr hashes,
archived paths, per-item results and fixture audit. Raw evidence is committed
under [verification/nsh-w3-native](verification/nsh-w3-native/). The runner is
[phase1_w3.py](../crates/riwork-desktop/verification/phase1_w3.py).

## Implementation and boundaries

The registry now exports Store, Sessions, Tabs, Settings, Appearance, Layouts
and P1 Dashboard global-window preferences. Native DTOs use camelCase and the
new Settings/Appearance/global-window payloads carry schema 2, documented in
contracts §1.10. Core/engine persistence serde formats are unchanged.

Store folder writes validate canonical UUIDs and publish through the same
process hub. Sessions create/close adapt SessionManager on the blocking pool;
tests inject fakes and never start a shell. Tabs uses the CLI's exact Update
validators and one project topic per shared home service. Settings and
Appearance use the existing home watcher: no additional watcher, poll, timer
or per-window sampler. Settings text/status changes read the current document
under the core store lock. An explicit choice of a default replaces an unknown
future saved value while unrelated unknown settings remain intact.

W3 applies the assignment's D-THEME-1 proposal: Follow Ghostty uses the RiWork
palette and retains its diagnostic. Terminal colors remain unknown unless the
RiWork override is selected. The app neither parses Ghostty configuration nor calls an appearance
publisher. Presets and Native expose the terminal override choice for the terminal
phase. TSX resolves against each window's current color scheme before painting
the workspace, follows both native subscriptions, installs font/casing with the matching palette
without remounting panels, and uses the supported
`system-ui` family plus SF Mono/Menlo technical accents.

The Settings-owned status bar renders all eleven kinds, including GPUI no-data
forms. Its two anchored menus and persistent notice use event-driven lifecycle
handling. The host pins RIWORK_APPEARANCE, reports installed font families, and
pushes the shown global view to each status menu. Layout edits use atomics
instead of the host's cross-thread mutex. Layouts has an injectable repository
and persistence backend; saves queue through Tokio's non-waiting sender. All
disk work and flush waits remain off the engine thread. Core default placement
moves every known tab and preserves selection. An opaque future tab causes a
reset refusal, preserving its stored pane identity.

| Exported calls | Kind | Thread / work |
|---|---|---|
| Store.snapshot/search/inspectProject/createFolder/renameFolder/moveFolder/removeFolder/moveProjectToFolder | async | Initialization and core Store IO/locks hop to the blocking pool. |
| Sessions.list/create/close | async | SessionManager disk/socket/tmux work stays on the blocking pool. |
| Tabs.update | async | Core Update validation and backend/Hub locks run on the blocking pool. |
| Settings.get/set/changeTextSize/moveItem/setSide/setVisible/setEnabled | async | Preference/backend locks and core SettingsStore IO run on blocking workers. |
| Appearance.get/resolve | async | Background preference snapshot and pure core palette derivation. |
| Dashboard.getGlobalWindow/saveGlobalWindow | async | Validation and core file reads/writes run on the blocking pool. |
| Layouts.openSession | async | Input parsing, repository load, host registration and initial enqueue run on the blocking pool. |
| Store/Sessions/Tabs/Settings/Appearance.subscribe | sync handle | Only owner task/cleanup setup and pure key validation; initialization and Hub registration run asynchronously on blocking workers. |
| Settings.cased/sentenceCase | sync | Pure core string formatting; no IO or cross-thread lock. |
| LayoutSession.dock/applyDock/setExtent/selectPanel/openPanel/selectPane/toggleLock/toggleMain/gather/defaultLayout | sync | Owned model work, atomic edit admission, ArcSwap publication and non-waiting save enqueue; no cross-thread mutex or domain IO. |
| LayoutSession.snapshot/panel | sync | Owned in-memory model traversal/DTO conversion. |
| LayoutSession.flush | async | Clone the host handle, wait for its persistence barrier on the blocking pool. |
| Subscription.close | sync | Abort owner task and release engine cleanup; queue removal is lazy. |

HostLayout disposal defers the persistence destructor, so closing a JS owner
cannot synchronously flush files. The persistence API documents that direct
Rust open/switch/flush/drop calls belong off the engine thread; the production
NSH engine supplies a multithreaded Tokio runtime. Streaming topics and their
16 ms coalescing remain outside these assigned sections; no full POLL-01 or
whole-Phase-1 sign-off is inferred from the quiet guard.

## Verification

All final commands exited **0**. No zero-test filter is counted as acceptance.

| Run | Result | Evidence |
|---|---|---|
| Core library | 278 passed; 20 pre-existing ignored | [core output](verification/nsh-w3-native/rust-2bxefok1/core-tests.stdout) |
| Workspace Rust | 27 unit + 1 fake-backend + 12 persistence tests passed | [workspace output](verification/nsh-w3-native/rust-2bxefok1/workspace-rust-tests.stdout) |
| Native | 28 passed; external-writer child also passed | [native output](verification/nsh-w3-native/rust-2bxefok1/native-tests.stdout) |
| Host unit | 28 passed on final host source | [host output](verification/nsh-w3-native/review-lpfgh996/host-final-tests.stdout) |
| Real AppKit View menu | `view_menu_text_size_items`, 1 passed; no ordered/activated windows | [AppKit output](verification/nsh-w3-native/review-lpfgh996/view-menu-text-size-items.stdout) |
| App `nsh check` / `nsh test` | 33/33 passed | [JUnit](verification/nsh-w3-native/nsh-axtslh5q/nsh.xml), [output](verification/nsh-w3-native/nsh-axtslh5q/nsh-test.stderr) |
| Native probe `nsh check` / `nsh test` | Imports seven modules; two engines, Add folder through native bridge; 1/1 passed | [JUnit](verification/nsh-w3-native/probe-8b6iqfa9/native-probe.xml), [output](verification/nsh-w3-native/probe-8b6iqfa9/native-probe-test.stderr) |
| Formatting, whitespace, source review | All pass; 142 protected GPUI files byte-identical to `ea0f2f5` | [source audit](verification/nsh-w3-native/review-lpfgh996/source-audit.stdout) |

The native guard ran **60 actual seconds** with Store, Sessions, Tabs, Settings
and Appearance subscriptions: after initial snapshots, zero deliveries and
exactly one empty-inventory session sample. Watcher discovery still arms the
allowed liveness fallback and last-session removal disarms it. NSH Idle.test
also advances 60,000 virtual milliseconds with unchanged host calls/posts.

The 24-session-tab fake-backend measurement reports every synchronous layout
entrypoint. Observed medians were 1–82 µs, p90 values 1–84 µs and maxima at most
102 µs, below 1 ms in this sample. These are debug measurements, not a universal
timing guarantee or a new performance threshold. See [raw timings](verification/nsh-w3-native/rust-2bxefok1/sync-call-timings.stdout).

### Assigned acceptance rows

| Item | Result | Executed successor / proof |
|---|---|---|
| THM-01 | PASS | Named seven-choice/unknown-key core round trip; strict native choice update; core-labelled seven-choice UI. |
| THM-02 | PASS | `every_preset_token_is_exact` compares every field and ANSI table against a fixed golden file; moved core palette/contrast/diff tests. |
| THM-03 | PASS | Named core override successors and explicit Appearance terminalOverride tests for presets, Native and Ghostty opt-in. |
| THM-04 | PASS | Actual first Native-light Projects span #1d1d1f; actual pre-Appearance context span #d3e1e6. |
| THM-05 | PASS | Real external-process settings write reaches both subscribers once; both window schemes flip in NSH; forced-env host test and Cua light fixture. |
| THM-06 | PASS | No reachable publisher; existing appearance bytes and mtime unchanged in the real fixture. |
| THM-07 | PASS | `follow_ghostty_without_a_parser_uses_the_riwork_palette_and_keeps_the_error`. |
| TXT-01 | PASS | Core tenth-point/clamp/plain-number successors; two-handle locked text-size race and explicit reset regression. |
| TXT-02 | PASS | Native AppKit menu shortcut route; TSX bigger/smaller/reset effects through Settings. |
| TXT-03 | PASS | Core casing test, actual span sizes, Native/Hermes offsets, matching suppression, system/SF Mono/Menlo selection and spacing. |
| STAT-01 | PASS | Existing core normalization/routing; locked native status edits; UI two-window order/side/visibility/master, rendered span metrics and narrow-width scrolling tests. |
| STAT-02 | PASS | Project/worktree opening, live snapshot count, disabled empty session ID and GPUI no-data forms. |
| STAT-03 | PASS | Layout toggle/default/gather/summary/hide tests; core default placement preserves tabs and high pane IDs. |
| STAT-04 | PASS | One replacement notice, no auto-dismiss over a virtual minute, dismiss/Escape and focus return. |
| STAT-05 | PASS | Dashboard/Chat window.open payloads, shown-view checks, menu replacement and escape/outside/hide/modal lifecycle. |
| NAT-01 | PASS | Actual native `nsh check` imports all seven P1 modules; generated declarations retained; no persistence format changes. |
| NAT-02 | PASS | Frozen-code regression, blocking-backend review, atomic close/edit race, queued flush and synchronous timing sample. |
| NAT-03 | PASS | Production/fake backend seams including Layouts and Dashboard; fixed home/nsh AppOptions; no native storage. |
| NAT-04 | PASS | Exact `tests/app/App.test.tsx` route: first snapshots in two engines; bridge Add folder updates both. |
| NAT-05 | PASS | `tabs_update_rejects_invalid_keys`, `store_rejects_non_uuid_ids`; invalid input rejected before backend work/writes. |

### Reproduction commands

From this worktree, with the existing pinned NSH 0.3.0 CLI and TypeScript paths
(overridable with `--nsh` / `--tsc`):

```sh
python3 crates/riwork-desktop/verification/phase1_w3.py rust --root /tmp/riwork-w3-c62ea465 --target-dir /private/tmp/riwork-w3-c62ea465/target
python3 crates/riwork-desktop/verification/phase1_w3.py nsh --root /tmp/riwork-w3-c62ea465 --target-dir /private/tmp/riwork-w3-c62ea465/target
python3 crates/riwork-desktop/verification/phase1_w3.py probe --root /tmp/riwork-w3-c62ea465 --target-dir /private/tmp/riwork-w3-c62ea465/target
python3 crates/riwork-desktop/verification/phase1_w3.py review --root /tmp/riwork-w3-c62ea465 --target-dir /private/tmp/riwork-w3-c62ea465/target
python3 crates/riwork-desktop/verification/phase1_w3.py fixture --root /tmp/riwork-w3-c62ea465 --target-dir /private/tmp/riwork-w3-c62ea465/target
```

Each command makes fresh fixture state and records exact child commands and
hashes. Final verification used a task-owned target after unrelated W1/W2
builds produced stale dependency artifacts in the earlier shared target. Earlier
failed runs remain under `/private/tmp/riwork-w3-c62ea465`; final evidence above
uses the isolated target and stable source.

The final fixture used a minimal environment and RIWORK_APPEARANCE=light. PID
70125 had Alpha window 6576 and Beta window 6577. Cua.ai Driver was used only
for read-only exact-PID inspection, with [raw calls](verification/nsh-w3-native/cua-windows.json)
and [Alpha](verification/nsh-w3-native/cua-alpha.json) /
[Beta](verification/nsh-w3-native/cua-beta.json) state retained. The screenshots
show Native light and the status bar in [Alpha](verification/nsh-w3-native/thm-05-light.png)
and [Beta](verification/nsh-w3-native/thm-05-light-beta.png). No OS light/dark
preference was changed; per-window switching is proven by the NSH route.

Fixture cleanup used SIGTERM (exit -15), not a graceful-quit assertion. The
[manifest](verification/nsh-w3-native/fixture-510pnw0y/manifest.json) proves only
`nsh/desktop.handoff.lock` and `run/desktop.lock` changed; no removal or disallowed
write, no session/chat creation, no tmux file/socket, and unchanged appearance
hash plus mtime. No merge, push, installation or task-status update was performed.
