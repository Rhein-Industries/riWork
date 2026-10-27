# Phase 1a verification

Task: `5c4cb942-905d-487c-9ba8-f562a40d1690`

Branch: `nsh-p1-native`, based on `ea0f2f5`

Date: 2026-10-27

`verification_environment = isolated RIWORK_HOME fixtures and two windowless NSH QuickJS engines; no desktop UI`

Host: Apple silicon (`aarch64-apple-darwin`), macOS 26.2 (25C56).
Toolchain: `rustc 1.99.1 (58cf8f9c3 2026-10-13)`,
`cargo 1.99.1 (5f94df478 2026-08-27)`.

Scratch root: `/tmp/riwork-nsh-native-5c4cb942.nN0f0k`.
The commands below ran from the assigned worktree. Headless engine fixtures
create disposable child homes beneath the supplied `RIWORK_HOME`; other tests
pass independent temporary homes directly to the Rust adapters. No test changes
the process environment, starts tmux, launches a platform host or uses live
RiWork data/windows. Session inventories are injected fakes. Production
`SessionManager` adapter integration is compiled; live tmux behavior is not
claimed by these tests.

## Commands and results

```sh
RIWORK_HOME=/tmp/riwork-nsh-native-5c4cb942.nN0f0k/riwork-home CARGO_TARGET_DIR=/tmp/riwork-nsh-native-5c4cb942.nN0f0k/target cargo check -p riwork-nsh-native --jobs 4 > /tmp/riwork-nsh-native-5c4cb942.nN0f0k/cargo-check.log 2>&1
```

Exit 0; initial crate compilation completed in 1m 04s. Exact direct NSH pins:
`nsh-core =0.3.0`, `nsh-base =0.3.0`; headless test dependency
`nsh-engine =0.3.0`. Lockfile comparison against the base found 118 added package
versions and zero removed existing package versions; existing workspace package
versions were preserved. The root workspace already includes `crates/*`.

```sh
RIWORK_HOME=/tmp/riwork-nsh-native-5c4cb942.nN0f0k/riwork-home CARGO_TARGET_DIR=/tmp/riwork-nsh-native-5c4cb942.nN0f0k/target cargo test --locked -p riwork-nsh-native --jobs 4 > /tmp/riwork-nsh-native-5c4cb942.nN0f0k/cargo-test-locked.log 2>&1
```

Exit 0 on the final committed source:

```text
Finished `test` profile [unoptimized] target(s) in 0.30s
unittests src/lib.rs:       5 passed; 0 failed (0.01s)
tests/native_headless.rs:  1 passed; 0 failed (0.58s)
tests/services.rs:         3 passed; 0 failed (4.12s)
Doc-tests:                0 tests;  0 failed
```

The headless test registers the real native registry and real core store adapter
in two NSH engines. An external `Store::create_project_folder` write reaches
both callback listeners with the same increasing revision. It also verifies
async Store/Sessions reads, quiet active subscriptions requesting no further
frames, explicit close, disposal of the rendering owner while JS retains its
handles, active engine shutdown, and continued service delivery afterward.

Other tests cover bounded queue overflow/resync, no delivery for equal data,
camelCase DTOs without persistence changes, constructor recovery, blocking-pool
thread boundaries, watcher-triggered session delivery, initial session
unavailability/recovery, and one two-second sampler shared by multiple listeners.

The initial `cargo test -p riwork-nsh-native --jobs 4` run exited 101 after its
five unit tests passed: the headless fixture had omitted the host capability
names used to collect assertions. The fixture now grants only those exact
names. The targeted rerun below exited 0 (1 passed, 0 failed, 0.59s), followed by
the complete successful locked run above.

```sh
RIWORK_HOME=/tmp/riwork-nsh-native-5c4cb942.nN0f0k/riwork-home CARGO_TARGET_DIR=/tmp/riwork-nsh-native-5c4cb942.nN0f0k/target cargo test --locked -p riwork-nsh-native --test native_headless --jobs 4 > /tmp/riwork-nsh-native-5c4cb942.nN0f0k/cargo-native-headless.log 2>&1
cargo fmt -p riwork-nsh-native -- --check
git diff --check
git diff --cached --check
```

All final formatting/whitespace checks exited 0. No `nsh test` CLI run was needed;
Cargo tests execute the actual NSH engine and callbacks headlessly.

Only this crate and `Cargo.lock` are part of the change. No desktop crate, TSX,
frozen contract or toolchain baseline edits; no merge, push, app installation or
task-status update. Chat is outside this phase and is untouched.
