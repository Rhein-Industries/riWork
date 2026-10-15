# Remote chat event recovery

`chat.events` keeps sequence slots in every mode (legacy, `bounded`, and
`complete`). A page may return a prefix with `more: true`; `next` is the last
returned sequence. Oversized individual events never cause a frame-size error
or disappear from that prefix.

The connector first shortens item body strings recursively (including tool
input/output and file diffs), deltas, and presentation attachment/image data.
Strings end with `…`; inline image sources become `kind: "unavailable"`, rather
than corrupted base64. The existing final body note remains:
`This message is too long to show here. Full text is on your Mac.`
String identity fields (`id`, `*_id`, `path`, `url`, and `name`) are never
shortened, including nested fields. They may be omitted with an entire body or
encoded source at the final fallback. Oversized base64 fields are detected
without requiring `kind: "data"`: `base64`/base64-named keys, image/binary data,
`encoding: "base64"` containers, image `data` sources, and base64 data URLs are
replaced by an unavailable marker, never a partial encoded stream. The CLI applies the same recovery before
its own page-size limit, so an event can reach the connector.

If shortening cannot fit an item event, its entire event object is replaced:

```json
{"seq":42,"event":{"event":"item_elided","of":"item_completed","item_id":"tool-7","kind":"tool_call","status":"completed","reason":"too_large","bytes":2097301}}
```

- `seq` is unchanged. `item_id` is the original `item.id` (or `item_id` for a
  delta); the field is omitted if that identity is absent, never fabricated as `""`.
- `of` is the original event tag (`item_started`, `item_completed`, or `item_delta`).
- `status` is the original item's status when present; an elided completion
  without an explicit status defaults to `"completed"`. Deltas omit status when
  unknown. New clients use `of` and `status` to end a streaming item correctly;
  older clients skip the unknown event and cannot update that item's final status.
- `kind` is the original `item.body.type` when known; otherwise it is omitted.
- `reason` is exactly `"too_large"`.
- `bytes` is the original event object's compact UTF-8 JSON byte count, before
  shortening; it excludes the outer `seq`/`event` envelope and frame overhead.
- The client advances its cursor normally and may show an omitted-item row.
  Older clients ignore the unknown `item_elided` event while still counting its
  sequence slot. This is a response-only transformation; logs remain unchanged.

Non-item events use a **new** event name, `control_elided`. The original name
moves to `of`; original string `id`/`*_id` fields remain at their original paths.
The event and retained nested identity objects carry `"elided": true`:

```json
{"seq":43,"event":{"event":"control_elided","of":"approval_requested","reason":"too_large","bytes":2097301,"approval":{"request_id":"req-9","item_id":"tool-7","elided":true},"elided":true}}
```

`reason` and `bytes` have the same meaning as for item placeholders. The relay
never emits an incomplete `approval_requested` or `question_requested` under
its old event name: existing decoders could otherwise turn it into an actionable
empty request. Older clients ignore `control_elided` and advance the sequence.
New clients show unavailable details and direct the user to the Mac; a placeholder
is never actionable. Pending state on an older client is not resolved by elision.

## Snapshots

`chat.snapshot` uses the same shortening/elision rules for `controls` (which
are event objects without `seq`). Its `items` are ordered rows, not event
entries. To preserve decoding by older clients, an elided row retains the
normal Item schema and carries the same placeholder under `item.elided`:

```json
{"order":42,"item":{"id":"tool-7","turn_id":"turn-3","status":"completed","body":{"type":"agent_message","text":"This message is too long to show here. Full text is on your Mac."},"elided":{"event":"item_elided","of":"item_completed","item_id":"tool-7","kind":"tool_call","status":"completed","reason":"too_large","bytes":2097260}}}
```

`id`, `turn_id` (if present), `status`, and `order` are preserved; snapshot
`elided.bytes` counts the original compact UTF-8 Item JSON. Older clients decode
the body note and ignore the additive `elided` field. New clients may use it to
render the omitted-item row. Other rows, `cursor`, `next`, `before`, and `more`
remain unchanged during individual-payload recovery. Only rows too large on
their own are shortened or elided. An aggregate of ordinary rows pages by
removing the oldest rows, updating `before`, and setting `more: true`; those
rows remain available from the next history request. Targeted `item_ids` reads
never drop requested rows; an oversized aggregate reports `snapshot_limit`.
An aggregate of controls, which has no history paging, may use `control_elided`.

The connector keeps the prior frame budget for CLI `--max-bytes` (128 KiB less
1 KiB and the 64-byte sealing margin). Only its read allowance is raised. Normal
CLI snapshot pagination therefore runs before relay fitting. If the CLI cannot
represent a single payload under that budget, it retries the read internally
under the existing 8 MiB resource cap and applies response-only recovery and
paging before printing. This does not change its requested output budget.

The connector reads at most 8 MiB of CLI JSON to permit recovery before sealing
into a frame. Malformed replies, invalid sequence/order, snapshot resource
limits, and replies larger than that input allowance still report their normal
validation/resource errors. `complete` preserves full payloads when they fit;
it uses the documented recovery when a single payload exceeds a response limit.

All three CLI event modes share the same identity-preserving recovery path.
The obsolete legacy all-string shrink and duplicate collectors were removed.
The CLI minimum string cut remains 256 bytes; relay recovery uses 128 bytes.

## Worker markers

A terminal (`riwork shell create`, MCP `riwork_shell_create`) or chat (`riwork chat new`)
that a chat starts is that chat's worker: its `parent_id` is the chat. The chat host, the
only writer of the parent's log, records the worker in the parent's timeline as one
ordinary item (`crates/riwork-engine/src/chat/workers.rs`):

```json
{"event":"item_completed","item":{
  "id":"worker:<session_id>","turn_id":"turn-3","status":"completed",
  "body":{"type":"tool_call","server":"riwork","tool":"worker",
          "input":{"session_id":"<uuid>","title":"Fix polling"},"output":"<final tail>"},
  "presentation":{"worker":{
    "session_id":"<uuid>","kind":"shell","title":"Fix polling","harness":"claude",
    "assignment":"Make the poller back off","noted_at":1760400000,
    "final":{"status":"done","ended_at":1760400600,"tail":"All tests pass"}}}}}
```

- **When.** At the start (written while the spawning command still runs, so the item
  follows it in the turn that started the worker), when the parent sends the worker
  something (`riwork shell send`, MCP `riwork_shell_send_line`, `riwork chat send`), and
  when the worker finishes. The first send in the same turn as the start joins its card
  (a `shell create` and its first `shell send` make one card); any other send is a new
  card `worker:<session_id>:<n>` with `follow_up: true`. Only a worker's latest card in a
  chat is live; an earlier one without `final` reads "Continued below".
- **Final.** The same item id is written again with `final` (`status`
  `done|exited|failed|stopped`, `ended_at`, and `tail`, the last six lines and at most
  2 KiB). A finish completes the latest open card it does not predate: a terminal's
  done at or before a card was written (`done_since <= noted_at`), or a worker chat's
  turn begun before it (`started_at < noted_at`), belongs to an earlier card. An exit
  ends the latest card if it is still open. Worker chats are finished by their host when
  their turn ends, with the bounded last reply as `tail` (`riwork chat send` notes the
  card before it delivers, so the turn it starts is never older than its card);
  terminals by the desktop app when its inventory sees them done (after the next sample
  of their screen) or exited.
- **Late.** The desktop app notes a live worker older than 30 s once per run with
  `late: true`; the host writes nothing if the worker already has a card, and otherwise
  appends one at the end ("Started earlier · first seen here").
- **Status.** Always `completed`: `turn_completed` closes in-progress items, and a worker
  outlives the turn that started it. What is live (working, waiting, elapsed time, the
  terminal's screen) is never logged; clients join `session_id` against `shells.list`
  and the chat list.
- **Writers.** Only the host, on a `note_worker` request (`chat::wire::Request::NoteWorker`)
  that it checks against the inventory: the worker's `parent_id` must be the chat, in the
  chat's project. A note never starts the parent's provider. A host from before it
  refuses the request as unreadable, which every caller treats as nothing recorded.
- **Session history.** A marker is not provider history: it never makes a new provider
  look resumable (`log::is_session_history`).
- **Readers from before.** The body is a plain `tool_call`, so an older desktop or phone
  shows a tool row titled `riwork.worker`. A `presentation.worker` a reader cannot decode
  is dropped, never the item. The relay never shortens `session_id`; `tail` is shortened
  like any other presentation string.
