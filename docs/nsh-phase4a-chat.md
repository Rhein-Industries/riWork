# NSH Phase 4a: chat frontend core

```yaml
assignment: 6a41bb58-3ee4-4f3e-8ba4-872f8e46b3da
branch: nsh-p4-chat (off nsh 4e033b8)
verification_environment: isolated_tmp_homes_headless_only
live_targets_touched: none
task_status: not_marked_done
```

## What this slice adds

| Part | Where | Notes |
|---|---|---|
| `Chat` module, `ChatHandle` class | `crates/riwork-nsh-native/src/chat/mod.rs` | Contracts §1.6 and its Phase 4a schema 1 table. Composed into the one `riwork/native` registry. |
| Feed | `chat/feed.rs` | Follower and ordered deliverer threads over the existing `client::Subscription`/`Client`. Gapless resubscribe, 500 ms→5 s backoff, deleted after 2 misses, unreadable-event notice, single retry only for broken connections, exactly-once submissions (CHATT-001..014). |
| View model | `chat/view.rs` | Event fold (`Applied`), engine `ProjectionCache` once per batch, flattened rows, viewport-paged bodies, compacted patch ops, factual activity titles including `· Interrupted`/`· Failed` (CHATT-026), banners, footer status, approvals, questions, toolbar choices. |
| Worker | `chat/worker.rs` | First change delivered at once, then at most one per 16 ms. The 1 s clock runs only while a turn runs. Bounded queue (64) with `resync` reset. Blocks on its inbox when quiet. |
| Host access | `chat/service.rs` | Only the desktop executable may run `riwork chat ensure` (`allow_host_start`, called by `nsh_host`). Debug/test-support fixture host for `/tmp` homes replays recorded Codex streams through the real Codex driver. |
| GPUI-free ports | `chat/markdown.rs` (+`document.rs`, `links.rs`), `chat/drafts.rs` (+`references.rs`), `chat/toolbar.rs`, `chat/activity_summary.rs` | Ported from the uncompiled parity reference, which stays byte-identical. Markdown becomes nsh/base TextView documents and streams as tail splices. Draft files keep their exact format. |
| Chat tab UI | `apps/riwork/chat/ChatTab.tsx`, `chat/model.ts` | Virtualized `List` of row headers. Each body has its own signal, so streaming re-renders one cell. TextView documents for Markdown. Command output box. Approval bar and question panel, always visible above the composer. Composer with drafts, send (steers while running), Stop/⌘., ⏎/⎋ approval keys (700 ms guard, offered decisions only), model/effort/Fast/mode pickers and provider switch. |
| Workspace routing | `apps/riwork/Workspace.tsx` | `chat:`/`chat-pending:` panels render `ChatTab` once first shown (CHATT-015), so restored background tabs never subscribe at launch. Non-UUID ids keep the placeholder. |

Session and tab writes: chat creation registers its shared tab entry inside the
chat host (`TabStore::register_created`, as before). The bridge adds no session
or tab write and no side-channel adapter.

## Acceptance (isolated RIWORK_HOME, fixture chat host, replayed provider streams)

| Requirement | Evidence | Result |
|---|---|---|
| < 100 materialized rows (bridge) | `chat_bridge::a_2200_call_disclosed_turn_in_normal_mode_materializes_under_100_bodies_and_never_reprojects_tokens`, `verbose_expand_all_on_2200_calls_keeps_bodies_windowed_and_markdown_streams_as_tail_splices`, `a_replayed_2200_call_turn_reaches_the_subscriber_with_bounded_bodies` (real host and Codex driver) | pass |
| < 100 materialized rows (UI) | `Chat.test.tsx` P4-CHAT-06: 2,200-call turn disclosed in Normal mode; live List cells and bodies < 100 after disclosure and after scrolling | pass |
| Zero token reprojection | `projection_runs` unchanged over 120 output deltas and 120 Markdown deltas | pass |
| < 1.5 ms per streamed event at the bridge (structural, every build) | Debug: 0.020 ms (output append, Normal, 2,200 disclosed); 0.045 ms (Markdown splice, Verbose, 2,200 open). Includes JSON payload creation. | pass |
| Coalescing ≥ 16 ms, no polling, quiet delivers nothing | `streaming_is_coalesced_to_one_delivery_per_16_ms_and_a_quiet_chat_delivers_nothing`. TSX polling guard (patterns of `check-polling.sh`) has no matches; chat TSX has no timers. | pass |
| Approval round trip | Bridge: `approval_round_trip_through_a_replayed_codex_stream`, `questions_are_answered_once_then_the_access_request_is_allowed_for_the_session`. UI: P4-CHAT-01 (Allow button), P4-CHAT-02 (⏎ allows in an empty box; ⎋ denies a file change), P4-CHAT-04 (question panel, then Allow for session). The fake provider recorded no mismatch. | pass |
| Drafts across windows | Store: `the_first_composers_of_two_windows_never_share_an_editor_identity`. UI: P4-CHAT-07 (two apps, i.e. two JS runtimes in one process, on one chat; each composer is its runtime's first and both are at the same edit generation; the first window's send leaves the second window's draft stored and restorable) and P4-CHAT-08 (a composer's own send clears its stored draft). Both also assert one composer per tab for the whole turn. | pass |
| Requests across panel deliveries | P4-CHAT-09: picked and typed answers, and the focused answer box, survive 8 provider refreshes while the question waits; the question then goes and the access approval comes and goes. P4-CHAT-10: an expanded five-line approval detail stays expanded through 8 refreshes, then Allow ends the turn. | pass |
| Draft restore | Bridge: `drafts_save_restore_survive_a_reload_and_clear_only_after_a_receipt` (plain legacy file bytes, restart, refused send keeps a saved submission, receipt clears) and `a_deleted_chat_reports_deleted_and_forgets_its_draft`. UI: P4-CHAT-05 (closed tab restores into an empty composer). Draft store: 38 unit tests including legacy-byte goldens. | pass |
| Interrupted-turn handling | Bridge: `an_interrupted_turn_closes_as_interrupted_and_stops_running`. UI: P4-CHAT-03 (Stop → `· Interrupted`, Send returns). | pass |
| Steering while running | Bridge: `a_message_sent_while_a_turn_runs_steers_it` (Codex `turn/steer` recording) | pass |
| Patch application agrees with the worker | Every host test ends with `assert_mirror_matches` (folded patches equal a fresh reset). | pass |

The approval, decline, interrupt, question, steer and turn recordings are the
engine's own `testdata/codex/*.ndjson`. The 2,200-call recording is synthesized
in the same Codex app-server frame format (`service::fixture::large_turn`).

## Runs

| Check | Result |
|---|---|
| `cargo test -p riwork-nsh-native -p riwork-nsh-workspace` | all pass (native lib 91 + 1 ignored, `chat_bridge` 11, services 15, layout and other suites unchanged) |
| Same with `riwork-nsh-native --features test-support` | all pass (lib 93 + 1 ignored) |
| `cargo test -p riwork-desktop` (pinned Ghostty install from the Phase 1 worktree via `RIWORK_GHOSTTY_INSTALL_DIR`, read-only) | 65 passed |
| `chat_bridge` repeated | 15/15 consecutive green runs after the fixes below |
| `nsh check apps/riwork` | passed |
| `nsh test apps/riwork` in a generated p1-layouts home with fake sessions and `RIWORK_NSH_CHAT_FIXTURE=1` | 97 passed, 0 failed (91 existing + 6 chat). No tmux files. |
| `nsh test apps/riwork --filter P4-MODEL` | 4/4 |
| `nsh test crates/riwork-nsh-native/tests/app` | 1/1 |

Fixes found by the tests:

- A Markdown message that shrank produced a truncating splice with no blocks, and the view dropped it as a no-op.
- The test mirror rebuilt row order from a `HashMap` (a test bug).
- A test that loaded a second live draft store on the shared directory swept the writer's temp files. It now loads a copy; one process per home is the host's job.

Environment: Rust 1.99.1, `--locked --offline`, pinned NSH CLI 0.3.0 (`76fa8a7`)
at `/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV`. RIWORK_HOME, runtime, tmux and
TMPDIR were under a short `/tmp/rw-p4c.*` root, because macOS limits socket paths
to 104 bytes. Test `HOME` was `target/nsh-integration-home-p4c`. No app window was
launched and no live chat, window or state was read or written.

## Follow-up: draft editor identity (task 2a017126)

Source review found that each composer took its draft editor identity from a
module-level `++editors` counter, which starts again in every window's JS
runtime, while the draft store matches `(editor, generation)` owners
process-wide. The first composers of two windows on one chat both got
identity 1, so one window's send could clear the other window's stored draft
whenever their edit generations matched.

- `Chat.newEditor()` (sync) returns an identity from one process-wide atomic
  counter (`chat::drafts::new_editor`), as the GPUI composer's entity id was.
  Each mounted composer takes one. Owners, generations, `begin_submission`,
  receipts and the draft files are unchanged.
- The regression test found a second cause of identity churn. `ChatTab` chose
  between the deleted notice and the transcript with `deleted()` read straight
  from `panel()`, so every panel delivery rebuilt the List, Requests and
  Composer: 7 composers per tab over one replayed turn. Each rebuild minted a
  new identity, restarted the generation and re-restored the stored draft.
  `deleted` is now a `computed` boolean.

Negative controls with the final tests:

| Variant | Result |
|---|---|
| Per-runtime `++editors` counter, `computed` kept | P4-CHAT-07 fails: stored draft `null`, expected `"draft"` |
| `newEditor()`, panel-derived `deleted` | P4-CHAT-07 fails: `[7,7]` composers, expected `[1,1]`; P4-CHAT-08 fails: 6, expected 1 |
| Both fixes | 8/8 chat tests pass |

The approval bar and question panel had the same rebuild problem; it is fixed
in the follow-up below.

## Follow-up: requests across panel deliveries (task 77827768)

`Requests` rendered `ApprovalBar` and `QuestionPanel` behind
`props.panel().approvals.length > 0` (and the questions equivalent), and
returned them in a fragment. Every panel delivery rebuilt both, so an expanded
approval detail collapsed and picked or typed answers were lost on the next
provider update while the request waited.

Two causes, both fixed in `ChatTab.tsx`:

- The gates read the whole panel. Each request is now a `computed` selector
  with content equality (`sameContent`): a delivery is always a new object, so
  identity alone would still notify. Each request is mounted per request id.
  The selector keeps the last request through the delivery that ends it, so the
  bar's own expressions never read `undefined` before the gate removes it.
- Expressions a component returns in a fragment re-run with the parent's
  other expressions (here `panel().notices.map(...)` and the state lines,
  which read the panel). The new tests showed this: with the selectors alone,
  the gate still re-ran on every delivery. Each gate now sits in its own `View`.

Fixture: `service::fixture` derives `question_refresh` and `approval_refresh`
from the recorded `question` and `approval_accept` streams. While the request
waits, the provider sends 8 `warning` notifications 300 ms apart, which are
panel deliveries that leave the request alone. `approval_refresh` also has a
five-line command detail, so the bar shows "show 2 more lines".

| Variant | P4-CHAT-09 (question) | P4-CHAT-10 (approval) |
|---|---|---|
| Original gating | fails: typed answer `""`, expected `"no thanks"` | fails: expansion lost |
| Content selectors and id gates, still in the fragment | fails the same way | fails the same way |
| Own `View`s and id gates, identity selectors | fails: the answer box was rebuilt (a key press no longer reaches it) | passes |
| Final | passes (5/5 repeats) | passes (5/5 repeats) |

The original gating failed both tests in 3/3 repeats.

## Known gaps and divergences (not in this slice)

- Verbose internals (rich tool cards, diffs, file excerpts), images and attachments, find, selection export, dictation, the new-chat chooser, `Chat.claimTab`, the Latest capsule and row keyboard focus are seams only. The pane menu's chat entries stay deferred (P1-PANE-12).
- Long final answers are not split into sections (CHATT-042..044). A single huge answer is one List item, as in GPUI (CHATT-301).
- Banners are the open notices, newest per kind (at most three), without expiry timers or history (CHATT-188..190 partial).
- Saved submissions show Dismiss and Replace only; Resend is a follow-up (CHATC-036 partial). Held-key and repeat guards depend on NSH key events that do not exist yet (`GAP:key-repeat-and-keyup`).
- The workspace's `closePanel` and Exit for chat tabs still use the W1 `session_io` adapter (the §8 known divergence, unchanged here).
- Wall-clock frame budgets (D-PERF-1 rule 2) need release builds on a quiet host and were not measured. The debug toggle on 2,200 calls is reported at 12.4 ms.
