# GPUI reference measurements for the AppKit NSH spike

These are existing repository measurements, read on 2026-10-27. No GPUI build,
test, application, or live window was run for this comparison. They are reference
points, not a contemporaneous matched benchmark. The report should preserve the
distinction between model/event work, native view update work, and drawn frames.

## Closest long-answer reference

Source: `PERFORMANCE_RECOVERY_REPORT.txt:536` and the current fixture in
`crates/riwork-desktop/src/chat_view/display_tests.rs:758`.

The fixture generates one 18,980-byte answer containing 60 Markdown sections:
headings, paragraphs, inline code/bold/link, a nested bullet list, and a table.
It streams the answer in 475 UTF-8-safe chunks of at most 40 bytes, without a
producer delay. Each delta is accepted into the chat and followed by an explicitly
drawn GPUI test frame. Fold time is measured inside `view.accept`; frame time is
separately measured around `window.render_frame`. The report gives the medians of
the first and last 100 samples, not p95 or throughput. The median implementation
selects sorted index `len / 2` (upper middle for 100 samples).

| Historical debug measurement | First 100 fold / frame (ms) | Last 100 fold / frame (ms) |
| --- | ---: | ---: |
| Original baseline | Not reported | 1.152 / 90.170 |
| Pre-review implementation | Not reported | 1.046 / 7.214 |
| Section-cache correction | 1.245 / 37.893 | 5.863 / 42.119 |
| Section-cache correction repeat | 1.230 / 37.590 | 5.895 / 42.104 |
| Contemporary pre-correction invalidation control | 1.391 / 43.190 | 5.846 / 43.974 |

The report explicitly says the earlier 12.5x speedup was not reproduced in the
correction runs. Do not select 7.214 ms as the sole current baseline or imply that
90.170 versus an NSH headless update time is a renderer speedup. The two corrected
late-frame medians (42.119 and 42.104 ms) are the more conservative historical
reference, with all five rows retained as context. No GPU-completion or
end-to-end input latency is established by these measurements.

## Large history and token-stream reference

Source: `docs/tool-ui-performance.md:3`, `:10`, `:63`, and `:258`.

All figures below are Debug with symbols (`CARGO_PROFILE_DEV_DEBUG=1`) in a
separate fixture process using a disposable `RIWORK_HOME`. The generated fixture
contains 2,400 items, a dense group of 1,200 reads, 64 KiB call outputs, 242 image
references to 64 images, and a 152 MB JSONL. An inert copy of a real large chat
adds 3,401 items / 26,764 events / 233 image references. Its fixture host produces
30 deltas/s over the desktop subscription protocol without starting a provider
or writing stream events to disk. Baseline source is `ff2bbf4` plus the same
timing driver. The fixture host was restarted between matched runs.

| Scenario | Before (ms) | After performance pass (ms) | After merged review (ms) |
| --- | ---: | ---: | ---: |
| Text stream drawn frame median / p95 | 13.214 / 14.233 | 12.487 / 14.140 | median unreported / 14.906 |
| Output stream drawn frame median / p95 | 12.541 / 13.341 | 12.170 / 14.116 | median unreported / 12.995 |
| Text event batch median / p95 | 0.442 / 0.639 | 0.389 / 0.591 | 0.408 / p95 unreported |
| Output event batch median / p95 | 0.155 / 0.176 | 0.090 / 0.116 | 0.079 / p95 unreported |
| Scroll frame p95 | 1648.199 | 38.922 | 32.752 |

`stream-event` measures one delivered batch, which may contain several deltas.
It is not interchangeable with cost per delta. The producer's text payload is
`token `; output payload is `output NNNNN: fixture payload\n`
(`scripts/tool-ui-perf-host.py:81`). Drawn frame figures include GPUI layout and
paint. They do not establish release frame rates.

The separate 2,200-call headless native regression starts with 2,200 command
items, each containing `bounded output line\n` repeated 3,500 times, expands
them, scrolls to item 1,000, and measures three explicit frames. Fewer than 100
rows and selection bodies must materialize. It then applies 120 six-byte output
deltas (`token `) to item 2,199 and averages model fold plus list synchronization
time; those individual-event timings do not include a frame after every delta.
Source: `crates/riwork-desktop/src/chat_view/display_tests.rs:346`.

- Initial performance-pass result: 5.648 ms drawn frame; 0.368 ms/event.
- Merged review result: 18.7 ms frame, followed by three runs with medians
  6.804 / 6.313 / 6.145 ms; event range 0.340–0.411 ms/event.
- Guards: 25 ms median frame (three samples), 1.5 ms/event, 60 ms median row/bulk
  toggles, fewer than 100 rendered rows, no token-triggered historical projection.

Later integration evidence in `PERFORMANCE_RECOVERY_REPORT.txt:109` is less
favorable: one serial baseline/current pair measured 34.0 / 40.5 ms median frame,
both failing the existing 25 ms guard. No warm-up or retries; background workload
and cache history were uncontrolled. Both stopped at the first frame guard, so
that run provides no event result. Earlier baseline/current failures were
33.6 / 35.2 ms. Do not present the early 5–7 ms figures as stable current results.

## Secondary release reference

`docs/performance-native-theme.md:15` describes release builds on macOS 26.2,
ARM64 Mac16,5, 64 GiB RAM, a 1,220 × 780 pt window, and other busy apps running.
Its 120-message synthetic Markdown chat scrolled at 4.090 ms median / 4.671 ms p95
(62 frames) under Native and 4.301 / 5.609 ms (66 frames) under RiWork; neither
exceeded 16.67 ms. This is scrolling, not streaming, and should remain secondary.
Settings results in that report are unrelated to the chat workload.

The log summarizer (`scripts/perf-frames.py:9`) reports conventional median,
nearest-rank p95 (`ceil(n * 0.95) - 1`), max, and count over 1,000/60 ms. The report
states GPUI's frame timer includes drawing, presentation submission, and arena
clearing, and excludes input queue latency and GPU completion.

## Defensible NSH comparison

1. Reuse the exact 18,980-byte generated prose and 40-byte chunks where practical;
   report its byte count, chunks, history row count, viewport, and whether Markdown
   is rendered or displayed as plain TextView text. Equal text bytes alone do not
   make plain TextView work equivalent to GPUI's Markdown widgets and selection.
2. Measure the individual phases separately: producer scheduling, JS/native event
   handling and reconciliation, AppKit layout/display, total replay wall time,
   and process CPU. If only headless reconciliation is timed, label it exactly
   that; it is not a drawn frame. Report dropped/coalesced updates and verify final
   text length/checksum and row count before claiming sustained throughput.
3. For the closest long-answer comparison, force native update/layout after every
   chunk and retain first/last 100 medians. Also report p95, max, total delta/s,
   bytes/s, and >16.67 ms count with a documented timer boundary. An unpaced replay
   gives service capacity, while a separate 30-delta/s replay matches the earlier
   producer rate and exposes queue lag. These answer different questions.
4. Run one-window and two-window workloads with identical bundle and different
   `host.params`. State whether both windows receive the stream or only one does.
   Measure each window and combined process cost; verify the untouched window's
   state if testing one active stream. Isolated hidden/offscreen NSWindows may
   prove view creation and reconciliation, but must not be called visible-frame
   or presentation measurements.
5. Record build profile, OS/CPU/RAM, exact dependency versions, commands, warm-up
   policy, sample counts, and fresh fixture paths. Retain raw samples. Compare to
   the historical table as reference, without unsupported speedup ratios or claims
   that controls are equivalent. The existing GPUI numbers vary materially.

`verification_environment`: repository file reads only for this baseline review;
no application was launched, no GPUI command was run, no live state was read, and
no desktop interaction occurred. This note alone makes no claim about the spike's
runtime verification.
