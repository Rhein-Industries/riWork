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
Identity fields are never shortened. The CLI applies the same recovery before
its own page-size limit, so an event can reach the connector.

If shortening cannot fit an item event, its entire event object is replaced:

```json
{"seq":42,"event":{"event":"item_elided","item_id":"tool-7","kind":"tool_call","reason":"too_large","bytes":2097301}}
```

- `seq` is unchanged. `item_id` is the original `item.id` (or `item_id` for a delta).
- `kind` is the original `item.body.type` when known; otherwise it is omitted.
- `reason` is exactly `"too_large"`.
- `bytes` is the original event object's compact UTF-8 JSON byte count, before
  shortening; it excludes the outer `seq`/`event` envelope and frame overhead.
- The client advances its cursor normally and may show an omitted-item row.
  Older clients ignore the unknown `item_elided` event while still counting its
  sequence slot. This is a response-only transformation; logs remain unchanged.

For non-item events, retain the original event name and string `id`/`*_id`
fields at their original object paths. Remove other payload fields and mark
both the event and any retained nested identity object with `"elided": true`:

```json
{"seq":43,"event":{"event":"approval_requested","approval":{"request_id":"req-9","item_id":"tool-7","elided":true},"elided":true}}
```

Clients must treat an elided control as unavailable details, not an actionable
partial approval/question. They should direct the user to the Mac and continue
following the chat. No shortened request details are presented as complete.

## Snapshots

`chat.snapshot` uses the same shortening/elision rules for `controls` (which
are event objects without `seq`). Its `items` are ordered rows, not event
entries. To preserve decoding by older clients, an elided row retains the
normal Item schema and carries the same placeholder under `item.elided`:

```json
{"order":42,"item":{"id":"tool-7","turn_id":"turn-3","status":"completed","body":{"type":"agent_message","text":"This message is too long to show here. Full text is on your Mac."},"elided":{"event":"item_elided","item_id":"tool-7","kind":"tool_call","reason":"too_large","bytes":2097260}}}
```

`id`, `turn_id` (if present), `status`, and `order` are preserved; snapshot
`elided.bytes` counts the original compact UTF-8 Item JSON. Older clients decode
the body note and ignore the additive `elided` field. New clients may use it to
render the omitted-item row. Other rows, `cursor`, `next`, `before`, and `more`
remain unchanged during recovery. Aggregate oversized rows/controls are elided
as necessary, without dropping snapshot rows. Normal CLI snapshot pagination
still applies before connector recovery.

The connector reads at most 8 MiB of CLI JSON to permit recovery before sealing
into a frame. Malformed replies, invalid sequence/order, snapshot resource
limits, and replies larger than that input allowance still report their normal
validation/resource errors. `complete` preserves full payloads when they fit;
it uses the documented recovery when a single payload exceeds a response limit.
