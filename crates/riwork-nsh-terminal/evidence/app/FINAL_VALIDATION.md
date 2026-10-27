# Phase 2a app/bridge validation

`verification_environment: isolated_RIWORK_HOME_headless_tests_and_code_review`

Worktree: `/Users/dominik/orca/projects/riWork-nsh-p2-terminals`.
All commands below ran from that directory. No desktop app was launched by this
worker, no Cua input or inspection was performed, and no live RiWork state was
used. HOME/toolchain caches were inherited; RIWORK_HOME and the headless runtime
root were explicit disposable fixtures. The shared Cargo target was authorized
by the parent worker. These results do not substitute for the parent's native
pressure, two-window or Cua acceptance evidence. No commit was made by this worker.

## Exact command arguments and isolation overrides

Common environment and executable paths used by the commands:

```sh
export CARGO_TARGET_DIR=/Users/dominik/orca/projects/riWork-nsh-p2-terminals/target
export NSH_CACHE=/tmp/riwork-p2-terminal-bridge/nsh-cache
export RIWORK_HOME=/tmp/riwork-p2-terminal-bridge/state
NSH=/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh
TSC=/Users/dominik/Library/Caches/nsh/typescript-7.0.2-darwin-arm64/package/lib/tsc
```

The state directory was created before tests. The following are the final
checks; each log is retained beside this report.

```sh
cargo test -p riwork-nsh-native -p riwork-nsh-workspace --offline \
  > /tmp/riwork-p2-terminal-bridge/cargo-safety.log 2>&1
cargo test -p riwork-nsh-native --test native_headless --offline \
  > /tmp/riwork-p2-terminal-bridge/cargo-headless-owner.log 2>&1
"$NSH" check apps/riwork --tsc "$TSC" \
  > /tmp/riwork-p2-terminal-bridge/nsh-check-safety.log 2>&1
"$NSH" build apps/riwork --out /tmp/riwork-p2-terminal-bridge/bundle \
  > /tmp/riwork-p2-terminal-bridge/nsh-build.log 2>&1
```

Headless NSH app fixture preparation and focused reruns:

```sh
cargo run -p riwork-nsh-workspace --offline --example seed_fixture -- \
  /tmp/riwork-p2-terminal-bridge/headless-home \
  > /tmp/riwork-p2-terminal-bridge/seed.log 2>&1
export RIWORK_HOME=/tmp/riwork-p2-terminal-bridge/headless-home
export RIWORK_RUNTIME_DIR=/tmp/riwork-p2-terminal-bridge/runtime
"$NSH" test apps/riwork --filter 'production restore' \
  > /tmp/riwork-p2-terminal-bridge/nsh-restore-fixed.log 2>&1
"$NSH" test apps/riwork --filter 'local shell content' \
  > /tmp/riwork-p2-terminal-bridge/nsh-local-safety.log 2>&1
```

`seed_fixture` requires an empty explicit scratch destination and refuses to
overwrite an existing layout. Do not rerun it against this already seeded home.

## Results

| Check | Result | Evidence |
| --- | --- | --- |
| Cargo native + workspace | 52 passed, 0 failed: native unit 7; native headless 1; services 5; workspace unit 29; layout roundtrip 10; doc tests 0 | `cargo-safety.log` |
| Retained subscription-owner teardown regression after latest test-source change | 1 passed, 0 failed (0.66 s) | `cargo-headless-owner.log` |
| NSH generated bridge + TS check | Passed | `nsh-check-safety.log` |
| Production restore and native layout readiness | 1 passed, 0 failed (240 ms) | `nsh-restore-fixed.log` |
| Local terminal content, Detach persistence, same-identity reopen | 1 passed, 0 failed (653 ms) | `nsh-local-safety.log` |
| Native-node lifecycle regression | Failed before fix (10 mounts instead of 2), then passed; selection/resize retain identity and only explicit Detach/reopen remounts | `nsh-mount-before.log`, `nsh-mount-fixed.log` |
| Latest full NSH suite | 16 passed, 0 failed (7.7 s), fresh fixture | `nsh-mount-full.log`, `nsh-mount-final.xml` |
| Latest NSH generated bridge + TS check | Passed after mount fix | `nsh-check-mount-fixed.log` |
| Latest production bundle | Passed: 23.6 KB JS, 94.7 KB bytecode | `nsh-build-mount-fixed.log` |

NSH's warning about missing exported-item doc comments remains non-fatal. The
earlier `cargo-test.log` and `cargo-test-final.log` contain intermediate successful
runs, superseded by `cargo-safety.log`; counts must not be added together.

Bundle for the native fixture:

```text
RIWORK_NSH_BUNDLE=/tmp/riwork-p2-terminal-bridge/bundle/bundle.js
SHA256=e8c7ef467434d37a1006da57e5daa1a5a4c50919112fbca9432f44b656037a98
```

Verified with `shasum -a 256 /tmp/riwork-p2-terminal-bridge/bundle/bundle.js`.
This supersedes the initial bundle with hash
`b68e3f9bd11b5b21f43cc7479553fac8c2d01261d6c1d82dc4932a8e5b9ddd95`.
Production app/bridge source was frozen after the mount-fix rebuild. The parent
worker's broader Cargo headless suite is separate evidence and is not counted here.

The mount regression and final app rerun used the same executable/cache/target
paths above, with these exact command arguments and state overrides:

```sh
export RIWORK_HOME=/tmp/riwork-p2-terminal-bridge/headless-home
export RIWORK_RUNTIME_DIR=/tmp/riwork-p2-terminal-bridge/runtime
"$NSH" test apps/riwork --filter 'terminal native nodes' \
  > /tmp/riwork-p2-terminal-bridge/nsh-mount-before.log 2>&1
# Apply stable DockArea mount fix, then repeat the same command:
"$NSH" test apps/riwork --filter 'terminal native nodes' \
  > /tmp/riwork-p2-terminal-bridge/nsh-mount-fixed.log 2>&1
export RIWORK_HOME=/tmp/riwork-p2-terminal-bridge/state
unset RIWORK_RUNTIME_DIR
"$NSH" build apps/riwork --out /tmp/riwork-p2-terminal-bridge/bundle \
  > /tmp/riwork-p2-terminal-bridge/nsh-build-mount-fixed.log 2>&1
"$NSH" check apps/riwork --tsc "$TSC" \
  > /tmp/riwork-p2-terminal-bridge/nsh-check-mount-fixed.log 2>&1
cargo run -p riwork-nsh-workspace --offline --example seed_fixture -- \
  /tmp/riwork-p2-terminal-bridge/headless-mount-home \
  > /tmp/riwork-p2-terminal-bridge/seed-mount.log 2>&1
export RIWORK_HOME=/tmp/riwork-p2-terminal-bridge/headless-mount-home
export RIWORK_RUNTIME_DIR=/tmp/riwork-p2-terminal-bridge/runtime-mount
"$NSH" test apps/riwork --junit /tmp/riwork-p2-terminal-bridge/nsh-mount-final.xml \
  > /tmp/riwork-p2-terminal-bridge/nsh-mount-full.log 2>&1
```

## Initial failures and fixes

1. Initial `"$NSH" check apps/riwork --tsc "$TSC"` failed in `nsh-check.log`:
   NSH 0.3's generated declarations did not expose `nsh/native`. The component
   now imports `nativeComponent` from the supported `nsh` export. The frozen
   Terminal props/events/commands were unchanged; final check passed.
2. Initial `"$NSH" test apps/riwork --junit
   /tmp/riwork-p2-terminal-bridge/nsh-headless.xml` stopped with SIGABRT during
   production restore after four preceding tests passed (`nsh-test.log`). The
   focused reproduction was `RUST_BACKTRACE=1 "$NSH" test apps/riwork --filter
   'production restore'`, retained in `nsh-panic.log`. Its backtrace identified
   `Subscription::Drop -> close -> Cleanup::release -> nsh_js::reactive::graph`
   after NSH cleared its reactive context during VM finalization. Drop now only
   aborts the subscriber task; explicit JS close still releases the cleanup
   registration while its context is live. Subscriptions are created during
   render, and app startup initializes the service before mounting Workspace.
   The focused production restore rerun passed. The native headless regression
   now retains subscription handles in the owner cleanup closure and passes
   through explicit close, owner disposal and engine drop.
3. The parent's first native fixture exposed terminal disappearance after mouse
   focus. The compiled `dock() && <DockArea>` insertion rebuilt the entire
   DockArea on every new projection, defeating its internal keyed panel cache.
   A headless native-node ref observer reproduced five mounts per terminal
   across three route selections and a resize (ten instead of two total). A
   separate boolean readiness gate now mounts DockArea once; new projections
   only update its layout prop. The regression proves stable node identity
   across selections/resize/appearance and a single new mount only after an
   explicit Detach/reopen. Both the focused test and full 16-test NSH suite pass.

## Coverage and review limits

Tests cover local detach/reopen identity and destination fallback, all-locked
refusal, corrupt shared membership refusing Exit before tmux initialization or
registry mutation, asynchronous mutation publication, two-subscriber appearance
updates, and VM teardown safety. Read-only review of host/native glue found no
additional severe issue in registry installation, close/quit draining, exact-tty
detach plus child-exit freeing, stale attachment guards, claims or validated
link routing. Native rendering, production input/composition callbacks and
20-cycle session survival are measured by the separate native fixture, not
these headless checks. Physical keyboard/input-source candidate UI and display
migration remain manual/hardware coverage limits. Remote/Vim/subshell/paste/drop
remain outside this slice.
