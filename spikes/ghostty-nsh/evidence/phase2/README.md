# Phase 2 evidence

The pinned-runtime retry is **GO under the assigned causal-profiling criterion**:
20×4, 80/80 free exactly once, zero remaining surfaces/clients/descendants, and
unchanged tmux identity. Closing gaps peak at 18.179 ms. The one global startup
interval of 215.880 ms is directly attributed to creation before any teardown;
the raw strict all-lifetime metric remains false. See
[pinned/README.md](pinned/README.md) and the current worker report.

The earlier cached-runtime entry gate is **NO-GO**. `cached/verification-summary.json` reports five
responsive-main-loop failures (156.212 ms maximum versus the frozen <100 ms
limit). The final 20 batches × 4 surfaces otherwise pass detach/child-exit
ordering, <2 s disposal, process/client cleanup and session/pane survival.

- `pinned/`: counted pinned retry, fixture PID 63191/window 6288, with full
  timestamped gap profiling, 20×4 readbacks, host exit and cleanup.
- `pinned-runtime/`: reused pinned archive verification, fixture build/link
  evidence and source/artifact hashes for the executed instrumented fixture.
- `cached/`: historical cached runtime capture, fixture PID 43353/window 6209,
  plus exact runtime/binary hashes, driver requests/results and cleanup.
- `host-wakeup-trial/`: preserved excluded trial. All surfaces freed, but the
  final host-exit readback failed before the AppKit wake-event fix. `note.json`
  and `host-stop-sample.txt` distinguish that failure from the Phase 1 join hang.
- `runtime/`: source/cache audit, upstream issue/PR snapshots, private pinned
  archive rebuild provenance, and both final fixture build/link logs. The
  original rebuilt fixture was never launched in that earlier assignment.
  The retry uses the same verified archive with a newly instrumented fixture.
- `source-sha256.json`, `pressure-summary.json`, `final-checks.json` and
  `excluded-preflight-cleanup.json`: source identification and cross-run checks.

Recompute analysis without desktop or tmux access:

```sh
python3 spikes/ghostty-nsh/scripts/verify_teardown.py spikes/ghostty-nsh/evidence/phase2/pinned --profile-gaps
# Expected exit 0: GO; raw startup exceedance remains explicitly recorded.
python3 spikes/ghostty-nsh/scripts/verify_teardown.py spikes/ghostty-nsh/evidence/phase2/cached
# Expected exit 1: NO_GO, solely the responsive-main-loop checks.
```

Raw native timing and Python capture timing have different clock domains.
The verifier correlates explicit surface IDs and capture postconditions;
it does not invent offsets. JSON summaries are derived analysis. No raw
native/client/cycle capture was rewritten to obtain a passing result.

Full interpretation and scope: [WORKER_REPORT.md](../../WORKER_REPORT.md).
