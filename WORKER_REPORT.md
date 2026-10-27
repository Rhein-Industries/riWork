# NSH multi-window host and chat streaming spike

Assignment: `2a537a8b-d318-47a1-8478-a871dd429025`

Branch: `nsh-spike-host`, based on `50b3b412d77b95a0637c7d72bb7e6d475b50e349`

Date: 2026-10-27

**verification_environment: isolated fixtures only**

Task state: **left open**. No task-completion action, merge, push, or installation.

**Decision: GO for the AppKit host architecture and further prototyping.** A custom
Rust executable successfully owns two NSWindows, each containing an independent
`nsh-platform-macos =0.3.0` AppView running the same mixed Rust/TSX bundle.
Both windows streamed the complete generated Markdown answer, including with
2,400 history rows. The performance evidence supports feasibility; a production
migration or a renderer speedup over GPUI remains unproven.

One API limitation matters: automatic reload from `nsh dev` works through the
public DevClient, but this custom host **remounts both apps and resets their
state**. NSH 0.3's built-in state-preserving dev session and surface hot-update
method are private. An embedding API extension would be needed to reuse that
implementation in this two-window host. No upstream crate was patched.

## What was proved

| Requirement | Evidence and result |
| --- | --- |
| Two NSWindows, two AppViews | [host/src/main.rs](spikes/host-multiwindow/host/src/main.rs) constructs two separate windows, surfaces, host channels and data directories. Final Cua proof: PID 64175, window IDs 6093/6094, both on screen. |
| Same mixed Rust + TSX bundle | `set_ui_sets(vec![nsh_base::registry(), riwork_nsh_probe::registry()])`; both views clone the same source Bundle, including its capability declarations. The Rust `riwork/native` module exports `Probe` and per-instance `ReplayCounter`. |
| Per-window parameters and state | `host.params.windowId` is A/B; displayed native identities are `riwork/native:0.3.0:A` and `:B`. Headless test streams A and verifies B remains Ready with zero chunks. |
| List + TextView | Virtualized List with generated history and a growing, selectable Markdown TextView using `streaming: true`. Counters confirm 475 reactive Markdown reads and 309 layout notifications per native replay; final TextView height is 18,463 points. Headless test verifies visible Part 59 and its table. |
| Type checking and headless tests | Final `nsh check` exit 0; `nsh test` **2 passed, 0 failed, 8.443 s**, including the full replay/isolation test (6.2 s). [JUnit](spikes/host-multiwindow/evidence/nsh-headless.xml), [test output](spikes/host-multiwindow/evidence/nsh-test.log). |
| Dev reload | Server-only `nsh dev`; the AppKit host connects through `nsh_dev::DevClient`, negotiates JS source, receives full builds 1 and 2, and reloads both views. Both A/B report revision-1 and revision-2 and complete all 475 chunks at each revision. Source restored to revision-1. [Raw events](spikes/host-multiwindow/evidence/hot-reload.jsonl), [verification](spikes/host-multiwindow/evidence/hot-reload-verification.log). |
| Native rendering inspection | RiWork's Cua.ai Driver MCP inspected only the fixture PID; no desktop input was sent. [A screenshot](spikes/host-multiwindow/evidence/window-A.png), [B screenshot](spikes/host-multiwindow/evidence/window-B.png), [Cua metadata](spikes/host-multiwindow/evidence/cua-proof.json). A final read found zero remaining fixture windows. |

The final static bundle is 6,775 bytes of JS; its hash and the executable hash
are in [environment.json](spikes/host-multiwindow/evidence/environment.json).
The fixture reproduces `display_tests.rs::long_answer_streaming_costs`: 60
Markdown sections with headings, inline code/bold/link, lists and tables,
**18,980 ASCII bytes / 475 chunks of at most 40 bytes / FNV-1a `4f4771e9`**.
Every one of the **24 measured window streams** passed byte count, chunk count,
checksum, window identity and TextView activity checks: 11,400 chunks and
455,520 bytes total. All streams had 475 Markdown reads and 309 layout callbacks.

## Streaming numbers

Twelve fresh processes: three repetitions of four cases, with **two concurrent
streams in each process**. Each window is 720 × 720 points. Rust host: release;
JS: ordinary `nsh build` output, with instrumentation. Reported display-link
refresh was 50 Hz. The 33 ms case approximates the existing report's 30 deltas/s
producer; actual completion throughput includes timer overhead.

Each range below covers six window observations (three paired A/B process runs).
These are paired observations, not six independent machine runs. Surface/core
columns are ranges of each window's reported p95, not a pooled percentile.

| History rows | Producer timer | Chunks/s per window | Replay wall time (ms) | Surface work p95 (ms) | Engine core p95 (ms) |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 0 | 0 ms | 512.692–516.942 | 918.866–926.483 | 3.820–4.229 | 3.964–4.103 |
| 0 | 33 ms | 29.447–29.454 | 16,126.682–16,130.694 | 2.983–3.390 | 3.792–4.368 |
| 2,400 | 0 ms | 514.960–532.671 | 891.733–922.401 | 4.171–4.694 | 3.846–4.054 |
| 2,400 | 33 ms | 29.427–29.452 | 16,127.804–16,141.708 | 3.098–3.471 | 3.816–4.133 |

Median per-window throughput across the six observations was respectively
513.567, 29.449, 520.526 and 29.437 chunks/s. No history-size throughput penalty
was observed in this small generated fixture. This does not establish the cost
of RiWork's complete tool, selection, image or conversation model.

Combined process throughput (950 chunks divided by the host interval from
replay dispatch to the later done event) had medians **1,026.796**, **58.896**,
**1,040.633**, and **58.866** chunks/s in the same table order. The independently
checked [benchmark table](spikes/host-multiwindow/evidence/benchmark-table.md)
also gives per-run counts, ranges and resource observations.

The synchronous JS signal/native-counter update's **last-100 median** ranged
0.018–0.019 ms, 0.031–0.037 ms, 0.017–0.018 ms and 0.033–0.039 ms respectively.
This is only synchronous application work. Engine core medians ranged
3.271–3.666 ms across all cases. The combined process's sampled peak CPU was
240.204% of one core and peak physical footprint was 240.782 MiB. Resource
readings belong to the whole two-window process; A and B must not be summed.

Raw per-run JSONL, stderr, and [benchmark-summary.json](spikes/host-multiwindow/evidence/benchmark-summary.json)
are retained. [run_benchmarks.py](spikes/host-multiwindow/run_benchmarks.py) validates
the results and produces the summary. No benchmark runs were discarded or retried.

## Measurement boundaries and GPUI comparison

NSH's surface timer records instrumented native batch application, synchronous
Core Animation commit, animations and input. The engine's core timer covers
component work, text/layout and mount diffs. These are separate from JS callback
cost and from producer wall time. The monitor skips four initial frames/commits,
keeps at most 120 samples, and reports rates over one second. This report uses
the `perfSettled` snapshot taken at least 250 ms after both done events so the
final update has time to apply. It is not a presentation acknowledgment.

The monitor starts when the views are created; replay starts 500 ms after both
are ready. Short unpaced runs therefore include prelude/idle work in their frame
distributions, and every settled snapshot includes the idle tail. There were
56 retained samples per window for unpaced zero-history runs, 78–79 for unpaced
2,400-row runs, and 120 for paced runs. **None of these retained surface samples
exceeded 16.67 ms**. This is a bounded-window observation, not a guarantee over
all frames. Rolling samples overlap across periodic snapshots, which were not
concatenated to manufacture a larger sample count.

Individual deltas can be coalesced before presentation. NSH can also tick an
occluded surface with a fallback timer; monitor FPS does not prove visible
presentation or GPU completion. Both fixture windows were inspected on screen
in a separate proof run. No screenshot/desktop automation ran during the timed
benchmark series. The machine was a shared desktop; other application activity
was uncontrolled. There were zero cargo/rustc/nsh build processes at benchmark
start. The main event pump uses a 5 ms wait deadline.

| Existing GPUI reference in this repository | Recorded number | Relationship to this spike |
| --- | --- | --- |
| Exact 18,980-byte/475-chunk long-answer correction and repeat, `PERFORMANCE_RECOVERY_REPORT.txt:536` | Last-100 fold/frame medians **5.863/42.119 ms** and **5.895/42.104 ms** | Closest text payload. GPUI Debug explicitly renders one frame after every delta; NSH Release streams asynchronously with different timer boundaries. |
| Same report, historical original and pre-review result | Late frame medians **90.170 ms** and **7.214 ms** | The report says the earlier large speedup was not reproduced. Both retained as context, neither selected as a current matched baseline. |
| 2,400-item text-stream fixture, `docs/tool-ui-performance.md` | After-pass frame median/p95 **12.487/14.140 ms**; merged-review p95 **14.906 ms** | Approximately 30 deltas/s, but a full RiWork tool/image/history workload in Debug; this spike's history rows contain generated prose. |
| Later 2,200-call integration regression | Baseline/current median frame **34.0/40.5 ms**, both failing the 25 ms guard | Shows the existing reference numbers vary with integration/environment. This spike did not rerun GPUI. |
| Release 120-message scrolling, `docs/performance-native-theme.md` | Native median/p95 **4.090/4.671 ms**; RiWork **4.301/5.609 ms** | Scrolling reference only, not streaming. |

[baseline-notes.md](spikes/host-multiwindow/baseline-notes.md) records the exact
source locations, workloads and additional reference numbers. **No renderer
speedup ratio or matched performance win is established.** A migration decision
needs the same profile, full workload, viewport and frame/presentation boundary
on both implementations, plus investigation of state-preserving embedding reload.

## Exact reproduction commands

Run from `/Users/dominik/orca/projects/riWork-nsh-spike-host`. The original
installed CLI is 0.2.0 and was left unchanged. A local CLI was built from the
published 0.3.0 crate, source commit `76fa8a7fed47ea51aea01fa85bb40960be15d984`;
all 45 published source files matched its archive. The CLI build took 3m45s;
the initial release host build took 2m38s. See [tooling provenance](spikes/host-multiwindow/evidence/tooling-provenance.json).

```sh
SPIKE="$PWD/spikes/host-multiwindow"
cargo build --locked --release \
  --manifest-path /Users/dominik/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/nsh-cli-0.3.0/Cargo.toml \
  --target-dir "$SPIKE/.tooling/target" --bin nsh
cargo build --release --locked --manifest-path "$SPIKE/host/Cargo.toml"

NSH_CACHE="$SPIKE/.tooling/cache" "$SPIKE/.tooling/target/release/nsh" \
  check "$SPIKE/app" \
  --tsc /Users/dominik/Library/Caches/nsh/typescript-7.0.2-darwin-arm64/package/lib/tsc
NSH_CACHE="$SPIKE/.tooling/cache" "$SPIKE/.tooling/target/release/nsh" \
  build "$SPIKE/app" --out "$SPIKE/app/.nsh/out"
NSH_CACHE="$SPIKE/.tooling/cache" "$SPIKE/.tooling/target/release/nsh" \
  test "$SPIKE/app" --junit "$SPIKE/evidence/nsh-headless.xml"

python3 "$SPIKE/verify_hot_reload.py"
python3 "$SPIKE/run_benchmarks.py"
cargo fmt --manifest-path "$SPIKE/host/Cargo.toml" --check
cargo fmt --manifest-path "$SPIKE/app/Cargo.toml" --check
```

The existing TypeScript 7.0.2 compiler was reused; no compiler installation was
performed. No `CARGO_TARGET_DIR` override was used for generated app helpers.
Headless tests have no `--target` flag. The hot-reload script runs exactly
`nsh dev <app> --server-only --port <free-port>` and connects only this custom
AppKit host; `--server-only` and `--target` are mutually exclusive in 0.3.
Its file lock prevents simultaneous verification runs, both child processes
are reaped, and the revision source is restored in an outer `finally`.

Final native UI proof command:

```sh
"$SPIKE/host/target/release/riwork-nsh-multiwindow-fixture" \
  --bundle "$SPIKE/app/.nsh/out/bundle.js" \
  --data-dir "$SPIKE/.run/ui-final-data" \
  --output "$SPIKE/evidence/ui-final.jsonl" \
  --history-rows 200 --interval-ms 16 --timeout-ms 45000 --linger-ms 15000
```

Each benchmark uses a fresh `nsh-isolated-*` temporary directory and exits on its
own. Only generated prose is read; no real transcript, RiWork home, account,
provider, repository application window or user conversation is accessed.
No GPUI backend was built or run; both spike lockfiles contain only NSH 0.3.0
packages and no GPUI package. Build caches/binaries and runtime data are ignored.
The global CLI's before/after SHA256 is identical. OS: macOS 26.2 (25C56), ARM64,
Apple M4 Max, 16 logical CPUs, 64 GiB RAM; Rust/Cargo 1.99.1.

Exploratory failures are retained separately: the 0.3 generated types expose
`host` from `nsh` (the initial `nsh/host` import failed type checking); a redundant
headless scroll failed because automatic scrolling had already reached the end;
and the first dev command combined mutually exclusive flags. The final commands
above passed after these corrections. Cua inspection also caught the system
dark-theme/light-background mismatch; the final fixture explicitly uses light
appearance. Production files were not modified by the spike.
