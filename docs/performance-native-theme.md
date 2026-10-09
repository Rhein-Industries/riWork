# Desktop Native performance investigation — 2026-10-05

Settings repeatedly measured its entire content during the workspace's intrinsic-size
layout passes. It exceeded a 60 Hz frame budget in both Native and RiWork. A definite-size
GPUI cached view now lays Settings out at its pane size and reuses it on unrelated workspace
redraws. Settings notifications, bounds, inherited text style and global window refreshes
invalidate that cache. No typography, colors, control styling or terminal code changed.

The RemoteState observer compares the host rows (including names and link state) and host
error before notifying. RemoteState also mutates to schedule polling; notifying on every
mutation caused an idle redraw regression in the first experiment and was removed before
the final measurements. Existing observers cover Settings, Appearance, Cua and accounts.

## Evidence

Baseline source: `f04d69a37f8ba6c5839c663f21032672e044121f`. Both binaries built with
`cargo build --locked --release`, Rust 1.95, Zig 0.16.0; distinct copies under
`target/native-perf/`. macOS 26.2, ARM64 Mac16,5, 64 GiB RAM. The test window was
1220 × 780 points, with a 27% navigation pane and Settings on the right, Native light,
default 13 pt displayed interface text, labels enabled. Other busy apps stayed running.

| Warm Settings scrolling | Before median / p95 | After median / p95 | Frames before / after |
| --- | --- | --- | --- |
| Native | 40.638 / 51.554 ms | 14.630 / 19.673 ms | 51 / 65 |
| RiWork | 36.608 / 47.856 ms | 14.860 / 20.958 ms | 60 / 66 |

Native's median fell 64%; its frames over 16.67 ms fell from 51/51 to 7/65.
RiWork's median fell 59%; over-budget frames fell from 60/60 to 8/66.
A separate initial Native run measured 39.397 ms median before the change, corroborating
the repeat. The sampler was **off** for the final timing runs.

The original active Native sample had 1,566 samples under
`TaffyLayoutEngine::compute_layout` out of 1,626 under the sampled frame update branch.
The large nested flex/block sizing tree dominated; tab label advances and cached symbols
were not significant in this workload. `native-scroll.sample.txt` records this stack.
The after sample retains layout work but avoids the workspace's repeated subtree sizing.

Idle Settings redraw frames fell from 46.177 ms median to 3.406 ms. Separate 20-second
process CPU-time deltas were 0.19 s before (0.949% of one core) and 0.13 s after (0.650%).
These are single short, contended samples, not a statistically established idle CPU gain.
A 5-second initial idle sample spent 4,219/4,234 main-thread samples waiting on Mach messages.

A saved synthetic chat with 120 Markdown messages (paragraphs, code, lists) scrolled at
4.090 ms median / 4.671 ms p95 under Native (62 frames) and 4.301 / 5.609 ms under RiWork
(66 frames); neither run exceeded 16.67 ms. The existing virtualized chat was left alone.
No provider or paid agent was started for this fixture.

Terminal checks used a test shell printing an ANSI-colored line every 50 ms for 60 seconds.
Both themes displayed the stream; Native scrollback reached older rows in tmux copy mode.
Eight-second samples did not expose a comparable busy render path; the process was around
0.1% CPU in the observed stream. Native typing generated visible terminal input and cheap
workspace frames (about 1–3 ms), but Cua's background text delivery rendered the requested
mixed-character string as repeated `a`s. This limits keyboard fidelity and input-latency
claims. GPUI's timer does not time Ghostty's independent renderer. Terminal rendering and
end-to-end keystroke latency are therefore smoke checks, not quantified improvements.

Tab switches between shell, Settings and chat were observed. A switch to Settings pays its
layout cost; shell/chat workspace frames were generally a few milliseconds. No claim of
accelerated Ghostty attachment or keyboard-to-pixel latency is made. The measured bottleneck
is GPUI layout, with no evidence implicating the separate SwiftUI iOS implementation;
iOS was not changed or profiled.

## Repeat the measurements

1. Build the baseline and candidate in isolated target directories with the README's Zig
   0.16 toolchain. Preserve copies of each executable. Do not use `riwork update` or reload.
2. Make a disposable `.app` wrapper whose executable exports a **new** `RIWORK_HOME`, a
   **new** `RIWORK_RUNTIME_DIR`, `RIWORK_APPEARANCE=light`, `ZED_MEASUREMENTS=1`, and
   `GHOSTTY_RESOURCES_DIR` pointing to a bundled Ghostty resources directory, then execs
   the selected binary with a disposable project path. Redirect stderr/stdout to a log.
   Retain HOME; do not copy real sessions, settings or credentials into the fixture.
3. Register the wrapper under a unique bundle id and launch it using RiWork cua-driver MCP
   `launch_app`, in the background. This run used `dev.riwork.perf-native` and a temporary
   symlink in `~/Applications`. Inspect its exact pid/window with `list_windows` and
   `get_window_state` before input. Do not operate the installed app's windows.
4. Use Cua to set the window to 1220 × 780, open Settings with Cmd+, in the right pane,
   and verify the selected theme and dimensions. Wait for startup/theme reflow to settle.
5. Append `PHASE <name>` to the log. With a fresh 1220-pixel-wide Cua screenshot, use
   `scroll` at x=800, y=550, amount=30, down. Verify the scrolled content, then repeat up
   with the same amount. Let the scroll animation finish before appending the next marker.
   Repeat for both themes and binaries. Do not run `sample` during timing runs.
6. Run `python3 scripts/perf-frames.py LOG` (or `--json`). It reads GPUI's
   `frame duration:` output, reports median, nearest-rank p95, maximum and over-budget
   count. The timer includes window drawing, presentation submission and arena clearing;
   it excludes input queue latency and GPU completion. Smooth scrolling can generate more
   frames when faster, so compare times rather than equal frame counts.
7. Separately sample the exact test pid, e.g. `sample PID 15 1 -file PROFILE`, during
   repeated scrolling. Measure idle CPU from `ps -p PID -o time=` deltas across 20 seconds.
   Quit only the disposable app through Cua; clean up only its fixture processes and link.

The committed [frame log](perf-native-theme-frames.log) contains only the selected timing
lines and phase markers. Recompute it with:

```sh
python3 scripts/perf-frames.py docs/perf-native-theme-frames.log
```

Full local evidence remains under `target/native-perf/`: `app.log`, original and after
samples, CPU JSON, saved chat, wrapper and Cua screenshots. This directory is ignored and
contains local account display names in screenshots; only timing lines are committed.
The initial experimental cache and transitions are excluded from the committed log.

## Verification and integration

- Settings tests: 25 passed; theme tests: 19 passed; ui_text tests: 12 passed;
  remote_tree tests: 38 passed, all with `cargo test --locked --release --bin riwork FILTER`.
- `cargo fmt --check`, `git diff --check`, parser units/phase-boundary sanity check passed.
- Final release build passed. `scripts/bundle-macos.sh release` passed with Zig 0.16.0;
  deep/strict codesign verification and both binary/bundled `riwork help` checks passed.
  Bundling emitted the existing older-tic description warning.
- Cua verified Native light/dark, theme polling, scrolling, 700/1220/2000-point widths
  (narrow/medium/wide layouts), centered focus and restore. Clicking text size changed
  displayed 13 → 14 pt and saved 11 → 12; Reset restored 13 pt. A switch repainted and
  persisted. Existing installed windows and settings were not modified.
- Remaining tails exceed 16.67 ms on the busy machine. No all-app latency, GPU completion,
  high-throughput terminal benchmark, paired-Mac connection, or iOS performance claim.
  Layout can still be expensive within Settings at larger widths; this focused fix avoids
  the measured parent sizing repetition without changing the settings page design.

Ready for coordinated review/integration; no merge, push, deploy, installed-app restart,
or task completion transition. Model fallback work in main was not edited or built.

Task: `385a16b3-760a-4a31-8edf-12d3c25ee4c6` (retain `in_progress`).
Project: `39832c2e-23a5-476d-aa8f-5ff34a02d314`.
Worktree: `03c33ce7-1e51-4783-af95-459635229615`, branch `perf-native-theme`,
`/Users/dominik/orca/projects/riWork-perf-native-theme`.
Codex shell: `89d0c281-0580-4034-93e6-f20997b140f6`.
Concurrent model-fallback task: `cd1cc4a7-da64-46c6-accc-e21926426173`,
Codex chat `9f2ca4f4-cde0-4680-b226-9418dac1155e` in main.
