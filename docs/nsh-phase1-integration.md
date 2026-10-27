# Phase 1 integration verification

Task `47be9d38-ad1b-41fa-b0a7-a2e32eeb5fc0`, branch `nsh-p1-integration`,
based on `nsh` at `1d63063`.

`verification_environment = isolated fixture homes, headless NSH engines, and two real AppKit fixture windows inspected read-only through RiWork cua-driver MCP with explicit fixture PID; no live RiWork windows or state.`

This implements the requested M4 integration subset, following acceptance §4's
P1a → P1c → P1b ordering and the assignment's adoption of D-OWN-1. It is not M5
acceptance of every P-SHELL row.

## Integrated behavior

- The desktop depends on `riwork-nsh-native`, which owns the sole production
  `#[native_module("riwork/native")]` registry and calls
  `riwork_nsh_workspace::register_layouts`. Store/Sessions/Subscription and
  Layouts/LayoutSession are installed together. The temporary registry and host
  stub are gone. All windows share P1a's fixed process home.
- `apps/riwork` is a root workspace member using the root lockfile; its obsolete
  independent lockfile is removed. Its NSH Rust entry points at P1a so generated
  types and headless tests use the same registry as the host.
- The real source entry is `apps/riwork/App.tsx`. The checkout default is
  `apps/riwork/.nsh/out/bundle.js`, built with
  `"$NSH" build apps/riwork --out apps/riwork/.nsh/out`.
  Built-bundle delivery uses `RIWORK_NSH_BUNDLE=/absolute/output/bundle.js`;
  keep assets/source maps/chunks beside it. The host reports a build recipe if
  missing and has no bootstrap fallback. See
  [README-NSH.md](../crates/riwork-desktop/README-NSH.md) for both workflows.
- Runtime snapshots obtain `host_layouts::lookup(windowId).project_layout()`
  directly in Rust, also during heartbeat publication. Close/quit gate new
  registrations and edits, retain handles, and call `flush_blocking()` on the
  service worker before stopping views. Flush/publication errors keep windows
  open and restore editing. Tests cover last-edit durability, two-window quit,
  failure/retry, pending registration and mutation races. Closed UUID markers
  persist for the process lifetime; they retain no sessions or workers.
- The host handoff guard is `nsh/desktop.handoff.lock`, within the acceptance
  write allow-list. The frozen `run/desktop.lock` and socket paths stay intact.
- Restore payloads pass through the real app to native Layouts. The app posts
  readiness once after installing the project projection (global immediately),
  and the host rejects project readiness without a registered native handle.
  Successful loads/restores queue their storage projection immediately so a
  host flush persists an untouched restore too; recovery defaults stay unsaved.

## Verification environment and reproduction

Host: macOS 26.2 (25C56), arm64. Rust/Cargo 1.99.1. NSH CLI:
`/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh`,
`nsh 0.3.0 (76fa8a7)`, SHA-256
`90b7633c80306407fc97b2051be00af868d4577281f918ff655c650a8a5a2e60`.
TypeScript:
`/Users/dominik/Library/Caches/nsh/typescript-7.0.2-darwin-arm64/package/lib/tsc`.
No tools were installed or upgraded.

Evidence/build root: `/private/tmp/riwork-p1-integration-mkzwamt7`.
Cargo test HOME is a disposable ignored `target/nsh-integration-home-*` directory
outside `/tmp`, required by existing folder-trust tests. RIWORK_HOME, runtime,
NSH cache, tmux directories and build output are isolated. Signal masks are
cleared for the existing subprocess tests. The GUI uses an explicit clean
environment with PATH `/usr/bin:/bin:/usr/sbin:/sbin` and fixture-only state.

The checked-in runner records exact argv, cwd, environment, exit status,
stdout/stderr paths and hashes in a per-run manifest:

```sh
ROOT=/private/tmp/riwork-p1-integration-mkzwamt7
python3 crates/riwork-desktop/verification/phase1_integration.py headless --root "$ROOT"
python3 crates/riwork-desktop/verification/phase1_integration.py targeted --root "$ROOT"
python3 crates/riwork-desktop/verification/phase1_integration.py nsh --root "$ROOT"
python3 crates/riwork-desktop/verification/phase1_integration.py build --root "$ROOT"
python3 crates/riwork-desktop/verification/phase1_integration.py parity --root "$ROOT"
python3 crates/riwork-desktop/verification/phase1_integration.py gui --root "$ROOT" --linger-seconds 180
cargo fmt -p riwork-desktop -p riwork-nsh-native -p riwork-nsh-workspace --check
git diff --check
```

## Exact checks and results

All commands ran from this worktree with the runner's recorded fixture environment.
The final records and hashes are committed in
[verification-summary.json](../crates/riwork-desktop/verification/integration-evidence/verification-summary.json).
Raw stdout/stderr remain under the evidence root above.

| Check | Exact command | Result |
|---|---|---|
| Six requested/integration crates | `cargo test -p riwork-core -p riwork-engine -p riwork-cli -p riwork-desktop -p riwork-nsh-native -p riwork-nsh-workspace --locked --offline` | Exit 0; **1,722 passed, 0 failed, 187 ignored**, 33 result groups. Existing live/external tests remain ignored. |
| Final changed crates after restore-save and handoff-path fixes | `cargo test -p riwork-desktop -p riwork-nsh-native -p riwork-nsh-workspace --locked --offline` | Exit 0; **70 passed, 0 failed, 0 ignored**. |
| TS/native declarations | `"$NSH" check apps/riwork --tsc /Users/dominik/Library/Caches/nsh/typescript-7.0.2-darwin-arm64/package/lib/tsc` | Exit 0. |
| NSH UI/model | `"$NSH" test apps/riwork --junit /private/tmp/riwork-p1-integration-mkzwamt7/nsh-wsawhnca/nsh.xml` | Exit 0; **14 passed** (8 TSX, 6 model), no failures/errors/skips. Rust suites are counted separately above. |
| Executables | `cargo build -p riwork-desktop -p riwork-cli --locked --offline` | Exit 0. |
| Real app bundle | `"$NSH" build apps/riwork --out /private/tmp/riwork-p1-integration-mkzwamt7/bundle` | Exit 0; `bundle.js` 12,873 bytes plus `.nshc`. |
| Binary dependency graph | `cargo tree -p riwork-desktop --target aarch64-apple-darwin --edges normal,build --locked --offline` | Exit 0; **zero GPUI/Ghostty entries**. |
| Full desktop graph | `cargo tree -p riwork-desktop --locked --offline` | Exit 0; **zero GPUI/Ghostty entries**, including dev dependencies. |
| Formatting/whitespace | Commands shown above | Exit 0. |

`$NSH` is the absolute pinned CLI path stated above. The cargo-tree outputs are
committed as [binary tree](../crates/riwork-desktop/verification/integration-evidence/cargo-tree-binary.txt)
and [full desktop tree](../crates/riwork-desktop/verification/integration-evidence/cargo-tree-full.txt).
Root lock resolution contains 941 package versions (previously 1,033): 13 added, 105
removed (including obsolete host dependencies); `objc2` moves from 0.6.4 to
0.6.5 for the real macOS host. The source workspace now has one lockfile.

CLI spot checks use identical fresh home copies for each side. The reference is
the standalone `riwork-cli` built from the unchanged CLI crate;
`git diff ea0f2f5 -- crates/riwork-cli` is empty. This checks the original dispatcher's
bytes without rebuilding the historical GPUI UI. Both binaries are recorded
with their paths and hashes. These five commands had identical stdout, stderr
and exit code:

| Arguments | Exit code | stdout / stderr bytes |
|---|---:|---:|
| `project list --json` | 0 | 557 / 0 |
| `settings list --json` | 0 | 2223 / 0 |
| `instances --json` | 0 | 3 / 0 |
| `appearance --json` | 2 (unpublished in fixture) | 0 / 73 |
| `nosuchcommand` | 2 | 0 / 172 |

## Two-window fixture and write audit

Final fixture: `/private/tmp/rwnshi-gui-x748i0lv`, PID **59579**, stopped after
inspection (SIGTERM exit -15). Its GUI manifest is
`/private/tmp/riwork-p1-integration-mkzwamt7/gui-2sfv9rtl/manifest.json`.
The fixture generator uses core Store and LayoutStore writers. The actual launch
was `/private/tmp/riwork-p1-integration-mkzwamt7/target/debug/riwork /private/tmp/rwnshi-gui-x748i0lv/repos/Alpha`;
the same executable with `.../repos/Beta` forwarded successfully and exited 0.

Read-only cua-driver calls, each restricted to PID 59579:

- `list_windows({pid:59579,session:"p1-integration"})`: exactly two windows,
  Alpha 6283 at `(1070,334)`, Beta 6284 at `(1092,356)`; both 1220×780,
  confirming the 22-point cascade.
- `get_window_state` on those exact IDs captured the real titles, tab strips,
  three-pane DockArea, empty pane and status bars. Actual arguments/results are
  in [cua-read-only.json](../crates/riwork-desktop/verification/integration-evidence/cua-read-only.json).
  The AX tree exposed chrome only; content was verified from screenshots and
  the native runtime snapshots, not claimed as AX-selector coverage.
- [Alpha screenshot](../crates/riwork-desktop/verification/integration-evidence/alpha.png),
  [Beta screenshot](../crates/riwork-desktop/verification/integration-evidence/beta.png),
  [runtime layouts](../crates/riwork-desktop/verification/integration-evidence/registry.json).

Both runtime entries contain their distinct project IDs and current native
ProjectLayout, including `Alpha-fixture-task` / `Beta-fixture-task`. The GUI log
was empty. Changed RIWORK_HOME files were only `nsh/desktop.handoff.lock` and
`run/desktop.lock`; layout saves retained identical existing bytes. The disallowed
write list and tmux directory were empty. No `sessions.json`, `appearance.json`
or `chats/` was created. No Cua input calls or live-window inspection occurred.
The earlier fixture PID 53265 was also stopped; it identified the handoff lock
allow-list issue fixed before this final passing run.

## Resolved verification failures

Raw diagnostics are retained rather than hidden. The first locked invocation
ran before root lock regeneration. The first compile found the fixture using
test-only `LayoutStore::save`, corrected to production `save_chat_state`.
The six-crate test command then passed, while its early enclosing runner failed
because the existing Cargo fixtures had left an empty tmux directory; isolated
NSH verification now gets its own directory and passed. The first real GUI
write audit found `run/desktop.handoff.lock`; the host now keeps its internal
guard in the permitted `nsh/` area, preserving the real run lock/socket and
handoff-race tests. Final targeted tests, build, CLI parity, Cua inspection and
write audit all passed after that change. No full-suite result is claimed to
postdate those small fixes; the affected integration suites were rerun.

## Remaining scope

Full restart/CLI reload handoff remains explicitly unavailable and keeps the old
host alive. Native drafts, Tabs/Settings/Appearance/Dashboard surfaces absent
from P1a, full global UI, terminal/chat content, remaining P-SHELL acceptance and
the carry-over ledger are not claimed complete. SIGTERM after the read-only GUI
inspection is fixture cleanup; graceful close/quit durability is proved by the
Rust tests, not by that signal.

The root lock still contains GPUI packages for the independent
`riwork-gpui-preview` workspace member; the required exclusion is the `riwork`
binary dependency tree. All 142 protected legacy desktop source files, including
GPUI `main.rs` and its tests, remain byte-for-byte unchanged. The deletion embargo
in acceptance §4.3 remains in force. No merge, push, install or task-status update
was performed; status belongs to the orchestrator.
