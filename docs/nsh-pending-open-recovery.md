# Pending App open recovery — task 82b54c81

The App now recovers a pending layout open rejected during a cancelled quit or
restart. The host reopens every native layout gate before notifying each surviving
window with `window.closeCancelled`, including windows that had no registered
layout when closing began. App serializes retries, remembers cancellation delivered
before the rejected promise settles, and retries only the closing-marker error.
Successful teardown retains its native closing marker and emits no cancellation.

Work started by running `git merge --no-edit nsh` on `nsh-p1-fix-host`: clean
fast-forward from `adea72c` to `fd58edc`. Source review covered `App.tsx`,
`src/host_layouts.rs`, the native `Layouts.openSession` implementation, and the
host's close/quit/restart flush and cancellation paths before implementation.
The native registration guard remains unchanged.

- `service_tier`: default (Fast off), retaining the user-configured session tier;
  no tier, model, effort, or account changes and no descendant sessions spawned.
- `verification_environment`: isolated fixtures only.
- `live_targets_touched`: none beyond own shell/worktree.
- No desktop process or window was launched, inspected, closed, reloaded or
  restarted. No install, push, merge back to `nsh`, or task-status change.
- Existing pinned NSH/TypeScript tools, Cargo cache and Ghostty link artifacts
  were reused. Each run has private `HOME`, `RIWORK_HOME`, runtime, config and
  tmux paths; the regression uses fake layout persistence and session inventory.

The fixture imports the production `App.tsx` into a windowless NSH engine with
the real native Layouts registry and `ClosingWindows` flush/cancel logic. A second
window's fake writer fails its flush. The fixture never manually retries
`Layouts.openSession`: only App can produce the second repository open and
`app.restoreReady`. It substitutes host-event delivery for the AppKit dispatcher;
it does not execute a live quit or restart command.

| Check | Result |
|---|---|
| Pre-fix App bundle, same recovery harness | Expected failure: `waiting for app.restoreReady: timed out waiting on channel`; Cargo exit 101 |
| Failed quit, cancellation after / before JavaScript rejection | Both pass: App opens exactly twice total, restores once, ignores duplicate cancellation |
| Failed restart, cancellation after / before JavaScript rejection | Both pass with the same assertions |
| Successful quit / restart while open is pending | Both pass: no session resurrection, writer, write or ready acknowledgement |
| Successful teardown after each recovered open | No remaining native session or writer |
| Host suite | 47 passed; 1 fixture test ignored here and explicitly run above |
| Workspace/layout suites | 168 passed across unit and integration suites; doc tests passed |
| NSH type check, Rust formatting, whitespace check | All exit 0 |

Exact runner commands (from the assigned worktree):

```sh
python3 crates/riwork-desktop/verification/phase1_pending_open.py regression \
  --root /tmp/riwork-pending-open-82b54c81 \
  --target-dir /tmp/riwork-w3-c62ea465/target \
  --ghostty-artifacts /Users/dominik/orca/projects/riWork-nsh-p2-terminals/crates/riwork-nsh-terminal/.runtime/pinned/install \
  --bundle /tmp/riwork-pending-open-82b54c81/regression-xo44kgbj/bundle/bundle.js \
  --expect-regression-failure

python3 crates/riwork-desktop/verification/phase1_pending_open.py regression \
  --root /tmp/riwork-pending-open-82b54c81 \
  --target-dir /tmp/riwork-w3-c62ea465/target \
  --ghostty-artifacts /Users/dominik/orca/projects/riWork-nsh-p2-terminals/crates/riwork-nsh-terminal/.runtime/pinned/install

python3 crates/riwork-desktop/verification/phase1_pending_open.py suites \
  --root /tmp/riwork-pending-open-82b54c81 \
  --target-dir /tmp/riwork-w3-c62ea465/target \
  --ghostty-artifacts /Users/dominik/orca/projects/riWork-nsh-p2-terminals/crates/riwork-nsh-terminal/.runtime/pinned/install
```

All three runners exited 0. Their subprocess commands were:

```sh
/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh build apps/riwork/PendingOpen.fixture.tsx --out /private/tmp/riwork-pending-open-82b54c81/regression-ahwwnk7u/bundle
cargo test -p riwork-desktop --bin riwork pending_open_tests::production_app_recovers_pending_open_after_failed_quit_and_restart --locked --offline -- --ignored --nocapture --test-threads=1
cargo test -p riwork-desktop --bin riwork --locked --offline
cargo test -p riwork-nsh-workspace --locked --offline
/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh check apps/riwork --tsc /Users/dominik/Library/Caches/nsh/typescript-7.0.2-darwin-arm64/package/lib/tsc
cargo fmt -p riwork-desktop -p riwork-nsh-workspace --check
git diff --check
```

The [artifact index](../crates/riwork-desktop/verification/pending-open-evidence/artifact-index.json)
records SHA-256 hashes of unmodified manifests and raw outputs. Manifests include
exact subprocess arguments, isolated environment paths, timings and exit codes:

- [Pre-fix failure](../crates/riwork-desktop/verification/pending-open-evidence/regression-opr738p9/manifest.json)
- [Fixed App: all six scenarios](../crates/riwork-desktop/verification/pending-open-evidence/regression-ahwwnk7u/app-recovery.stdout)
- [Full suites and checks](../crates/riwork-desktop/verification/pending-open-evidence/suites-edfniyj5/manifest.json)

The pre-fix bundle was built before changing App, from the one-shot open at
`fd58edc` (App source SHA-256
`b020eeedd82e92db55e5690bd8e16d1bfe9a4d49a1073161eacd499e077870e4`;
bundle SHA-256
`68f80157d6bc56642f9c0f92144bc16f34a16ba49917a038c92f11df2aa79739`).
The first harness compile exposed a missing `HashMap` import; that superseded
run's raw evidence is retained as `regression-xo44kgbj` and is not counted as a
reproduction. After fixing the import, `regression-opr738p9` used the same saved
bundle and reached the intended runtime timeout. Its source hashes describe the
working tree at test time; its separate bundle hash identifies the old App.
The fixed bundle SHA-256 is
`8d891043b281213bc7c92bd8b47088c391b334c84a549b9ec694173518bf050f`.
Fixture state, bundles, executables and caches are excluded from the archive.
The archive's local Git whitespace attribute permits Cargo's trailing blank line
in raw `.stdout` files so evidence bytes and hashes remain intact.
