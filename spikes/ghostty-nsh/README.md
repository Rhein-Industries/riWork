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
