# Phase 6c — Cua surfaces and AppView preview tabs

Task `8fb9a8cb-761d-4c58-a3d6-44c53c24dc91`, Phase 6 umbrella `6ef6a636`.
The first operation was `git merge --no-edit nsh`, a clean fast-forward from
`7e55db0ba1879170ca78f45ffbfcdc809d575493` to
`4df3822dee9f4018be1670823ad49c2987cd6579`. No conflict resolution was needed.
The accepted pending-open/quit-gate fixes and their raw archives are preserved.

Cua now has a standalone `CuaSetup` section and a shared native `Cua` service.
It preserves the driver manager's status/messages, skill states, primary-action
precedence, single-flight setup/access/check operations, and the process-wide
15-second Check until ready. Settings rechecks on showing. Missing-driver startup
opens Settings in the active pane through a dedicated layout operation. The
section is independent of appearance data and can be placed by the 6a Settings
screen without moving its backend ownership. The Cua driver and proxy are unchanged.

App-preview tabs now render `AppPreviewTab` in the workspace AppView. Rust retains
SavedTab identity, spec persistence, title derivation, and main/active/focus-mode
routing. The selected worktree supplies the default cwd. Opening/restoring stays
idle; only Run reaches the unchanged engine launch path. One native handle owns
the worker for the tab, including stop/restart/visibility/window selection/input
and bounded log projection. Edited configurations wait for the old worker to stop
before launch. Disposal during an outstanding launch cannot publish a new owner.
Configuration commits keep the component mounted: the render branch depends only
on the stable tab kind, not a changing snapshot/spec object.

This implements the requested setup and tab-lifecycle slice. It does **not** close
all native preview-surface parity. In particular, the current NSH Image/View
surface is not the reserved full `riwork.preview` native input component.

| Parity rows | Coverage / remaining work |
|---|---|
| DOCS-165 | PASS in fakes: production NSH HostRuntime registration plus the unchanged guard policy refuses own-PID, nested-target, untargeted and desktop input. Read-only own-target and explicit other-target calls resolve only to a fake sink. Existing driverless production MCP proxy regressions also pass. |
| SHELL-009 | PASS in headless AppView fixture: missing-driver status opens Settings in the active pane, without changing ordinary Settings command routing. |
| PANEL-139..141 | PASS in headless fixtures: initial/missing/signature/version/service/permissions/capture/ready/custom-driver/skill-link/error/pending surfaces, labels, precedence, installed chips, skill chips and disabled actions. Core manager remains responsible for real status and access requests. |
| SHELL-318 (Cua), PANEL-142 | Shared Rust Check every 15 s while unready, skipping overlapping actions; subscriptions work outside Settings. Unit proof covers single-flight/read-only status interpretation and ready gating. No real driver or wall-clock permission recovery was exercised. |
| PANEL-138 | Cua recheck on show implemented. Settings-wide account, Keychain and exact focus-order work belongs to 6a and is not claimed here. |
| SHELL-113, TERM-168..170 | PASS: UUID tab, default worktree cwd, Cooperative default, main/active/focus routing, Run-only start, Enter/mode/Run commits, saved spec/title, restored idle tab, close. Blur uses the same commit operation. Remote/no-local-root creation is unavailable. |
| TERM-171..172, 175..177 | Modes/fields, empty-command/scheme validation, Stop/Restart, edited-spec wait, live-PID Stop eligibility, 100 ms polling, window/tab visibility and teardown ownership implemented. Fixtures cover mode changes, empty scheme persistence, Run/Stop, visibility, restart/relaunch, and disposal during delayed launch. Engine invalid-cwd/input-limit tests pass without launching a process. Control-window visibility override is still open. |
| TERM-183..189 | Partial: aspect-fit Image, bounded frame metadata/base64 caching, placeholders, coordinate mapping and bounded viewport, basic pointer/focus forwarding, bounded/toggleable logs. **Open:** native decoder's exact allocation/generation guarantees, full mouse capture/up-outside/middle/wheel, IME/text/key-up/clipboard shortcuts and complete input-release parity. These are not certified by the pure coordinate tests. |
| SHELL-328, DOCS-214..215 | **Open:** NSH preview-control mailbox/CLI control-window hosting. Existing engine/control implementation is unchanged. |
| TERM-173..174, 178..182, 190..195 | Existing preview engine remains unchanged. No live process-group/capture/simulator validation was done. This report does not re-certify those rows through a real NSH window. |
| Settings 6a; schedules/remote 6b | Other lanes; no implementations changed here. |

Verification used private HOME/RIWORK_HOME/runtime directories, a private worktree
Cargo target and at most two Cargo jobs, with heavy commands serialized. Debug
fixture homes select fake Cua and preview backends before any driver or preview
engine construction. Headless `nsh test` was run without a platform `--target`;
`platform: "macos"` in tests selects simulated semantics, not an AppKit window.
The guard registers only the test process in its private runtime. Its synthetic
other PID is never queried or sent input. No Cua provider was invoked.

| Command group | Result |
|---|---|
| Phase 6c UI | 18 passed, 0 failed |
| Missing-driver startup UI | 1 passed, 0 failed |
| Native Cua/preview fixtures | 3 passed |
| NSH registration/guard fixtures | 8 passed (new registration case + unchanged policy tests) |
| Selected engine guard/validation tests | 4 passed; no driver or preview process launched |
| Host suite | 68 passed; 1 ignored acceptance test explicitly run below |
| Workspace/layout suites | 169 passed; doc tests passed |
| Native suites | 189 passed; existing manual streaming benchmark remains ignored |
| Prior App recovery acceptance | All 9 scenarios passed, including the six original cases and three production-admission ordering cases |
| Cargo check, NSH/TypeScript check, format, whitespace | Exit 0 |

Reproduction, from this worktree (each mode was run separately with these exact
arguments; raw manifests record every subprocess argv, environment, result and
output hash):

```sh
python3 crates/riwork-desktop/verification/phase6c.py ui --root /tmp/riwork-phase6c --target-dir /Users/dominik/orca/projects/riWork-nsh-p1-fix-host/target --ghostty-artifacts /Users/dominik/orca/projects/riWork-nsh-p2-terminals/crates/riwork-nsh-terminal/.runtime/pinned/install
python3 crates/riwork-desktop/verification/phase6c.py ui-missing --root /tmp/riwork-phase6c --target-dir /Users/dominik/orca/projects/riWork-nsh-p1-fix-host/target --ghostty-artifacts /Users/dominik/orca/projects/riWork-nsh-p2-terminals/crates/riwork-nsh-terminal/.runtime/pinned/install
python3 crates/riwork-desktop/verification/phase6c.py fixtures --root /tmp/riwork-phase6c --target-dir /Users/dominik/orca/projects/riWork-nsh-p1-fix-host/target --ghostty-artifacts /Users/dominik/orca/projects/riWork-nsh-p2-terminals/crates/riwork-nsh-terminal/.runtime/pinned/install
python3 crates/riwork-desktop/verification/phase6c.py suites --root /tmp/riwork-phase6c --target-dir /Users/dominik/orca/projects/riWork-nsh-p1-fix-host/target --ghostty-artifacts /Users/dominik/orca/projects/riWork-nsh-p2-terminals/crates/riwork-nsh-terminal/.runtime/pinned/install
python3 crates/riwork-desktop/verification/phase6c.py recovery --root /tmp/riwork-phase6c --target-dir /Users/dominik/orca/projects/riWork-nsh-p1-fix-host/target --ghostty-artifacts /Users/dominik/orca/projects/riWork-nsh-p2-terminals/crates/riwork-nsh-terminal/.runtime/pinned/install
python3 crates/riwork-desktop/verification/phase6c.py check --root /tmp/riwork-phase6c --target-dir /Users/dominik/orca/projects/riWork-nsh-p1-fix-host/target --ghostty-artifacts /Users/dominik/orca/projects/riWork-nsh-p2-terminals/crates/riwork-nsh-terminal/.runtime/pinned/install
```

Ghostty artifacts above were existing read-only link inputs; no build/install or
coordination occurred in that other worktree. All Cargo calls use `--locked
--offline` and `CARGO_BUILD_JOBS=2`. The runner never invokes a desktop executable,
Cua driver, Docker, installer, network command, or live lifecycle operation.

Raw evidence is indexed in
[phase6c-evidence/artifact-index.json](../crates/riwork-desktop/verification/phase6c-evidence/artifact-index.json).
Direct raw logs: [UI states and preview lifecycle](../crates/riwork-desktop/verification/phase6c-evidence/ui-lyy4h6ie/ui-fixtures.stderr),
[missing-driver startup](../crates/riwork-desktop/verification/phase6c-evidence/ui-missing-ehrbleaq/ui-fixtures.stderr),
[guard fake dispatch](../crates/riwork-desktop/verification/phase6c-evidence/fixtures-5cw3ur09/guard-fixture.stdout),
[host/workspace/native suite commands](../crates/riwork-desktop/verification/phase6c-evidence/suites-6g4lbhxr/manifest.json),
[prior recovery scenarios](../crates/riwork-desktop/verification/phase6c-evidence/recovery-srex6tk8/app-recovery.stdout), and
[final checks](../crates/riwork-desktop/verification/phase6c-evidence/check-q8ftbc9g/manifest.json).

The archive contains byte-preserved manifests/stdout/stderr/JUnit, not fixture state or
binaries. Superseded logs retain the preview remount and missing-driver routing
failures found during implementation, plus the fixture's original locked-pane
close assertion. The fixed runs exercise those same paths successfully. Earlier
`pending-open-evidence` and `quit-ordering-evidence` archives are unchanged.

`verification_environment`: isolated private fixtures/fakes only.
`live_targets_touched`: none beyond own shell/worktree.
`effective_service_tier`: Service tier default, Fast OFF, as verified in the
assignment; GPT-6-Astra ultra and system-default account unchanged. No `/fast`,
model/effort/account changes or descendant sessions were used.
No merge to nsh, push, installation, reload or restart. Task status is left to
the orchestrator.

## Settings integration follow-up

Merged `nsh` at `087aa897a7b19256ac18c98bf443f39bb1c418d3` in merge
commit `8daa3460da85330f63270b9310b3889545430828`. The sole textual conflict
was the native registry assertion; both `Previews` and `ProjectSettings` were
retained. Workspace now passes `CuaSetup` through AppSettings's `computerUse`
slot (section 01), with its theme and reactive panel visibility. The Cua service,
startup `useCua` behavior and preview engine/bridge are unchanged. DeferredPanel's
only caller is Workspace's fallback, which cannot receive Settings after 6a's
explicit routing; its dead Settings/Cua/appearance branch and unused props were
removed. AppSettings retains ownership of its form and layout.

Headless reruns passed: **19 Phase 6c UI cases**, including the new ordinary
Settings command/route reuse case; **1 missing-driver startup case**, now requiring
AppSettings, Computer use and exactly one Cua section in the active pane;
**68 host**, **169 workspace**, and **199 native** tests. The existing ignored
host recovery acceptance and native manual benchmark remain ignored in these
suite runs. Cargo check, `nsh check` (including TypeScript), format and whitespace
checks exited 0. No additional parity rows are claimed beyond the coverage above.

Exact rerun commands (serialized, in this worktree):

```sh
python3 crates/riwork-desktop/verification/phase6c.py ui --root /tmp/riwork-phase6c-integration --target-dir /Users/dominik/orca/projects/riWork-nsh-p1-fix-host/target --ghostty-artifacts /Users/dominik/orca/projects/riWork-nsh-p2-terminals/crates/riwork-nsh-terminal/.runtime/pinned/install
python3 crates/riwork-desktop/verification/phase6c.py ui-missing --root /tmp/riwork-phase6c-integration --target-dir /Users/dominik/orca/projects/riWork-nsh-p1-fix-host/target --ghostty-artifacts /Users/dominik/orca/projects/riWork-nsh-p2-terminals/crates/riwork-nsh-terminal/.runtime/pinned/install
python3 crates/riwork-desktop/verification/phase6c.py suites --root /tmp/riwork-phase6c-integration --target-dir /Users/dominik/orca/projects/riWork-nsh-p1-fix-host/target --ghostty-artifacts /Users/dominik/orca/projects/riWork-nsh-p2-terminals/crates/riwork-nsh-terminal/.runtime/pinned/install
python3 crates/riwork-desktop/verification/phase6c.py check --root /tmp/riwork-phase6c-integration --target-dir /Users/dominik/orca/projects/riWork-nsh-p1-fix-host/target --ghostty-artifacts /Users/dominik/orca/projects/riWork-nsh-p2-terminals/crates/riwork-nsh-terminal/.runtime/pinned/install
```

Raw logs and exact subprocess commands/environment are in the separate
[integration archive](../crates/riwork-desktop/verification/phase6c-integration-evidence/artifact-index.json):
[UI](../crates/riwork-desktop/verification/phase6c-integration-evidence/ui-0qjwhl6m/ui-fixtures.stderr),
[missing driver](../crates/riwork-desktop/verification/phase6c-integration-evidence/ui-missing-4l0wva10/ui-fixtures.stderr),
[suites](../crates/riwork-desktop/verification/phase6c-integration-evidence/suites-dddpt9pi/manifest.json),
[checks](../crates/riwork-desktop/verification/phase6c-integration-evidence/check-elhwbcaq/manifest.json).
The prior 64 Phase 6c, 24 pending-open and 22 quit-ordering artifact hashes were
reverified and those archives remain byte-unchanged.

`verification_environment`: private HOME/RIWORK_HOME, headless fixtures/fakes only,
own Cargo target, jobs=2, offline. `live_targets_touched`: none beyond own
shell/worktree. `effective_service_tier`: default / Fast OFF (assignment verified);
GPT-6-Astra ultra and system-default account unchanged; no descendants. No live
Cua/GUI input, network, Docker, merge to nsh, push, install, reload or restart.
