# Phase 1b host verification

Assignment: `ddba58a0-a80b-47a0-96bd-4be509c145bc`.
Branch: `nsh-p1-host`, based on `ea0f2f568dafdac1ff1064b70f34f1791b5b57e5`.
The final commit SHA is supplied in the assignment completion message.

`verification_environment`: **isolated fixtures only**. macOS 26.2 (25C56), arm64;
Rust 1.99.1 / Cargo 1.99.1; NSH crates pinned to 0.3.0. No installed binary was
replaced, no live RiWork state/window was tested, no update/merge/push/install
command ran, and task status remains for the orchestrator.

## Implementation and boundaries

The `src/nsh_main.rs` entry point dispatches unchanged `cli::run_cli` first, with identical
error formatting and status 2. The GUI path now owns AppKit NSWindows and NSH
AppViews with independent channels and frozen schema-1 params. It implements the
window-management host calls, menus/Dock/key routing, per-home single-instance
forwarding, runtime registration/heartbeat and the restore-ready skeleton.

P1a's `crates/riwork-nsh-native` was absent at the branch base. Its frozen
`riwork/native` registry name is present as a Rust stub, with the dependency
replacement documented in Cargo.toml and README-NSH.md. P1c terminal registration
and Rust layout/draft flush services remain integration hooks. Restart refuses
explicitly until those flush hooks exist; the bootstrap cannot acknowledge a
restored layout. System integrations and the complete TSX product are not
claimed as part of this host foundation.

## Commands and results

Run all commands from the repository root.

```sh
cargo check -p riwork-desktop --offline
cargo fmt -p riwork-desktop -- --check
python3 crates/riwork-desktop/verification/run_headless.py
python3 crates/riwork-desktop/verification/gui_acceptance.py \
  --binary target/debug/riwork --linger-seconds 120
cargo tree -p riwork-desktop --offline --edges normal
```

Results:

- `cargo check -p riwork-desktop --offline`: exit 0.
- `cargo fmt -p riwork-desktop -- --check` and `git diff --check`: exit 0.
- `run_headless.py`: **1,669 passed, 0 failed, 187 ignored**, exit 0 across
  26 test groups. Log: `/private/tmp/rwnsh-tests-yd1ouyr4/tests.log`; exact
  environment: `evidence/headless-manifest.json`.
- Final `cargo test -p riwork-desktop --offline`, using that same isolated
  environment and cleared signal mask after the coordinate-conversion and
  explicit `src/nsh_main.rs` target changes: **13 unit tests + 5 process tests
  passed**, exit 0. Log copied to `evidence/final-host-tests.log`.
- GUI fixture: **passed**, both parameter identities and host calls verified;
  graceful quit exited 0 and removed registration/socket.
- Dependency tree: **no GPUI/Ghostty crate**; Cargo metadata confirms exactly
  one binary target, named `riwork`, at `src/nsh_main.rs` (`autobins = false`).

The runners retain a manifest with exact argv/environment and logs in their
printed fixture directory. `run_headless.py` runs:

```sh
cargo test -p riwork-core -p riwork-engine -p riwork-cli -p riwork-desktop --offline
```

It clears the automation environment's inherited blocked signal mask, then uses
an isolated HOME under the ignored target directory and isolated
RIWORK_HOME/RIWORK_RUNTIME_DIR under `/tmp`. This is required by existing tests:
blocked SIGTERM prevented fixture chat-host shutdown, and putting HOME itself
under temp_dir made the existing trust-safety guard treat it as a permitted test
path. Neither issue required changes to CLI/core/engine source. Initial failing
runs are retained at `/tmp/rwnsh-p1-6yi869qx/tests.log` and `headless-tests.log`.

The new executable's process tests compare stdout, stderr and exit code
byte-for-byte to the unchanged `riwork_cli::cli::run_cli` dispatcher for help
aliases, version/errors, real fixture project lists (text/JSON), MCP ping and
chat listing. The same executable also runs a real fixture chat host and exits
on its idle deadline. Poisoned GUI bundle/restore settings prove these paths do
not initialize AppKit, a desktop lock, a GUI runtime registry or NSH caches.

## GUI evidence

Successful full capture fixture: `/private/tmp/rwnsh-dkounh87`.
Process PID `4106`, one process with two windows:

| Kind | Window UUID | Native window |
|---|---|---|
| Project | `1ceb6944-668e-40fe-8ecf-86fa30e48af7` | `6187` |
| Global | `8e6ad77d-d933-4c87-b0ad-271e40f2b545` | `6188` |

Both AppViews loaded the same source bundle. `host.params` distinguished project
id/path from the null-project global window. The fixture asserted global-window
creation and singleton reuse, list identity, placement, and routed event delivery.
It posted `app.quit`, exited 0, and removed both the runtime registration and
socket. Its timers are one-shot fixture lifetime timers, not product polling.

RiWork's **cua-driver MCP** inspected only PID 4106. Both permission preflights
were granted. `list_windows({pid:4106})` found exactly the two fixture windows;
`get_window_state({pid:4106,window_id:6187})` and `6188` verified the rendered
identities. No desktop input was sent. Screenshots and raw fixture evidence are
in [evidence](evidence/), including `cua-proof.json`, both PNGs, `manifest.json`,
`bridge-records.json`, `registry.json`, and unchanged help/project-list outputs.

A second invocation of the same binary/project with the same isolated home and
`RIWORK_NSH_BUNDLE=/this-bundle-must-not-be-read-by-forwarder.js` exited 0 with
empty stdout/stderr, proving forwarding happened before bundle loading. Exact
argv/result: `evidence/forwarding.json`.

Final-source GUI rerun after the runtime-coordinate fix and explicit binary target:

```sh
python3 crates/riwork-desktop/verification/gui_acceptance.py \
  --binary target/debug/riwork --linger-seconds 5
```

Result: **passed**, PID `81839`, fixture `/private/tmp/rwnsh-qcwucday`, exit 0,
unregistered and socket removed. Its exact params and converted runtime geometry
are retained in `evidence/final-manifest.json`, `final-bridge-records.json`, and
`final-registry.json`. The earlier screenshots remain proof of the same rendered
window identities/host path; the final rerun verifies the final source changes.

## Commit scope

Only `crates/riwork-desktop/**` is committed. The old GPUI main is retained as
`src/main.rs`, uncompiled, for the remaining parity work. The new executable's
normal dependency tree contains no GPUI or Ghostty crate. Cargo's temporary
repository-root lock resolution is excluded and restored; the integration owner
must regenerate the root Cargo.lock when combining the NSH tasks.
