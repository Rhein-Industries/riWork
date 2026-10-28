# Native preview surface — TERM-183–189

Task `9319ad32-317b-4d3a-aba7-203a09f43b23`, branch `nsh-p1-fix-host`.
First fast-forwarded the branch to `nsh` commit
`82f5a12215793596d81d420f1045071980ecf9b9`, retaining the accepted W2,
quit/restart recovery, Phase 6c and PANEL-138 work.

The app-preview tab now hosts the registered `riwork.preview` NSView through the
existing NSH 0.3 `NativeComponent` extension. No upstream framework change or
vendor patch is required. Native appearance uses AppKit system background,
divider and focus colors. Abandoned styles remain untouched.

## Coverage

| Row | Implementation and isolated proof |
| --- | --- |
| TERM-183 | Off-thread bounded PNG decode; metadata dimensions checked; RGBA→BGRA; malformed source tried once; last good image retained. One decode remains in flight across reset. Worker generation and atomic main-thread image installation reject stale work after reset/close. Tests cover delayed decode and delayed installation independently. A 16 MiB source yields a 161-byte JSON metadata snapshot. Actual CGImage/provider tests verify identical BGRA and exactly-once pixel-owner release. |
| TERM-184 | NSView/CALayer aspect-fit frame, exact waiting/configure placeholders, system colors and focused border. Source/compile verification plus headless CGImage ownership and aspect-fit geometry fixtures; no live visual inspection claimed. |
| TERM-185 | Native input policy rejects letterbox presses, clamps captured drag/up outside and sends cooperative resize only on change, bounded to 1920×1200 with 32-pixel minimum. |
| TERM-186 | AppKit L/R/M down/up/drag callbacks, original-recipient up-outside handling, capture-mode Click, non-capture movement, precise wheel values and 20 pixels per line. Raw native translation and policy fixtures cover each button and outside coordinates. |
| TERM-187 | NSTextInputClient keeps marked text local and commits Unicode. Unhandled IME navigation forwards once; consumed composition stays local. GPUI key spelling, modifier changes, matching key-up, paste and menu shortcuts are covered. A process-local KeyUp monitor handles only this focused view/window's held key. Native focus events select the owning pane; preview Escape is not intercepted unless workspace transient UI needs it. |
| TERM-188 | All tab/capture/window/active/running/frame gates; release all held buttons and keys then Focus(false) before capture hide, and on blur/reset/stop/close. Actual native first-responder reconciliation restores typing after reactivation. Five-second control capture lease expires without allowing hidden local input; refresh checks the current engine frame and phase, including Error with a retained PID. |
| TERM-189 | Existing engine bounds unchanged and rerun; metadata snapshot limits UI logs to last 160 entries/2000 Unicode characters with control stripping. Existing gold stderr and 160–180-pixel log toggle/scroll UI retained. |

These seven rows are implemented for the requested isolated acceptance. No
upstream source blocker remains within TERM-183–189. Live AppKit first-responder,
IME candidate UI, drag capture and visual rendering were deliberately not tested;
the evidence uses production-shared native translators, real CGImage ownership,
pure admission policy and actual QuickJS/native registry/keymap delivery with a
fake mount sink. It never constructs an NSApplication, NSWindow or NSView.

The surrounding preview-control mailbox migration **remains open**: the active
NSH host has no `PreviewHost` request polling/response/status publication wiring
(TERM-201–207). The old implementation remains in
`crates/riwork-desktop/src/preview_host.rs` and the legacy `app_preview.rs`, outside
the active NSH host. `startControlled`/`refreshControlled` establish and test the
surface's five-second lease; they do not claim end-to-end CLI mailbox support.
This is repository migration work outside these seven rows, not an invented
upstream blocker. Other preview rows are not closed by this report.

## Ownership and transport

`PreviewHandle.surfaceId()` supplies an opaque process-local binding. JavaScript
receives frame dimensions, status and bounded display logs only. PNG decode and
BGRA stay in Rust; CGDataProvider retains the same decoded allocation. One
pending native publication replaces older queued snapshots. Image installation
holds the same short state lock as reset/close; reentrant AppKit focus updates
stay outside that lock. Closed owners can publish only their current empty
image, and view disposal clears its layer and native event monitor.

The 32 MiB limit is the decoder allocation budget, not an aggregate process
memory claim while old/new images overlap. The unchanged engine's cooperative
wire protocol still carries its documented PNG message; this change removes
bulk encoded frames from the engine-to-JavaScript/UI bridge. Engine source and
its capture/launch/IPC behavior are unchanged.

Construction and restore remain idle. Only Run or an explicit control-start
request launches. Owner close rejects late launches and releases native input;
worker completion and native installation cannot revive a retired generation.
A window-capture selection keeps the same Session, whose engine only queues
the request. The surface stays without a frame or input until the snapshot
reports the selected pid/window and then a frame newer than the one first
seen with that identity. The engine reports no failed selection
(`riwork-engine/src/app_preview.rs` 208-210, 584-592), so a refused selection
leaves the preview blank until the user selects again or runs.

## Changes shared with other lanes

- `apps/riwork/AppPreviewTab.tsx`: replace Image/JS pointer surface with native
  owner binding, visibility and focus event; retain configuration/Run/lifecycle.
- `apps/riwork/Workspace.tsx`: seven additive lines for preview focus ownership,
  pane selection and Escape precedence.
- `apps/riwork/commands.ts`: optional native-preview Escape owner hook and reactive
  binding; preserve all global tab-cycle keys and transient dismissal.
- `apps/riwork/tests/keymap.test.ts`: remove the stale pre-Phase-6c assertion
  that appPreview is deferred; production command declarations are unchanged.
- `crates/riwork-desktop/src/nsh_host.rs`: one native component registration.
- `crates/riwork-nsh-native/src/app_preview.rs`: native owner/session transport,
  metadata-only snapshot, explicit control lease methods and maintained fixtures.
- `crates/riwork-nsh-native/src/lib.rs`, its `Cargo.toml`, and root `Cargo.lock`:
  register modules and reuse already-locked PNG/AppKit/dispatch/cc dependencies.
- `docs/nsh-contracts.md` and `docs/nsh-parity.md`: freeze the component interface
  and identify the seven implemented rows. Phase 6c report receives an addendum.

New implementation files: `PreviewSurface.tsx`, `preview_input.rs`,
`preview_surface.rs`, `preview_macos.rs`, `native/preview.m`, and native `build.rs`.
New maintained verification: `PreviewFocus.fixture.tsx` entry/fixture,
`tests/preview_focus.rs`, and `verification/preview_surface.py`.
**No vendor/base, RIWORK.patch, Settings, preview-engine, or non-Native style
files were changed.**

## Verification

All final commands exited **0**:

- **41** preview native/transport/policy fixtures plus the unchanged engine log-bound test.
- **1** native-event/Workspace/keymap integration test, covering 11 Escape transitions,
  pane selection, tab/window visibility, modal precedence, unmount and four global shortcuts.
- **21** Phase 6c UI cases, including the existing preview Run/restore lifecycle.
- **9** recovery scenarios in the explicit production-App host test, including delayed
  quit and both restart failures: restoreReady accepted exactly once; no orphan writer.
- **68** host and **169** workspace tests. The native suite reports **236** passed
  (235 top-level cases plus one nested fixture subprocess), zero failed, four ignored.
  The native benchmark and two settings-focus fixture tests remain ignored; this task's
  new native focus test is ignored generically and explicitly passes via `focus`.
  The host recovery test is similarly explicitly run via `recovery`.
- **5** existing P1 keyboard cases, after updating their stale appPreview deferred golden.
- Cargo check, NSH/TypeScript, Rust format and whitespace checks passed.

[Raw evidence index](../crates/riwork-desktop/verification/native-preview-evidence/artifact-index.json):
[transport/input](../crates/riwork-desktop/verification/native-preview-evidence/surface-k50yuo5b/surface-fixtures.stdout),
[native translator/provider output](../crates/riwork-desktop/verification/native-preview-evidence/surface-k50yuo5b/surface-fixtures.stderr),
[focus wiring](../crates/riwork-desktop/verification/native-preview-evidence/focus-393ihygr/native-focus-wiring.stderr),
[UI](../crates/riwork-desktop/verification/native-preview-evidence/ui-sxh028v_/preview.xml),
[recovery](../crates/riwork-desktop/verification/native-preview-evidence/recovery-ibxmtew8/app-recovery.stdout),
[suites](../crates/riwork-desktop/verification/native-preview-evidence/suites-4yfk76qu/manifest.json),
[keyboard](../crates/riwork-desktop/verification/native-preview-evidence/keys-qjxc1kmc/keys.xml),
[checks](../crates/riwork-desktop/verification/native-preview-evidence/check-h6wuk4rx/manifest.json).

Superseded raw runs are preserved too: an initially uncompressed maximum-edge fixture
exceeded the 16 MiB compressed-input limit; the first focus fixture directly changed
LayoutSession without installing its returned layout, corrected to drive real global
commands; the CLI rejected a test-file path without an app entry, corrected to a root
filter. The final keyboard rerun found the pre-existing golden still calling appPreview
deferred. Its original production table and test are byte-identical to `82f5a12`, as
recorded in `stale-keymap-golden.json`; only the obsolete expected entry was removed.
These are identified separately from product regressions; no failing output is replaced.

All **207** prior artifacts were hash-verified and remain byte-unchanged:
Phase 6c 64, integration 29, PANEL-138 68, pending-open 24 and quit-ordering 22.

Commands, run from this worktree (each mode is serialized, never concurrent):

```sh
python3 crates/riwork-desktop/verification/preview_surface.py focus --root /tmp/riwork-native-preview --target-dir /Users/dominik/orca/projects/riWork-nsh-p1-fix-host/target --ghostty-artifacts /Users/dominik/orca/projects/riWork-nsh-p2-terminals/crates/riwork-nsh-terminal/.runtime/pinned/install
python3 crates/riwork-desktop/verification/preview_surface.py surface --root /tmp/riwork-native-preview --target-dir /Users/dominik/orca/projects/riWork-nsh-p1-fix-host/target --ghostty-artifacts /Users/dominik/orca/projects/riWork-nsh-p2-terminals/crates/riwork-nsh-terminal/.runtime/pinned/install
python3 crates/riwork-desktop/verification/preview_surface.py ui --root /tmp/riwork-native-preview --target-dir /Users/dominik/orca/projects/riWork-nsh-p1-fix-host/target --ghostty-artifacts /Users/dominik/orca/projects/riWork-nsh-p2-terminals/crates/riwork-nsh-terminal/.runtime/pinned/install
python3 crates/riwork-desktop/verification/preview_surface.py recovery --root /tmp/riwork-native-preview --target-dir /Users/dominik/orca/projects/riWork-nsh-p1-fix-host/target --ghostty-artifacts /Users/dominik/orca/projects/riWork-nsh-p2-terminals/crates/riwork-nsh-terminal/.runtime/pinned/install
python3 crates/riwork-desktop/verification/preview_surface.py suites --root /tmp/riwork-native-preview --target-dir /Users/dominik/orca/projects/riWork-nsh-p1-fix-host/target --ghostty-artifacts /Users/dominik/orca/projects/riWork-nsh-p2-terminals/crates/riwork-nsh-terminal/.runtime/pinned/install
python3 crates/riwork-desktop/verification/preview_surface.py keys --root /tmp/riwork-native-preview --target-dir /Users/dominik/orca/projects/riWork-nsh-p1-fix-host/target --ghostty-artifacts /Users/dominik/orca/projects/riWork-nsh-p2-terminals/crates/riwork-nsh-terminal/.runtime/pinned/install
python3 crates/riwork-desktop/verification/preview_surface.py check --root /tmp/riwork-native-preview --target-dir /Users/dominik/orca/projects/riWork-nsh-p1-fix-host/target --ghostty-artifacts /Users/dominik/orca/projects/riWork-nsh-p2-terminals/crates/riwork-nsh-terminal/.runtime/pinned/install
```

Each manifest records exact subprocess argv, exit status, elapsed time, private
paths and output hashes. Cargo uses `--locked --offline`, own target and jobs=2.
The pre-existing pinned Ghostty artifacts are read-only link inputs; no build or
coordination took place in that worktree.

`verification_environment`: private HOME/RIWORK_HOME; headless fixtures/fakes;
no GUI, Cua input, real preview processes/projects, network or Docker.
`live_targets_touched`: none beyond own shell/worktree.
`effective_service_tier`: default, Fast OFF (user verified); GPT-6-Astra ultra,
system-default account unchanged. Two descendants used the inherited model and
unchanged default tier/Fast-off; neither spawned descendants or ran builds.
No merge to nsh, push, install, reload or restart was performed.
