# Tool call UI performance pass

Branch `tool-ui-perf`, starting at `ff2bbf4`. All measurements use Debug builds
with symbols (`CARGO_PROFILE_DEV_DEBUG=1`), a separate process and
`RIWORK_HOME=/tmp/riwork-perf-home`. No installed bundle or live application was
used. The original chat logs were opened read-only and copied into the fixture.

## Fixture and repeatable profiling

The generated fixture contains 2,400 items, including one uninterrupted group of
1,200 reads, 64 KiB call outputs, and 242 image references to 64 distinct images
(152 MB JSONL). An inert copy of the largest real chat adds 3,401 items, 26,764
events and 233 image references. The fixture host produces 30 deltas/second using
the normal desktop subscription protocol; it never starts a provider or appends
stream events to disk.

```sh
python3 scripts/tool-ui-fixture.py --home /tmp/riwork-perf-home \
  --root /tmp/riwork-perf-project --items 2400 --dense-calls 1200 \
  --copy-largest-real-chat
python3 scripts/tool-ui-perf-host.py --home /tmp/riwork-perf-home
CARGO_PROFILE_DEV_DEBUG=1 cargo build -p riwork-desktop
python3 scripts/tool-ui-profile.py target/debug/riwork \
  --output /tmp/riwork-perf-run
python3 scripts/perf-frames.py /tmp/riwork-perf-run.log
```

Restart the inert host between comparisons to remove its in-memory streamed
turns. The profile runner activates only the disposable fixture's chat layout,
records Time Profiler and GPUI frame durations, and waits for each debug command
to finish before issuing the next. Preview discovery can otherwise block the
main thread long enough to overwrite pending scenario commands.

`RIWORK_TOOL_UI_PERF=1` enables wall timings for projection, disclosure, mode,
selection document generation, event batches, and clock ticks. The command driver
is compiled only in Debug and refuses a home outside `/tmp`. `stream-event`
timings describe **one delivered batch**, which can contain several deltas;
the engine/native regression tests separately measure individual events.

## Findings

- A top-level GPUI virtual list eagerly rendered all children inside a disclosed
  group. The desktop now gives every visible header/body a separate virtual item,
  while preserving the full grouping tree for disclosure, focus, selection and
  scroll anchors. Image completion and clock ticks remeasure only affected items.
- Both desktop mode switching and engine mode cache eviction discarded historical
  projection work. The engine now keeps both modes and borrows large item bodies
  and inline image payloads. Dirty turns invalidate both modes. Body-only desktop
  deltas already skipped projection and continue to do so.
- Disclosure rebuilt all historical selection bodies, including output cleanup and
  markdown parsing. A per-row source cache now reuses unchanged bodies; event and
  auxiliary disclosure invalidation retain exact copy behavior.
- Opening a preview synchronously rediscovered output links and validated the same
  paths thousands of times. Discovery now memoizes validation, normalization and
  repeated output scans and uses indexed entry deduplication.
- Desktop image decode/thumbnail work already used background tasks and bounded
  caches. The initial Time Profiler trace found no image-decode samples on the
  main thread; ordinary streaming was dominated by GPUI layout. Diff/syntax was
  only 0.03% of those main-thread samples.

The matched run results and regression checks follow.

## Desktop results

Baseline uses `ff2bbf4` with only the same timing/command driver added.
Matched native Time Profiler runs: `/tmp/riwork-perf-baseline4.trace` and
`/tmp/riwork-perf-final2.trace`; identical acknowledged actions, restarted fixture
host before each run. Frame figures include actual GPUI layout/paint. Command
figures include the synchronous UI handler, with selection work shown separately.
These are Debug measurements, not a claim about release-build frame rates.

| Scenario | Before (ms) | After (ms) |
|---|---:|---:|
| Text stream frame median / p95 | 13.214 / 14.233 | 12.487 / 14.140 |
| Output stream frame median / p95 | 12.541 / 13.341 | 12.170 / 14.116 |
| Text event batch median / p95 | 0.442 / 0.639 | 0.389 / 0.591 |
| Output event batch median / p95 | 0.155 / 0.176 | 0.090 / 0.116 |
| Scroll frame p95 | 1648.199 | 38.922 |
| Group toggle handler / frame p95 | 92.746 / 1856.582 | 32.919 / 30.365 |
| Expand all handler / frame p95 | 45.841 / 272.350 | 22.921 / 27.194 |
| Collapse all handler | 52.020 | 25.052 |
| Normal switch handler median | 40.152 | 33.140 |
| Verbose switch handler median | 33.140 | 21.985 |
| Preview discovery handler | 11751.545 | 147.151 |
| Expand all selection work | 254.119 | 4.033 |
| Group toggle selection work median | 176.986 | 3.441 |

The native 2,200-call regression measures individual event fold + list updates
at 0.368 ms/event and an explicitly drawn virtual-list frame at 5.648 ms.
Before/after engine 2,305-item measurements (about 128 MB of outputs and inline
image payloads): mode switches 12.234 -> 4.113 ms; streaming fold + full cache
lookup 3.754 -> 3.885 ms (effectively unchanged). The desktop avoids that full
cache lookup for token deltas. Expanded 1,201-override remapping averages 2.901 ms.

The earlier matched exported profile (before the final lazy-selection/remap
improvements) attributes 11,750 -> 150 ms of main-thread CPU to preview discovery,
24,090 -> 12,092 ms to GPUI layout ancestry, and 4,053 -> 776 ms to projected-row
rendering. Categories overlap. The final trace is retained; its timings show no
new dominant stall, so another large XML export was unnecessary.

## Source map

Line numbers refer to this pass's final source tree.

| Cause / change | Source |
|---|---|
| Mode cache eviction and payload ownership | `crates/riwork-engine/src/chat/projection.rs:200`, `:447` |
| Quadratic disclosure remapping | `crates/riwork-engine/src/chat/projection.rs:1358` |
| Offscreen disclosed calls eager inside one list row | `crates/riwork-desktop/src/chat_view/rows.rs:48`, `display.rs:19` |
| Selection body work for offscreen rows | `crates/riwork-desktop/src/chat_view/selection_document.rs:28`, `select.rs:88` |
| Source revision and mode invalidation | `crates/riwork-desktop/src/chat_view.rs:811`, `:1267` |
| Image completion invalidated whole list | `crates/riwork-desktop/src/chat_view/media.rs:1324` |
| Repeated preview path validation and output scans | `crates/riwork-desktop/src/chat_view/links.rs:391`, `:420`, `:626` |

## Regression checks

The repository does not define CI workflows. These checks run in the normal
Cargo/XCTest commands and are available to any CI invoking those commands.
All performance checks are enabled tests, not ignored benchmarks. Thresholds are
intentionally generous; structural assertions also catch repeated projection,
offscreen rendering and selection reconstruction without relying on machine speed.

- Engine: normal `cargo test -p riwork-engine chat::projection` runs the 2,305-item
  event/toggle timing guard and 1,201-override linear-remapping guard, alongside
  all shared grouping fixtures (9 tests passed).
- Desktop: normal `cargo test -p riwork-desktop` runs the 2,200-call native draw,
  individual streaming-event, disclosure and lazy-selection guard. It asserts
  fewer than 100 rows/selection bodies are materialized, no token reprojection,
  less than 1.5 ms/event, a 25 ms draw median and 60 ms toggle medians. Draw and
  toggle timings use three samples. Rich selection,
  virtualized copy, backfill/focus anchoring, image and viewer tests also pass.
- Preview: normal desktop tests include 1,200 reads with 64 KiB outputs, exact
  link-set assertions and a 500 ms median of three fresh discovery runs. Every
  run checks the complete expected link set.

Remaining scope: a very large single user/final message and a bounded artifact
page still occupy one virtual item. Some Normal-mode Debug layout frames remain
about 36 ms median / 48 ms p95 during mode changes. Release hardware/frame pacing,
line-level virtualization of individual messages, and fully asynchronous cold
preview link discovery are deferred. Image decoding remains off the main thread.
Full selection export is assembled when Copy is requested, including offscreen
content, instead of being paid on every expansion.


## iOS Simulator results

The same 2,400-item desktop fixture was imported into the Debug Simulator test,
including its image bytes. Main-thread CPU measurements exclude the deliberate
settling sleeps; these are CPU per event/toggle/frame, not wall-clock animation
latency. A useful 24 s Time Profiler recording is retained at
`/tmp/riwork-perf-ios-shared-hot.trace`. The first column below is the first
hosted measurement during the pass, after projection caching was already in
place; it measures subsequent UI improvements rather than the entire uncached
baseline. The separate pure-projection comparison below includes uncached work.

| Exact shared fixture scenario | Before (ms) | After (ms) |
|---|---:|---:|
| Streaming event | 41.151 | 22.042 |
| Scroll frame | 14.054 | 4.323 |
| Normal / Verbose toggle | 162.601 | 76.791 |
| Expand / Collapse all | 1397 | 146.554 |
| Individual row toggle | 26.150 | 29.111 |
| Open image preview | 108.243 | 69.274 |

The self-contained XCTest fixture also has 2,200 items, a 1,200-read dense
turn, 100 images and 64 KiB outputs. It measures 21.431 ms/event, 3.745 ms/scroll
frame, 70.785 ms/mode toggle, 105.974 ms/bulk toggle, 27.984 ms/row toggle and
55.727 ms/preview. Only 5.33 output bodies are evaluated per bulk toggle, one row
body per token, and no list bodies over a 2.2 s activity-label timer check.

The iOS findings/fixes are cached grouping keyed by a distinct projection
revision; indexed item lookup and narrow row equality; activity-label-local clock
updates; detached ImageIO decoding with a bounded shared cache; bounded output
splitting and cached diff work; and a native SwiftUI `List` for the outer
transcript. Nested `LazyVStack` alone still spent about a second enumerating lazy
identities and estimated lengths despite avoiding offscreen output bodies.
The native list retains reader anchors through a UIScrollView offset bridge and
keeps viewport keyboard dismissal working.

The pure 2,200-item projection benchmark changes 31.751 -> 0.001 ms/token; cached
mode/disclosure work is 1.737 ms/toggle. Hosted Simulator regression budgets are
80 ms/event, 80 ms/scroll frame, 300 ms/mode toggle and 750 ms/bulk toggle. The
750 ms ceiling has margin over the new dense-fixture measurement and fails the
old 1.4 s path. Structural checks require no token projection rebuild, no clock
list rebuild and fewer than 100 output bodies per toggle. The exact desktop
fixture test is an additional local test; the self-contained dense fixture
runs without an external fixture in the normal XCTest suite.

Final validation: 481 desktop tests passed (6 existing ignored); 9 engine
projection tests passed; 114 iOS core tests and 21 selected Simulator app tests
passed, including the dense/shared performance cases, reader-anchor prepend,
keyboard dismissal, stream following, preview, image-cache and file-index tests.
Individual row toggles are effectively unchanged within Debug-run variation.

Additional iOS source locations: grouping cache
`ios/Core/ChatProjection.swift:604`; revision separation
`ios/Core/ChatTranscript.swift:9`; concrete row equality and native virtualization
`ios/RiWorkRemote/ChatView.swift:720` and `:796`; row-local elapsed timer
`ios/RiWorkRemote/ChatActivityRows.swift:741`; bounded/background diff preparation
in that file at `:653`; detached downsample/decode and shared image cache
`ios/RiWorkRemote/ChatFileViewer.swift:408`; asynchronous attachment thumbnails
`ios/RiWorkRemote/AttachmentViews.swift:457`.

The additional broad `swift test --package-path ios` run executed 1,245 tests
and exposed three existing assertions in two unrelated Muse/provider-kind tests:
`ios/Tests/NewOrchestratorTests.swift:195` and `:202` expect five kinds instead of
six, and `ios/Tests/NewTerminalTests.swift:314` expects wrapping directly to shell.
A targeted 53-test run against the original `ff2bbf4` projection/transcript/test
sources reproduced all three failures. Those expectations were deferred during
the performance pass; the temporary baseline source substitution was restored
byte-for-byte afterward. The subsequent tool-UI polish test cleanup corrected
the stale expectations to include Muse, without changing provider behavior.

Implementation commits: `1adc3d8` (engine cache/payloads), `9147510` (fixtures and
inert host), `0ea75b2` (preview discovery), `6b53b28` (linear remapping), `4261198`
(desktop virtualization/lazy selection/profiling driver), `2e7c030` (iOS).

Raw evidence remains under `/tmp/riwork-perf-*`: `final-comparison.json`,
`full-summary.json`, acknowledged action logs, Debug build/test logs, Simulator
XCTest results, and Time Profiler traces. The isolated debug app and fixture host
were stopped after measurement; no push, rebase, update or additional worktree was
used.

## Review fixes and polish merge

`a7eb610` fixes the retained-cell action before the requested `git merge
local-build`. Merge commit `25cff5a` has parents `a7eb610` and `0d17fbe` and
includes the iOS polish/performance conflict resolutions. The desktop follow-up
preserves polish's containers, gutter text, CR cleanup, full-width stderr tint
and nonselectable omission markers while combining consecutive ordinary output
lines into a text run. A typical head/marker/tail output now has two selectable
layouts rather than fifteen. Plain output cleanup borrows the source and only
copies the bounded preview; the extra compact preview is computed only when its
disclosure needs it.

Review source locations in the merged tree:

- Stale equatable-cell closure: `ios/RiWorkRemote/ChatView.swift:1099` reads the
  current projection inside `toggle`, with a prepend/retained-action anchor test.
- Shared strip filtering: `ios/RiWorkRemote/ChatActivityRows.swift:46` is used by
  nested rows, flattened row construction and the virtualized footer. Only images
  actually drawn inline are excluded.
- One cleaned and bounded output snapshot: `ChatActivityRows.swift:699`; excerpt
  snapshots at `:686` bound parsing too. UTF-8 windows preserve scalar boundaries,
  full newline counts drive omission labels, and copy retains complete source.
- Batched desktop text and borrowed cleanup: `crates/riwork-desktop/src/chat_view/cards.rs:246`
  and `:590`. Shared run keys keep full/offscreen Copy and per-link styling exact.
- Image completion fallback: `chat_view/media.rs:1743` remeasures the list when
  an image's owner is prose or an activity artifact rather than its own item.
- Selection retention: `chat_view/select.rs:235` prunes removed/hidden row IDs;
  `selection_document.rs:67` builds unseen Copy sources without retaining them.
  `select.rs:114` safely falls back if full-source UTF-8 offsets have changed.
- iOS fullscreen previews decode native resolution off the main actor; bounded
  thumbnails retain their separate cache. Canceled decodes cannot overwrite a
  newly selected page. A held-decode/paging test covers that race. Elapsed labels
  use the shared Core title and duration formatter.

The review's merged 2,200-call native drawn-frame result was **18.7 ms**.
Three follow-up guard runs measured **6.804 / 6.313 / 6.145 ms** medians, with
0.340–0.411 ms/event, 19.6–22.3 ms/row toggle and 18.8–19.8 ms/Expand all.
The preview test's three fresh discovery runs measured 174.9 / 181.6 / 189.9 ms.
The enabled guards now use 25 ms/drawn frame, 60 ms/toggle and 1.5 ms/event;
preview remains 500 ms with a median of three and functional assertions on every
run. Draw and toggle guards also use medians of three while retaining actual
render-count and lazy-selection checks. The repository has no CI workflows;
these tests run in the normal Cargo/XCTest commands.

The complete merged Debug Time Profiler run is
`/tmp/riwork-perf-review-merged.trace`. All scripted actions were acknowledged,
the isolated app quit normally, and the inert fixture host was stopped. Comparison
data is `/tmp/riwork-perf-review-comparison.json`. The previous column is the
pre-polish perf HEAD's matched profile, not the review's unoptimized merged tree.

| Desktop native fixture scenario | Previous perf HEAD (ms) | Merged/fixed (ms) |
|---|---:|---:|
| Text event batch median / frame p95 | 0.389 / 14.140 | 0.408 / 14.906 |
| Output event batch median / frame p95 | 0.090 / 14.116 | 0.079 / 12.995 |
| Scroll frame p95 | 38.922 | 32.752 |
| Group toggle handler / frame p95 | 32.919 / 30.365 | 24.566 / 36.264 |
| Expand all handler / frame p95 | 22.921 / 27.194 | 36.220 / 39.234 |
| Collapse all handler | 25.052 | 20.065 |
| Normal / Verbose handler median | 33.140 / 21.985 | 38.036 / 26.357 |
| Preview discovery handler | 147.151 | 147.296 |

Merged iOS measurements use the same Simulator and test harness. Polish adds
container/artifact presentation work; the table reports that cost as measured.

| Exact shared 2,400-item iOS fixture | Previous perf HEAD (ms) | Merged/fixed (ms) |
|---|---:|---:|
| Streaming event | 22.042 | 24.407 |
| Scroll frame | 4.323 | 4.645 |
| Normal / Verbose toggle | 76.791 | 71.584 |
| Expand / Collapse all | 146.554 | 195.262 |
| Individual row toggle | 29.111 | 34.665 |
| Open image preview | 69.274 | 61.525 |

The independent self-contained 2,200-item Simulator guard measured
20.013 ms/event, 4.821 ms/scroll frame, 73.861 ms/mode toggle,
143.619 ms/bulk toggle, 28.980 ms/row toggle and 51.580 ms/preview. Both fixtures
evaluate one visible row per token, fewer than three output bodies per bulk
toggle, no token projection rebuild and no activity-timer list rebuild. Core's
2,200-item projection measurement is 28.603 ms uncached versus 0.001 ms cached
per token and 1.489 ms/toggle.

Scoped merged Simulator validation: 115 Core projection tests, 27 viewer tests,
two dense performance tests and six container/image/reader/retained-cell-anchor/
keyboard/stream-follow tests passed. Logs: `/tmp/riwork-merged-ios-projection.log`,
`/tmp/riwork-merged-ios-containers.log`, `/tmp/riwork-merged-ios-final-perf.log`.
The held-decode paging regression and native 6144 px decode check also passed
separately before the final viewer suite.

The requested `cargo test -p riwork-desktop -p riwork-engine` passed on the merged
tree: **502 desktop and 950 engine tests**, with 6 and 118 existing ignored
tests respectively. The explicitly ignored offscreen partial-Copy test was also
run separately and passed. The first full engine run hit the unchanged
`a_terminal_orchestrator_is_typed_into_only_at_a_fresh_idle_prompt` assertion
(no fake-pane delivery). An isolated retry and the complete second run passed.
Its nonblocking delivery lock can skip a tick during concurrent test subprocess
launches; no unrelated engine code was changed. Logs:
`/tmp/riwork-perf-review-full-cargo{,-retry}.log` and
`/tmp/riwork-perf-review-engine-retry.log`.

Deferred minor review item: moving a subagent's images after its virtualized
children requires a trailing virtual fragment. The debug command poller remains
per-chat and environment gated. Output-only header invalidation predates this
pass. Large single-message line-level virtualization remains deferred as above.
