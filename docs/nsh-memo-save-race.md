# Memo delayed-receipt follow-up

Assignment: `6079db58-d912-4dba-8398-d6fb9bfee175`.

```yaml
branch: nsh-p3b-files
merged_base: fd58edcd51e57ba5b2e2cc736f0c9b71cb09b5e8
effective_service_tier: default
fast: off
model_effort_account: unchanged
verification_environment: isolated temporary fixture repositories and RIWORK_HOME/RIWORK_RUNTIME_DIR; windowless NSH 0.3.0 engines; macOS 26.2 arm64; Rust/Cargo 1.99.1
live_targets_touched: none
```

`git merge nsh` fast-forwarded this branch from `f67f88b` to `fd58edc` before
changes. No merge to `nsh`, push, install, reload, live window or live RiWork
state was used. The review subagent inherited default tier with Fast off; no
model, effort, account or tier settings were changed.

## Source finding and change

The current `MemoEditor` compared new input with the last acknowledged text.
After saved A, an in-flight B save and an edit back to A, that comparison cleared
`dirty`. B's successful receipt then changed `saved` to B without recomputing
`dirty`, stopping the autosave loop and reporting saved while the input still
showed A and the store contained B.

`apps/riwork/Files.tsx` now keeps edits dirty while a save is outstanding and
recomputes dirty against every successful receipt after applying any authoritative
normalization. This queues A after B's receipt and retains the pending status
until A's own receipt settles. The optional save callback lets the fixture control
the interleaving; production still defaults to `Store.setProjectMemo`.

Native close staging/flush is unchanged and may repair the old divergence on
close. This finding and fix concern active-editor autosave/status accounting;
they do not establish close-time data loss.

## Regression and results

`MemoReceipt.fixture.tsx` creates a real isolated project, saves A, and separates
native writes from their JavaScript receipts. The test commits B before editing
back to A, then releases B's receipt. It holds A's queued write to assert that
the input shows A, disk still has B, status remains `Saving…`, and Save is
disabled. It then commits A and releases A's receipt, checking disk/input A and
`Saved automatically` before disposal. Close-time flushing cannot supply the
successful assertion.

Before the fix, the regression failed with this captured state:

```text
memo: B; requests: [B delivered]; visible: A; status: Saved automatically
```

Evidence root: `/tmp/riwork-memo-6079db58.IPzPRL`.

| Check | Result | Evidence |
|---|---|---|
| Regression with original dirty logic | Expected failure: store B, visible A, falsely saved, no queued A | `red2/nsh-test.log` |
| Regression after fix | 1 passed, 0 failed | `green/nsh-test.log`, `green/nsh.xml` |
| Native + workspace Cargo suites | 333 passed, 1 existing ignored test | `cargo-tests.log` |
| Engine preview/containment suite | 14 passed | `engine-preview.log` |
| Full NSH check + headless suite | Check passed; 112 tests passed, 0 failed | `full/nsh-check.log`, `full/nsh-test.log`, `full/nsh.xml` |
| Formatting, polling guard, diff checks | Passed | `fmt.log`, `polling.log`, shell output |

The existing Files, memo, native close-flush and stale-revision tests remain in
the suites. No native persistence or containment code changed.

## Exact commands

All commands ran in `/Users/dominik/orca/projects/riWork-nsh-p3b-files`.
The NSH runner creates a fresh temporary home, enables only fixture backends and
asserts its isolated tmux directory is empty. It records its expanded subprocess
argv in each run's `commands.json`.

```sh
git merge nsh
# exit 0: fast-forward f67f88b..fd58edc

export MEMO_VERIFY=/tmp/riwork-memo-6079db58.IPzPRL
export RIWORK_HOME=$MEMO_VERIFY/home
export RIWORK_RUNTIME_DIR=$MEMO_VERIFY/runtime
export TMPDIR=$MEMO_VERIFY/tmp
export CARGO_TARGET_DIR=/tmp/riwork-nsh-p3a-500f21fc.FuPHBM/target
export NSH_CLI=/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh
export NSH_TSC=/Users/dominik/Library/Caches/nsh/typescript-7.0.2-darwin-arm64/package/lib/tsc
export RUSTUP_TOOLCHAIN=1.99.1
export CARGO_BUILD_JOBS=4
export CARGO_NET_OFFLINE=true

# With the regression fixture but original dirty logic:
python3 crates/riwork-desktop/verification/phase3b_files.py \
  --root "$MEMO_VERIFY/red2" --target-dir "$CARGO_TARGET_DIR" \
  --nsh "$NSH_CLI" --tsc "$NSH_TSC" --filter 'P3b memo delayed receipt'
# expected exit 1: regression reproduces

# After fixing dirty/receipt accounting:
python3 crates/riwork-desktop/verification/phase3b_files.py \
  --root "$MEMO_VERIFY/green" --target-dir "$CARGO_TARGET_DIR" \
  --nsh "$NSH_CLI" --tsc "$NSH_TSC" --filter 'P3b memo delayed receipt'
# exit 0: check + 1 regression passed

cargo test --locked --offline -p riwork-nsh-native -p riwork-nsh-workspace
# exit 0: 333 primary tests passed, 1 existing ignored test
cargo test --locked --offline -p riwork-engine file_preview::tests --lib -- --test-threads=1
# exit 0: 14 passed

python3 crates/riwork-desktop/verification/phase3b_files.py \
  --root "$MEMO_VERIFY/full" --target-dir "$CARGO_TARGET_DIR" \
  --nsh "$NSH_CLI" --tsc "$NSH_TSC"
# exit 0: check + 112 tests passed

cargo fmt --all -- --check
sh apps/riwork/check-polling.sh
git diff --check
git diff --cached --check
# each exit 0
```

The already installed pinned toolchain and disposable build cache were reused.
The checked-in runner and native module API were not modified by this follow-up.
