# Throwaway libghostty / NSH AppKit spike

This directory is an experiment, not a production terminal implementation. Its
fixture hosts libghostty directly in an AppKit view and registers that view with
NSH. The fixture does not depend on the `gpui` crate or create a
`gpui-libghostty::Entity`. See `WORKER_REPORT.md` for the actual result and limits
of verification; the presence of a harness is not proof that each behavior works.

## Isolated verification environment

Only run the dedicated fixture app. Never attach automation to live RiWork or
orchestrator windows. All desktop inspection and input must use RiWork's Cua.ai
Driver, with the fixture application's PID passed on every input call. Do not
switch to another desktop provider if that driver is unavailable.

Prepare a new temporary home and a separate tmux server:

```sh
spikes/ghostty-nsh/scripts/prepare.sh
# Source the exact fixture.env path printed by prepare.sh:
. /absolute/path/printed/by/prepare/fixture.env
```

`prepare.sh` creates `RIWORK_HOME` using `mktemp`, starts tmux with an explicit
`-S "$RIWORK_HOME/tmux.sock" -f /dev/null`, and launches a raw input recorder in
the `ghostty-spike` session. It never uses the user's default tmux server. It
requires an existing `tmux` and `python3`; it installs nothing.

The environment file exports these inputs to the native fixture:

| Variable | Meaning |
| --- | --- |
| `RIWORK_HOME` | Isolated fixture data directory |
| `SPIKE_COMMAND` | `/absolute/path/to/tmux -S <isolated socket> attach-session -t ghostty-spike` |
| `SPIKE_CONFIG` | Dark Ghostty config |
| `SPIKE_LIGHT_CONFIG` | Light Ghostty config |
| `SPIKE_INPUT_LOG` | JSONL evidence produced by the application inside tmux |

Launch the fixture executable from that shell as described in the report. Keep
it under the sourced environment. Both Ghostty configs disable shell integration
and differ in background, foreground, and selection colors to make a live update
visible. The fixture's command attaches to the already-running session.
The host requires the `GHOSTTY_NSH_SPIKE_FIXTURE` marker created by preparation.
Pass the plain tmux executable and arguments as `opts.command`. The pinned macOS
embedding runtime adds an `exec -l` shell wrapper. An initial GUI run with an
additional leading `exec` failed with `exec: exec: not found`; reading only the
vendored API's shell-command documentation did not reveal this cached-runtime
behavior. Both generated configs use the existing macOS Menlo font.

## Evidence collection

```sh
spikes/ghostty-nsh/scripts/snapshot.sh > "$RIWORK_HOME/before.txt"
# Use only the fixture window: type keys/composed text, click/drag, and resize.
# Exercise native detach/reattach, theme updates, and backing-scale handling.
spikes/ghostty-nsh/scripts/snapshot.sh > "$RIWORK_HOME/after.txt"
cat "$RIWORK_HOME/input-events.jsonl"
```

The recorder logs raw input bytes, decoded UTF-8, SGR mouse events, terminal
dimensions after `SIGWINCH`, and its process identity. It requests mouse button
and drag reporting, focus events, and bracketed paste. GUI input is established
by bytes arriving in this application through libghostty and tmux. A tmux
`send-keys` test only verifies the harness and is not evidence of GUI input.

`session-initial.txt` records the session ID, creation time, pane ID, and pane PID.
Compare these with snapshots around native-view detach/reattach and inspect the
recorder PID in the log. Reusing those identities proves that the tmux session
and application persisted; it does not by itself prove that the native surface
or renderer survived. Verify the reattached display and further GUI input too.

The recorder does not prove IME marked-text positioning. Composed Unicode bytes
and the native input-client callbacks are separate evidence requirements.
Similarly, a forced content-scale call is distinct from moving the fixture
between physical displays with different backing scales.

When the fixture app is closed, stop only its isolated server:

```sh
spikes/ghostty-nsh/scripts/cleanup.sh
```

Cleanup retains the environment, logs, and snapshots for the report. Do not
install, merge, push, or mark the assigned task done as part of this spike.

## Phase 2 detach-before-free reproduction

Run each runtime in a fresh isolated environment, from the repository root:

```sh
SPIKE_PRESSURE=1 spikes/ghostty-nsh/scripts/prepare.sh
# Source the exact fixture.env path printed above:
. /absolute/path/printed/by/prepare/fixture.env
unset GHOSTTY_LIB_DIR GHOSTTY_INCLUDE_DIR
cargo build --locked --manifest-path spikes/ghostty-nsh/Cargo.toml
spikes/ghostty-nsh/target/debug/riwork-ghostty-nsh-spike \
  > "$RIWORK_HOME/fixture.log" 2>&1 &
SPIKE_FIXTURE_PID=$!
export SPIKE_FIXTURE_PID
```

Pressure mode mounts four simultaneous libghostty surfaces, each attaching to
the same isolated tmux session and pane. `pressure.py` continuously writes pane
output at a nominal 4 MiB/s, including while clients are detached; actual
progress is recorded in `pressure.json`. Preparation exports `SPIKE_SURFACES=4`,
`SPIKE_PYTHON` and `SPIKE_DETACH_HELPER`, along with the isolated socket/session
and configuration paths. Keep these exports for the fixture and capture tools.

The default build selects the cached `1.3.2-dev` archive described in
[evaluation.md](evaluation.md). To repeat with the freshly rebuilt pinned
archive, stop the prior fixture, prepare and source a new pressure environment,
then set these variables before building and launching:

```sh
export GHOSTTY_LIB_DIR="$PWD/spikes/ghostty-nsh/.runtime/pinned/install/lib"
export GHOSTTY_INCLUDE_DIR="$PWD/spikes/ghostty-nsh/.runtime/pinned/install/include"
cargo build --locked --manifest-path spikes/ghostty-nsh/Cargo.toml
```

Unset both overrides when selecting the cached runtime again. The pinned
rebuild uses the already-installed Zig 0.16.0 at
`/Users/dominik/.local/share/riwork/toolchains/zig-0.16.0/zig`; it requires no new
installation. Its exact source revision, build command, isolated cache paths and
artifact hashes are in
[runtime provenance](evidence/phase2/runtime/pinned-rebuild-provenance.json).
[The cache audit](evidence/phase2/runtime/native-cache-audit.json) records the
cached/rebuilt comparison and upstream status. Builds and intermediate runtime
artifacts remain ignored under `.runtime/`.

Read RiWork Cua Driver's tool descriptions and current state before operating
the fixture. Use that server for all desktop inspection and interaction, pass
`SPIKE_FIXTURE_PID` explicitly as the target `pid` on every input call, and
never operate live RiWork/orchestrator windows. If the server is unavailable or
reports missing macOS permissions, stop desktop interaction and use RiWork's
Cua setup to resolve the reported problem.

For each of at least 20 batches per runtime, capture the four attached clients,
use the fixture's **Close surfaces** button through the driver, then capture
the closed batch:

```sh
"$SPIKE_PYTHON" spikes/ghostty-nsh/scripts/record_cycle.py \
  "$RIWORK_HOME" "$SPIKE_FIXTURE_PID" 1 attached
# Driver input to this fixture PID: Close surfaces.
"$SPIKE_PYTHON" spikes/ghostty-nsh/scripts/record_cycle.py \
  "$RIWORK_HOME" "$SPIKE_FIXTURE_PID" 1 closed
# Driver input to this fixture PID: Create surfaces. Repeat with cycle 2, etc.
```

**Close surfaces** sets the NSH mounted state to false, invoking the actual
`NativeComponent` drop. Shutdown removes the view and dispatches the exact-tty
`tmux detach-client` handshake off the main thread. The main loop continues
pumping Ghostty until the helper confirms that client is detached and
`ghostty_surface_process_exited` confirms PTY-child exit; only then does the
main thread call `ghostty_surface_free`. **Create surfaces** remounts four new
surfaces. Closing the fixture's native window starts the same handshake and
stops the application only after every surface has been freed. Use native
window close for the final batch, then record its closed state with
`--expect-exit` instead of clicking **Close surfaces**.

`record_cycle.py` only observes clients, process descendants, session/pane
identity and pressure progress; it sends no GUI or terminal input. Preserve
`cycles.jsonl`, `teardown-events.jsonl`, all `detach-*.json` helper captures and
a `runtime.json` with `runtime_label`, `archive_path`, `header_path`,
`archive_sha256`, `header_sha256` and `fixture_sha256` fields identifying the
selected artifacts. Run the read-only verifier on each runtime's
capture directory:

```sh
"$SPIKE_PYTHON" spikes/ghostty-nsh/scripts/verify_teardown.py \
  "$RIWORK_HOME" --expected-min-cycles 20 > "$RIWORK_HOME/verification-summary.json"
```

The verifier requires at least 20 complete batches of four surfaces, a
detach/child-exit barrier before every free, disposal of each surface and batch
in less than 2 s, main-thread shutdown/free work and observed loop gaps below
100 ms, zero remaining clients or fixture descendants, continuing output
pressure and unchanged session/pane identity after every close. A failed
handshake or a 2 s child-exit timeout fails the fixture; parked or leaked
surfaces never count as success. The evidence and explicit GO/NO-GO belong in
[WORKER_REPORT.md](WORKER_REPORT.md). This remains a throwaway fixture, and
successful captures do not establish a general production teardown guarantee.
