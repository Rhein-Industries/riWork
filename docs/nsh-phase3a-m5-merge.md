# Phase 3a / M5 merge verification

Assignment: `500f21fc-0a69-40e6-b158-2df7a8fbf2bc`

Merge on `nsh-p3-panels`: panels `5f4c023dde3d1d6f4b26ab9feb14a38cfdd17bf6`
and `nsh` / M5 `ed1b456340d66d5cc9ead9d60f8c9111e75e1a40`.

`verification_environment = isolated /tmp RIWORK_HOME and RIWORK_RUNTIME_DIR fixtures, windowless NSH engines, and test-owned unshown AppKit objects`

No live RiWork windows or state were used. The AppKit test executable uses
activation policy Prohibited, never orders or activates its fixture windows,
and asserts that all its windows are invisible. No desktop automation, push,
installation or task-status update was performed.

## Conflict resolution

Git reported four conflicts, including `Workspace.tsx` in addition to the three
originally identified by the orchestrator.

- `crates/riwork-desktop/src/nsh_host.rs`: retain M5's `RuntimeTick` and busy
  publication retry; pass the home into Phase 3a's `current_layouts`, retaining
  current switched-project identity, root and layout capture. M5's extracted
  chrome implementation and corrected titlebar coordinates remain unchanged.
- `crates/riwork-nsh-native/src/service.rs`: retain M5's disabled session
  fallback for empty inventories and Phase 3a's independent worktree-presence
  deadline. An empty home has no periodic deadline; worktrees alone arm only
  presence checks, and shells arm the session fallback. CRUD transitions wake
  and update these flags immediately. Presence-only checks do not refresh Tabs,
  preventing indirect tmux sampling. Watcher and shell changes still refresh
  shared Tabs, and transient failures retain last known inventory state.
- `apps/riwork/Workspace.tsx`: retain M5's stable `dockReady` subtree, keyboard
  aliases, divider accessibility label and deferred surfaces while keeping the
  real panels, controller, editors, toolbar and geometry refs. The implemented
  panel Cmd+F search takes precedence over M5's deferred search no-op; the other
  deferred chords remain inert. Deferred views keep M5's mount/visibility proof.
- `apps/riwork/App.test.tsx`: combine the measured drag source/destination with
  M5's mount, visibility and resize assertions, using Files as the deferred
  identity probe alongside the functional panels.

Automatic merge changes also preserve M5's CLI `serde_json/preserve_order`
feature and frozen settings-order golden. No dependency version was changed by
the resolution.

The native regression suite now proves that worktree presence refreshes without
restarting the empty session fallback or sampling Tabs, and that a fully empty
home performs no presence refresh during M5's 60-second quiet check.

## Commands and results

Scratch root: `/tmp/riwork-nsh-p3a-merge-500f21fc.kJ8tV0`.
Existing isolated build/cache directories are reused from the original panel
verification; all domain state and runtime fixtures are fresh.

`env.sh` sets:

```sh
export NSH_MERGE=/tmp/riwork-nsh-p3a-merge-500f21fc.kJ8tV0
export NSH_CLI=/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh
export RIWORK_HOME=$NSH_MERGE/home
export RIWORK_RUNTIME_DIR=$NSH_MERGE/runtime
export CARGO_TARGET_DIR=/tmp/riwork-nsh-p3a-500f21fc.FuPHBM/target
export NSH_CACHE=/tmp/riwork-nsh-p3a-500f21fc.FuPHBM/nsh-cache
export TMPDIR=$NSH_MERGE/tmp
export RUSTUP_TOOLCHAIN=1.99.1
export CARGO_BUILD_JOBS=4
export CARGO_NET_OFFLINE=true
```

Host: macOS 26.2 (25C56), arm64; Rust/Cargo 1.99.1. CLI:
`nsh 0.3.0 (76fa8a7)`, SHA-256
`90b7633c80306407fc97b2051be00af868d4577281f918ff655c650a8a5a2e60`.

`git merge --no-commit nsh` initially exited 1 for the four conflicts above.
After resolution, these exact verification commands from the assigned worktree
all exited 0:

```sh
source /tmp/riwork-nsh-p3a-merge-500f21fc.kJ8tV0/env.sh
cargo test --locked --offline -p riwork-nsh-native -p riwork-nsh-workspace -p riwork-desktop --jobs 4
cargo run --locked --offline -p riwork-nsh-workspace --example seed_fixture -- "$RIWORK_HOME"
"$NSH_CLI" check apps/riwork --tsc '/Users/dominik/Library/Caches/nsh/typescript-7.0.2-darwin-arm64/package/lib/tsc'
"$NSH_CLI" test apps/riwork --junit "$NSH_MERGE/nsh-headless.xml"
sh apps/riwork/check-polling.sh
cargo fmt -p riwork-nsh-native -p riwork-nsh-workspace -p riwork-desktop -- --check
git diff --cached --check
```

| Check | Result | Evidence under scratch root |
|---|---|---|
| Full requested Cargo packages | 105 passed, 0 failed, 0 ignored: desktop 42, native 21, workspace 42 | `cargo-tests.log` |
| CLI settings insertion-order golden | Passed as part of desktop's six CLI tests | `cargo-tests.log` |
| Unshown AppKit chrome/menu fixtures | Seven passed, including titlebar coordinates | `cargo-tests.log` |
| Native empty inventory / independent presence | All six service tests passed, including M5's quiet check | `cargo-tests.log` |
| NSH check | Rust check/build and TypeScript passed | `nsh-check.log` |
| NSH headless | 27 passed, 0 failed (18.9 s), including both panel workflows and M5 idle/keyboard/edit/identity coverage | `nsh-test.log`, `nsh-headless.xml` |
| Fixture seed | Fresh core-written layout created | `seed-fixture.log` |
| Polling guard, formatting, staged whitespace | Passed | Commands above |

NSH's existing informational warning about native exports without doc comments
remains; it does not fail checking or tests. The original panel scope boundaries
and gated terminal placeholders are unchanged. Task status remains with the
orchestrator.
