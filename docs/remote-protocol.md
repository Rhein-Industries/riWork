# RiWork remote protocol v1 — FROZEN (2026-09-27)

Contract owner: `feature/encrypted-relay`. This is the interoperability contract for
`ios/`. No silent wire changes. Changes require an explicit dated note below and
agreement with the iOS worker.

## Changelog

- 2026-10-02: Additive desktop terminal extension, **protocol v2 only**: a second kind of paired device, a *desktop* (another Mac running RiWork, paired with `pair --protocol 2 --kind desktop`), may open terminal streams with `pty.open`, `pty.read`, `pty.write`, `pty.resize` and `pty.close`: a real tmux client on a pseudo-terminal of the host, whose raw bytes travel over those RPCs, so the other Mac shows the shell exactly as a Ghostty window here would. It adds `features.pty` to `ready` (only for desktop devices), two lanes (`Attach`, `Stream`), the error code `pty_limit`, and `riwork shell attach ID --exec` / `"shell_attach_exec": true` in `riwork capabilities --json` on the CLI side; see "Desktop terminal extension" below. A phone, and any v1 device, finds none of it: `pty.*` is `invalid_request` "unsupported RPC method" for them, as it is on a connector from before it, and no existing method, byte or fixture changes. A config that holds only phones is written byte for byte as before.
- 2026-10-01: Additive project creation: `project.create` (make a new project, named by the phone, in the desktop's default projects folder, as a Git repository unless told otherwise), in the ordered lane and not cut short when the phone's session ends, and the error code `already_exists`; see "Project creation extension" below. The phone never names a path. Applies to v1 and v2 sessions. Existing methods, bytes and fixtures are unchanged. A desktop whose connector predates it answers `invalid_request` "unsupported RPC method".
- 2026-10-01: Additive link extension: `server_ms` on every response (the desktop's own time, so the phone can tell the network's share of a round trip), `features` in the first encrypted frame (`ready`), the `link.configure` request, optionally deflated reply frames for a session that opted in (a marker byte inside the ciphertext; the envelope, AAD and fixtures are unchanged), larger limits that go with them (a reply may be up to 2 MiB of JSON if it fits one frame deflated; `shell.history` `lines` up to 5000), and `link.json` vectors; see "Link extension" below. Applies to v1 and v2 sessions. An older phone or desktop sees no difference: nothing is compressed until the phone asks, and only a desktop that announced the feature is asked.
- 2026-10-01: Additive terminal creation: `shell.create` (start a plain shell, Codex, Claude or Grok in a project or worktree that exists on the desktop) and `shell.close` (end a project terminal), both in the ordered lane and not cut short when the phone's session ends, and the error code `harness_unavailable`; see "Terminal creation extension" below. Applies to v1 and v2 sessions. Existing methods, bytes and fixtures are unchanged. A desktop whose connector predates it answers `invalid_request` "unsupported RPC method".
- 2026-10-01: No wire change; how the desktop answers got cheaper. A waiting `shell.output` no longer captures the pane every 80 ms: the CLI is told by tmux when the pane is written to (a control-mode client attached read-only, never sizing a window) and captures then, so a quiet terminal costs no processes and a change reaches the phone within about 10 ms of the CLI instead of up to 80 ms later. The connector asks the CLI whether it checks that a shell exists and is alive itself (`riwork capabilities`), and if so no longer lists sessions first. Results, errors and bytes are the same; see "Waiting" below.
- 2026-09-30: Additive deep scrollback: `shell.history` (pages of scrollback above the screen, plain or styled) and the `history_size` and `alternate` fields on the `shell.output` result, also in its `unchanged` form and in its `hash`; see "Deep scrollback extension" below. Applies to v1 and v2 sessions. Existing methods, bytes and fixtures are unchanged.
- 2026-09-30: Additive live terminal sync: optional `styled`, `if_changed` and `wait_ms` on `shell.output`, a `hash` on its result and an `unchanged` result, and requests of one device are now carried out concurrently (responses may arrive out of order, matched by `id`); see "Live terminal extension" below. Applies to v1 and v2 sessions. Existing methods, bytes and fixtures are unchanged.
- 2026-09-30: Additive `appearance.get`, so the phone can show the desktop's colors; see "Theme sync extension" below. Applies to v1 and v2 sessions. Existing methods, bytes and fixtures are unchanged.
- 2026-09-30: Additive `shell.keys` and cursor/size fields on `shell.output`, for typing straight into a shell from the phone; see "Direct typing extension" below. Applies to v1 and v2 sessions. Existing methods, bytes and fixtures are unchanged.
- 2026-09-29: Protocol v2 is specified in [remote-protocol-v2.md](remote-protocol-v2.md). The v1 bytes in this document are unchanged. `pair` still defaults to v1. v2 is opt-in (`--protocol 2`). A v1 device is not rewritten in place; moving a phone to v2 is revoke plus a new pairing.

v1 has no RPC that creates workers, schedules or tasks. Since 2026-10-01 it can
make a project (`project.create`, "Project creation extension" below, only in the
desktop's default projects folder), and its only way to change which terminals exist
is `shell.create` and `shell.close` ("Terminal creation extension" below); everything
else works on existing sessions. That is an API-level limit only:
`shell.input` reaches every live project shell and orchestrator, including
unrestricted harness sessions and editor (Vim) tabs, so a paired device can run
arbitrary commands as the desktop user and can have a harness create anything it
can. Treat pairing as full remote control of the desktop's terminals. All JSON is
UTF-8; object key order is immaterial. Integers below are JSON numbers except
frame counters (strings). Base64 is URL-safe, **without padding**. UUIDs are full lowercase canonical UUIDs.

## Pairing and routing

Desktop generates independent random 32-byte pairing secret and two independent
32-byte relay bearer tokens per mobile device. Desktop, device and route IDs are
UUIDs. Pairing is an out-of-band secret transfer (QR/file/deep link); never send
pairing JSON to the relay, logs or analytics. Each device has a separate route.

Pairing JSON (no extra nesting):

```json
{"v":1,"relay_url":"wss://relay.example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"22222222-2222-4222-8222-222222222222","route_id":"33333333-3333-4333-8333-333333333333","device_name":"My iPhone","pairing_secret":"BASE64URL_32_BYTES","relay_token":"BASE64URL_32_BYTES"}
```

Deep link is exactly `riwork://pair?v=1&data=BASE64URL_UTF8_PAIRING_JSON`.
`relay_token` in pairing is the **mobile** routing token. Desktop token stays local.
Relay provisioning file is `{"v":1,"routes":[{"route_id":"UUID",
"desktop_token_sha256":"lowercase_hex_SHA256_OF_RAW_TOKEN_BYTES",
"mobile_token_sha256":"lowercase_hex_SHA256_OF_RAW_TOKEN_BYTES"}]}`.
It contains no endpoint pairing secret. Provision routes through operator-controlled
configuration, not an unauthenticated HTTP enrollment endpoint. Token compromise
permits denial of service/metadata access, not endpoint authentication/decryption.

WebSocket endpoint is **`/v1/ws`**. Production requires `wss://`; explicitly enabled
plaintext development requires `ws://127.0.0.1:PORT/v1/ws` or `[::1]`/`localhost`.
First WebSocket **text** message within 3 seconds of the upgrade (was 10 seconds
until 2026-09-28; clients register immediately after connecting):

```json
{"v":1,"type":"register","route_id":"UUID","role":"mobile","token":"BASE64URL_32_BYTES"}
```

`role` is `desktop` or `mobile`. Server responds
`{"v":1,"type":"registered","peer_online":false}`. Later peer changes are
`{"v":1,"type":"peer","online":true}` / `false`. One socket per role per route;
a duplicate is rejected, not allowed to replace a live socket. Unauthorized,
malformed, unavailable peer, full queue, oversized/binary frames close the socket.
A relay at its authenticated-socket limit closes a valid registration the same way.
No relay buffering across disconnects. On peer loss discard handshake/session
keys and counters. On peer availability mobile starts a fresh handshake. Relay
forwards each endpoint text frame **unchanged**, one message in/one message out,
without wrapping it. Transport ping/pong is allowed and has no protocol meaning.
Relay controls are not endpoint authenticated and must never authorize an RPC.

Liveness: the relay sends a transport ping about every 20 seconds and closes a
socket it has received nothing from (pongs and endpoint frames count) for 60
seconds; the desktop connector likewise abandons a relay that is silent for 60
seconds. WebSocket stacks answer pings automatically. A registration that
authenticates with the role's token **replaces** an existing registration for the
same route and role that has been silent for 30 seconds: the old socket is closed
and the other role sees `peer` `online:false` then `online:true`, so it resets its
session. A registration that is not silent is still rejected as a duplicate, and
a failed authentication never displaces anyone.

## Authenticated reconnect handshake

Use established HMAC-SHA256, HKDF-SHA256 and RFC 8439 ChaCha20-Poly1305 (CryptoKit
`HMAC<SHA256>`, `HKDF<SHA256>`, `ChaChaPoly`). PSK = decoded 32-byte pairing secret.
Generate fresh CSPRNG 32-byte client nonce C and desktop nonce D for **every**
handshake. UUID bytes are the 16 raw bytes in their displayed order, not UTF-8.
`||` means byte concatenation; `\0` means one zero byte; quoted labels are ASCII.

`I = desktop_uuid_bytes || device_uuid_bytes || route_uuid_bytes` (48 bytes).
`T = "riwork/v1/session\0" || I || C || D`.

1. Mobile sends `{"v":1,"type":"client_hello","desktop_id":"UUID","device_id":"UUID","route_id":"UUID","client_nonce":"B64(C)","mac":"B64(HMAC(PSK, \"riwork/v1/client-hello\\0\" || I || C))"}`.
2. Desktop checks all IDs against its route/device and constant-time verifies MAC;
   sends `{"v":1,"type":"server_hello","desktop_nonce":"B64(D)","mac":"B64(HMAC(PSK, \"riwork/v1/server-hello\\0\" || T))"}`.
3. Mobile verifies then sends `{"v":1,"type":"client_finish","mac":"B64(HMAC(PSK, \"riwork/v1/client-finish\\0\" || T))"}`.
4. Desktop verifies finish and sends first encrypted frame (counter `"0"`)
   containing `{"v":1,"type":"ready","desktop_id":"UUID","device_id":"UUID"}`.
   Mobile sends RPCs only after authenticating this ready. Both sides reject
   unexpected handshake order, invalid proofs and premature encrypted frames.

Salt = SHA256(T), IKM = PSK. Independently HKDF-expand to 32 bytes with info
`"riwork/v1/c2d"` and `"riwork/v1/d2c"` (no trailing zero). No ECDH/no forward
secrecy in v1. Pairing PSK compromise exposes recorded sessions; rotate pairing.
Session ID S = first 16 bytes SHA256(T); transmit B64(S).

## Encryption, counters and replay

Every post-handshake endpoint message is a text JSON envelope:

```json
{"v":1,"type":"encrypted","session_id":"BASE64URL_16_BYTES","direction":"c2d","counter":"0","ciphertext":"BASE64URL_CIPHERTEXT_THEN_16_BYTE_TAG"}
```

Direction is `c2d` or `d2c`. ChaCha20-Poly1305 uses the matching directional key.
Nonce = four zero bytes || counter as **8-byte unsigned big-endian** (12 bytes).
AAD = `"riwork/v1/frame\0" || S || direction_byte || counter_be64`, where direction
byte is `0x00` for c2d and `0x01` for d2c. Ciphertext is encrypted UTF-8 JSON followed
by the 16-byte tag, **without nonce** (CryptoKit combined includes a nonce; remove
its first 12 bytes for transmission). Counters start at zero independently for
both directions. Decimal string must be canonical (`0` or no leading zero).
Receiver accepts **only exactly next expected counter** and increments only after
successful authentication. Replay, gaps, wrong direction/session, tampering and
counter exhaustion close the endpoint session. Fresh reconnect derives new keys;
never reuse old keys/counters or replay old encrypted envelopes. RPC retry uses a
new envelope/counter and the original request UUID.

Limits: text WS message <=262144 bytes, decrypted plaintext <=131072 bytes (the JSON text of a reply is also at most that, unless the phone has opted into compression, see "Link extension"), shell line
<=8192 UTF-8 bytes, output lines 1..2000 (default 200), handshake <=10 seconds.
Relay defaults: <=256 authenticated sockets, <=128 configured routes, outgoing queue
<=16 messages/socket, no payload logging. Sockets that have not yet registered have
a separate budget of 16; a newcomer beyond it drops the oldest, so idle unauthenticated
sockets cannot lock out a real registration. Requests of a device are carried out
concurrently, at most 4 at a time, with typing, resizing, creating and closing in arrival order (see
"Live terminal extension", "Terminal creation extension" and "Project creation extension"); terminal input is additionally serialized per selected
shell across devices and local CLI callers.

## Encrypted RPC JSON

Request: `{"v":1,"type":"request","id":"UUID","method":"projects.list","params":{}}`.
Success: `{"v":1,"type":"response","id":"SAME_UUID","ok":true,"result":{...}}`.
Error: `{"v":1,"type":"response","id":"SAME_UUID","ok":false,"error":{"code":"invalid_request","message":"Human readable"}}`.
Unknown methods/fields, malformed UUIDs/params fail `invalid_request`. A request
that is not a JSON object, or whose `id` is missing, not a string or longer than 64
bytes, also fails `invalid_request`; with nothing to correlate, that response has
`"id":null`, and the session stays open. Responses are correlated by UUID; no
unsolicited response except handshake `ready`.

| Method | Exact params | Result |
| --- | --- | --- |
| `projects.list` | `{}` | `{"projects":[Project]}` |
| `worktrees.list` | `{"project_id":"UUID"}` | `{"worktrees":[Worktree]}` |
| `tasks.list` | `{"project_id":"UUID"}` optionally `"worktree_id":"UUID"` | `{"tasks":[Task]}` |
| `shells.list` | `{"project_id":"UUID"}` | `{"shells":[Session]}` (existing project shells) |
| `orchestrators.list` | `{}` | `{"orchestrators":[Session]}` (global + project) |
| `shell.output` | `{"shell_id":"UUID"}` optionally `"lines":200`, and additively `"styled":true`, `"if_changed":"HASH"`, `"wait_ms":5000` (see Live terminal) | `{"shell_id":"UUID","output":"terminal text"}` plus, additively, `"cursor":{"x":0,"y":0},"rows":24,"cols":80,"in_mode":false` (see Direct typing) and `"hash":"0123456789abcdef"`, `"history_size":4991,"alternate":false` (see Deep scrollback); or `{"shell_id":"UUID","unchanged":true,"hash":"0123456789abcdef","history_size":4991,"alternate":false}` (see Live terminal) |
| `shell.history` | `{"shell_id":"UUID","end":0,"lines":200}` (`lines` 1 to 5000 since 2026-10-01, 1000 before) optionally `"styled":true` (see Deep scrollback, Link extension) | `{"shell_id":"UUID","output":"older lines","line_count":200,"history_size":4991,"complete":false}` |
| `shell.input` | `{"shell_id":"UUID","line":"one physical line"}` | `{"shell_id":"UUID","status":"sent"}` |
| `shell.keys` | `{"shell_id":"UUID","batch":"UUID","items":[{"text":"ls"},{"key":"Enter"}]}` (Direct typing) | `{"shell_id":"UUID","batch":"UUID","status":"sent\|duplicate\|uncertain"}` |
| `link.configure` | `{"compression":"deflate"}` or `"none"`, or `{}` (Link extension) | `{"compression":"deflate\|none","min_bytes":2048,"max_inflated":2097152}` |
| `appearance.get` | `{}` (Theme sync) | the appearance object below: `{"v":1,"updated_at":1790000000,"dark":true,"palette":{...},"terminal":{...}}` |
| `shell.resize` | `{"shell_id":"UUID","columns":43,"rows":17}` | `{"shell_id":"UUID","columns":43,"rows":17}` |
| `shell.resize.clear` | `{"shell_id":"UUID"}` | `{"shell_id":"UUID","status":"cleared"}` |
| `shell.create` | `{"worktree_id":"UUID"}` or `{"project_id":"UUID"}`, plus `"kind":"shell\|codex\|claude\|grok"` and optionally `"unrestricted":false`, `"command":"..."` (Terminal creation) | `{"shell_id":"UUID","shell":Session}` |
| `shell.close` | `{"shell_id":"UUID"}` (Terminal creation) | `{"shell_id":"UUID","status":"closed"}` |
| `project.create` | `{"name":"My App"}` optionally `"git":false` (Project creation) | `{"project_id":"UUID","project":Project}` |

Project fields: `id,name,root` strings; `created_at` Unix seconds number.
Worktree: `id,project_id,branch,path` strings; `is_primary` boolean; `created_at`.
Task: `id,project_id,title,details` strings; `status` `todo|in_progress|done`;
`worktree_id` UUID or null; `created_at,updated_at` Unix seconds.
Session: `id` UUID, `project_id,worktree_id` UUID or null, `kind` `project|orchestrator`,
`cwd` string, `harness` `codex|claude|grok|null`, `alive` boolean, `created_at_unix` number.
Clients tolerate additive result/entity fields but must reject unknown protocol
versions. Lists expose existing CLI entities; output/input resolve a full shell
UUID against existing project shells **and** orchestrators. Dead/missing sessions
fail clearly. No project default, tmux attachment or free-form CLI RPC, and no
creation or close except `shell.create`, `shell.close` and `project.create`. `shell.input` intentionally
submits terminal input followed by Return and can run commands in the selected
shell (an unrestricted harness, a Vim tab, a plain shell prompt); clients must show
the selected shell before sending. CR, LF, NUL and other
Unicode control characters are forbidden. The v1 base supports line submission
only; keystroke-level typing is the additive `shell.keys` below. Use CLI argv directly, never an intermediate shell.

### Mobile terminal viewport extension (v1, 2026-09-27)

The iOS flow chooses a project, lists its existing open terminal sessions, and
shows one existing shell per tab, with a single visible terminal and no splits.
Use `shells.list` with an explicit project UUID; project orchestrators may be
included from `orchestrators.list` by matching `project_id`. Each tab retains its
original full shell UUID. No session/pane is created when a tab is opened.

`columns` and `rows` are **integer character-cell counts**, not pixels or points.
Both are required: columns **20..300**, rows **8..160**. Unknown fields, nulls,
floats, out-of-range values and malformed shell UUIDs fail `invalid_request`.
Compute cells from the actual visible iOS terminal area and its monospace font,
clamp to these bounds, and send `shell.resize` on tab selection, rotation or
keyboard/viewport change. Success reports the actual effective tmux pane cells;
the process in the existing PTY receives the resize (SIGWINCH). Its shell UUID,
pane identity and process remain unchanged. Ghostty renders this same tmux grid;
this does not resize the native macOS app window or create a split layout.

The desktop temporarily pins that existing terminal's tmux window to the mobile
size, saving its prior dimensions and explicit/inherited `window-size` policy.
Only the RiWork-owned single pane in window 0 is supported; a terminal with an
unexpected split/layout fails `viewport_unsupported` rather than altering it.
There is at most **one overridden shell per authenticated mobile connection**.
Resizing a different tab first releases that connection's prior override. Repeated
resize of the same tab updates its size without replacing the saved baseline.

Ownership is bound to the authenticated device **and fresh connection**, never a
caller-supplied owner or lease token. A different device cannot overwrite or clear
an active override: it receives `viewport_busy`. Both RPCs are idempotent state
operations and may be retried with a fresh RPC UUID; they do not enter the input
outcome ledger. A retry/late clear from an old connection cannot clear the new
connection's override. Input request UUID deduplication is unchanged.

`shell.resize.clear` releases only the current connection's
override on `shell_id`. It does **not** clear terminal output/history, send input,
close/detach a process, or change session identity. It succeeds with `cleared` when
already clear; another active owner's override fails `viewport_busy`. The shell must
exist and be alive (`not_found` otherwise) unless this connection holds an override
on it, which can always be released, even after the shell died. On release,
restore the previous local/inherited sizing policy. With desktop clients attached,
that policy chooses their current size; with no attached client, restore saved
rows/columns. Clients clear the previous tab on deselection/background/disconnect
when possible. Desktop also releases on peer loss, authentication failure,
revocation and connector shutdown; a crash-recovery lease restores it within
**15 seconds** even if orderly cleanup cannot run. The desktop renews that lease
only while it has authenticated a request from the connection within the last 20
seconds, so a phone that vanished stops pinning the terminal; a client keeps
sending requests while it holds an override (the iOS client polls `shell.output`
every 3 seconds) and reissues `shell.resize` to resume after a lapse. Reconnect
starts clear; iOS must reissue resize for its currently selected tab after
authenticating ready.

Validated before publication against isolated tmux 3.6a: 120x40 -> 43x17 ->
120x40 with identical session UUID, pane `%0`, and process PID. No user sessions
were touched. The implemented isolated relay/connector/CLI test also verifies
actual PTY cells, tab switching, ownership denial, lease renewal and restoration
after disconnect, reconnect, revocation and connector SIGKILL. It retains the
same UUID/window/pane/PID and completes crash recovery within the 15-second bound.

Input deduplication: UUIDs are unique per device and logical operation. Desktop
persists (device ID, request ID, canonical request digest, state/result) **before** sending
input and retains it across reconnect/restart. Retry same UUID + same request
returns cached response; changed contents fail `request_conflict`. Pending/uncertain
outcome returns `outcome_unknown`, never re-sends automatically. CLI failures after
attempting submission also return `outcome_unknown`. Clients must retain pending
UUID/line and warn on unknown outcomes, never generate a fresh UUID to auto-retry.
No eviction: max 4096 recorded inputs/device; capacity returns `cache_full` before
sending. Re-pair with a new device after reviewing old outcomes to reset capacity.
Reads may repeat safely. Errors also include `not_found`, `cli_error`,
`response_too_large` (also returned when the CLI's own output exceeds 128 KiB).
Error messages are diagnostic, not machine enums beyond code.

Revocation: local `revoke DEVICE_UUID` removes/marks the device revoked in the
protected desktop config. Running connector notices within 1 second, closes its
socket, discards keys and refuses further requests. No in-flight command can be
undone. Remove the relay route and restart relay to invalidate its routing tokens;
endpoint revocation alone already denies endpoint access. A transport dying must
never kill/recreate/close a tmux or harness session.

### Direct typing extension (v1 and v2, 2026-09-30)

Additive and compatible: two changes, no change to the handshake, envelopes,
fixtures or any existing method. They apply to protocol v1 and v2 sessions alike
(the methods do not depend on the crypto version). A client that ignores them
behaves exactly as before. Needs the iOS worker's agreement; the iOS side is built
against this text.

**`shell.keys`** types into a shell as the user types, like a terminal. It adds no
Return and no line: literal text and named keys go to the pane in order.

```json
{"shell_id":"UUID","batch":"UUID","items":[{"text":"ls -la"},{"key":"Enter"}]}
```

- `shell_id` is the full shell UUID, resolved like `shell.input` (existing project
  shells and orchestrators, alive). `batch` is a UUID chosen by the client for this
  batch.
- `items` holds 1..=64 items, in delivery order. Each is exactly one of
  `{"text":"..."}` or `{"key":"NAME"}`. An item with both, neither, a null, another
  field, or a non-string value fails `invalid_request`, as does an unknown top-level
  field.
- `text` is literal, 1..=4096 UTF-8 bytes, and is never interpreted (no shell
  expansion, no key names, a trailing `;` or `\;` arrives as written). It must not
  contain any `char::is_control` character (NUL, CR, LF, Tab, Escape, DEL, C1
  controls) or U+2028 / U+2029: send `Enter` and `Tab` as keys. The text of all
  items in a batch adds up to at most 4096 bytes.
- `key` is one of `Enter`, `Tab`, `BTab` (Shift-Tab), `Escape`, `Backspace`,
  `Delete` (forward delete), `Up`, `Down`, `Left`, `Right`, `Home`, `End`,
  `PageUp`, `PageDown`, or `C-a` to `C-z` (Control plus a lowercase letter). Names
  are case-sensitive.

Result: `{"shell_id":"UUID","batch":"UUID","status":"sent"}`. `status` is

- `sent`: the batch was delivered to the pane.
- `duplicate`: this batch UUID was already delivered for this device; nothing was
  sent again.
- `uncertain`: an earlier attempt with this batch UUID started and its outcome is
  unknown (for example the connector died mid-send); nothing was sent again. The
  client must not invent a new batch UUID to type the same text again without
  looking at the screen.

Errors use the usual shape: `invalid_request` (any validation failure, before
anything runs), `not_found` (unknown or dead shell, or one the device cannot reach,
as for `shell.input`), `input_unavailable` (new: the pane has terminal input
disabled), and `cli_error` as elsewhere. A pane in copy mode
is taken out of it first so the keys reach the program.

Delivery and retries:

- The desktop processes a device's batches one at a time, and serializes typing per
  shell across devices and local CLI callers with the lock `shell.input` uses. The
  client sends its batches sequentially and waits for each result.
- Dedupe is by `batch` and never by request `id`, which stays unique per request as
  before. A retry uses a new request `id` and the same `batch`; the same batch with
  other contents is still `duplicate`.
- The desktop keeps a per-device ledger of the 4096 most recent batches
  (`keys-DEVICE_UUID.json`, mode 600, atomic writes), separate from the `shell.input`
  outcome ledger, oldest pruned first. It records a batch as pending before typing
  and as sent afterwards, and it survives reconnects and connector restarts. A batch
  pruned after 4096 newer ones is new again.
- `invalid_request`, `not_found` and `input_unavailable` mean nothing was typed and
  the batch is not recorded, so the same batch UUID may be retried later. A
  `cli_error` after typing began leaves the batch pending: a retry answers
  `uncertain`. A `cli_error` before typing began (the desktop could not reach
  tmux) also forgets the batch. Either way a retry is always safe.
- Delivery note, not a wire change: when a key item directly follows a text item in
  a batch, the desktop sends the key about 150 ms after the text (Codex reads
  characters arriving within 120 ms of each other as a paste and would take an
  `Enter` for a newline). Consecutive text items go together, keys with no text
  before them are not delayed, and a batch with no text-then-key boundary is
  delivered at once. The pauses happen inside the one `shell.keys` call, under the
  shell's input lock, so the reply comes after them: a batch that alternates text
  and keys takes up to 32 x 150 ms, and a client's timeout for `shell.keys` should
  allow about 6 s plus the usual round trip.
- Batches that the phone could not deliver are the phone's to buffer; the desktop
  holds nothing. Because typed text may run commands, `shell.keys` has the same
  authority as `shell.input`.

**`shell.output` additive result fields.** The result gains the four fields below.
Old clients ignore them.

```json
{"shell_id":"UUID","output":"terminal text","cursor":{"x":5,"y":2},"rows":24,"cols":80,"in_mode":false}
```

- `rows` and `cols` are the pane size in cells; `cursor.x` and `cursor.y` are the
  0-based cursor cell within the visible screen (tmux `cursor_x`, `cursor_y`). `x`
  counts terminal cells, so a wide character before the cursor takes two.
  `in_mode` is true while the pane is in copy mode (the text is still the live
  screen, not the copy-mode view).
- The screen rule: `output` is the scrollback requested by `lines` followed by the
  visible screen, and the visible screen is **exactly the last `rows` lines of
  `output`**, blank rows at the bottom included. Every line ends with `\n`, one line
  per screen row (wrapped lines are not joined), so split at `\n`, drop the empty
  piece after the final `\n`, and take the last `rows` pieces; `cursor.y` indexes
  them. `output` therefore holds `min(lines, scrollback) + rows` lines. The desktop
  captures the text and the pane report in one tmux command list, and adds any
  bottom rows a tmux would trim, so this holds for full-screen programs and copy
  mode too.
- If the desktop cannot read the pane report, or it does not fit the text (for
  example an older desktop), the four fields are omitted together and the call
  still succeeds.

### Live terminal extension (v1 and v2, 2026-09-30)

Additive and compatible, like the direct typing extension: three optional
parameters and one result field on `shell.output`, one alternative result, and a
change in how a device's requests are scheduled. No change to the handshake,
envelopes, fixtures or any other method; it applies to protocol v1 and v2
sessions alike. A client that sends none of the new parameters gets what it got
before, plus a `hash` field it may ignore. The iOS side is built against this text.

**`shell.output` parameters**, all optional:

- `styled` (boolean, default `false`): keep colors and text attributes in `output`
  as SGR sequences. See "SGR only" below.
- `if_changed` (string): the `hash` of an earlier result. 1 to 64 printable ASCII
  characters, no spaces; anything else fails `invalid_request`.
- `wait_ms` (integer `0..=10000`, default `0`): how long to wait for a change. It
  only means something together with `if_changed` and is ignored without it. A
  value outside the range, or not an integer, fails `invalid_request` even then.

**`hash`.** Every result that carries `output` also carries `"hash"`: a short
string, in practice 16 lowercase hex digits (clients treat it as an opaque string
of at most 64 characters). It names the whole answer: the `output` text exactly as
returned (styled or plain, as asked), the cursor, `rows`, `cols`, `in_mode`,
`history_size` and `alternate` (or that the desktop could not report them), the
`lines` value the request used
(after clamping) and the `styled` flag. The same request parameters against the
same screen give the same hash, in any connection and after a desktop restart. A
hash therefore only ever matches an answer to the same question: after changing
`lines` or `styled`, an old hash never matches and the content is returned at once.
Anything visible changes it: text, cursor movement, a resize, copy mode. It is not
a secret and not a proof of anything.

**Waiting.** With `if_changed`, the desktop looks at the pane and compares:

- If the hash differs, the result is the usual full one (with its new `hash`), at
  once.
- If it is equal and `wait_ms` is `0`, the result is `unchanged`, at once.
- If it is equal and `wait_ms` is positive, the desktop waits inside one process
  (`riwork shell output ID --lines N --json [--styled] --if-changed=HASH --wait-ms N`,
  not one process per check) and captures again whenever tmux says the pane was
  written to, resized or put in a mode (after a few milliseconds, so that a screen
  drawn by several writes is read whole), and in any case every 4 seconds. Where
  tmux cannot say so, it captures about every 80 ms instead. It returns the full
  result as soon as the hash differs. If nothing changed within `wait_ms`, it
  returns

```json
{"shell_id":"UUID","unchanged":true,"hash":"SAME_HASH"}
```

  with no `output`, `cursor`, `rows`, `cols` or `in_mode` (since the deep scrollback
  extension it also carries `history_size` and `alternate`, see below). The call takes `wait_ms`
  plus at most a capture or two (each limited to 5 seconds by the CLI); a client's
  timeout for a waiting call should allow `wait_ms` plus about 20 seconds. The
  connector gives the CLI `wait_ms` plus 8 seconds (never less than 15) before it
  fails the call with `cli_error`.
- Errors are as before: `invalid_request` before anything runs, `not_found` for an
  unknown or dead shell at the start, `cli_error` and `response_too_large`. A shell
  that ends during a wait fails the call with `cli_error` ("has exited").
- A wait ends early, and the desktop stops the capture process, when the
  connection closes, the phone goes offline, or the device is revoked (see
  Concurrency).
- A connector paired with an older `riwork` CLI answers `cli_error` "the installed
  riwork CLI does not support styled output or waiting for changes; update RiWork"
  to a request that uses a new parameter. A desktop whose connector predates this
  extension answers `invalid_request` (unknown field) to them; a client then falls
  back to plain polling.

**SGR only.** With `styled`, `output` keeps the SGR sequences exactly as tmux
`capture-pane -e` writes them and nothing else that is escaped:

- Kept: `ESC [ P m`, where `P` is 0 to 64 characters from `0-9`, `;` and `:`. That
  covers reset, bold, dim, italic, underline, inverse, the 16, 256 and truecolor
  foreground and background forms (`38;5;N`, `38;2;R;G;B`, and the same with
  `:`), and their resets. Sequences are passed byte for byte.
- Removed, whole: OSC (titles, hyperlinks `ESC ] 8 ; ; url ST`), DCS, SOS, PM and
  APC strings; CSI with any final byte other than `m`, a private prefix
  (`ESC [ > 4 ; 2 m` is not SGR) or intermediate bytes; charset selection
  (`ESC ( 0`, SO and SI) and every other escape (`ESC 7`, `ESC c`, ...); the 8-bit
  C1 forms of all of these (U+0080 to U+009F); and every control character except
  `\n` and `\t`.
- A sequence that is cut short is dropped up to the byte that cut it, which is then
  read normally. A string sequence ends at BEL, `ESC \` or the end of the line. No
  newline is ever removed, so the line count is unchanged.
- The only ESC in a styled `output` is therefore the start of an SGR sequence. The
  desktop filters, and the connector checks again and answers `cli_error` rather
  than pass on anything else.
- The lines are the ones of the plain capture: the screen rule is unchanged (the
  visible screen is the last `rows` lines), and removing the SGR sequences from a
  styled `output` gives exactly the plain `output` of the same screen. Text
  attributes are state that carries across line ends: tmux does not reset at a
  newline (a line may begin with `ESC [ 0 m`), so a renderer keeps its state from
  one line to the next until it sees a reset. The `hash` of a styled answer covers
  the styled text.

**Concurrency.** Before this extension a device's requests were carried out one at
a time. They are now carried out concurrently, so that a waiting `shell.output`
never delays anything else from the same device, above all `shell.keys` and
`shell.resize`:

- Responses may arrive in any order. Each carries the `id` of its request; match
  them by `id`. A client that has one request outstanding at a time sees no
  difference.
- `shell.keys`, `shell.input`, `shell.resize`, `shell.resize.clear`, `shell.create`,
  `shell.close` and `project.create` run one at a time, in the order they arrived. Batch ledgers, the input outcome ledger and the
  viewport keep their meaning: a `shell.keys` batch is still delivered once per
  device and batch UUID, and resizing is still consistent per connection.
- At most 4 requests run at once per device: one of those four, and three others
  (reads such as `projects.list`, `appearance.get`, `shell.history` and `shell.output`), of which at
  most 2 may be waiting `shell.output` calls (`if_changed` with a positive
  `wait_ms`). So typing and resizing always have a slot of their own and a plain
  read always has one too. Further requests wait in arrival order and start as
  slots free up; a request that has to wait does not hold up a later one it does
  not compete with. Up to 64 requests may be waiting; beyond that the desktop stops
  reading from the connection until it has started some.
- Responses are encrypted and sent one at a time, so their counters stay in order.
- Authorization is checked when a request starts and again before its response is
  sent. A revoked device's connection closes within about 250 ms even while a wait
  is pending, and the wait's capture process is stopped. A wait is also ended when
  the phone goes offline or the connection closes. Typing and resizing that were
  already accepted still finish, and their answers are dropped, as before.
- The viewport lease is renewed as before, only while the desktop has received a
  request from the connection within the last 20 seconds; a pending wait does not
  count by itself, so a client that holds an override and waits for changes sends
  its next request (a re-poll counts) before the lease lapses.

### Deep scrollback extension (v1 and v2, 2026-09-30)

Additive and compatible, like the two before it: two result fields on
`shell.output` and one new method, `shell.history`. No change to the handshake,
envelopes, fixtures, any other method or the concurrency rules; it applies to
protocol v1 and v2 sessions alike. A client that ignores the new fields and never
calls the method is unaffected. The iOS side is built against this text.

tmux keeps up to 100000 lines of scrollback per shell. `shell.output` only ever
returns the newest `lines` (at most 2000) of them together with the screen;
`shell.history` reads the rest, a page at a time.

**`shell.output` result fields.** Every result that carries `output`, and the
`unchanged` result, gains:

```json
{"shell_id":"UUID","output":"terminal text","history_size":4991,"alternate":false,"hash":"..."}
{"shell_id":"UUID","unchanged":true,"hash":"...","history_size":4991,"alternate":false}
```

- `history_size` (integer, at least 0): the number of scrollback lines above the
  visible screen when the desktop captured the pane (tmux `history_size`). It
  counts lines the screen has scrolled away, not `output`'s own lines: `output`
  holds `min(lines, history_size) + rows` of them.
- `alternate` (boolean): true while a full-screen program (vim, less, htop) is on
  the terminal's alternate screen (tmux `alternate_on`). The alternate screen has
  no scrollback of its own; the `history_size` lines above it are the normal
  screen's, are still there, and `shell.history` still reads them. The visible
  screen, `output`'s last `rows` lines, is the program's.
- RiWork runs Codex, Grok and Claude Code on the main screen by default (the
  desktop setting `agent_inline_mode`), so for them `alternate` is false and the
  whole conversation is in `history_size`. Codex and Grok clear their scrollback
  and draw the transcript again at the new width on every resize, including
  `shell.resize` and `shell.resize.clear`, and settle within about half a second.
  After a resize, `history_size` and the line offsets of earlier pages no longer
  name the same lines: a client discards the pages it holds and reads them again.
- Both are part of the `hash`: the same screen with a longer history, or with the
  program entering or leaving the alternate screen, has another hash. So a client
  that polls with `if_changed` learns of output that scrolled lines away without
  changing what is visible (a screen full of identical lines), and the
  `unchanged` result, which says the hash still matches, carries the same two
  values as the answer it stands for, so `history_size` can be followed without
  fetching any text.
- Both are present or both absent. They are absent when the desktop could not
  read the pane report and from a desktop that predates this extension; treat
  that as unknown. The hash now covers these fields, so a hash handed out before
  the desktop was updated never matches again: the client is simply sent the full
  answer once.

**`shell.history`** reads one page of scrollback, oldest lines first within the
page. Params (unknown fields fail `invalid_request`):

```json
{"shell_id":"UUID","end":0,"lines":200,"styled":false}
```

- `end` (integer `0..=4294967295`, required): how many scrollback lines directly
  above the screen to skip. `0` makes the page end at the line just above the
  screen, `200` at the line 201 above it.
- `lines` (integer `1..=1000`, required): the page size. Anything else, a float
  or a string fails `invalid_request`.
- `styled` (boolean, default `false`): as for `shell.output`, see "SGR only".

The page holds the scrollback lines from `end + lines` above the screen down to
`end + 1` above it, both included (in tmux terms `capture-pane -p [-e] -S
-(end+lines) -E -(end+1)`), clamped at the top of the history. With
`history_size` H, line 1 of the history is the oldest and line H is the one just
above the screen, so the page is lines `max(1, H-end-lines+1)` through `H-end`.
For example, with H = 5000:

| `end` | `lines` | page | `line_count` | `complete` |
| --- | --- | --- | --- | --- |
| 0 | 3 | lines 4998, 4999, 5000 | 3 | false |
| 3 | 2 | lines 4996, 4997 | 2 | false |
| 4990 | 100 | lines 1 to 10 | 10 | true |
| 4900 | 100 | lines 1 to 100 | 100 | true |
| 4999 | 1000 | line 1 | 1 | true |
| 5000 or more | any | none | 0 | true |

Result:

```json
{"shell_id":"UUID","output":"line\nline\nline","line_count":3,"history_size":5000,"complete":false}
```

- `output` is the page's lines from top to bottom joined by `\n`, with **no**
  newline after the last one. `line_count` is how many lines that is, so a split at
  `\n` gives exactly `line_count` pieces, and the count tells an empty page
  (`""`, 0 lines) from one blank line (`""`, 1 line). Lines are exactly the
  scrollback lines, one per terminal row like `shell.output` (wrapped lines are not
  joined), blank ones included at the top, in the middle and at the bottom of a
  page: nothing is trimmed. They are the lines `shell.output` returns above its
  screen, with the same trailing-space handling; with `styled` the same SGR-only
  filter as `shell.output` is the only difference, and removing the SGR sequences
  gives exactly the plain page.
- `line_count` is `min(lines, history_size - end)`, or 0 when `end >= history_size`.
- `history_size` is the history length at the moment of the capture. The page and
  this number come from one tmux command list, so they agree even while the shell
  prints.
- `complete` is true when the page reached the very top of the history, so no older
  line exists (`end + lines >= history_size`). It is also true for an empty page:
  when `end >= history_size`, `output` is `""`, `line_count` 0 and `complete`
  true.
- A styled page starts from the default text attributes, exactly as tmux writes it:
  the first line carries whatever sequences it needs, so a page can be drawn on its
  own. Within a page, state carries across line ends as for `shell.output`.

*Paging.* To read further up, ask again with `end` increased by `line_count`, until
`complete` is true. `end` counts from the screen, so it moves when the shell prints:
if `history_size` grew by `d` since the page before, add `d` to the next `end` to
continue from the line just above that page. Once the history is full (100000
lines), `history_size` stops growing while the oldest lines fall off the top, so a
client cannot tell how far the lines have moved; it should treat pages of a shell
that is printing heavily as a snapshot of the moment they were read. `shell.history`
never waits and changes nothing; the only way to learn of new history is
`history_size` in `shell.output`.

*Page length since 2026-10-01.* `lines` is `1..=5000` on a connector that has the link extension, which announces it as `history_max_lines` in `ready`; a phone never asks for more than the desktop announced, and treats 1000 as the limit of a desktop that announces nothing. The installed `riwork` CLI has a limit of its own (1000 in builds from before 2026-10-01) and may be an older build than the connector. The first page it refuses for being too long is `cli_error` (`--lines needs an integer from 1 to 1000`); the connector remembers that limit, answers longer pages itself from then on (`invalid_request`, `lines must be 1..=1000`, without running anything) and announces it as `history_max_lines` in the `ready` of later sessions. The phone reads the number from either message and carries on with pages of that size.

*Errors.* `invalid_request` for any parameter above or a UUID that is not one;
`not_found` for an unknown shell or one that is not alive (the same rules and
message as `shell.output`); `response_too_large` when the page does not fit one
encrypted response (128 KiB: a 1000-line page of wide styled lines can), in which
case the client halves `lines` and asks again; `cli_error` for anything the desktop
CLI reports, and for a CLI too old to know `riwork shell history` ("the installed
riwork CLI does not support shell history; update RiWork"). A desktop whose
connector predates this extension answers `invalid_request` "unsupported RPC
method"; a client then offers only the newest `lines` of `shell.output`.

*Scheduling.* `shell.history` runs in the shared lanes with the other reads (see
Concurrency): it takes one of the three shared slots, never the slot of typing and
resizing, and never one of the two wait slots, so a page in flight cannot delay
`shell.keys`, `shell.resize` or a waiting `shell.output`. Three slow pages at once
leave only the ordered slot free, and further reads queue. A page is one bounded
tmux call, and the connector gives the CLI the usual 15 seconds before it fails the
call with `cli_error`.

### Link extension (v1 and v2, 2026-10-01)

Additive and compatible, like the extensions before it: one field on every response, one on `ready`, one new request, and a second form of the sealed reply. No change to the handshake, the envelope, the AAD, the counters or any other method; it applies to protocol v1 and v2 sessions alike. A phone that ignores all of it, and an older desktop, behave exactly as before: nothing is compressed unless the phone asks, and the phone asks only a desktop that said it can. The iOS side is built against this text.

Why. A page of history costs the phone `round trip + what the desktop does (the CLI starting, tmux capturing, the filter) + transfer`, and only the last term says how fast the link is. The desktop's term is 50 to 300 ms and grows with the page and the load of the machine, so a fast link looked slow. And terminal text with SGR escapes is highly repetitive: it deflates 3 to 15 times, which turns a 3 MB history into a few hundred KB.

**`server_ms`.** Every response, error responses included, carries `"server_ms": N` (a non-negative integer, milliseconds) as a top-level field next to `ok` and `id`, never inside `result`. It is the time the connector had the request: from the moment its frame was decrypted until the reply was ready to be sealed. That includes waiting for a slot (see Concurrency), the CLI and tmux, a long poll's wait (so an `unchanged` answer to `wait_ms: 8000` carries about 8000) and the time spent deflating the body; it does not include sealing or writing the socket, a few hundred microseconds. The phone takes it out of the time it measured between sending a request and receiving the whole reply: what is left is the network's, and for a small reply that is the round trip, which makes every key acknowledgement and every `unchanged` answer a clean sample of it. Clients that do not know the field ignore it. The connector writes it last, so a deflated reply can pay for its own compression: the body is deflated and flushed to a byte boundary, the clock is read, and the closing field is deflated onto the same stream.

**`ready` announces what this desktop does.** The first encrypted frame gains `features`, an object an older phone does not read (it checks `type`, `desktop_id` and `device_id` only):

```json
{"v":1,"type":"ready","desktop_id":"UUID","device_id":"UUID","features":{"deflate":{"min_bytes":2048,"max_inflated":2097152},"history_max_lines":5000}}
```

- `deflate` is present when the desktop can deflate replies. `min_bytes`: replies whose JSON is shorter are never compressed. `max_inflated`: the most a compressed reply may inflate to.
- `history_max_lines`: the most `shell.history` takes in one page (see above): 5000, or less once the installed CLI has been seen to refuse longer pages. Absent: 1000.
- A phone treats a missing `features`, a missing key, or one it cannot read as "not offered". `ready` is authenticated and encrypted, so a relay can neither add nor strip it.

**`link.configure`** opts this session in or out of compression:

```json
{"v":1,"type":"request","id":"UUID","method":"link.configure","params":{"compression":"deflate"}}
```

- `compression` is `"deflate"` or `"none"`; `params` may be `{}` (nothing changes, the answer says what is). Anything else, and any other field, is `invalid_request` as for every method.
- Result: `{"compression":"deflate","min_bytes":2048,"max_inflated":2097152}` (`"none"` when off).
- The setting belongs to the connection's session and is off at the start of every session (a new handshake, or any peer change). The desktop answers at once, in arrival order, outside the lanes; every response it seals after the answer may be compressed. The answer itself is plain. The form of a reply is chosen when it is sealed, so a request that was already running when the setting changed is answered in the new form. The size limit it was allowed (below) is the one in force when it was received: a request received before the opt-in keeps the old limit, and a large reply to one received before an opt-out becomes `response_too_large`.
- A phone sends it once, right after `ready`, only to a desktop whose `ready` has `features.deflate`, and again when the person changes the setting. A desktop from before the extension answers `invalid_request` "unsupported RPC method", which is why it is not sent blindly. The phone does not wait for the answer; whichever form arrives, it reads.

**The two forms of a sealed reply.** The plaintext that is sealed (what the envelope's ciphertext holds) is

- the JSON text, starting with `{`, as it always was; or
- `0x01 || inflated_length || deflate`: the byte `0x01`, the length of the JSON text as an unsigned 32-bit big-endian number, and the JSON text compressed as raw deflate (RFC 1951: no zlib header, no checksum). That is what zlib reads with `windowBits = -15`, what Rust's `flate2::DeflateEncoder` writes and what Apple's `COMPRESSION_ZLIB` decodes.

A receiver tells them apart by the first byte (JSON never starts with `0x01`), refuses any other first byte except white space, refuses a length above `max_inflated` (2 MiB) before it allocates anything, and refuses a stream that does not inflate to exactly the length it declares. The marker is inside the authenticated ciphertext, so nobody on the path can set or clear it. Everything else about the frame is unchanged: the sealed plaintext is still at most 131072 bytes, the counters and AAD are the v1 or v2 rules, and a text WebSocket message is still at most 262144 bytes (a full compressed frame is about 175 KB in base64url).

The desktop compresses a reply only if the session opted in, the reply's JSON is at least `min_bytes` (2048) long, and the compressed frame is smaller than the plain one. Only the desktop's replies are ever compressed; requests, which carry what the person types, never are.

**Larger limits that go with it.** Until now a reply was at most 131072 bytes of JSON because that was what one frame held. For a session that has opted in, the JSON text of a reply may be up to `max_inflated` (2 MiB); what decides is whether the frame fits 131072 bytes once deflated, and if it does not the reply is replaced by `response_too_large` for the same request id (the phone asks for fewer lines), exactly as an oversized plain reply always was. A session that has not opted in keeps the old limit. For `shell.output` and `shell.history` the connector lets the CLI write up to 2 MiB to its pipe before it cuts it off (128 KiB for every other method, as before), and checks the reply against the session's limit afterwards. `shell.history` `lines` is 1 to 5000 (above): measured on Claude Code's scrollback (about 200 to 270 bytes a line as JSON, 22 deflated) a 50,000-line history is 11.8 MB as JSON and about 1.1 MB deflated, which the phone fetches in about a dozen requests of at most 112 KiB on the wire instead of fifty of 80 KiB.

**Measured** on 2026-10-01 on real styled output (`riwork shell output --styled`, `riwork shell history --styled`; raw deflate at level 6, ratio = JSON bytes / deflated bytes): a 1000-line page of Claude Code scrollback 10.4 times (200 KB to 19 KB), a 2000-line history 11.1 times, a 500-line screen of the same shell 8.7 times, a plain zsh screen 7.4 times; the worst content, short dense Codex screens, 3.0 to 3.6 times; a 200-line page 7.6 times and a 25-line page 6.1 times (the ratio rises with the page and levels off above about 500 lines). Level 1 gives 8.6 times and level 9 12.0 times on the largest page; level 6 costs about half a millisecond per 100 KB. The connector uses level 6, and its own encoder (flate2, sync flush and closing field included) gives the same within a few percent on the same shells: 10.1 times on a 140 KB page of that scrollback (18.6 KB on the wire instead of 187 KB, 0.9 ms), 11.8 times on a 252 KB page (1.5 ms), 7.1 times on a 200-line page, 5.5 to 8.9 times on Claude Code screens and 2.8 to 3.3 times on Codex screens. Compression stops paying below about 100 bytes of JSON, so the 2 KiB minimum is a policy, not a limit.

**Security.** Compressing text that an attacker can influence together with text the attacker wants to learn, and letting the attacker see the length of the result, is the CRIME/BREACH oracle. A reply here is terminal text; an attacker who can get chosen text onto a terminal (a file or page the person prints, the output of a hostile repository) and who can see the size of the frames (the relay operator, or anyone who can read the sizes of TLS records between the endpoints and the relay) and where a secret is on the same screen, could in principle guess it a byte at a time by watching how the compressed size moves. The relay is blind and normally self-hosted, and it already sees sizes and timing, so in the intended deployment the party who could do this is the one that runs the relay; but with compression the size of a frame now depends on what is on the screen, which it did not before, and that is a real, if narrow, new channel. The extension limits the exposure: only the desktop's replies are compressed; a reply is compressed on its own, never together with another or with anything from the request, and only if it is at least 2 KiB; nothing secret is added to a reply by the protocol. And it is optional on both sides: the phone's Settings has a switch ("Compress traffic", `link.configure` with `"none"`), and a phone that does not ask never gets a compressed frame. Deployments that treat the relay or the path as hostile should turn it off. Frames are not padded.

**Compatibility.** An older phone: sends no `link.configure`, so receives plain JSON that gains a `server_ms` field and a `features` field in `ready`, none of which it reads. An older desktop: its `ready` has no `features`, so the phone sends nothing new and reads plain frames; its `shell.history` takes 1000 lines. A phone that asks a desktop for something it announced and is refused anyway (an older CLI behind a newer connector) learns from the error and adapts. The vectors in `remote/fixtures/link.json` (made by Python's zlib and by the connector itself, with frames that must be refused) are read by the Rust tests and by the iOS tests; `remote/fixtures/generate_link.py` writes them.

### Theme sync extension (v1 and v2, 2026-09-30)

Additive and compatible, like the direct typing extension: one new read-only
method, no change to the handshake, envelopes, fixtures or any existing method,
and it applies to protocol v1 and v2 sessions alike. A client that never calls it
behaves exactly as before. The iOS side is built against this text.

**`appearance.get`** returns the colors the desktop shows, so the phone can match
them. Params are exactly `{}` (unknown fields fail `invalid_request`, as for every
method). It is read-only, needs no shell selection, enters no ledger and may be
repeated freely. Result:

```json
{
  "v": 1,
  "updated_at": 1790000000,
  "dark": true,
  "palette": {"bg":"#090d14","panel":"#101720","panel_active":"#14212a","divider":"#253c45",
              "cyan":"#55e6dc","magenta":"#ce78ef","gold":"#f4bf75","text":"#d3e1e6","muted":"#708993"},
  "terminal": {"background":"#090d14","foreground":"#d3e1e6",
               "palette":["#131b25","#f0738b","#61d5ae","#f4bf75","#78a9ff","#ce78ef","#55e6dc","#d3e1e6",
                          "#58707b","#ff8ba0","#83ebc3","#ffd191","#9bc0ff","#dfa3f7","#84f3ea","#ffffff"]}
}
```

- Every color is lowercase `#rrggbb`. `v` is 1; a client rejects another value.
  Clients tolerate additive fields.
- `palette` is the application palette (backgrounds, dividers, accents, text).
  `terminal` is what the desktop's terminals show: the background, the foreground
  and the 16 ANSI colors in index order (exactly 16). It follows the selected theme,
  the Ghostty configuration while following Ghostty, and the "Use RiWork terminal
  colors" option. It is omitted when the desktop could not read its terminal colors.
- `dark` is true when the palette background is dark: its relative luminance
  (WCAG, on the linearized sRGB channels) is below 0.5.
- `updated_at` is Unix seconds of the last change to the colors. It does not move
  when the desktop restarts with the same colors. There is no push: a client reads
  the object again when it wants to follow later changes, and compares the object
  (or `updated_at`).
- Errors: `not_found` with the message "appearance not published" when the desktop
  has not published usable colors: the file is missing, unreadable, not valid or
  over 16 KiB (open the RiWork app on the desktop to publish). `invalid_request`
  for bad params, and
  `cli_error` as elsewhere; a connector paired with an older `riwork` CLI answers
  `cli_error` "the installed riwork CLI does not support appearance; update RiWork".
- A desktop whose connector predates this method answers `invalid_request`
  "unsupported RPC method". The phone treats it, and `not_found`, as "no desktop
  colors" and keeps its own theme.

Where the colors come from. The GUI publishes `appearance.json` in `RIWORK_HOME`
(mode 600, written to a temporary file and renamed into place) at startup and
whenever the resolved palette or terminal colors change: a theme choice, an edit to
the Ghostty configuration, or the terminal colors option. It writes only when the
colors differ from the file, so several windows or app processes, which share the
one settings-level appearance, publish the same content without rewriting it. The
connector never reads the file itself: `appearance.get` runs
`riwork appearance --json`, which prints the file validated and re-serialized in
the shape above and exits non-zero with "RiWork has not published its appearance
yet; open the RiWork app" when the file is missing or invalid. `riwork appearance`
without `--json` prints a short summary. The connector re-validates the CLI output
and treats output over 16 KiB as invalid.

### Terminal creation extension (v1 and v2, 2026-10-01)

Additive and compatible, like the extensions before it: two new methods and one
new error code. No change to the handshake, envelopes, fixtures or any existing
method; it applies to protocol v1 and v2 sessions alike. A client that never
calls the methods is unaffected. The iOS side is built against this text.

**`shell.create`** starts a terminal, exactly as `riwork shell create` does on the
desktop. Params (an object; unknown fields, nulls and wrong types fail
`invalid_request` before anything runs):

```json
{"worktree_id":"UUID","kind":"codex"}
{"project_id":"UUID","kind":"shell","command":"npm run dev"}
{"worktree_id":"UUID","kind":"claude","unrestricted":true}
```

- Exactly one of `project_id` and `worktree_id`, a full lowercase canonical UUID.
  Names, branch names, paths and UUID prefixes are refused (the CLI would resolve
  them). A `project_id` starts the terminal in the project's root and primary
  worktree, a `worktree_id` in that worktree.
- `kind` (required): `shell` (the user's own login shell, or `command`), `codex`,
  `claude` or `grok`, spelled exactly. The agents start as the desktop's own
  "New Tab" entries do: the official CLI in a persistent terminal with the same
  Cua driver connection, account selection and inline-mode setting.
- `unrestricted` (optional boolean, default `false`): only for the three agents
  (`true` with `shell` fails `invalid_request`). The desktop offers "Codex ·
  unrestricted", "Claude · unrestricted" and "Grok · unrestricted" as separate
  entries of its New Tab menu, next to the restricted ones its shortcuts open,
  and has no setting that makes unrestricted the default, so the phone may ask
  for it and the default is the same as the desktop's: restricted (the agent's
  own approval prompts stay on). Unrestricted adds the agent's
  permission-bypass flag. The phone should make it a deliberate choice.
- `command` (optional string, only with `shell`): 1 to 4096 UTF-8 bytes, not
  blank, no `char::is_control` character and no U+2028 / U+2029 (one physical
  line, as for `shell.input`), and it must not begin with `-`. The desktop's new
  terminal runs it in place of an interactive prompt and ends with it, as with
  `riwork shell create --command`. It is text for that terminal's shell: it is
  never interpreted by the connector.

Result: `shell_id` is the new terminal's full UUID and `shell` is its entry as
`shells.list` shows it (`id`, `project_id`, `worktree_id`, `kind` `project`, `cwd`,
`harness`, `alive`, `created_at_unix`), so the phone can open it at once:

```json
{"shell_id":"UUID","shell":{"id":"UUID","project_id":"UUID","worktree_id":"UUID","kind":"project","cwd":"/Users/me/app","harness":"codex","alive":true,"created_at_unix":1790000000}}
```

Accounts, homes, the launch command and `unrestricted` are not part of the entry,
as for `shells.list`. The connector checks that the CLI's session is the one asked
for (a project terminal in that project or worktree, running that agent) and
answers `cli_error` if it is not. `alive` is the state at creation: a `command`
that ends at once leaves a terminal that is already gone from `shell.output`
(`not_found`).

Errors (`error.code`): `invalid_request` for any validation failure above (and
`unsupported RPC method` from a desktop that predates the method); `not_found`
when the project or worktree does not exist on the desktop or its folder is gone;
`harness_unavailable` (new) when the agent's CLI is not installed or not on the
desktop's PATH, or the Cua driver the agents need is not installed, with the CLI's
sentence as the message (for example `codex is not installed or is not on PATH`);
`cli_error` for anything else (tmux, a directory tmux cannot use, an agent that
refuses to start), with the CLI's first line as the message, and for a start that
took longer than 60 seconds. The desktop does not report which agents are
installed in advance; a client lists all four kinds and shows
`harness_unavailable` when one fails.

Security and validation. Creation is not a new kind of authority: a paired device
can already type into any shell (`shell.input`, `shell.keys`), so it can start any
command or agent in an existing terminal. It is still validated as strictly as the
other methods:

- Everything is checked before a CLI runs: object shape, unknown fields, UUID
  form, the four kinds, the `unrestricted` and `command` rules above.
- The CLI is run with its argument vector built from validated values, one
  argument per value (`shell create --worktree ID --harness codex --unrestricted
  --json`). Nothing is concatenated into a shell string; a `command` is one
  argument. The rule against a leading `-` keeps it from being read as an option of
  its own, and `--json` is always last.
- The project or worktree is looked up first (`riwork project show ID --json`, or
  `worktree show`), and its `id` must equal the id sent: the CLI also resolves
  names, branch names, paths and id prefixes, so an id that is nobody's could
  otherwise start a terminal in some other worktree. An unknown id is `not_found`
  and starts nothing. The phone never names a directory, so it cannot start a
  terminal anywhere that is not a registered project or worktree. If the CLI still
  reports a session other than the one asked for, the connector answers `cli_error`
  and ends that session.
- The device's authorization is checked when the request starts and again just
  before the CLI runs, so a device revoked while queued creates nothing.
- Both methods are in the ordered lane and are not cut short when the phone's
  session ends (see below), and are not recorded in any ledger.

Scheduling and retries. `shell.create` and `shell.close` run in the ordered lane
with `shell.keys`, `shell.input`, `shell.resize` and `shell.resize.clear`: one at
a time per device, in arrival order, and, like typing, not dropped half done when
the phone's session ends. A creation goes further: the desktop CLI starts the tmux
session and only then writes it into its registry, and a CLI killed in between
leaves a session nobody can see or close, so the connector runs it in a task that
outlives the request. When the connector ends the connection for any other reason
(revocation, a relay error) the CLI is not killed; only the answer is lost. (A
start that takes longer than 60 seconds is stopped, which needs a hung tmux, and
can leave such a session.) A close that is cut short leaves a listed terminal that
is gone and can be closed again.
The cost is that typing from the same device waits behind a start (usually well
under a second; an agent can take 20 seconds or more, Grok waits for the driver for
up to 20, and the connector gives the CLI 60 seconds). A client's timeout for
`shell.create` should allow about 90 seconds, and about 30 for `shell.close`.

Creation is **not idempotent** and is not deduplicated: a second request starts a
second terminal, whatever its request `id`. A client that loses the answer (a
timeout, a lost connection) cannot tell whether the terminal exists. It must not
retry by itself; it lists the project's terminals (`shells.list`) and lets the user
decide.

On the desktop. The terminal is created by the same CLI the desktop app uses
(`SessionManager::create` or `create_harness`), registered in `sessions.json` and
started in the desktop's tmux. A running RiWork window for that project notices it
within its two-second refresh and adds a tab for it to its active pane, behind the
current tab: it takes no focus and does not change the selected tab, and the
terminal view attaches when the tab is first shown (in a pane the user emptied
there is no current tab, so it is shown there, still without focus). The same
happens for any terminal made with `riwork shell create` while a window is open. A
terminal the user closed in the window is not brought back. Of several windows that
show the same project, the first to notice takes it. If no window shows the
project, the terminal joins it when the project is next opened.

**`shell.close`** ends a project terminal and its process, as `riwork shell close`
does. Params `{"shell_id":"UUID"}` (unknown fields fail `invalid_request`). The id
must be a project terminal the desktop lists; an orchestrator is refused with
`invalid_request` and an unknown id is `not_found`. A terminal that already exited
can be closed too, which removes it from the desktop's list. Result
`{"shell_id":"UUID","status":"closed"}`. A resize override this connection holds on
the shell is released first. Errors are `invalid_request`, `not_found`, `cli_error`.
Closing kills whatever runs in the terminal, so a client confirms it with the user.
Any tab a desktop window has for the closed terminal is left to that window, as
after `riwork shell close` from a command line.

An older connector answers both methods `invalid_request` "unsupported RPC
method"; a client then hides or disables terminal creation for that connection. A
connector paired with an older `riwork` CLI still works: `shell create --json`,
`--harness`, `--unrestricted` and `shell close` predate this extension. Only the
desktop window's live tab for a new terminal needs the new app.

### Project creation extension (v1 and v2, 2026-10-01)

Additive and compatible, like the extensions before it: one new method and one new
error code. No change to the handshake, envelopes, fixtures or any existing method; it
applies to protocol v1 and v2 sessions alike. A client that never calls the method is
unaffected. The iOS side is built against this text.

**`project.create`** makes a new project on the desktop, as `riwork project create
--name NAME` does, and returns it. Params (an object; unknown fields, nulls and wrong
types fail `invalid_request` before anything runs):

```json
{"name":"My App"}
{"name":"Scratch","git":false}
```

- `name` (required string) is the project's name **and** the name of its folder. The
  phone never sends a path: the project is always made in the desktop's default projects
  folder (`~/Documents/riwork/NAME`), so a paired device cannot choose where on the
  desktop anything is written. A name is valid when
  - it is not empty and has no whitespace at either end (the CLI trims, so a name that
    differs from its trimmed form would not be the folder that was validated; the
    phone trims what the person typed before it sends);
  - it is at most 100 characters (Unicode scalar values, not grapheme clusters) and at
    most 255 UTF-8 bytes (a folder name on the desktop);
  - it contains no `char::is_control` character (NUL, tab, CR, LF, Escape, DEL, C1
    controls) and no U+2028 / U+2029;
  - it contains no `/` and no `\`, and does not start with `.` (so not `.` or `..`, and no
    hidden name such as `.git`, which the desktop reserves for tools);
  - it does not start with `-`, so that the CLI cannot read it as an option of its own.

  Everything else is legal as far as validation goes, including spaces, quotes, `;`,
  `$`, `~` and emoji. The name is text for a folder name and a project label: never
  interpreted, never part of a shell string.
- `git` (optional boolean, default `true`): make the new folder a Git repository
  (`git init`, no commit, Git's own default branch). `false` makes a plain folder. The
  phone sends `false` only when the person turned it off.

Result: `project_id` is the new project's UUID and `project` is its entry as
`projects.list` shows it (`id`, `name`, `root`, `created_at`; the same projection, so
nothing private about the project leaves the desktop):

```json
{"project_id":"UUID","project":{"id":"UUID","name":"My App","root":"/Users/me/Documents/riwork/My App","created_at":1790000000}}
```

The connector checks that the CLI made the project that was asked for (a canonical id,
the name that was sent, an absolute root whose folder has that name) and answers
`cli_error` if not.

Errors (`error.code`): `invalid_request` for any validation failure above (and
`unsupported RPC method` from a desktop that predates the method); `already_exists`
(new) when a project with that name exists (compared ignoring case, wherever that
project lives) or something is already at the folder's place (a folder, a file, a link,
or the folder of a registered project). The message is the connector's own and names no
path: `A project named "X" already exists on the desktop` or `A folder named "X" already
exists in the desktop's projects folder`. Nothing that was there is read, changed or made
a project, and no `git init` is run in it (plain `riwork project create`, which people
type, adopts a folder it finds; the phone's request never does). `not_found` for
"device revoked"; `cli_error` for everything else (the disk, Git, a permission, a
start that took longer than 60 seconds), with the CLI's first line as the message, and
for an installed `riwork` too old to create exclusively (`the installed riwork CLI cannot
create projects from the phone; update RiWork`).

Security and validation. Creation here is a smaller authority than a paired device has
already (`shell.input` runs any command as the desktop user), and it is still validated as
strictly as the other methods:

- Everything is checked before a CLI runs: object shape, unknown fields, types and the
  name rules above. The failures are `invalid_request`.
- The CLI is run with its argument vector built from validated values, one argument per
  value (`project create --name NAME [--no-git] --exclusive --json`). Nothing is
  concatenated into a shell string; the name is one argument after `--name`, and the
  rule against a leading `-` keeps it from being read as an option. `--json` is last.
- `--exclusive` makes the CLI refuse anything that exists instead of registering it. It is
  sent only to a CLI that says it knows the flag (`riwork capabilities --json` has
  `"project_create_exclusive":true`; asked before each creation), because an older CLI would read
  the flag as a PATH and create a folder of that name next to the connector.
- The folder itself is made with a plain `mkdir` (only its parent, the projects folder,
  is made as needed), so of two requests for one name exactly one wins; a creation
  that fails after the folder was made removes the folder (and the `.git` it just made)
  again, so a retry is not met by what it left behind.
- The device's authorization is checked when the request starts and again just before
  the CLI runs, so a device revoked while queued creates nothing.

Scheduling and retries. `project.create` runs in the ordered lane with `shell.keys`,
`shell.input`, `shell.resize`, `shell.resize.clear`, `shell.create` and `shell.close`: one
at a time per device, in arrival order, and not dropped half done when the phone's session
ends. The CLI makes the folder and only then writes the project down, so the connector runs it
in a task that outlives the request; when the connector ends the connection for any other
reason (revocation, a relay error) the CLI is not killed, only the answer is lost. (A start
that takes longer than 60 seconds is stopped, which needs a hung disk or Git.) Typing from the
same device waits behind it (well under a second for a normal folder). A client's timeout for
`project.create` should allow about 90 seconds.

Creation is **not idempotent**, but it is safe to ask again: a repeat finds the first and
answers `already_exists`. A client that loses the answer (a timeout, a lost connection)
cannot tell whether the project exists, and must not retry by itself; it refreshes
`projects.list` and lets the person decide (an `already_exists` after an unknown outcome
usually means the first request worked).

On the desktop. The project is written to the desktop's shared store, as by the CLI or the
app's own "New project". A running RiWork window re-reads the store every two seconds and lists
the new project in its Projects panel (when that is showing) on its next refresh, without
taking focus or switching that window's project. (Under the default "Last edited" sort a project that has no source file
yet follows those that have a date, as an empty project from the app does.)

An older connector answers `invalid_request` "unsupported RPC method"; a client then hides
project creation for that connection. A connector paired with an older `riwork` CLI answers
`cli_error` with the "update RiWork" sentence above, and creates nothing.

### Desktop terminal extension (v2 only, 2026-10-02)

Additive, and only for a second kind of device. The phone flow above shows a terminal as captured text and sends typing as `shell.keys`; a Mac running RiWork wants the real thing: its own Ghostty window showing this Mac's shell with every mode, mouse and scroll behaviour intact. So the host does not capture anything. It runs a **tmux client** (`riwork shell attach SHELL --exec`) inside a pseudo-terminal it owns and carries that terminal's raw bytes to the other Mac, where they are written to a Ghostty surface exactly as a local tmux client's would be. Redraws, the alternate screen, mouse and cursor modes, copy-mode scrollback, and the full repaint a freshly attached client gets all come from tmux. The shell itself lives on in its tmux server; closing a stream hangs a *client* up and nothing else.

**Who may.** Only a *desktop* device on a **protocol v2** session. The desktop's owner decides at pairing, and the device cannot change it:

```sh
riwork remote pair --relay wss://relay.example.com/v1/ws --protocol 2 --kind desktop --name 'Studio Mac' --out ...
```

`--kind` is `mobile` (the default, a phone) or `desktop`; `--kind desktop` with `--protocol 1` is refused, and `devices` prints each device's `kind`. The kind is a field of the host's own device record (`kind`, absent for a phone, so a config that holds only phones is byte for byte what it was); the pairing the other Mac imports does not carry it. A desktop has the same authority as a phone (full terminal control, see the introduction), is revoked the same way, and is limited to the same 64 devices. For a phone, a v1 device, or a connector from before this extension, every `pty.*` request is `invalid_request` "unsupported RPC method".

**`ready` announces it** to a desktop device and to nobody else, in `features` beside `deflate` and `history_max_lines`:

```json
"pty": {"max_streams": 8, "max_reads": 12, "max_write": 32768, "max_chunk": 65536}
```

A client opens streams only if `features.pty` is there, and holds itself to the numbers: `max_streams` open streams at once, `max_reads` parked `pty.read` requests at once (across all streams), `max_write` bytes per `pty.write` and `max_chunk` bytes per `pty.read` answer.

| Method | Exact params | Result |
| --- | --- | --- |
| `pty.open` | `{"shell_id":"UUID","columns":1..1000,"rows":1..500,"term":"xterm-ghostty"\|"xterm-256color","ignore_size":false}` (`ignore_size` optional) | `{"stream":"UUID","shell_id":"UUID"}` |
| `pty.read` | `{"stream":"UUID","wait_ms":0..25000}` (`wait_ms` optional, default 0) | `{"stream","seq":N,"data":"BASE64URL"}`, or `{"stream","seq":N,"eof":true,"reason":"exited\|closed\|limit"}` |
| `pty.write` | `{"stream":"UUID","seq":N,"data":"BASE64URL","gap_ms":0..1000}` (`gap_ms` optional, default 0) | `{"stream","seq":N,"status":"written"}` |
| `pty.resize` | `{"stream":"UUID","columns":1..1000,"rows":1..500}` | `{"stream","status":"resized"}` |
| `pty.close` | `{"stream":"UUID"}` | `{"stream","status":"closed"}` |

Every id is a full lowercase canonical UUID, `data` is base64url **without padding** in its one canonical spelling, every number is a JSON integer, and no other field is accepted (an unknown field, a `null`, a float or a number out of range is `invalid_request`, before anything is started).

**`pty.open`.** `term` is the terminal type the client wants the tmux client to announce. `xterm-ghostty` is what a Ghostty speaks natively; the host honours it only if it can describe it (RiWork's app bundle ships Ghostty's terminfo in `Contents/Resources/terminfo`, and the CLI also accepts the `TERMINFO` of a Ghostty that started it); otherwise the tmux client announces `xterm-256color`, which every system describes. `columns` and `rows` are the pseudo-terminal's size at start; `ignore_size` starts the client with tmux's `-f ignore-size`, so this display does not resize the window the shell has on the host. The host checks `riwork capabilities --json` for `shell_attach_exec` before it starts anything (an older CLI answers `cli_error` "the installed riwork CLI cannot open terminal streams; update RiWork"), then runs `riwork shell attach SHELL_ID --exec [--ignore-size]` with the same CLI path the other methods use. The CLI checks that the shell is registered and live, applies the usual scrolling configuration, and becomes the tmux client. **It answers when the terminal starts to draw** (a freshly attached tmux client begins with an escape sequence at once), so the client's first `pty.read` finds the repaint waiting. If the client instead prints plain text and ends, that text is the answer: the CLI's `unknown shell ID` is `not_found` "existing shell ID not found", its `shell ID has exited` and tmux's own "can't find session" / "no server running" / "no sessions" are `not_found` "selected shell is not alive", anything else is `cli_error` with its first line. (Plain text that does not end within 400 ms is taken for a screen.) At most `max_streams` streams exist per session; one more is `pty_limit`. It runs in its own lane (`Attach`, one at a time); a client that does not hear back within 15 seconds gets `cli_error` and the process is killed.

The client runs with an **allowlisted environment** and nothing else: `PATH`, `HOME`, `LANG` and `LC_*` (a locale that is not UTF-8 becomes `LANG=en_US.UTF-8`, because tmux decides from it whether a client speaks UTF-8), `RIWORK_HOME` and `RIWORK_RUNTIME_DIR` (so the CLI uses the connector's state), `TERM` as asked, `COLORTERM=truecolor`, and `TERMINFO` if the connector has one. `TMUX` and `TMUX_TMPDIR` are removed, and so is everything else, secrets included. The process is the leader of a session of its own with the pseudo-terminal as its controlling terminal.

**`pty.read`** returns the next bytes of the terminal's output, waiting up to `wait_ms` for there to be any, and collects for 3 ms after the first byte so that a burst travels as one answer, at most `max_chunk` bytes. `seq` is the number of bytes of this stream read before this answer: the offset of `data`. The stream's output is a byte sequence and `seq` names the position in it, so a client may keep **two reads parked** per stream and put the answers in order by `seq` (the connector answers requests in the order they finish, as always). Nothing within the wait is `{"stream","seq":N,"data":""}` with the offset to expect. `eof` ends it: `exited` (the client process ended on its own, for instance tmux detached it or the shell died; everything it said is read before this answer), `closed` (a `pty.close`, or the session ended, which discards what was unread), or `limit` (the host gave up on a stalled stream: output left unread for 30 seconds with a full 1 MiB buffer, or input that could not be written for 30 seconds). A terminal whose client has hung up refuses further input (`not_found`) while its last output is still read out, and then ends as `exited`. The end is not used up: asking again gives it again, and the stream stays (counting against `max_streams`) until it is closed. A `pty.read` parked on a stream that is closed returns `eof` `closed` at once. Reads run in their own lane (`Stream`): they are parked tasks, not processes, bounded by `max_reads`; one more waits its turn in arrival order. The host stops reading the terminal while 1 MiB of output is unread, which holds tmux's client back rather than losing bytes, and the relay closes a socket that queues more than 16 messages, so a client keeps its reads parked and does not ask for more than it can take.

**`pty.write`** types bytes into the terminal. `seq` is the client's running byte offset in the stream's input: the first write is `seq` 0, and each next one is the sum of the lengths of those before it. The host **rejects gaps and duplicates**: a `seq` below the offset it expects ("written already") or above it ("skips bytes") is `invalid_request`, names the offset it expects, and writes nothing; the offset does not move. The answer repeats the request's `seq`. It is given at once, by the connection loop, in arrival order with `pty.resize` and `pty.close` (they never wait, like `link.configure`), and means that the host has taken the bytes: they are written to the terminal in order by a task of the stream's own, so a stuck client holds up only its own stream. The host accepts up to 256 KiB, or 256 writes, that it has not yet written; beyond that the answer is `pty_limit` "terminal input is backed up", the offset does not move, and the client sends the same write again shortly (every later write it already sent fails as a gap, so it resumes from the first refused one).

**`gap_ms`** protects a Return from being read as part of a paste. Codex treats a Return that follows text by less than 150 ms as a newline inside the paste instead of "submit". The client knows how long it has been idle; before a write whose **first byte is a Return (CR)** the host waits `gap_ms` (at most 150 ms, whatever was asked, up to 1000) before writing, and other writes are never delayed. The wait is in the host's writer, not in the answer.

**`pty.resize`** sets the pseudo-terminal's size (`TIOCSWINSZ`); the tmux client gets its window-change signal and redraws. **`pty.close`** ends the stream: the client process is killed and reaped, parked reads on it return `eof` `closed`, and the stream is forgotten (a second close, or any request for it afterwards, is `not_found`). A write or resize to a stream that has ended on its own is `not_found`.

**Errors.** `invalid_request` (parameters, seq gaps and repeats), `not_found` (unknown stream, unknown or dead shell, a session that has ended), `cli_error` (the CLI could not start or is too old), and the new code **`pty_limit`**: too many streams, or too much input waiting. Everything else is the usual.

**Teardown.** A stream belongs to the *session* that opened it. When the session ends (the peer changes or goes offline, so the epoch changes; the device is revoked, within a second; or the connector stops or dies) every stream of it is closed and its client process killed, and an open that is still starting is cut short and its process killed. A connector killed without a chance to clean up closes its pseudo-terminals with the process, which hangs every client up. A new session starts with no streams; a client that reconnects opens them again (and tmux repaints). The shell is never touched by any of it.

**Security.** A desktop is as powerful as a phone: its streams type into any live shell. What is new is a process per stream, so: the argument vector is built by the host from validated values (one argument each, nothing through a shell); the environment is an allowlist; at most 8 streams, 12 parked reads, 32 KiB per write, 64 KiB per answer, 1 MiB of output and 256 KiB of input buffered per stream; streams exist only inside an authenticated v2 session of an unrevoked desktop device and die with it; and `pty.*` is not reachable by any other kind of device, which is checked where the session begins, not by trusting the request.

**Compatibility.** Nothing a phone or an older desktop sends or receives changes. An older connector answers `invalid_request` "unsupported RPC method" to `pty.*` and sends no `features.pty`, which is why a client reads `ready` first. The tests are `remote/tests/pty.rs` (a stand-in CLI whose `shell attach --exec` acts out a few lines of `sh`, through the RPC layer and through a real relay and connector) and the root crate's `tests/shell_attach_exec_cli.rs` (the CLI side, with a real tmux in a throwaway `RIWORK_HOME`).

## Fixtures and change log

`remote/fixtures/v1.json` supplies deterministic PSK, UUIDs, nonces, proof MACs,
transcript, salt, directional keys, session ID, nonce, AAD and encrypted request/
ready response. Values are test-only and must never provision production devices.

- 2026-09-27: v1 frozen before initial implementation.
- 2026-09-27 follow-up, task ae291d60-f96a-4ffe-af40-a81a8fde9a51: user-authorized
  additive `shell.resize` / `shell.resize.clear` extension, validated ranges,
  connection ownership, restoration and errors published before implementation.
  Existing routing/crypto/RPC fields and fixture bytes are unchanged.
- 2026-09-28: `grok` was added to the Session `harness` values
  (`codex|claude|grok|null`) by commit 02c4054 without a note; recorded here.
  Additive: the client `harness` field is a free-form string, so no client change.
- 2026-09-28, remote hardening (routing behavior and RPC error handling; handshake,
  envelope, fixture bytes and method params are unchanged). Needs the iOS worker's
  agreement; checked against `ios/Core/RelayClient.swift`, which registers
  immediately after `resume()`, polls output every 3 seconds and lets URLSession
  answer pings, so no client change is required:
  registration window 10 s -> 3 s; relay pings ~20 s and closes sockets silent for
  60 s; a silent (30 s) registration is replaced by a same-credential registration
  instead of being rejected as a duplicate; only authenticated sockets count toward
  the 256 limit, and a full relay now closes after registration instead of failing
  the HTTP upgrade with 503; a request without a usable `id` gets
  `invalid_request` with `"id":null` instead of ending the session; CLI output over
  128 KiB returns `response_too_large` instead of `cli_error`; `shell.resize.clear`
  on an unknown/dead shell the connection does not hold returns `not_found`
  instead of `cleared`; the viewport lease is renewed only within 20 seconds of
  authenticated phone traffic. The API-level wording in the introduction now states
  that `shell.input` amounts to arbitrary command execution (documentation only).

- 2026-09-30: additive and backward compatible. `shell.keys` (ordered batch of
  literal text and named keys, exactly-once per device by batch UUID with a separate
  4096-entry write-ahead ledger, `duplicate`/`uncertain` results, new error code
  `input_unavailable`) and the `cursor`, `rows`, `cols`, `in_mode` fields on the
  `shell.output` result, whose last `rows` lines are the visible screen. A client
  that ignores both is unaffected; `shell.input` and its ledger are untouched. Needs
  the iOS worker's agreement; the iOS side implements the same text.

- 2026-09-30: additive and backward compatible. `appearance.get` (read-only, params
  `{}`) returns the desktop's published palette and terminal colors (`v`,
  `updated_at`, `dark`, `palette`, optional `terminal`, lowercase `#rrggbb`), or
  `not_found` "appearance not published". The desktop GUI writes `appearance.json` in
  `RIWORK_HOME`; `riwork appearance [--json]` reads it. A client that never calls the
  method is unaffected, and an older desktop answers `invalid_request` "unsupported
  RPC method". Needs the iOS worker's agreement; the iOS side implements the same
  text.

- 2026-09-30: additive and backward compatible. Live terminal sync. `shell.output`
  accepts `styled` (SGR sequences kept, every other escape and control sequence
  removed), `if_changed` (a previous `hash`) and `wait_ms` (0 to 10000, wait for a
  change inside one desktop process, capturing about every 80 ms); its result gains
  `hash`, or is `{"shell_id","unchanged":true,"hash"}` when nothing changed within
  the wait. A device's requests are carried out concurrently (at most 4 in flight;
  typing and resizing one at a time in arrival order; at most 2 waits), responses
  may arrive out of order and are matched by `id`, and a pending wait ends when the
  connection closes or the device is revoked. A client that sends none of the new
  parameters and awaits each response before the next request is unaffected. Needs
  the iOS worker's agreement; the iOS side implements the same text.

- 2026-09-30: additive and backward compatible. Deep scrollback. `shell.history`
  (`shell_id`, `end`, `lines` 1 to 1000, optional `styled`) returns a page of the
  scrollback above the screen: the lines from `end + lines` to `end + 1` above it,
  clamped at the top, as `output` (lines joined by `\n`, no newline after the last),
  `line_count`, `history_size` and `complete` (the page reached the top; empty and
  complete when `end >= history_size`); it never waits and runs in the shared lanes.
  The `shell.output` result, and its `unchanged` form, gain `history_size` (scrollback
  lines above the screen) and `alternate` (a full-screen program is on the alternate
  screen), both covered by the `hash`, so the hashes handed out before the desktop
  is updated stop matching (the client is sent the full answer once). Needs the iOS
  worker's agreement; the iOS side implements the same text.

- 2026-10-01: additive and backward compatible. Link extension: `server_ms` on every response
  (the connector's time from decrypting a request to having its reply ready); `features`
  (`deflate`, `history_max_lines`) in `ready`; `link.configure` (`compression` `deflate` or
  `none`) answered by the connection itself; replies of 2 KiB and more may be sent as
  `0x01 || u32 inflated length || raw deflate` of their JSON inside the ciphertext once the
  session opted in; such a session may have replies of up to 2 MiB of JSON that fit one frame
  deflated; `shell.history` `lines` up to 5000. An older phone is not affected: it neither
  asks nor reads the new fields. Needs the iOS worker's agreement; the iOS side implements the
  same text. Security note (CRIME/BREACH) in the section.
- 2026-10-01: additive and backward compatible. Terminal creation. `shell.create`
  (`project_id` or `worktree_id`, `kind` `shell|codex|claude|grok`, optional
  `unrestricted` for the agents, optional `command` for a plain shell) starts a
  terminal and returns its id and `shells.list` entry; `shell.close` ends a project
  terminal. Both run in the ordered lane and are not cut short when the phone's session ends (a creation's CLI also survives the connection being torn down), and creation is not
  idempotent (a client must not retry it by itself). New error code
  `harness_unavailable`. Parameters are validated before any CLI runs and the CLI is
  run with an argument vector, never a shell string. `unrestricted` defaults to
  `false`, as in the desktop's New Tab menu, which offers it as separate entries. The
  desktop app adds a tab for any terminal created while it is open, without taking
  focus. A client that never calls the methods is unaffected, and an older desktop
  answers `invalid_request` "unsupported RPC method". Needs the iOS worker's
  agreement; the iOS side implements the same text.

- 2026-10-01: additive and backward compatible. Project creation. `project.create`
  (`name`, optional `git`, default `true`) makes a new project in the desktop's default
  projects folder and returns its id and `projects.list` entry; the phone never sends a
  path. The name is one folder name (at most 100 characters and 255 bytes, no control
  characters, no `/` or `\`, not starting with `.` or `-`, no whitespace at either end).
  It runs in the ordered lane and is not cut short when the phone's session ends (its CLI also
  survives the connection being torn down), and creation is not idempotent: a repeat, or a
  name or folder that already exists, answers the new error code `already_exists`
  without touching what is there. Parameters are validated before any CLI runs and the CLI is
  run with an argument vector, never a shell string, and only with the `--exclusive` flag it
  announces in `riwork capabilities`. A running desktop window lists the project within its
  two-second refresh. A client that never calls the method is unaffected, and an older
  desktop answers `invalid_request` "unsupported RPC method". Needs the iOS worker's
  agreement; the iOS side implements the same text.

References: [RFC 8439](https://www.rfc-editor.org/rfc/rfc8439),
[RFC 5869](https://www.rfc-editor.org/rfc/rfc5869),
[CryptoKit ChaChaPoly](https://developer.apple.com/documentation/cryptokit/chachapoly),
[RustCrypto ChaCha20Poly1305](https://docs.rs/chacha20poly1305/0.10.1/chacha20poly1305/),
[RustCrypto HKDF](https://docs.rs/hkdf/0.12.4/hkdf/).

Viewport reference: [official tmux manual](https://raw.githubusercontent.com/tmux/tmux/master/tmux.1).
