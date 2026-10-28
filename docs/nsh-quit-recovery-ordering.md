# Quit recovery ordering — task a6de9001

Failed quit/restart recovery now clears the host quit gate before exposing
cancellation to surviving Apps. The service finishes its failure bookkeeping and
forwarding recovery, then queues one `Recovery::QuitFailed` message carrying the
retained closing batch. In that main-thread turn, the host clears `quitting`,
reopens native layout gates, and notifies Apps. A recovered App can therefore send
its one-shot `app.restoreReady` through production call admission successfully.

`git merge --no-edit nsh` cleanly fast-forwarded this branch from `dc4864f` to
`21c80746da885e2d6796b884a848e2ff2ec435dd` before changes. `App.tsx` and the native
close guards in `src/host_layouts.rs` are unchanged. Successful shutdown still
retains native closing markers and never emits recovery.

The source review and fix cover these paths:

| Failure path | Ordering after the fix |
|---|---|
| Quit flush/publication failure | Resume forwarding, queue one failure message, clear quit gate and recover Apps together |
| Restart flush/launch failure | Finish `fail_reload` and forwarding recovery, then the same main-thread transition |
| Replacement readiness failure | Resume forwarding, then the same main-thread transition with the retained reload batch |
| Single-close flush failure | Carry the failed batch in `Main::Closed`; clear window closing state and pending quit before cancelling the batch |
| Main-thread Close send error | Clear window closing state before native cancellation and queued notification |
| Main-thread Quit/Restart send error | Clear `quitting` before native cancellation and queued notification; these execute in one main-thread turn |

The regression uses the production App and native Layouts implementation in a
windowless engine. `Host::call` and the fixture now share `admit_host_call`,
including the real calling-window, quit-gate and native-layout-ready checks.
`Host::handle` and the fixture share `Recovery::handle`; all three service failure
paths use `complete_failed_quit`. The fixture substitutes an in-memory main
message queue and window inventory for AppKit. It deliberately holds the service
completion closure, allowing earlier queued recovery and the actual App's
readiness post to run before failure completion. No live lifecycle is exercised.

| Verification | Result |
|---|---|
| Old ordering, with the same shared admission/dispatch extraction | All three delayed-completion cases post readiness once, reject it with `unavailable: host is quitting`, then never repost. Cargo exit 101 at `lost ready acknowledgement` |
| Fixed ordering | All three cases withhold recovery while completion is blocked, then accept readiness exactly once with zero rejections |
| Original six App scenarios | All pass in both runs: both cancellation/rejection orderings for failed quit/restart, plus successful pending-open teardown |
| Successful teardown after each new recovery case | No native session or orphan writer |
| Host suite | 47 passed; the fixture test is ignored here and explicitly executed above |
| Workspace/layout suites | 168 passed; doc tests passed |
| NSH check, Rust format check, whitespace check | All exit 0 |

Old and fixed runs used identical App bundle and regression-harness hashes. The
old run preserved the original order while extracting the shared production
functions; it is not an unmodified `21c8074` binary. The
[source review](../crates/riwork-desktop/verification/quit-ordering-evidence/source-review/source-review.json)
records both host source hashes and an exact fixed-to-old source patch for
reproduction in a disposable checkout. `git apply --check` accepts that patch.

Raw, byte-preserved logs:

- [Old-fail stdout](../crates/riwork-desktop/verification/quit-ordering-evidence/ordering-akd3ufaz/app-recovery.stdout) and [stderr](../crates/riwork-desktop/verification/quit-ordering-evidence/ordering-akd3ufaz/app-recovery.stderr)
- [Fixed-pass stdout](../crates/riwork-desktop/verification/quit-ordering-evidence/ordering-ubvi41iy/app-recovery.stdout) and [stderr](../crates/riwork-desktop/verification/quit-ordering-evidence/ordering-ubvi41iy/app-recovery.stderr)
- [Suite manifest](../crates/riwork-desktop/verification/quit-ordering-evidence/suites-6c4743q1/manifest.json)
- [Artifact index and hashes](../crates/riwork-desktop/verification/quit-ordering-evidence/artifact-index.json)

The earlier `pending-open-evidence` archive is unchanged. The new archive contains
logs, manifests and source-review metadata; no fixture state, bundles or binaries.
Its local whitespace attribute preserves Cargo's raw trailing blank lines.

Exact commands, run in this worktree (the first command ran before applying the
ordering fix):

```sh
python3 crates/riwork-desktop/verification/phase1_pending_open.py ordering \
  --root /tmp/riwork-pending-open-a6de9001 \
  --target-dir /Users/dominik/orca/projects/riWork-nsh-p1-fix-host/target \
  --ghostty-artifacts /Users/dominik/orca/projects/riWork-nsh-p2-terminals/crates/riwork-nsh-terminal/.runtime/pinned/install \
  --expect-regression-failure

python3 crates/riwork-desktop/verification/phase1_pending_open.py ordering \
  --root /tmp/riwork-pending-open-a6de9001 \
  --target-dir /Users/dominik/orca/projects/riWork-nsh-p1-fix-host/target \
  --ghostty-artifacts /Users/dominik/orca/projects/riWork-nsh-p2-terminals/crates/riwork-nsh-terminal/.runtime/pinned/install

python3 crates/riwork-desktop/verification/phase1_pending_open.py suites \
  --root /tmp/riwork-pending-open-a6de9001 \
  --target-dir /Users/dominik/orca/projects/riWork-nsh-p1-fix-host/target \
  --ghostty-artifacts /Users/dominik/orca/projects/riWork-nsh-p2-terminals/crates/riwork-nsh-terminal/.runtime/pinned/install
```

All three runners exited 0; the old-ordering runner required the observed test
failure. Each manifest records exact subprocess arguments, environment and exit
codes. The test and check subprocesses were:

```sh
cargo test -p riwork-desktop --bin riwork pending_open_tests::production_app_recovers_pending_open_after_failed_quit_and_restart --locked --offline -- --ignored --nocapture --test-threads=1
cargo test -p riwork-desktop --bin riwork --locked --offline
cargo test -p riwork-nsh-workspace --locked --offline
/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh check apps/riwork --tsc /Users/dominik/Library/Caches/nsh/typescript-7.0.2-darwin-arm64/package/lib/tsc
cargo fmt -p riwork-desktop -p riwork-nsh-workspace --check
git diff --check
```

- `verification_environment`: isolated fixtures only; private `HOME`,
  `RIWORK_HOME`, runtime, config and tmux paths, fake persistence/session inventory.
- `live_targets_touched`: none beyond own shell/worktree.
- `service_tier`: default (Fast off), retaining the user-verified session; model
  `gpt-6-astra`, effort and account unchanged. No descendants spawned.
- Builds used this worktree's target directory with two Cargo jobs and offline
  dependencies. Previously prepared pinned Ghostty link inputs were read-only;
  no other worker's source or build outputs were modified or coordinated.
- No live quit/restart/teardown, GUI, network, Docker, install, reload, push,
  merge back to `nsh`, or task-status change.
