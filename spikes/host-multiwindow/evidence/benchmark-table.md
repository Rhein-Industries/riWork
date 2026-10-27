# AppKit NSH benchmark aggregates

Independent aggregation of the completed `benchmark-summary.json` and its 12 run logs. All 24 window streams passed the runner assertions: 18,980 bytes, 475 chunks, checksum `4f4771e9`, correct window/native identity, and nonzero streamed TextView layout. Each case contains three new fixture processes, with two windows streaming concurrently (six per-window observations). Rates below are **per window under two-window load**, not single-window process benchmarks.

Reproduction command from the spike directory: `python3 run_benchmarks.py` (defaults: `--rows 0 2400 --intervals 0 33 --repetitions 3`). This aggregation did not execute an app or repeat any benchmark.

## Replay throughput and wall time

Cells show median (minimum–maximum) across the six per-window observations. Replay wall time is the app's `elapsedMs`, from scheduling the first tick through the separate final done timer; it includes timer delays, but is not measured through GPU completion.

| History rows | Requested timer delay (ms) | Chunks/s/window | Bytes/s/window | Replay wall time (s) |
| ---: | ---: | ---: | ---: | ---: |
| 0 | 0 | 513.567 (512.692–516.942) | 20521.0 (20486.1–20655.9) | 0.925 (0.919–0.926) |
| 0 | 33 | 29.449 (29.447–29.454) | 1176.7 (1176.6–1176.9) | 16.130 (16.127–16.131) |
| 2400 | 0 | 520.526 (514.960–532.671) | 20799.1 (20576.7–21284.4) | 0.913 (0.892–0.922) |
| 2400 | 33 | 29.437 (29.427–29.452) | 1176.2 (1175.8–1176.8) | 16.136 (16.128–16.142) |

## Combined two-window completion rate

This is `950 * 1000 / (max(done.hostMs) - replayStart.hostMs)` per fixture process, with three processes per case. It measures completion of both window streams against the host stopwatch; it does not count presented frames. Cells show median (minimum–maximum).

| History rows | Delay (ms) | Combined chunks/s/process | Host replay interval (s) |
| ---: | ---: | ---: | ---: |
| 0 | 0 | 1026.796 (1025.070–1029.544) | 0.925 (0.923–0.927) |
| 0 | 33 | 58.896 (58.893–58.902) | 16.130 (16.128–16.131) |
| 2400 | 0 | 1040.633 (1029.583–1060.897) | 0.913 (0.895–0.923) |
| 2400 | 33 | 58.866 (58.853–58.892) | 16.138 (16.131–16.142) |

## Final instrumented surface and engine distributions

Each cell is the minimum–maximum of the six per-window snapshot statistics, in milliseconds. The final `perfSettled` snapshot is taken at least 250 ms after both done events. NSH retains up to 120 samples per distribution and excludes its first four frames/commits. Monitoring starts at view creation; replay starts 500 ms after both ready events. These are **final retained snapshots**, which include the idle tail and, especially for short unpaced runs, the prelude. They are not 120 active-stream frames or whole-replay distributions.

| History rows | Delay (ms) | Retained frames/window | Surface frame p50 | Surface frame p95 | Engine layout p50 | Engine layout p95 | Frames >16.67 ms/window |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 0 | 0 | 56–56 | 3.133–3.485 | 3.820–4.229 | 3.379–3.439 | 3.964–4.103 | 0–0 |
| 0 | 33 | 120–120 | 0.127–0.235 | 2.983–3.390 | 3.362–3.666 | 3.792–4.368 | 0–0 |
| 2400 | 0 | 78–79 | 3.049–3.444 | 4.171–4.694 | 3.271–3.336 | 3.846–4.054 | 0–0 |
| 2400 | 33 | 120–120 | 0.152–0.255 | 3.098–3.470 | 3.291–3.660 | 3.816–4.133 | 0–0 |

All final snapshots report a **50 Hz** display-link period. The requested comparison count above uses 1,000/60 ms (16.6667 ms); NSH's own over-budget fraction uses the observed display period instead (20 ms here). All final retained frame samples are below both limits. This establishes only the recorded surface-work costs; it does not establish that every chunk appeared as a separate frame, nor a 60 FPS presentation rate. No repeated time snapshots were added together.

`frameMs` measures instrumented main-thread surface work accumulated between display-link ticks (mount batches, animations/input, and the Core Animation transaction commit included by `Surface::drain`). `layoutMs` is separate engine-thread work: ops, component rendering, text typesetting, layout, and mount diffing. Do not add their percentile values or treat either as end-to-end latency. Sources: `nsh-engine-0.3.0/src/perf.rs` and `nsh-platform-macos-0.3.0/src/surface.rs` in the Cargo registry.

## Synchronous JavaScript/native update cost

These are the final 100 delta callbacks, timed only around native counter append, signal writes, and string slicing. They exclude asynchronous engine work, native painting, and presentation. Values are ranges across six per-window statistics. The app summary uses an upper-middle median and `floor(n * 0.95)` zero-based p95 index (the 96th sample for n=100); the native monitor uses nearest-rank quantiles.

| History rows | Delay (ms) | Last 100 sync-update median (ms) | Last 100 sync-update p95 (ms) |
| ---: | ---: | ---: | ---: |
| 0 | 0 | 0.018–0.019 | 0.025–0.031 |
| 0 | 33 | 0.031–0.037 | 0.041–0.046 |
| 2400 | 0 | 0.017–0.018 | 0.025–0.029 |
| 2400 | 33 | 0.033–0.039 | 0.040–0.047 |

## Process observations

Host stopwatch durations cover fixture setup after AppKit initialization, readiness waiting, concurrent replay, and final settle; they are not subprocess-launch timings. Resource values are **observed per-process peaks per run**, using the maximum over that run's `perfSample`, `perfDone`, and `perfSettled` readings. They are never summed between A/B. Brief sampling does not establish a true peak or time-averaged CPU. Memory is Apple physical footprint in MiB; CPU is percent of one core.

| History rows | Delay (ms) | Host stopwatch (s), median (range) | Observed peak footprint/run (MiB), range | Observed peak CPU/run (%), range |
| ---: | ---: | ---: | ---: | ---: |
| 0 | 0 | 1.814 (1.808–1.842) | 213.92–218.56 | 234.97–240.20 |
| 0 | 33 | 17.023 (17.020–17.034) | 222.17–229.45 | 38.29–41.26 |
| 2400 | 0 | 1.808 (1.779–1.809) | 230.63–234.50 | 225.55–239.18 |
| 2400 | 33 | 17.028 (17.020–17.057) | 224.97–240.78 | 37.30–39.42 |

## GPUI comparison

The prose bytes and 40-byte chunks match `long_answer_streaming_costs` in `crates/riwork-desktop/src/chat_view/display_tests.rs`. That GPUI test explicitly draws a frame after each accepted delta. `PERFORMANCE_RECOVERY_REPORT.txt:538` records late-100 fold/frame medians of 1.152/90.170 ms for the historical baseline, 1.046/7.214 ms for an earlier pre-review implementation that was not reproduced, and 5.863/42.119 plus 5.895/42.104 ms for corrected repeats. NSH's separate asynchronous engine and display-link surface work, release profile, two concurrent windows, retained sampling window, and different UI composition prevent a matched speedup claim.

For the older 2,400-item Debug desktop fixture at 30 deltas/s, `docs/tool-ui-performance.md:74` reports GPUI text stream frame median/p95 of 12.487/14.140 ms after the performance pass and a merged-review p95 of 14.906 ms (`:276`). Event-batch medians were 0.389 and 0.408 ms, respectively; those are batches, not individual deltas or the small synchronous NSH callback interval. The GPUI history includes large command outputs/images, unlike this spike's generated Markdown history. The NSH 33 ms case delivered 29.427–29.454 chunks/s/window with final retained surface p95 ranges shown above; this is an independently measured reference, not equivalent work.

Later GPUI 2,200-call validation measured 34.0/40.5 ms baseline/current frame medians and both failed the 25 ms guard (`PERFORMANCE_RECOVERY_REPORT.txt:109`). Historical GPUI figures vary materially; this table does not select only the fastest baseline or convert any of these differences to a renderer speedup.

## Per-window audit

One final snapshot per distinct surface is represented below. The six observations per case are two windows in each of three processes, not six independent process runs.

| Run | Window | Chunks/s | Replay (s) | Frames retained | Surface p50/p95 (ms) | Layout p50/p95 (ms) | Sync last100 p50/p95 (ms) | >16.67 ms |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| rows0-interval0-run1 | A | 512.692 | 0.926 | 56 | 3.426/4.229 | 3.379/4.081 | 0.018/0.025 | 0 |
| rows0-interval0-run1 | B | 512.767 | 0.926 | 56 | 3.238/3.822 | 3.385/4.055 | 0.018/0.029 | 0 |
| rows0-interval0-run2 | A | 513.563 | 0.925 | 56 | 3.420/4.102 | 3.439/4.103 | 0.018/0.027 | 0 |
| rows0-interval0-run2 | B | 513.570 | 0.925 | 56 | 3.151/3.909 | 3.416/4.101 | 0.019/0.031 | 0 |
| rows0-interval0-run3 | A | 516.942 | 0.919 | 56 | 3.485/4.132 | 3.386/3.964 | 0.018/0.027 | 0 |
| rows0-interval0-run3 | B | 514.938 | 0.922 | 56 | 3.133/3.820 | 3.420/3.995 | 0.019/0.029 | 0 |
| rows0-interval33-run1 | A | 29.452 | 16.128 | 120 | 0.223/3.320 | 3.399/3.792 | 0.034/0.041 | 0 |
| rows0-interval33-run1 | B | 29.454 | 16.127 | 120 | 0.140/3.186 | 3.508/4.041 | 0.037/0.046 | 0 |
| rows0-interval33-run2 | A | 29.448 | 16.130 | 120 | 0.224/3.390 | 3.665/4.368 | 0.036/0.046 | 0 |
| rows0-interval33-run2 | B | 29.448 | 16.130 | 120 | 0.127/3.067 | 3.666/4.368 | 0.035/0.044 | 0 |
| rows0-interval33-run3 | A | 29.447 | 16.131 | 120 | 0.235/3.192 | 3.362/3.910 | 0.031/0.041 | 0 |
| rows0-interval33-run3 | B | 29.449 | 16.129 | 120 | 0.146/2.983 | 3.527/4.148 | 0.037/0.046 | 0 |
| rows2400-interval0-run1 | A | 514.960 | 0.922 | 79 | 3.310/4.683 | 3.328/4.023 | 0.018/0.025 | 0 |
| rows2400-interval0-run1 | B | 517.127 | 0.919 | 79 | 3.181/4.694 | 3.271/4.054 | 0.018/0.027 | 0 |
| rows2400-interval0-run2 | A | 520.519 | 0.913 | 79 | 3.292/4.643 | 3.289/3.860 | 0.017/0.028 | 0 |
| rows2400-interval0-run2 | B | 520.533 | 0.913 | 79 | 3.049/4.340 | 3.303/3.846 | 0.018/0.029 | 0 |
| rows2400-interval0-run3 | A | 532.671 | 0.892 | 78 | 3.444/4.394 | 3.336/3.966 | 0.018/0.029 | 0 |
| rows2400-interval0-run3 | B | 530.627 | 0.895 | 78 | 3.158/4.171 | 3.307/3.963 | 0.018/0.028 | 0 |
| rows2400-interval33-run1 | A | 29.427 | 16.142 | 120 | 0.232/3.389 | 3.628/4.053 | 0.034/0.042 | 0 |
| rows2400-interval33-run1 | B | 29.432 | 16.139 | 120 | 0.152/3.098 | 3.660/4.133 | 0.036/0.045 | 0 |
| rows2400-interval33-run2 | A | 29.440 | 16.134 | 120 | 0.255/3.470 | 3.506/4.045 | 0.039/0.047 | 0 |
| rows2400-interval33-run2 | B | 29.434 | 16.138 | 120 | 0.161/3.270 | 3.382/3.904 | 0.035/0.042 | 0 |
| rows2400-interval33-run3 | A | 29.447 | 16.131 | 120 | 0.251/3.225 | 3.291/3.816 | 0.033/0.040 | 0 |
| rows2400-interval33-run3 | B | 29.452 | 16.128 | 120 | 0.165/3.162 | 3.418/3.946 | 0.034/0.043 | 0 |

`verification_environment`: isolated fixture results only; this aggregation used file reads and wrote only this table. No app execution, GPUI execution, live-window interaction, or task-state action was performed by the aggregation step.

Input summary SHA256: `79416498531cae22fd692c3c1553138d9e1ea898144671f55fe4e196bfd42b19`.
