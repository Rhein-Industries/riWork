# Inline worker timeline: stage 1 design review

Task `dcfe60d0-9678-488c-bf55-38a7a1b48233` · branch `feat/inline-worker-timeline-20261014` · base `1330298` · read-only review, no code changed.

## Verdict

**I agree with the user: workers belong inline in the timeline.** The pinned section's main problem is not where it sits. It is a project dashboard copied into every chat. `Workspace::publish_workers` sends every worker shell of the project to every chat tab of that project (`crates/riwork-desktop/src/subshells.rs:235-303`). It ignores `ShellSession.parent_id`, which already records the chat that spawned the worker. As a result, an orchestrator chat, its worker chats and an unrelated chat in the same project all show the same strip. iOS does the same: it filters by project and `parent_id != nil`, never by the chat (`ios/Core/SubshellCards.swift:30-34`). A card that isn't tied to the turn that caused it can't tell you why a worker exists, and it disappears from history once the shell is gone.

The pinned section does one useful thing: you see what is running without scrolling. Keep that as a one-line indicator that jumps to the inline card. Don't keep it as a section.

**Provider subagents are already inline.** Claude `Task`/`Agent`, Codex `collabAgentToolCall` and Muse `presentation.subagent` render as transcript cards on desktop (`chat_view/rows.rs:234-273`) and on iOS (`ChatItemViews.swift`, `ChatSubagentCard`). This rework only concerns RiWork's own workers: worker shells, and I recommend worker chats too, which no surface shows today.

**Recommended mechanism: a hybrid.**
- When a chat spawns a worker, the chat host appends a small marker item to that chat's own event log.
- When the worker finishes, the host replaces that same item with a bounded final snapshot.
- Live status, elapsed time and the output tail come from the existing inventory and sampler, matched by session id.
- Nothing is placed by timestamp, because the log has none. Nothing is scraped from command output.

## Recommendation in one screen

1. **Marker shape.** Use an existing body kind plus an additive presentation field: `ItemBody::ToolCall { server: Some("riwork"), tool: "worker", input: {...} }` with a new `Presentation.worker: Option<WorkerRef>`. Status is always `Completed`. Do not add a new `ItemBody` or `ChatEvent` variant: older Rust hosts would then fail to resume the chat and older CLIs would fail `chat.snapshot` and `chat.events` (details under Risks).
2. **Writer.** Only the chat host writes to a chat's log (one host per `RIWORK_HOME`, `chat/host.rs:3-6`). A new host request `NoteWorker { chat_id, session_id, ... }` appends idempotently by item id, stamps the chat's current turn, and never starts or resumes the provider.
   - Worker shells: `riwork shell create` and the MCP `riwork_shell_create` call it right after creation, when `caller_parent()` is a chat.
   - Worker chats: the host's own `Create` writes the marker in-process, with no new round trip.
3. **Position.** A marker is appended while the spawning tool call is still running, so it lands right after the `riwork shell create` command item, in the turn that spawned it. For the desktop list this is an **append**, not a mid-list insert. That avoids the selection-retire and row-id-shift problems (see Live updates).
4. **Live state.** A card's status, elapsed time and bounded output come from the inventory, matched by `session_id`. They are never written to the log. Only two transitions are persisted: spawn, and the final snapshot.
5. **Finish.** At the worker's first Done or Exited after the marker, a `NoteWorker` replaces the marker in place (upsert by id) with `final: { status, ended_at, tail }`.
   - For shells, the desktop sampler sends it when it sees the transition.
   - For worker chats, the host does it itself at the child's turn completion, using the child's final message excerpt.
   - History then shows the card collapsed to that result, even after the shell is pruned. The phone gets it through the ordinary chat event stream.
6. **Pinned section.** Remove it on desktop and iOS. Add a compact "N workers running" chip next to the existing "Latest" button. It appears only when a live card of *this* chat is scrolled out of view, and clicking it scrolls to that card. Workers that have no parent chat are not shown in any chat (see Q5).
7. **Fallback.** When the spawn-time note did not arrive (an old long-lived host, an unreachable socket, workers that predate this change), the desktop sends one idempotent late `NoteWorker`. It does so for each live worker whose `parent_id` is a chat, if that worker is more than about 30 s old and this run has not reconciled it yet. The host no-ops if the marker exists; otherwise it appends at the end, flagged `late` ("started earlier; first seen here").

## Facts this design rests on (verified in code)

- **The log has no time.** `Item` has `presentation, id, turn_id, status, body` and nothing else (`chat/model.rs:537-548`). `Envelope` has `chat_id, seq, event` (`chat/wire.rs:71-76`). The only clock is `ChatInfo.created_at_unix`. Placing a worker afterwards by its `created_at_unix` is therefore impossible.
- **Items keep their first-appearance position.** `ItemStarted` and `ItemCompleted` replace an item by id in place, and new ids are pushed (`model.rs:941-948`). Snapshot `order` is the seq of an item's first appearance. A marker that is later updated stays where it was spawned.
- **`TurnCompleted` closes every `InProgress` item of that turn, and every in-progress item with no turn** (`model.rs:921-940`). A marker can never use `InProgress` to mean "worker still running". It must be stored `Completed`, with liveness held elsewhere. The Claude background-`Task` rule in `subagents.rs:166-170` exists for the same reason.
- **The parent link already exists, but only as `parent_id`.**
  - The chat's provider process gets `RIWORK_CHAT_ID` (`chat/launch.rs:282-296`).
  - `SessionManager::create` sets `parent_id = same_project_parent(caller_parent())` for project shells (`sessions.rs:2378-2389`).
  - Worker chats get `NewChat.parent_id = caller_parent()` (`riwork-cli/src/cli.rs:2491-2495`), and the host validates it (`chat/host.rs:1416-1443`).
  - `same_project_parent` drops a parent from another project (`project_tabs.rs:133-151`). Workers of the global orchestrator therefore have no parent. Workers of a terminal orchestrator have a *shell* id as parent.
  - Nothing records the turn or item that spawned a worker.
- **Nothing except the host can add an item to a chat's log today.**
  - `Request` has `Capabilities, StageAttachment, Create, List, Command, Subscribe, Close, Delete` (`wire.rs:12-51`).
  - `wire::Capabilities` is `deny_unknown_fields` (`wire.rs:53-57`), so a new capability flag would break older clients.
  - An old host answers an unknown request with "unreadable request". That is a clean refusal, which a best-effort note can tolerate.
  - Writing to `events.jsonl` from another process would corrupt the log: the host keeps `next_seq` in memory and `open` truncates at a numbering break.
- **Old readers handle new data in three different ways.**
  - Unknown *presentation fields* are ignored everywhere, and the iOS port decodes `subagent` with `try?`. bb284d1 relied on this.
  - An unknown *body variant* is lenient on iOS: the item is skipped (`Chat.swift:381`, with the `Lenient` wrappers).
  - On desktop, the feed shows "An event from the chat host could not be read" (`chat_view/feed.rs:354-367`).
  - On the host, `provider_session_has_history` fails on an unparseable line (`chat/log.rs:143-144`), so **an older host cannot resume the chat**. `read_snapshot` and CLI `chat events` also fail closed on such a line.
- **Desktop rows are keyed by item index.**
  - `display::Row::{Item(usize), Details(usize), Artifacts{..}, Outcome(..)}` (`chat_view/display.rs:7-16`). Element ids use the list position (`transcript-row:{ix}`).
  - `refresh_projection` splices only the changed middle of the list. It **retires the selection whenever the new rows are not a pure append** of the old (`chat_view.rs:841-870`).
  - `sync_list` remeasures touched rows, and clears the selection if a selected row's item changed (`chat_view.rs:798-839`).
  - In debug builds, `LeafRenderer` asserts that selectable text matches its source document (`select.rs:470`): selectable text changed outside `transcript_selection.changed()` panics.
- **Scroll anchoring in gpui `list` is logical.** The list is gpui `list` with `FollowMode::Tail` (`chat_view.rs:445-446`).
  - While following, the tail stays pinned.
  - When not following, the scroll top is `(item_ix, offset)`. Growth or shrinkage of rows above or below it does not move what you read. Only growth of the scroll-top row itself shifts the view by a pixel offset.
  - Rows inserted by a splice have no height until measured, which hides the "Latest" button until they are (`panels.rs:384-387`).
- **No in-chat search exists.** cmd-f is the workspace search. Search parity is therefore not a constraint.
- **Normal display mode already hides successful command rows** (`display.rs:172-194`). Markers for several workers spawned in one turn will therefore render adjacent, with the `riwork shell create` commands between them hidden.
- **iOS.**
  - `ChatWorkerShells` sits between the toolbar and the transcript (`ios/RiWorkRemote/ChatView.swift:55`). It is read-only and fed by `shells.list`, which is polled every 4 s while the tab strip is visible.
  - The transcript is a `LazyVStack` keyed by `item.id`, with sticky-bottom logic.
  - `shells.list` already passes `parent_id`, `activity`, `activity_since_unix` and `created_at_unix`.

## Design, by the assignment's questions

### Q1. Inline or pinned?

Inline. A worker is a consequence of a specific turn ("I'll split this into three workers"). The card belongs under that turn, it should stay in the history, and it should read as part of the conversation. The pinned strip has three faults:
- It mixes in workers this chat never started.
- Its order changes when a worker finishes: it sorts by `(finished, created_at)`.
- It takes 240 px of a transcript that is already a scroll region, as a second nested scroller.

The real cost of inline: a live card far up the history is invisible. The compact indicator (Q5) covers that, and so does the re-anchor-on-reassignment option (open question 1).

### Q2. How worker shells become timeline entries

**Marker contents (`Presentation.worker`, additive, no `deny_unknown_fields`):**
```
WorkerRef {
  session_id: String,          // shell or chat UUID; an `*_id` key, so remote_payload never cuts it
  kind: Shell | Chat,
  harness: Option<String>, model: Option<String>, worktree_id: Option<String>,
  late: bool,                  // appended by reconciliation, not at spawn
  final: Option<WorkerFinal>,  // absent while running
}
WorkerFinal { status: Done | Exited | Failed | Stopped, ended_at: u64, tail: String /* ≤2 KiB, bounded_output */ }
```
- **Body.** `ToolCall { server: Some("riwork"), tool: "worker", input: { session_id, title }, output: final.tail }`. An old desktop or phone shows a plain tool row instead of failing. `subagents::of` matches only `server: None` (`subagents.rs:152-160`), so a marker never becomes a provider-subagent card by accident.
- **Id.** `worker:{session_id}`. The host checks its item index, so a second note for the same id is either a no-op or a `final` upsert.
- **Status.** Always `Completed`.
- **Turn.** `turn_id` is the host's `open.turn`.

**Writer.** A new `Request::NoteWorker { id, chat_id, worker: WorkerRef }` in `wire.rs`. The host:
- validates that `chat_id` exists and that the inventory (or, for chats, the host's own list) has `session_id` with `parent_id == chat_id` in the same project;
- upserts the item under the chat's `inner` lock;
- never resumes a stopped provider. It is not a `ChatCommand`, because a `Command` resumes a stopped chat first.

The spawner treats a refusal or an unreachable host as a soft failure. The shell already exists, and `shell create` must not fail because of a note.

**Why the hybrid, not the alternatives (details under Alternatives):**
- **View-layer interleaving** has nothing to place by: there are no item timestamps. The only exact anchor would be scraping the shell id out of the `--json` output of a command item, which breaks without `--json`, inside scripts and loops, through MCP, and when the phone receives a shortened output.
- **Fully persisted live state** would write to the log every 2-30 s per worker, forever, into chats that are idle.
- **The hybrid** persists exactly the two facts the history needs, spawn and result, at the moment they happen.

**Lifecycle:**
- **Reload and history.** The marker replays from the log like any item. While the shell is still in the inventory, live state overlays it. Once the shell is gone, the persisted `final` remains. If neither exists (the shell vanished while no RiWork app was running), the card says "Ended · details unavailable" honestly.
- **Before the sampler first sees the shell.** The marker renders immediately with the status "Starting" and the elapsed time from the marker's arrival. Nothing is waiting on a poll.
- **After exit.** The sampler sees `alive=false`, sends the final `NoteWorker` with status `Exited` and the last captured tail, and the card collapses.

### Q3. Ordering and identity

- **Spawn detected late.** This is no longer a polling problem: the spawner writes the marker synchronously. Late discovery only happens through the fallback, which appends at the tail with `late: true` and the label "started earlier · first seen here". It never inserts in the middle. A worker younger than about 30 s is skipped, so the fallback does not race the on-time note.
- **Shells that predate the chat view.** These are irrelevant now: markers come from the log, so the view's lifetime doesn't matter.
- **Shells from other chats in the same project.** They are not shown in this chat. They appear in the timeline of the chat that spawned them. This is the main behavior change from the shipped strip.
- **Workers without a parent chat** have no timeline to live in:
  - workers of a terminal orchestrator (their parent is a shell);
  - workers of the global orchestrator (cross-project, so they have no parent);
  - shells created with `--no-parent`;
  - automation shells (`!user_opened` with no parent).

  They stay visible where shells already are: the project's sessions and tabs. See open question 3.
- **Provider subagents: two mechanisms, one card.** Keep the data paths separate, because they are different facts:
  - Provider subagents *are* provider items with their own lifecycle. Codex reports child state only in `agentsStates` snapshots. Claude's sidechain tool calls appear as flat sibling items.
  - RiWork workers are sessions in RiWork's own inventory.

  Unify the presentation:
  - One `TimelineAgentCard` view model in the engine: title, purpose, a unified status enum (Running, Waiting, Done, Failed, Interrupted, Exited, Unknown), elapsed time, live tail, result and actions.
  - It is built from either `subagents::Subagent` or `WorkerRef` plus `ShellCard`.
  - One renderer on desktop and one on iOS.

  Do not extend `in_transcript`'s whole-transcript scan to workers. A worker's live state is a map lookup by session id.

### Q4. Live updates without disrupting the transcript

- **Arrival.** A marker arrives through the feed as an ordinary `ItemStarted`. The existing append path splices it at the end with no selection retirement.
- **Updates.** Live updates arrive through `set_subshells`, which is now keyed per chat and per session id, outside the feed. On each publish, ChatView:
  1. finds the rows whose session's live state changed;
  2. calls `list.remeasure_items(row..row+1)` for just those rows;
  3. calls `transcript_selection.changed()` only if selectable text in those rows changed.

  This replaces bb284d1's full `list.remeasure()`, which is heavy and the wrong thing to copy.
- **Fixed height while running.** The live body reserves a fixed block, for example a header plus 3 monospace lines, padded when the output is shorter. Poll-to-poll output changes then never change the row's height. Height changes only on transitions (Starting → Running, Running → finished and collapsed). Combined with gpui's logical anchoring, a reader scrolled up is not moved. The only case left, the card being the scroll-top row at the moment it collapses, keeps its pixel offset, which is acceptable.
- **Selection and copy.**
  - The live tail is plain text, not `selectable`, as in today's strip. It therefore never clears a selection and can't hit the drift assert.
  - The header (title, harness and model, task) and the persisted `final.tail` are selectable leaves (`worker:{session_id}`). Copy and export include the result, not a moving screen.
- **Collapse and expand.** Use the image/artifact toggle pattern (`pause_following_tail()` + remeasure + restore the offset). Don't use plain `toggle`, which does not pause tail-following (`chat_view.rs:1433-1451`).
- **Actions.**
  - Open shell and Stop stay on the card as buttons.
  - **Message moves out of the card** into a "To worker …" bar above the composer. A focused text input inside a virtualized row is unmounted when you scroll it away, because ChatView splices without `splice_focusable`. Keystrokes would then go nowhere.
- **Elapsed time** is computed at render time, as now. The live badge animation already re-renders at about 20 fps while a running card is on screen, and nothing re-renders when none is.
- **Fan-out (several workers in one turn).** Use N compact cards, no group card, in stage 2. Normal mode already hides the successful spawn commands, so the cards stack together under the turn. A group header ("4 workers started") can be added later in `display::rows`, the way `Row::Artifacts` groups images, if N cards feel noisy. See open question 7.
- **Display modes.** Markers are items, so Verbose mode (all items) includes them for free. Normal mode makes them always visible, the way subagent items are (`display.rs:184-185`). A view-layer `Row::Worker` would have needed special handling in both branches and in `remap_anchor`.

### Q5. The pinned section

- **Remove it.** That means `worker_section`, its `panels.rs:393` slot, and the "Worker shells · last checked…" line on every render.
- **Add a running-workers indicator** as a chip by the "Latest" overlay: "2 workers running ↑".
  - It shows only when at least one live worker card of this chat is outside the viewport.
  - Clicking it scrolls to the nearest such card.
  - The "Live / Backing off / Paused / Check failed" diagnostics move into the card. They show as a muted line only when data is stale (more than 30 s since the last successful check) or a check failed. That information was useful during development; on every chat all the time it is noise.
- **Workers with no related chat turn** (started from a terminal orchestrator) are not shown in any chat. The chat strip was never the right place for them either: they showed up in every chat of the project, attributed to none.

### Q6. iOS parity

- **Markers arrive for free.** The phone already reads `chat.events` and `chat.snapshot`, so markers and their final snapshots need no new RPC. History works offline from the shells inventory.
- **Phone changes:**
  - Decode `presentation.worker` (additive, `try?`, as for `subagent`) in `Chat.swift`.
  - Render the shared card inline in `ChatItemRow` before the body switch, as `ChatSubagentCard` is rendered now.
  - Join live state by `session_id` against `model.shells` (and the chat list, for worker chats).
  - Delete `ChatWorkerShells` from `ChatView.swift:55`.
  - Make sure `keepFresh(.sessions)` also runs while a chat with an unfinished worker card is on screen, not only while the tab strip is.
  - The phone stays read-only.
- **The wire model needs nothing new.** `shells.list` already carries `parent_id`, `activity`, `activity_since_unix` and `created_at_unix`. A live output tail on the phone would need a `shells.output`-style call. I would not add it: the phone gets the final tail through the log.
- **Old phone builds** see a plain tool row titled "worker" for each marker, and can still read the chat.

### Q7. Effort, files, tests, risks

**Size.** About the same as bb284d1 (≈2.3k lines with tests):
- engine, CLI and MCP: ≈700-900;
- desktop: ≈600-800, minus ≈250 removed;
- iOS: ≈350-450.

Two focused stage-2 slices:
- (a) engine and host marker, CLI and MCP notes, desktop inline card, removal of the strip;
- (b) iOS, the finalization path, and the reconciliation fallback.

**Files I would touch:**
- Engine:
  - `chat/model.rs` (`Presentation.worker`, `WorkerRef`)
  - `chat/wire.rs` (`Request::NoteWorker`)
  - `chat/host.rs` (request handler; marker on worker-chat `Create`; finalize on a child chat's turn completion)
  - `chat/client.rs`
  - `chat/log.rs` (`is_session_history` must ignore worker markers, as it ignores notices, so that a marker or final upsert can't flip the resume decision after a provider switch)
  - new `chat/workers.rs` (marker construction, `of(item)`, the unified card model)
  - `subshells.rs` (`ShellCard` gains `parent_id`; filter helpers)
  - `chat/text.rs` and `handoff/sources.rs` (render a marker as one line, "Started Claude worker for task X → Done")
- CLI and MCP: `riwork-cli/src/cli.rs` (shell create; send, if follow-up markers are approved) and `riwork-cli/src/mcp.rs` (`riwork_shell_create`).
- Desktop:
  - `chat_view/subshells.rs` (rewritten as a row renderer plus the per-session `WorkerUi`)
  - `chat_view/rows.rs`, `display.rs`, `selection_document.rs`
  - `chat_view.rs` (`set_subshells` → targeted remeasure)
  - `panels.rs` (remove the strip, add the indicator)
  - `chat_view/editors.rs` or the composer (the "To worker" bar)
  - `subshells.rs` (publish per chat by `parent_id`; finalize and reconcile notes; sample output only for workers whose parent chat is open)
- iOS: `Core/Chat.swift`, `Core/SubshellCards.swift`, `RiWorkRemote/ChatItemViews.swift`, `ChatView.swift`, `SubshellViews.swift`, `RemoteModel(+Activity).swift`, `Tests/SubshellCardsTests.swift`.
- Docs: `docs/chat-events.md` (the marker item), `docs/remote-protocol.md` (no RPC change; document the presentation field).

**Existing tests that encode the pinned placement or project scope:**
- `crates/riwork-desktop/src/chat_view/subshells_tests.rs:29` `worker_appears_updates_and_collapses_without_taking_composer_focus` drives `set_subshells` and finds `worker:worker` outside the list. Rewrite it around a marker item in the transcript.
- `subshells_tests.rs:100` `worker_actions_dispatch_once_and_a_refused_message_keeps_its_text` covers the in-card message input. Rewrite it for the composer bar; keep the dispatch-once and keep-text guarantees.
- `ios/Tests/SubshellCardsTests.swift:23` `testAncestryAndProjectScopeAreRequiredAndTaskAmbiguityIsPreserved` asserts project scope. It becomes chat scope by `parent_id`.
- These stay as they are:
  - `subshells_tests.rs:167` `provider_delegation_has_a_structured_header_and_expandable_result`
  - the engine `subshells.rs` poll and card tests
  - the desktop `subshells.rs` dispatch tests
  - the other iOS subagent tests

**New tests:**
- Host:
  - `NoteWorker` appends once (idempotent by id), stamps the open turn, and stores `Completed`;
  - a following `TurnCompleted` leaves it `Completed`;
  - a final upsert keeps the marker's position and snapshot `order`;
  - the request refuses a session whose `parent_id` is a different chat;
  - it never resumes a stopped chat;
  - `is_session_history` ignores markers;
  - an old-host "unreadable request" is a soft failure for `shell create`.
- Replay: in a fixture log, the marker follows the spawning command item, and the phone snapshot and CLI events carry it.
- Desktop:
  - a marker arriving during a selection elsewhere does not retire the selection;
  - a live update remeasures only that row and keeps an unrelated selection;
  - a running card's height is constant across output changes;
  - finishing collapses the card to its result while a reader scrolled above keeps their position;
  - Verbose mode includes markers;
  - the indicator appears only when a live card is off-screen, and jumps to it;
  - a worker with a different `parent_id` never appears.
- iOS: `presentation.worker` decodes, ignores unknown fields and tolerates the old body; the join by `session_id` works; the strip is gone.

**Top risks:**
1. **Log compatibility.** Only the `ToolCall` + `presentation` shape is safe. A new body or event variant would make older long-lived hosts unable to resume the chat, and would break `chat.snapshot` and `chat events` on older CLIs.
2. **Old long-lived host.** A host survives an app upgrade until it has been idle for 15 minutes, with no version check. Until then, notes are refused and the reconciliation fallback must cover. It will append late markers at the end rather than at the spawn point.
3. **Resume semantics.** Writing items into a chat outside its driver must not change `provider_session_has_history`. This needs the explicit `log.rs` exclusion and a test.
4. **Who finalizes shells.** It is the desktop sampler, so finalization happens only while a RiWork app runs. Two windows are fine because the host upserts idempotently. If no app is running, finalization happens late, at the next start, or not at all if the shell was pruned meanwhile ("details unavailable").
5. **Terminal text in the chat log.** The final tail (≤2 KiB of screen) becomes part of the parent chat's log, its exports, handoff documents and the `context.md` passed on a provider switch. Orchestrator wakes deliberately carry identifiers only. This is a product call: see open question 4.
6. **Sandbox reachability.** A Codex orchestrator in a restrictive sandbox may not reach the host socket. `riwork chat new` already requires it, so this is probably fine, but the soft-failure path and the fallback must hold.
7. **Already in the base, not introduced here.**
   - Codex `in_transcript` is O(n²) per render.
   - Any touched subagent triggers a full `remeasure()`.
   - `toggle` does not pause tail-following.

   Don't copy these; fix the last two while in the area if cheap.

## Alternatives considered

| Option | For | Against | Verdict |
|---|---|---|---|
| **A. Keep the pinned strip, scope it to `parent_id`** | Smallest change (≈50 lines). No transcript risk. Always visible. | Still not a timeline. No history after the shell is pruned. Disconnected from the turn that spawned it. The user rejected the placement. | No. Its scoping fix is part of the recommendation anyway. |
| **B. View-layer interleaving** (nothing persisted) | No log or wire change. Works with old hosts. | The log has no times, so there is no anchor. The exact anchor would be scraping the shell id from a command's `--json` output, which is fragile (no `--json`, scripts and loops, MCP tool shape, phone output shortening) and against this codebase's "never structure scraped from prose" rule. Late discovery means mid-list inserts, which retire the selection and shift row ids. History disappears with the shell. The phone needs a second port of the heuristics. | No. |
| **B′. Render the spawning command item itself as the card** | Exact position when it matches. | The same scraping fragility. Only shells created by a visible command qualify. | No. |
| **C. Fully persisted synthetic items** (every status and output change logged) | Perfect replay. The phone needs no inventory. | Log churn every 2-30 s per worker, indefinitely, into idle chats. Seq and phone event traffic grow. Screen text is written repeatedly. It also needs a writer that runs when no app does. | No. Persist transitions only. |
| **D. Hybrid: persisted marker plus final snapshot, live state from the inventory** | Exact position. Appends only. Survives reload and history. Phone parity through the existing stream. Small log footprint. | A new host request. An old-host and back-compat path. Finalization depends on an app running for shells. | **Recommended.** |
| **E. Fold workers into provider-subagent items** (one mechanism) | One data path. | They are different facts with different owners. It would mean synthesizing provider-shaped items and widening `in_transcript`'s scan. | Unify the card, not the data. |

## Open questions for the user

1. **Re-assignment.** When the orchestrator sends a worker a follow-up 30 minutes later, should a new live card appear at that point (and the earlier card collapse to its result), or should the original card at the spawn point go live again, with the indicator pointing up to it?
   - My lean is a new card at each re-assignment, coalescing a `shell create` and the first `shell send` in the same turn. That is the truer timeline.
   - It costs a note on `shell send` and `chat send`, plus the rule "only a worker's latest marker in a chat is live" (≈150 lines). It could ship in a slice after the spawn markers.
2. **Worker chats.** Include `riwork chat new` children as inline cards? My lean is yes: the host owns both chats, so it is exact and cheap, and no surface shows worker chats today.
3. **Workers with no parent chat.** Workers of terminal or global orchestrators, `--no-parent` shells and automation shells would appear in no chat. Is the project sessions list enough for them? My lean is yes.
4. **Final output in the log.** Should a finished worker's bounded screen tail (≤2 KiB) be persisted in the parent chat's log, and so in exports, handoff documents and provider-switch context? Or should the card persist only status and times, with "Open shell" for detail? My lean is to persist the tail: a result you can read in the history is what the user asked for.
5. **Actions on the inline card.** Open shell and Stop stay on the card. Message moves to a "To worker" bar above the composer, for the focus-safety reason in Q4. OK? The phone stays read-only.
6. **Backfill at upgrade.** Workers already running when this ships would get a `late` marker at the end of their parent chat, the first time the app sees them. Do that, or show only workers spawned after the upgrade?
7. **Fan-out.** N compact cards (my lean), or one group card per turn ("4 workers started") with rows inside?
