# Phase 2 evidence

The entry gate is **NO-GO**. `cached/verification-summary.json` reports five
responsive-main-loop failures (156.212 ms maximum versus the frozen <100 ms
limit). The final 20 batches × 4 surfaces otherwise pass detach/child-exit
ordering, <2 s disposal, process/client cleanup and session/pane survival.

- `cached/`: authoritative final runtime capture, fixture PID 43353/window 6209,
  plus exact runtime/binary hashes, driver requests/results and cleanup.
- `host-wakeup-trial/`: preserved excluded trial. All surfaces freed, but the
  final host-exit readback failed before the AppKit wake-event fix. `note.json`
  and `host-stop-sample.txt` distinguish that failure from the Phase 1 join hang.
- `runtime/`: source/cache audit, upstream issue/PR snapshots, private pinned
  archive rebuild provenance, and both final fixture build/link logs. The
  rebuilt fixture was never launched after the final NO-GO.
- `source-sha256.json`, `pressure-summary.json`, `final-checks.json` and
  `excluded-preflight-cleanup.json`: source identification and cross-run checks.

Recompute analysis without desktop or tmux access:

```sh
python3 spikes/ghostty-nsh/scripts/verify_teardown.py spikes/ghostty-nsh/evidence/phase2/cached
# Expected exit 1: NO_GO, solely the responsive-main-loop checks.
```

Raw native timing and Python capture timing have different clock domains.
The verifier correlates explicit surface IDs and capture postconditions;
it does not invent offsets. JSON summaries are derived analysis. No raw
native/client/cycle capture was rewritten to obtain a passing result.

Full interpretation and scope: [WORKER_REPORT.md](../../WORKER_REPORT.md).
