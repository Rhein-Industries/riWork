# RiWork chat improvement roadmap

Improve the reading, review, and continuation of agent work in RiWork. Deliver the desktop output improvements first, then conversation workflows, durable worker coordination, and mobile parity. Keep the existing native UI, chat host, operation receipts, and provider adapters.

Updated: 2026-10-19. The user authorized recording this roadmap and starting implementation. Status records completed behavior, not merely code being drafted. Each item remains open until its acceptance checks pass.

## Baseline

The previous desktop batch implements selected-text replies, diff-comment drafting, turn navigation, richer per-turn activity summaries, and recovery of text, completed attachments, and unresolved submissions. These changes were tested and included in a local release bundle. They remain part of the current working tree.

Syntax highlighting, code and table wrapping, Markdown/CSV table copying, image viewing and zoom, worker summaries and follow-ups, context handoffs, schedules, and operation receipts already exist. iOS already saves drafts, completed attachment references, and interrupted sends. The additions below build on those capabilities.

## Delivery order and status

The first delivery covers items 1–4. The second delivery covers items 5–9, completing the first output batch and adding diagram, code, file, and image actions. Independent changes proceed in parallel; shared transcript selection and rendering hooks are integrated centrally.

| Item | Work | Status | Dependencies |
| --- | --- | --- | --- |
| 1 | Live command logs | Implemented and tested | Existing command items and selection |
| 2 | Better plan cards | Implemented and tested | Existing Markdown plans and native save dialogs |
| 3 | Search collapsed content | Implemented and tested | Stable selection keys and Find |
| 4 | Responsive long answers and attachment recovery | Implemented and tested | Existing parse caches and draft validation |
| 5 | Structured reply references | Implemented and tested | Existing feedback and draft persistence |
| 6 | Focused file review | Implemented and tested | Existing reported edits and diff feedback |
| 7 | Rendered diagrams | Implemented and tested | Existing image viewer; bounded renderer |
| 8 | Code block actions | Implemented and tested | Native save/open actions |
| 9 | File and image actions | Implemented and tested | Existing scoped file/image access |
| 10 | Long document navigation | Planned | Items 3–4; stable heading identities |
| 11 | Rich artifact previews | Planned | Artifact transport and isolated rendering |
| 12 | Editable follow-up queue | Planned | Host operation receipts and persistent queue |
| 13 | Draft stash and prompt history | Planned | Existing complete draft snapshots |
| 14 | Branch from a response | Planned | Stable source turn and log boundaries |
| 15 | Bring findings back | Planned | Items 14 and 19; bounded handoffs |
| 16 | Turn checkpoints and undo | Planned | Workspace ownership and snapshot capture |
| 17 | Background delivery | Planned | Item 12; process claims and receipts |
| 18 | Reliable parent delivery | Planned | Items 12 and 17; assignment identity |
| 19 | Tracked worker follow-up rounds | Planned | Item 18; existing worker controls |
| 20 | Deeper worker inspection | Planned | Verified provider child identities |
| 21 | Mobile output parity | Planned | Completed desktop contracts and fixtures |
| 22 | Recent chats available offline | Planned | Existing mobile catch-up and paging |
| 23 | Receipt-aware mobile outbox | Planned | Stable operation IDs through remote API |
| 24 | Recover unfinished attachments | Planned | Local retention and upload identity |
| 25 | Accessible rich output | Planned | Native accessibility and mobile output parity |

## First output batch

**1. Live command logs.** Preserve terminal colors and bold text. Follow new output independently within each command panel, pause when the reader scrolls up, and allow them to return to the latest output. Provide access to earlier retained output without mounting an unbounded document. Keep copying and selection tied to the displayed text, with terminal control sequences excluded. Main areas: desktop `chat_view/rows.rs`, `cards.rs`, selection documents, and a bounded terminal-style parser.

Acceptance: ANSI colors and resets, carriage-return progress updates, fragmented sequences, long logs, theme contrast, paused following, and copying are verified in isolated fixtures. A command panel's scrolling does not pull the transcript away from its reading position.

**2. Better plan cards.** Show a compact preview for long Markdown plans, with Expand/Collapse, Save as Markdown, and Save to workspace. Use a useful filename and preserve the exact plan source in saved files. Keep Copy and normal short-plan rendering. Main areas: desktop plan rows and native file save flow.

Acceptance: short/long/streaming plans, Unicode, narrow windows, keyboard activation, cancelled saves, write failures, and exact saved contents are checked. Saving uses an explicit destination and never silently overwrites a workspace file.

**3. Search collapsed content.** Search closed tool details and Activity groups, then open only what is needed for the selected match. Keep current matching, navigation, copy, and selection semantics coherent. Main areas: `chat_view/find.rs`, selection documents, and transcript projection.

Acceptance: hidden matches are discoverable; Next/Previous reveal the right item; projection changes retain stable match identity; current selection and composer contents survive; irrelevant groups stay closed. Large outputs remain bounded and any search limit is explicit.

**4. Responsive long answers and attachment recovery.** Reuse completed Markdown blocks while an answer streams, keeping syntax highlighting incremental. Move recovered attachment content validation off the UI thread while preventing unvalidated attachments from becoming sendable. Main areas: Markdown parsing/caches, selection projection, `chat_drafts.rs`, and draft restoration.

Acceptance: incremental output equals a full parse across headings, lists, quotes, tables, complete/incomplete fences, Unicode, and non-append replacements. Benchmark long streamed answers and attachment-heavy reopen operations before/after; retain responsiveness, source identity, and draft ownership. No performance improvement is claimed without measurements.

**5. Structured reply references.** Upgrade plain quoted draft context into editable/removable source chips with Go to source. Preserve exact source identity and revision, support several references in one draft, and serialize readable text for providers. Add saved inline review comments associated with the file and revision; handle stale sources explicitly.

Acceptance: references and comments survive draft recovery, remain navigable, and preserve attachments and existing writing. Removing/editing a reference affects only that reference. No action sends a message unless the user invokes Send. Durable comment persistence and revision re-anchoring are RiWork extensions.

**6. Focused file review.** Group changed files by folder, offer expand/collapse-all, navigate between files and hunks in a focused review surface, and collect comments. Keep reported changes distinct from actual net workspace changes until item 16 provides snapshots.

Acceptance: many files remain usable, renamed/deleted files are represented honestly, diff selection keeps old/new coordinates, and returning to chat restores the reading position.

## Richer output

**7. Rendered diagrams.** Render completed Mermaid fences with Diagram/Source switching, enlargement through the image viewer, and source fallback on errors. Bound rendering work and preserve readable source while streaming.

**8. Code block actions.** Add Save snippet and a larger code reader/editor. Offer a deliberate handoff of a completed shell snippet to a chosen terminal. Preserve the existing filename/language caption, highlighting, Copy, and Wrap; never execute merely because a block appeared.

**9. File and image actions.** Add relative/full-path copy, Reveal in Finder, opening in the system's preferred application, Save image, Copy image, and Open original where access is available. Reuse existing path scopes and surface missing or changed files clearly. The system file association chooses the editor/application; this delivery does not add a separate RiWork editor preference.

**10. Long document navigation.** Add heading anchors, native collapsible Markdown sections, and an optional per-answer contents menu. Include rich clipboard HTML alongside the plain-text fallback where it preserves the selected content accurately. Integrate headings and disclosures with Find and keyboard navigation.

**11. Rich artifact previews.** Start with isolated HTML previews and video playback. Add interactive tool-provided app surfaces only after artifact transport, lifecycle, permissions, and containment are defined. Reuse existing image handling instead of replacing it.

Acceptance across items 7–11: text remains selectable/copyable, source fallback is available, keyboard access and narrow layouts work, asynchronous media completion does not steal focus or displace the reader, and heavy content loads only when needed.

## Conversation workflows

**12. Editable follow-up queue.** Separate Send now from After this turn. Persist prompts and attachments in the host; support edit, reorder, remove, pause, and explicit promotion to steering. Track execution order separately from enqueue order. Hold ambiguous/recovered work for review and bind dispatch to operation receipts.

**13. Draft stash and prompt history.** Set unfinished writing aside, prepare another message, then restore the complete draft with its attachments. Recall prior prompts without overwriting current writing. Bound retained entries and make removal explicit.

**14. Branch from a response.** Create a separate conversation from an exact selected-turn boundary, with parent/source links. Exclude later messages, retain the original conversation, and use bounded context handoff when provider-native forking is unavailable.

**15. Bring findings back.** Bring selected new findings from a worker or branch into the parent. Track which source ranges were covered, show important omissions, and avoid resending the whole conversation. This action transfers conversational context; any file integration is a separate explicit action.

**16. Turn checkpoints and undo.** Capture before/after workspace states bound to the worktree and turn. First deliver true net diffs; then offer restoration that checks workspace ownership and newer edits. Shared or conflicting workspaces must not be silently reset.

Acceptance across items 12–16: stable identities survive reconnect/restart, cancelled work cannot dispatch later, uncertain delivery is not replayed automatically, branch boundaries are exact, and restoration never overwrites unrelated work.

## Worker coordination

**17. Background delivery.** Move scheduled native-chat starts and orchestrator notifications into an owned background service so delivery continues with desktop windows closed. Retain persistent schedules, cross-process claims, receipt identity, and uncertain-outcome handling. Treat terminal delivery separately until equivalent support exists.

**18. Reliable parent delivery.** Route child questions and results to the assigning parent, with pending/delivered state. Persist notification identities, withdraw resolved questions, suppress obsolete/cancelled notifications, and queue behind active parent work. Permission decisions remain explicit.

**19. Tracked worker follow-up rounds.** Make a revision request a distinct assignment/result round in an existing worker. Preserve original results, link each completion to its round, and scope cancellation to the expected run. Extend the existing worker detail instead of creating a duplicate dashboard.

**20. Deeper worker inspection.** Show real child conversations and provider-reported workflow phases where verified identities exist. Link every summary to its evidence and read long transcripts incrementally. Do not infer unavailable phases or report an unsupported provider capability as active.

Acceptance across items 17–20: parent targeting and duplicate suppression survive process loss; cancelled rounds cannot deliver into a later assignment; window closure does not lose native-chat work; summaries expose their source and freshness.

## Mobile consistency and recovery

**21. Mobile output parity.** Bring desktop selection replies, diff feedback, turn navigation, activity summaries, normalized tool results, code highlighting, richer plan documents, and old/new diff coordinates to iOS. Reuse shared contracts and fixtures.

**22. Recent chats available offline.** Persist bounded recent-chat snapshots and reading positions per host. Show cached content immediately after relaunch with freshness, then reconcile through existing paging/catch-up. Enable decisions only after current state has been verified.

**23. Receipt-aware mobile outbox.** Keep uncertain submissions separate from newer writing. Carry stable operation IDs through the remote API and query receipts after reconnect. Add explicit queued/offline messages with edit/remove controls once acknowledgment is reliable; retain existing mobile draft recovery.

**24. Recover unfinished attachments.** Retain bounded local copies for pending uploads, offer explicit Resume/Restage after restart or expiration, and distinguish upload failure from expired remote data. Keep completed attachment recovery as the baseline.

**25. Accessible rich output.** Add VoiceOver turn/heading navigation, accessible reply/review actions, table cells announced with their column headers, and linear reading options for wide content. Preserve Dynamic Type and current spoken diff markers.

Acceptance across items 21–25: host identities never mix, cached content reconciles without duplicate messages, recovery preserves newer drafts, retained files have clear bounds/cleanup, and touch, hardware-keyboard, and VoiceOver flows reach the same actions.

## Implementation references

T3 references are pinned to the reviewed revision `a356999dbf632f17029aa62724e4f47541153a71`. Adapt the behavior into RiWork's Rust/GPUI and Swift architecture.

- Output: [command logs](https://github.com/pingdotgg/t3code/blob/a356999dbf632f17029aa62724e4f47541153a71/apps/web/src/components/chat/CommandOutputPanel.tsx), [plans](https://github.com/pingdotgg/t3code/blob/a356999dbf632f17029aa62724e4f47541153a71/apps/web/src/components/chat/ProposedPlanCard.tsx), [incremental Markdown](https://github.com/pingdotgg/t3code/blob/a356999dbf632f17029aa62724e4f47541153a71/apps/web/src/markdown-incremental.ts), [citation chips](https://github.com/pingdotgg/t3code/blob/a356999dbf632f17029aa62724e4f47541153a71/apps/web/src/components/chat/AssistantCitationChip.tsx), [changed files](https://github.com/pingdotgg/t3code/blob/a356999dbf632f17029aa62724e4f47541153a71/apps/web/src/components/chat/ChangedFilesTree.tsx).
- Rich content: [Mermaid](https://github.com/pingdotgg/t3code/blob/a356999dbf632f17029aa62724e4f47541153a71/apps/web/src/components/chat/MermaidDiagram.tsx), [Markdown behavior](https://github.com/pingdotgg/t3code/blob/a356999dbf632f17029aa62724e4f47541153a71/apps/web/src/components/ChatMarkdown.tsx), [HTML previews](https://github.com/pingdotgg/t3code/blob/a356999dbf632f17029aa62724e4f47541153a71/apps/web/src/components/chat/HtmlRenderFrame.tsx). Generic Save/Expand code actions and an answer contents menu are RiWork additions.
- Workflows: [queue controls](https://github.com/pingdotgg/t3code/blob/a356999dbf632f17029aa62724e4f47541153a71/apps/web/src/components/chat/QueuedRunsControl.tsx), [forks](https://github.com/pingdotgg/t3code/blob/a356999dbf632f17029aa62724e4f47541153a71/apps/server/src/orchestration-v2/ThreadForkService.ts), [checkpoint safety](https://github.com/pingdotgg/t3code/blob/a356999dbf632f17029aa62724e4f47541153a71/apps/server/src/orchestration-v2/CheckpointRestoreSafety.ts).
- Coordination: [background work](https://github.com/pingdotgg/t3code/blob/a356999dbf632f17029aa62724e4f47541153a71/apps/server/src/orchestration-v2/EffectWorker.ts), [delegated questions](https://github.com/pingdotgg/t3code/blob/a356999dbf632f17029aa62724e4f47541153a71/apps/server/src/orchestration-v2/DelegatedTaskQuestionReactor.ts), [handoffs](https://github.com/pingdotgg/t3code/blob/a356999dbf632f17029aa62724e4f47541153a71/apps/server/src/orchestration-v2/ContextHandoffService.ts), [workflow inspection](https://github.com/pingdotgg/t3code/blob/a356999dbf632f17029aa62724e4f47541153a71/apps/web/src/components/chat/WorkflowOpeningBlock.tsx).
- Mobile: [outbox](https://github.com/pingdotgg/t3code/blob/a356999dbf632f17029aa62724e4f47541153a71/apps/mobile/src/state/thread-outbox-model.ts), [environment cache](https://github.com/pingdotgg/t3code/blob/a356999dbf632f17029aa62724e4f47541153a71/apps/mobile/src/connection/environment-cache-store.ts). Native VoiceOver navigation is a RiWork addition.

## Verification and delivery

Use isolated stores and in-memory native fixtures for functional checks. Any desktop interaction uses RiWork's Cua.ai Driver MCP with an explicit target pid and a separate fixture home/runtime. Live user chats, terminals, and orchestrator windows are not test fixtures.

Run meaningful parser/state/native interaction tests for changed behavior, `cargo fmt --all --check`, and `git diff --check`. Centralize Cargo runs while agents edit disjoint files. Build and package after integration; record test counts, performance evidence, limitations, and remaining work here. Restarting the live app is a separate explicit action.

## Progress record

- 2026-10-19: Recorded all 25 authorized roadmap items. Started the first delivery, items 1–4. Items 5–25 remain planned.
- 2026-10-19: Implemented items 1–4: command paging/color/independent following, expandable and exportable plan documents, hidden-content Find, completed-prefix Markdown caching, and asynchronous attachment verification. Fixed the plan expansion scroll jump, an older display fixture's unintended feed threads, and a stale-window attachment retry that could overwrite another window's draft.
- Verification: the full desktop suite passed **676 tests, 0 failures, 8 ignored**. The normally ignored long selection test also passed separately. Both optimized performance fixtures below passed. Formatting and whitespace checks passed. Native interaction tests ran in in-memory GPUI windows with isolated stores; no live sessions were used for testing.
- Build: the locked desktop release build passed. A separate local preview was packaged at `target/chat-ui-preview/RiWork.app`; its deep/strict code-signature verification passed. The running app was not restarted. Source changes remain in the current working tree.

Current bounds: command logs retain up to 128 KiB / 4,000 rendered lines from the last 256 KiB of raw output; each page mounts at most 200 lines / 32 KiB. Find searches the current command page and retained tool/diff previews, and reveals its scope in the bar. The log reader handles ANSI styling and common progress controls; it is not a full terminal emulator. Ready attachments require successful verification, and recovery metadata remains saved while checks are pending or unavailable.

Completed deliveries: items 1–9. The remaining roadmap starts at item 10 and continues in the dependency order above.

### Items 5–9 implementation notes

- References retain immutable excerpts, message/file identity, SHA-256 revisions, and separately editable quote/comment text. Up to 16 references recover with a draft; quote/comment fields accept up to 8 KiB each, selected diffs up to 80 displayed lines, and serialized references up to 192 KiB in total. An explicit re-anchor requires a unique unchanged excerpt and retains the original provenance. Provider text identifies stale and user-edited quotes.
- Focused review has its own scroll state and a bounded folder browser. It shows reported edits, including honest missing/rename metadata, with at most 400 displayed patch lines; full patch Copy remains available. Find searches the current file preview. Back restores the conversation's position and follow state.
- Mermaid uses the complete pinned 11.17.2 browser bundle in a disposable, nonpersistent, windowless WebKit helper. Source is capped at 32 KiB, output at 2048 pixels per axis, active helpers at two, and retained rendered images at four per chat. Source remains available on errors, while streaming, and after eviction. The actual helper passed flowchart, sequence, class, state, Gantt, pie, mind map, ER, light/dark, malformed/oversized-input, and blocked-network fixtures.
- Snippet Reader and Save accept completed blocks up to 256 KiB. The reader edits an independent scratch copy and asks before discarding unsaved edits. A shell handoff copies the snippet and focuses the explicitly chosen local terminal; the user pastes it. File opening follows the OS application association.
- File actions recheck registered scope, regular-file/no-symlink access, and panel lifetime before acting; lists are paged. Image Save/Copy encode the displayed pixels as PNG in the background. A local original is revalidated against the displayed preview before opening; URL originals open only after a click. Exports share the existing explicit replacement check and atomic file writer.

The integrated desktop suite passed **713 tests, 0 failures, 8 ignored**. This includes reference recovery and newer-draft receipt ownership, reader/IME/clipboard/export interactions, stale source and patch changes, scoped file actions, image snapshot exports, modal cancellation, and focused review selection/scroll restoration. Tests use isolated stores and in-memory native windows; the Mermaid fixture opens no app window.

The normally ignored long virtualized-selection fixture passed separately. Formatting and whitespace checks passed. The locked offline release build passed; the optimized Mermaid helper passed the same eight-family and network-isolation harness. A new, separate preview is packaged at `target/chat-next-five-preview/RiWork.app`, including the signed diagram helper and upstream license notices. Deep/strict signature verification passed. The running app was not restarted; source changes remain in the working tree.

### Performance evidence

Measured locally on 2026-10-19 in an optimized Rust test binary, using isolated fixtures:

| Workload | Previous path | New path | Meaning |
| --- | --- | --- | --- |
| 173,026-byte Markdown answer, 338 streaming chunks, median of five runs | Full parsing: 134.06 ms total | Incremental parsing: 22.52 ms total | 5.95× faster parsing for this code-heavy answer |
| Eight recovered attachments totaling 8 MiB | Synchronous content validation: 17.45 ms | UI restore and scheduling: 1.40 ms | Content validation now finishes in the background; this is not a total validation-time reduction |

Markdown reuse begins after a closed top-level code fence followed by a complete blank line. Prose-only answers and CR/BOM-containing sources retain the full-parse path. These measurements do not establish application frame rates or performance on every workload.

Reproduce with `cargo test -p riwork-desktop --release --locked streaming_benchmark -- --ignored --nocapture` and `cargo test -p riwork-desktop --release --locked attachment_recovery_ui_scheduling_benchmark -- --ignored --nocapture`, using the repository's documented Zig toolchain.
