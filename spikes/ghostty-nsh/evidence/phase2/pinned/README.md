# Pinned Phase 2 retry

Task `e940bd0a-2981-4d88-8a53-6b2cdd06822c`: **GO under the assigned causal-profiling criterion**.
The raw 100 ms metric is unchanged. Closing gaps max 18.179 ms; the only global
exceedance is startup (215.880 ms), with 190.275 ms of directly timed creation and
zero teardown/worker overlap. See `gap-attribution.json` for exact timestamps.

- `verification-summary.json`: authoritative `--profile-gaps` gate, exact 20×4.
- `legacy-metric-summary.json`: original closing-only checks on this run.
- `main-profile.jsonl.gz`: losslessly compressed native in-memory trace, 312,175
  records, zero drops; emitted after all 80 surfaces were disposed.
- `teardown-events.jsonl`, `detach-*.json`, `cycles.jsonl`: native barrier/free
  events, exact-client readbacks, and attached/closed batch readbacks.
- `runtime.json`, `../pinned-runtime/*`: executed fixture/archive identity,
  source hashes, copied artifact provenance and build/link evidence.
- `driver-actions.json`, PNGs: RiWork Cua.ai Driver MCP fixture-only actions and
  snapshots; all 39 inputs carry explicit PID/window.
- `host-exit.json`, `launcher-exit.json`, `cleanup.json`: orderly native-window
  close, process exit 0, zero windows, and eventual exact-session cleanup.
- `verification_environment.json`, `pressure-summary.json`: isolation and load.

Recheck without launching anything:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 spikes/ghostty-nsh/scripts/verify_teardown.py spikes/ghostty-nsh/evidence/phase2/pinned --profile-gaps
PYTHONDONTWRITEBYTECODE=1 python3 spikes/ghostty-nsh/scripts/test_profile_gaps.py
```

To reproduce a new run, first provide the verified private pinned archive/header
as documented in `../pinned-runtime/artifact-provenance.json`, then build with
explicit `GHOSTTY_LIB_DIR` and `GHOSTTY_INCLUDE_DIR` pointing there. Copy the built
fixture to `.runtime/pinned/fixture`. Run `SPIKE_PRESSURE=1 scripts/prepare.sh`
from the spike directory, source its printed `fixture.env`, and run
`python3 scripts/launch_pinned.py`. The launcher asserts the fixture marker,
isolated socket, four surfaces, and pinned artifact hashes and records the PID,
binary hash and exit status. It does not send desktop input.

Use RiWork's Cua.ai Driver MCP to inspect that exact fixture PID/window. Record
batch 1 attached with `record_cycle.py "$RIWORK_HOME" PID 1 attached`; click
Close surfaces; record closed. For batches 2–19, Create → attached readback →
Close → closed readback. Batch 20 uses Create → attached → native-window close
→ `closed --expect-exit`. Stop on a failed action or readback. Preserve the
launcher exit, explicit-PID zero-window readback, native profile, helper files,
cycles, runtime and driver evidence before checking the gate. Only then clean
the exact fixture session with `scripts/cleanup.sh` and verify its processes are
absent. Never operate live RiWork or use the default tmux socket.

The observer begins inside the first NSH source. One leading partial run-loop
entry is unobserved; its exit is seq 9. No idle time is inferred there. All later
loop pairs, native scopes and worker scopes balance. The standalone profiler
reports this boundary explicitly. Its initial parser assumption was corrected
without changing this raw trace or repeating the counted run.
