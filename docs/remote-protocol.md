# RiWork remote protocol v1 — FROZEN (2026-09-27)

Contract owner: `feature/encrypted-relay`. This is the interoperability contract for
`ios/`. No silent wire changes. Changes require an explicit dated note below and
agreement with the iOS worker. v1 supports existing sessions only; it never creates
projects, shells, workers, schedules or tasks. All JSON is UTF-8; object key order
is immaterial. Integers below are JSON numbers except frame counters (strings).
Base64 is URL-safe, **without padding**. UUIDs are full lowercase canonical UUIDs.

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
First WebSocket **text** message within 10 seconds:

```json
{"v":1,"type":"register","route_id":"UUID","role":"mobile","token":"BASE64URL_32_BYTES"}
```

`role` is `desktop` or `mobile`. Server responds
`{"v":1,"type":"registered","peer_online":false}`. Later peer changes are
`{"v":1,"type":"peer","online":true}` / `false`. One socket per role per route;
a duplicate is rejected, not allowed to replace a live socket. Unauthorized,
malformed, unavailable peer, full queue, oversized/binary frames close the socket.
No relay buffering across disconnects. On peer loss discard handshake/session
keys and counters. On peer availability mobile starts a fresh handshake. Relay
forwards each endpoint text frame **unchanged**, one message in/one message out,
without wrapping it. Transport ping/pong is allowed and has no protocol meaning.
Relay controls are not endpoint authenticated and must never authorize an RPC.

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

Limits: text WS message <=262144 bytes, decrypted JSON <=131072 bytes, shell line
<=8192 UTF-8 bytes, output lines 1..2000 (default 200), handshake <=10 seconds.
Relay defaults: <=256 sockets total, <=128 configured routes, outgoing queue <=16
messages/socket, no payload logging. RPC processing is serial per device.

## Encrypted RPC JSON

Request: `{"v":1,"type":"request","id":"UUID","method":"projects.list","params":{}}`.
Success: `{"v":1,"type":"response","id":"SAME_UUID","ok":true,"result":{...}}`.
Error: `{"v":1,"type":"response","id":"SAME_UUID","ok":false,"error":{"code":"invalid_request","message":"Human readable"}}`.
Unknown methods/fields, malformed UUIDs/params fail `invalid_request`. Responses
are correlated by UUID; no unsolicited response except handshake `ready`.

| Method | Exact params | Result |
| --- | --- | --- |
| `projects.list` | `{}` | `{"projects":[Project]}` |
| `worktrees.list` | `{"project_id":"UUID"}` | `{"worktrees":[Worktree]}` |
| `tasks.list` | `{"project_id":"UUID"}` optionally `"worktree_id":"UUID"` | `{"tasks":[Task]}` |
| `shells.list` | `{"project_id":"UUID"}` | `{"shells":[Session]}` (existing project shells) |
| `orchestrators.list` | `{}` | `{"orchestrators":[Session]}` (global + project) |
| `shell.output` | `{"shell_id":"UUID"}` optionally `"lines":200` | `{"shell_id":"UUID","output":"terminal text"}` |
| `shell.input` | `{"shell_id":"UUID","line":"one physical line"}` | `{"shell_id":"UUID","status":"sent"}` |

Project fields: `id,name,root` strings; `created_at` Unix seconds number.
Worktree: `id,project_id,branch,path` strings; `is_primary` boolean; `created_at`.
Task: `id,project_id,title,details` strings; `status` `todo|in_progress|done`;
`worktree_id` UUID or null; `created_at,updated_at` Unix seconds.
Session: `id` UUID, `project_id,worktree_id` UUID or null, `kind` `project|orchestrator`,
`cwd` string, `harness` `codex|claude|null`, `alive` boolean, `created_at_unix` number.
Clients tolerate additive result/entity fields but must reject unknown protocol
versions. Lists expose existing CLI entities; output/input resolve a full shell
UUID against existing project shells **and** orchestrators. Dead/missing sessions
fail clearly. No project default, command construction from arbitrary CLI text,
creation, close, tmux attachment or free-form CLI RPC. `shell.input` intentionally
submits terminal input followed by Return and can run commands in the selected
shell; clients must show the selected shell before sending. CR, LF, NUL and other
Unicode control characters are forbidden. v1 supports line submission only (no
terminal action method). Use CLI argv directly, never an intermediate shell.

Input deduplication: UUIDs are unique per device and logical operation. Desktop
persists (device ID, request ID, canonical request, state/result) **before** sending
input and retains it across reconnect/restart. Retry same UUID + same request
returns cached response; changed contents fail `request_conflict`. Pending/uncertain
outcome returns `outcome_unknown`, never re-sends automatically. CLI failures after
attempting submission also return `outcome_unknown`. Clients must retain pending
UUID/line and warn on unknown outcomes, never generate a fresh UUID to auto-retry.
No eviction: max 4096 recorded inputs/device; capacity returns `cache_full` before
sending. Re-pair with a new device after reviewing old outcomes to reset capacity.
Reads may repeat safely. Errors also include `not_found`, `cli_error`,
`response_too_large`. Error messages are diagnostic, not machine enums beyond code.

Revocation: local `revoke DEVICE_UUID` removes/marks the device revoked in the
protected desktop config. Running connector notices within 1 second, closes its
socket, discards keys and refuses further requests. No in-flight command can be
undone. Remove the relay route and restart relay to invalidate its routing tokens;
endpoint revocation alone already denies endpoint access. A transport dying must
never kill/recreate/close a tmux or harness session.

## Fixtures and change log

`remote/fixtures/v1.json` supplies deterministic PSK, UUIDs, nonces, proof MACs,
transcript, salt, directional keys, session ID, nonce, AAD and encrypted request/
ready response. Values are test-only and must never provision production devices.

- 2026-09-27: v1 frozen before implementation. No wire changes yet.

References: [RFC 8439](https://www.rfc-editor.org/rfc/rfc8439),
[RFC 5869](https://www.rfc-editor.org/rfc/rfc5869),
[CryptoKit ChaChaPoly](https://developer.apple.com/documentation/cryptokit/chachapoly),
[RustCrypto ChaCha20Poly1305](https://docs.rs/chacha20poly1305/0.10.1/chacha20poly1305/),
[RustCrypto HKDF](https://docs.rs/hkdf/0.12.4/hkdf/).
