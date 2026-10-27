Task 40e6ac8b-b083-4798-b9b0-3624b0fed5c3
Branch: nsh-p1-workspace
Commit: ec59ee5383bc0921ec089ba68807aaa7015ad80e
verification_environment = isolated /tmp RIWORK_HOME + RIWORK_RUNTIME_DIR fixtures, Rust tests, and NSH 0.3.0 headless engines; no live windows or desktop interaction.

Implemented in apps/riwork (independent mixed Rust/TSX workspace):
- Controlled DockArea shell, tab/pane view model, title/status bars, tab shortcuts, pane activation, lock/main menus, gather, and sticky notices.
- ProjectLayout remains authoritative. Every DockArea callback is replayed in Rust, normalized by riwork-core, and projected back. TSX never saves DockAreaLayout or installs an unvalidated reported tree.
- Binary/n-ary mapping, core geometry and locked resize, empty/opaque-pane sentinels, gesture classification, selection/reorder/move/edge split/removal, logged compound fallback, transactional invalid-input refusal, and ID aliases with allocator headroom.
- LayoutStore is the sole production writer. Reads and trailing 250ms writes run off engine threads; flush/drop/project-switch persistence is tested. A weak process registry exposes atomic snapshots and flush handles for P1b without a JS round trip.
- Isolated GPUI-equivalent fixtures are written using the same riwork-core LayoutStore APIs as GPUI. Tests preserve empty/locked panes, unknown tab kinds and top-level fields, every saved tab variant, detachment/selection metadata, narrow ratios, newer schemas and unreadable entries.

Integration and scope limits:
- P1a is not integrated into this branch. Its frozen package name is riwork-nsh-native and its one native specifier is riwork/native, confirmed in docs and the sibling worktree. This app temporarily exports that specifier itself. P1a should invoke riwork_nsh_workspace::register_layouts in its existing registry; do not install both registries. process_home deliberately requires an explicit RIWORK_HOME while the P1a service owner is pending.
- P1b still needs to call host_layouts::lookup(windowId), stop accepting edits, flush on its blocking path, and capture snapshot() before close/quit/restart. The host handle is implemented and unit-tested; this task did not modify or run P1b.
- Shared-membership/session reconciliation and production Store/Sessions subscriptions remain P1a integration work. The shell currently uses the contract's SavedPane order fallback. Actual terminal/chat/navigation content remains a placeholder for later lanes. The title currently derives its label from host.params.path until Store supplies project/remote metadata. Global windows intentionally have no DockArea.
- This is a shell/mapping implementation, not a claim that every P-SHELL row is finished. Full host menus, focus-mode presentation, production tab-close flows, Default Layout command, settings-backed status items/themes, pending-chat identity flows, and project-switch UI are not added here. Core default-layout initialization, gather, and persistence project-switch behavior are implemented/tested.
- Structural/core-geometry parity is proved; exact renderer pixel parity is not. NSH 0.3 DockArea uses an overlaid 1pt handle without core's 5pt layout gap and always makes the last visible split child flexible. Its global tab-header styling also lacks per-group traffic-light inset. The shell uses a separate 28pt title bar. These upstream presentation differences remain, as do frozen-contract root-edge drop-zone gaps.
- The unchanged LayoutStore keeps opaque unknown tabs in a private map keyed by original disk PaneId. A JS-unsafe pane containing opaque tabs therefore retains its original disk ID on save; it uses a safe session alias. Ordinary unsafe IDs persist the safe remap. Already-safe IDs above u32::MAX also get session aliases to avoid poisoning NSH's allocator, but retain their original disk IDs. Boundary and new-ID collision tests cover this intentional compatibility exception.

Toolchain and isolation:
macOS 26.2 (25C56), arm64; Rust/Cargo toolchain 1.99.1.
CLI: /tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh
Version: nsh 0.3.0 (76fa8a7)
CLI SHA-256: 90b7633c80306407fc97b2051be00af868d4577281f918ff655c650a8a5a2e60
Matches docs/nsh-toolchain.md. No installer was run.
NSH cache, main Cargo target, TMPDIR, RIWORK_HOME, runtime directory, logs and JUnit are under /tmp/riwork-nsh-p1c.nUb7G7.
Generated .nsh files and an early app-local Cargo target are ignored, not committed. Repository root Cargo.toml, Cargo.lock and rust-toolchain.toml were unchanged.

Exact successful commands (cwd /Users/dominik/orca/projects/riWork-nsh-p1-workspace):

source /tmp/riwork-nsh-p1c.nUb7G7/env.sh
# env.sh sets NSH_CLI to the isolated path above; NSH_CACHE=$NSH_P1C/nsh-cache;
# CARGO_TARGET_DIR=$NSH_P1C/target; RIWORK_RUNTIME_DIR=$NSH_P1C/runtime;
# TMPDIR=$NSH_P1C/tmp; RUSTUP_TOOLCHAIN=1.99.1; CARGO_BUILD_JOBS=4.

"$NSH_CLI" check apps/riwork --tsc '/Users/dominik/Library/Caches/nsh/typescript-7.0.2-darwin-arm64/package/lib/tsc' > "$NSH_P1C/nsh-check-final.log" 2>&1
Result: exit 0; checked riwork-nsh-workspace (122ms), built Rust crate (139ms), TS check passed.

export RIWORK_HOME="$NSH_P1C/home2"
cargo run --manifest-path apps/riwork/Cargo.toml --locked --offline --example seed_fixture -- "$RIWORK_HOME" > "$NSH_P1C/seed-fixture2.log" 2>&1
Result: exit 0; GPUI LayoutStore fixture written to /private/tmp/riwork-nsh-p1c.nUb7G7/home2 for fixture-project.
The seed helper refuses overwriting an existing layouts.json. Choose a fresh /tmp home when reproducing.

"$NSH_CLI" test apps/riwork --junit "$NSH_P1C/nsh-headless2.xml" > "$NSH_P1C/nsh-test2.log" 2>&1
Result: exit 0; 47 passed, 0 failed (5.9s): 24 Rust mapping tests, 1 host-registry test, 10 Rust roundtrip/persistence tests, 6 TSX UI tests, 6 TS model tests.
Includes real NSH PaneTree operations for every gesture class, projection/core geometry at 1280x800/1920x1080/640x400, two headless window engines, an actual headless DockArea drag into an opaque empty pane, selection/keyboard/menu/resize flows, and global routing.

cargo test --manifest-path apps/riwork/Cargo.toml --locked --offline --test layout_roundtrip
Result: exit 0; 10 passed, 0 failed. Log: layout-roundtrip-locked.log.

cargo test --manifest-path apps/riwork/Cargo.toml --lib layout::tests --offline
Result: exit 0; 24 passed, 0 failed, 1 host test filtered. Included again in the final nsh test.

cargo fmt --manifest-path apps/riwork/Cargo.toml --check
git diff --check
git diff --cached --check
Result: each exit 0. Formatting log: fmt-check.log.

Post-test Python assertions (preservation-check.log) compared all SHA-256 hashes recorded in user-nsh-before.json plus the ~/.local/bin/nsh symlink, and inspected home2/layouts.json after the real UI drag.
Result: all 49 ~/.nsh files and symlink unchanged; schema 1, futureTopLevel, future_browser tab, locks {1,3}, main pane 2 survived; shell:first was saved in pane 3; no empty sentinel persisted.

/Users/dominik/.nsh/bin/nsh --version
Result: exit 0; nsh 0.2.0 (68531fb), unchanged.

shasum -a 256 /tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh
Result: exit 0; SHA-256 shown above.

git commit -m "Add NSH Dock workspace shell and core layout mapping"
Result: exit 0; ec59ee5383bc0921ec089ba68807aaa7015ad80e; 19 app/mapping/test files only.
git status --short --branch
Result: ## nsh-p1-workspace; clean worktree.

Exploratory failures resolved before the final checks: a reserved Rust identifier; Cargo lock/manifest races during parallel implementation; a TypeScript host.params assertion requiring an intermediate unknown cast; fixture entry capabilities needed their own tests/nsh.json because NSH loads capabilities beside the entry. The first UI run recorded 39/44 passing due to that fixture grant; final isolated rerun passed all 47. Earlier logs remain alongside final evidence.

No merge, push, install, live application launch, or task-status update was performed. Task status remains the orchestrator's responsibility.
