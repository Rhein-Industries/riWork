# RiWork performance audit handover — 2026-11-06

**Subsequent deployment:** The user later authorized building, installing and
restarting the completed improvements. The release desktop is now installed at
`/Applications/Somtus.app`; five windows and all 39 terminal panes were preserved.
The older chat backend remains running to preserve a waiting chat. See
[deployment evidence and scope](performance-audit-deployment-2026-11-06.txt).
Pass11 optimization work remains unstarted. The boundary below describes the
earlier handover turn, before this deployment authorization.

## Where the work stopped

The long-running performance goal is still active and incomplete:

> Improve RiWork performance across the codebase through deep analysis; continue until no further improvements are found.

Pass10 is the latest completed and validated pass. Pass11 had just begun: the 40-file pass10 source checkpoint was reverified with no mismatches, and no `AGENTS.md` files were found in the workspace or its ancestors. The next intended targets were large-search cancellation, remaining shared-State copies, host-coordinator wakeups, and rendering/update costs.

No pass11 source work has been completed. Followups for the search and engine tasks returned errors before starting because `gpt-6-astra` is unsupported for this account. No workspace/rendering task was dispatched. Root work on the host coordinator had not started. The current user instruction was to make no code changes and write this handover; this turn only adds this Markdown file.

## Completed work and authoritative evidence

The detailed pass10 report is [performance-audit-2026-11-06-pass10.txt](performance-audit-2026-11-06-pass10.txt). The overall history is in [performance-audit-2026-11-06.txt](performance-audit-2026-11-06.txt) and the pass2–pass9 reports beside it. Detailed source fingerprints, proof harnesses, measurements, and logs are under `target/performance-audit/`.

Pass10 adopted four changes:

1. **Host-cache persistence:** `Cache::save_owned` consumes the detached cache snapshot, eliminating a second full copy after `HostsService::persist` has already detached it. The first copy under the state lock remains. Large-fixture requested allocations fell by about 42.7 MB; optimized helper timings improved for representative tiny, typical, and large cases. Debug timings were mostly flat. See `target/performance-audit/pass10-host-cache-owned/`.
2. **Overview and schedule reads:** concrete readers use shared Store State snapshots, avoiding residual full-State copies while preserving compatibility for existing owned `OverviewSource` implementations. The cached store was already parsed; this is not a reparse improvement. Large-fixture allocation reductions were substantial, and all optimized-caller median samples improved. Measurements are caller helpers linked to compatible cached Debug dependencies, not release-app results. See `target/performance-audit/pass10-shared-readers/`.
3. **Chat review reads:** `files()` materializes only selected survivors, and `read()` reuses classification statistics. The deferred-materialization variant was chosen after comparing a reverse-entry alternative. Differential proof covers 128,038 serialized-result comparisons per profile; repeated-patch timings improved sharply, while unique-row CPU results remain mixed. See `target/performance-audit/pass10-diff-review/`.
4. **Remote idle polling:** the per-service planning loop retires when validated subscriptions request no remote data, while cached-title subscribers remain live. Active polling, planning, deduplication, and in-flight completion are preserved. Three current-source tests cover demand flags, worker lifecycle, resubscription, service ownership, and weak references. This removes recurring idle wakeups structurally; no live CPU/battery gain was measured. See `target/performance-audit/pass10-remote-idle/`.

Combined validation passed: 260 native library tests (10 ignored), 12 `chat_bridge`, 1 `chat_search_bridge`, and 5 `dashboard_bridge` tests; the focused engine host-session tests passed 8/8. Exact commands, source/binary hashes, timestamps, and output are in `target/performance-audit/pass10-native-validation/` and `target/performance-audit/pass10-host-cache-owned/engine-host-sessions-tests.log`. This is scoped validation, not a clean full-workspace claim.

The authoritative pass10 source list is `target/performance-audit/pass10-production-sources.sha256` (40 files; manifest SHA-256 `651be34add3f764c8ff7f4d47f702192f0226729bcbbf1afc738b2a6a6cf2e07`). Its verification record is `target/performance-audit/pass10-final-verification.json`, timestamped `2026-11-06T10:53:42.874043Z`. At pass11 start, all 40 listed hashes still matched. Treat this as a scoped checkpoint, not as a fingerprint of every dirty workspace file.

Prior passes also adopted host-producer queue batching, shared settings and layout state, bounded file metadata reads, Markdown borrowed-line processing, JS model update improvements, chat review and screen-output scanning improvements, plus other scoped changes. The pass reports and artifacts record their semantics, rejected variants, measurements, and limitations; consult them before revisiting those areas.

## Next technical targets

The next search-cancellation candidate is described in `target/performance-audit/search-cancellation-next/plan.txt` (see also its `sources.json`). Current search checks cancellation around whole items. A large cold JSON item can still be fully pretty-serialized, joined, lowercased, scanned, and cached before the next generation check. A warmed lazy preview can also materialize a large item.

If the user resumes implementation work, investigate cancellation checks during large JSON preparation using coarse UTF-8-safe chunks and preserve the canonical serde JSON formatting, Unicode lowercasing, matching, and preview-offset behavior. Cancellation must be distinct from serialization failure: do not fall back to full serialization after cancellation and do not admit partial cache entries or hits. Keep the ordinary non-cancellable path canonical. Existing exact callback-count tests may need semantic assertions instead. The plan calls out Unicode sigma/context pitfalls, cache-bound accounting, the distinction between scan bounds and whole-item latency, and the need for differential proof against the current implementation before timing.

Other high-value unfinished targets:

- `crates/riwork-engine/src/host_sessions.rs::local(home)` still reads an owned full Store State. Check whether it can use `snapshot_shared` without changing filesystem-stamp checks, errors, shell sampling, or operation order. Inventory other concrete readers and benchmark actual caller work, not only a synthetic clone.
- `crates/riwork-nsh-native/src/hosts.rs` coordinator wakes every 250 ms. It also performs explicit requested refreshes, periodic dirty persistence and registry refreshes, local/remote polling with distinct cadence/backoff, and keeps a local slot reserved. Do not simply stop it when there are no subscribers; preserve those duties and request wakeups. Pass10 already solved a related but separate remote polling worker.
- Active remote polling still uses 500 ms cadence; deadline scheduling remains open.
- Large-tail Markdown parsing/splicing, edited layout queue growth, render/frame pacing, startup/terminal attach/release, and representative UI/mobile measurements remain open.

For UI inspection or interaction, use Somtus setup's **Cua.ai Driver through the `cua-driver` MCP server only**. Read the relevant tool descriptions and current state before interacting; pass the target app `pid` on every input call. The driver refuses input to Somtus's own live windows and input with no target app while Somtus runs. If Cua is unavailable or reports missing macOS permissions, report that and point to Somtus Cua setup; do not silently switch providers. No desktop interaction, screenshot, GUI launch, app restart, deployment, or installed-app measurement occurred in pass10 or the pass11 kickoff.

## Workspace cautions

The repository has many uncommitted changes from prior work. `git status --short` showed edits across the React app, core/engine/native/desktop/terminal crates, and `remote/`, along with untracked test/support files and `output/`. Some of this is the established performance work and some is unrelated design/settings/project-tree work. **Do not reset, clean, revert, overwrite, or assume the working tree is clean.** Inspect before editing and preserve unrelated changes. The exact current status can be re-read with `git status --short`.

Known pre-existing or baseline failures are documented in the reports: core/native settings expectation failures, workspace layout geometry failures, engine worker/temp-alias behavior, native services periodic-tick expectation, and an old remote polling interval assertion. Do not claim a full-workspace green result or spend time broadly rerunning those suites without a specific reason.

## Working rules and current boundary

- The user's latest explicit task was documentation only: do not change code while completing it. This handover does not itself authorize code edits or create a new agent. Continue code work only when the user resumes or authorizes that work.
- The broader performance goal remains active; do not mark it complete or imply all possible gains have been exhausted.
- Preserve raw evidence, rejected candidates, source variants, and historical manifests. Do not rewrite old pass checkpoints with newer source hashes.
- Serialize timing runs after builds/tests/proofs have finished; record uncontrolled desktop load. Avoid attribution beyond the measured workload and build profile.
- Do not stop unrelated processes, use paid providers, send external messages, commit, reset, or push.
- Use `rg`/`rg --files` for source discovery. Check new untracked files separately for whitespace because `git diff --check` omits them.
