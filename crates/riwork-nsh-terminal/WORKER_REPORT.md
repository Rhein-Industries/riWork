# Phase 2a production local terminals — PASS

Task: `24ab1d55-508b-473b-af92-f0aa26503792`.
Branch: `nsh-p2-terminals`, based on `nsh` at `2e851e1`.
Worktree: `/Users/dominik/orca/projects/riWork-nsh-p2-terminals`.

The integrated app now hosts local shell tabs with the frozen
`riwork.terminal` props, events and commands. The final two-window run passed
20 close/reopen cycles under continuous output, then normal window close and
quit: 84 native surfaces created, 84 freed, 84 callback contexts released, zero
pending disposals, zero leaked tmux clients/attach descendants, and all four
original sessions and panes surviving. Cargo and NSH checks pass. Both Cargo
dependency trees contain zero GPUI/Ghostty Rust crates.

## Delivered behavior

- Registers the native component on the shared NSH registry before every
  AppView. App content routes local New Shell, open, Detach, guarded Exit,
  remembered-pane reopen, keyboard menu commands and resolved links.
- Attaches through the engine's existing tmux command without a viewport lease
  or session replacement. The component never creates or kills sessions.
- Disposes by hiding/disabling input, detaching the exact tty off-main,
  confirming its absence, waiting asynchronously for Ghostty's PTY child to
  exit, then freeing on the main thread. Early tty absence is provisional.
  Unfinished disposals park/retry; normal quit drains before stopping AppKit.
- Implements AppKit keyboard and NSTextInputClient composition/candidate
  geometry, mouse/drag/scroll, resize, backing scale and display notifications,
  focus/occlusion handling, live appearance, Ghostty config watching, native
  link hover and Command-click routing. Native input/rendering bypass JS.
- Preserves the existing hidden-surface policy, frozen last frame, and ended
  session screen. Liveness errors do not become authoritative session death.
- Fixes a DockArea mount condition that recreated terminal nodes on selection
  and resize, plus subscription cleanup after NSH VM finalization. Both have
  focused regression coverage.
- Adds feature-gated fixture transport and a repeatable integrated acceptance
  runner. The normal production build has no fixture transport.

See [component ownership and scope](README.md), [private build instructions](BUILD.md),
[frozen contracts](../../../docs/nsh-contracts.md#3-riworkterminal-native-component),
and [behavior specification](../../../docs/nsh-spec-terminal-layout.md).

## verification_environment

All runtime verification used explicit disposable `RIWORK_HOME` fixtures.
Final GUI fixture: `/private/tmp/rwnsh-p2-upf_p6zi`, state directory
`/private/tmp/rwnsh-p2-upf_p6zi/state`, private tmux server
`riwork-b5719c06acdf0acb`. HOME, XDG config, runtime and TMPDIR were also isolated
for this run. Exact child argv, environment and readbacks are retained in the
[native manifest](evidence/native/manifest.json).

Host: macOS 26.2 (25C56), arm64/Darwin 25.2.0; Xcode 26.0.1; Rust 1.99.1,
Cargo 1.99.1; tmux 3.6a; NSH 0.3.0; private Zig 0.16.0. Build/toolchain caches
were shared where recorded; application state was never shared with live
RiWork. No live RiWork/orchestrator window or state was inspected or operated.

RiWork's Cua.ai Driver MCP was used read-only with explicit fixture PID
`69604`: `list_windows(pid=69604)` and `get_window_state` for windows `6573`
and `6574`. There were zero Cua input calls. The feature-gated fixture arranged
its own two windows side by side without activating them. Both screenshots
show legible output pressure in both panes:
[Alpha](evidence/native/cua-alpha.png), [Beta](evidence/native/cua-beta.png).
[Cua metadata](evidence/native/cua.json) records the calls and observations.

Final accepted executable SHA256:
`5f7ac09cfd48e0442df2e585060e9fb6c67b65407d37cf92a9ce366ab8651c02`.
Production bundle SHA256:
`e8c7ef467434d37a1006da57e5daa1a5a4c50919112fbca9432f44b656037a98`.
Earlier executable snapshots in the build evidence are historical and do not
identify this final run.

## Pinned native library

Ghostty revision `9f0e1719dc918368367d368bfe300f59bb68b5a4`, with the pinned
vendor patch set documented in [VENDOR.md](VENDOR.md). The linked archive is the
exact prior GO artifact, SHA256
`b51367aa565527eddfa180d5ae6304899d903af6fc094b4531671ffc20fb064f`;
header SHA256
`f2c12e1bb02540d79feed6bd29ee37312c4e0d24391442a63e66f06e1f7f2e4a`.
It lives only in this worktree's ignored `.runtime/pinned/install`.

The hashed `gpui-libghostty-0.3.1.crate` is a source carrier: only its pinned
`vendor/ghostty` tree is extracted. Its Rust code/build script/dependencies are
never built or linked. Source archive, all 1,242 source files, Zig binary,
archive/header provenance and 602 theme resources are verified. A fresh private
Zig build also passed; its different archive hash is documented as build
validation only and was not substituted for the GO runtime. Nothing was
installed system-wide. [Build evidence](evidence/pinned/pinned-final.md) contains
the exact source-build command, Zig options, environment and hashes.

## Executed commands and results

Commands ran from the worktree above. Logs and exact expanded child commands
are retained under [evidence](evidence/README.md); counts below overlap and must
not be summed.

```sh
python3 crates/riwork-desktop/verification/phase1_integration.py headless \
  --root /tmp/riwork-p2-full-checks \
  --target-dir /Users/dominik/orca/projects/riWork-nsh-p2-terminals/target --jobs 4
```

Exit 0. Cargo core/engine/CLI/desktop/native/workspace: **1,728 passed, 0 failed,
188 ignored**, across 33 test result groups. The runner used locked/offline
Cargo and isolated homes. Its earlier 15-test NSH run passed and is superseded
by the final 16-test run below. [Exact commands/environment](evidence/headless/manifest.json),
[Cargo output](evidence/headless/cargo-tests.stdout).

```sh
RIWORK_HOME=/tmp/riwork-p2-terminal-verification/final-unit-state \
RIWORK_SKIP_SPEECH_HELPER=1 \
cargo test -p riwork-nsh-terminal -p riwork-desktop --offline
```

Final production-feature check: exit 0, **42 passed, 0 failed** (17 terminal,
20 desktop, 5 CLI contract tests). [Raw output](evidence/native/final-cargo-tests.log).
Final native/workspace validation separately passed **52 tests** and the
retained subscription-owner regression passed. [App validation](evidence/app/FINAL_VALIDATION.md)
retains their exact commands and fixture environment.

```sh
NSH=/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh
TSC=/Users/dominik/Library/Caches/nsh/typescript-7.0.2-darwin-arm64/package/lib/tsc
NSH_CACHE=/tmp/riwork-p2-terminal-bridge/nsh-cache \
RIWORK_HOME=/tmp/riwork-p2-terminal-bridge/state \
"$NSH" check apps/riwork --tsc "$TSC"
NSH_CACHE=/tmp/riwork-p2-terminal-bridge/nsh-cache \
RIWORK_HOME=/tmp/riwork-p2-terminal-bridge/state \
"$NSH" build apps/riwork --out /tmp/riwork-p2-terminal-bridge/bundle
NSH_CACHE=/tmp/riwork-p2-terminal-bridge/nsh-cache \
RIWORK_HOME=/tmp/riwork-p2-terminal-bridge/headless-mount-home \
RIWORK_RUNTIME_DIR=/tmp/riwork-p2-terminal-bridge/runtime-mount \
"$NSH" test apps/riwork --junit /tmp/riwork-p2-terminal-bridge/nsh-mount-final.xml
```

All exit 0. Final NSH suite **16 passed, 0 failed**; typecheck and production
bundle passed. Fixture seed and focused before/after regression commands are
in [app validation](evidence/app/FINAL_VALIDATION.md). The mount regression
failed before the fix (10 mounts instead of 2) and passed after it.

```sh
RIWORK_HOME=/tmp/riwork-p2-terminal-verification/state RIWORK_SKIP_SPEECH_HELPER=1 \
cargo test -p riwork-engine terminal_detach_is_exact_tty_scoped_and_requires_readback \
  -- --ignored --nocapture
RIWORK_HOME=/tmp/riwork-terminal-detach-6bv8m2c_ TMPDIR=/tmp RIWORK_SKIP_SPEECH_HELPER=1 \
cargo test -p riwork-engine --test terminal_detach --offline -- --ignored --nocapture
PYTHONDONTWRITEBYTECODE=1 python3 crates/riwork-nsh-terminal/scripts/test_prepare_ghostty.py
python3 crates/riwork-nsh-terminal/scripts/prepare_ghostty.py verify
cargo tree -p riwork-desktop --prefix none
cargo tree -p riwork-desktop --features terminal-fixture --prefix none
```

All exit 0. Fake exact-tty and real-PTY tests each passed; the real test uses two
clients/two sessions and checks wrong-session refusal and survival. Both are
normally ignored and were explicitly executed. Private-build integrity:
**9 passed**. Installed pin verification passed. Both Cargo trees have **412
unique packages and zero names containing gpui or ghostty**. Raw tree files and
native C link directives are in [pinned evidence](evidence/pinned/pinned-final.json).

```sh
RIWORK_HOME=/tmp/riwork-p2-terminal-verification/state RIWORK_SKIP_SPEECH_HELPER=1 \
cargo build -p riwork-desktop --features terminal-fixture
python3 crates/riwork-desktop/verification/phase2_terminals.py \
  --binary target/debug/riwork \
  --bundle /tmp/riwork-p2-terminal-bridge/bundle/bundle.js \
  --cycles 20 --linger-seconds 120
```

Both exit 0. [Build output](evidence/native/build-final-visible.log),
[acceptance output](evidence/native/acceptance-visible-final.log),
[per-cycle record](evidence/native/cycles.json),
[full native lifecycle snapshot, gzip](evidence/native/final-snapshot.json.gz).

| Final integrated acceptance | Result |
| --- | --- |
| Two project windows, two local panes each | Rendered and observed by read-only Cua |
| Close/reopen under continuous output | 20 cycles × 4 tabs; same session/pane identities every cycle |
| Native resource accounting | 84 created / 84 freed / 84 contexts released / 0 pending |
| Detach/free ordering | Exact-tty absence, then child exit, then main-thread free for every lifetime |
| Maximum synchronous shutdown / native free | 0.159 ms / 5.840 ms |
| Maximum total disposal | 40.868 ms |
| Main-loop responsiveness | 3,312 heartbeats; maximum teardown gap 16.644 ms; zero global gaps ≥100 ms |
| Input | Text once, arrow, Return, Ctrl-C, Japanese marked-text commit once, SGR mouse/drag |
| Resize | Grid readback changed; client PIDs remained stable |
| Themes | Live gruvbox_light → ri_work; expected backgrounds; client PIDs stable |
| Links | Capture/resolver, native hover and Command-click; external open intercepted in fixture |
| Render verification | All four initial and four final surfaces contain pressure text and nonuniform IOSurface pixels |
| Window close / normal quit | Exit 0; all four sessions survive; zero remaining clients/attach descendants |
| Fixture cleanup | Own app, tmux server and four pressure processes absent |

The pressure program is deliberately a raw-mode local session process, allowing
byte-exact input/mouse verification while it emits output continuously. It
uses the normal engine shell creation and native attach path.

## Limits and diagnostic correction

Leak acceptance measures owned native surfaces, callback contexts, tmux clients
and attach descendants. It is not a general heap profiler claim. Input probes
invoke the production NSResponder/NSTextInputClient handlers; physical Japanese
input-source candidate UI and physical migration between displays were not
tested. The final run verifies geometry/rendering on a 2× display. Host link
routing was intercepted in the fixture, so no external application was opened.

Earlier blank screenshots came from fully occluded background fixture windows.
`NSWindowOcclusionStateVisible` is bit 2; 8192 alone is not visible. Rendering
was correctly paused. Presenting only fixture windows without activation fixed
the observation; no forced visibility/rendering bypass or additional production
poller remains. Early lifecycle-only runs are not substituted for the final
visible acceptance. [Diagnostic record](evidence/diagnostics/render-diagnosis.md).

Paste/drop, subshells, remote tabs and Vim tabs remain next-slice seams, as
assigned. Full restart/replacement handoff and physical display migration are
not newly certified by this slice. No merge, push, system install or task-status
mutation was performed. Task status remains with the orchestrator. The final
response records the commit SHA containing this report.
