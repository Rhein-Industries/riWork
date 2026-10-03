# RiWork remote protocol v2

Dated 2026-09-29. v1 in [remote-protocol.md](remote-protocol.md) stays frozen.
`riwork remote pair` still writes a v1 device. v2 is `pair --protocol 2`.
A phone moves to v2 by revoking the old device and pairing again. v1 and v2
devices share `devices.json` and the same relay. The relay registration window,
ping, duplicate-socket and route-hash rules are unchanged.

## What v2 changes

The invite is single-use and expires. Redeeming it derives a root key, deletes
the invite secret, and stores only the root. Each connection then does an
X25519 handshake so a stolen root does not decrypt a finished session.

The relay still forwards endpoint text frames unchanged and does not learn
plaintext, the invite secret, the root key, or ephemeral private keys. Routing
tokens are still presented at registration. They authorize a route, not an RPC.

Inner RPC JSON stays `v:1`, including `ready`. The envelope `v` is the crypto
version. `shell.input` is still arbitrary command execution as the desktop user.

## Pairing record

v2 JSON omits `pairing_secret`. Pending:

```json
{"v":2,"relay_url":"wss://relay.example.com/v1/ws","desktop_id":"UUID","device_id":"UUID","route_id":"UUID","device_name":"My iPhone","relay_token":"BASE64URL_32","invite_id":"UUID","invite_secret":"BASE64URL_32","expires_at":1893456000,"invite_state":"pending"}
```

`expires_at` is Unix seconds. Lifetime at mint is 30 to 3600 seconds (default
600). The deep link is `riwork://pair?v=2&data=BASE64URL_UTF8_JSON`. The link
version and the JSON `v` must match. After redeem the record is `established`,
`invite_secret` is absent, and `root_key` is the 32-byte root. `expired` keeps
neither secret. A v1 record must not contain invite fields.

The iOS `connect` call returns the established pairing. The app persists that
record, with the invite secret gone, before it issues RPCs. The original export
file is not rewritten. Delete it: it still holds the invite secret and the
relay token. Replaying the secret fails once the desktop has consumed the
invite, but the token can still occupy the route.

## Invite transcript

`I` is the same 48 UUID bytes as v1. `invite` is the raw invite UUID.
`exp` is `expires_at` as 8-byte unsigned big-endian. The relay URL is its
UTF-8 bytes, at most 2048, with a 2-byte unsigned big-endian length in front.

```
prefix = I || invite || exp || len(url) || url
P      = "riwork/v2/pair\0" || prefix || C || D
root   = HKDF-SHA256(salt=SHA256(P), ikm=invite_secret, info="riwork/v2/root")
```

`C` and `D` are fresh 32-byte nonces. HKDF info strings have no trailing NUL.
MAC labels do. Base64 is URL-safe without padding. MAC comparison is
constant-time.

1. Phone sends `pair_hello` (`v:2`): invite id, the three identity UUIDs, `C`,
   and `HMAC(invite_secret, "riwork/v2/pair-hello\0" || prefix || C)`.
2. Desktop checks the stored record, expiry, and MAC. On success it derives
   `root`, deletes `invite_secret`, saves `established`, then sends
   `pair_accept`: `D` and `HMAC(root, "riwork/v2/pair-accept\0" || P)`.
3. Phone checks that MAC, deletes its invite secret, and sends `pair_finish`:
   `HMAC(root, "riwork/v2/pair-finish\0" || P)`.

The save happens before `pair_accept` is sent. A crash after the save and
before the phone sees the accept consumes the invite. Revoke that device and
pair again. A bad MAC returns `invite_rejected` and leaves the invite pending.

Failures are `{"v":2,"type":"pair_error","error":CODE}` and then the desktop
closes. Codes, and nothing else, are:

| Code | Meaning |
| --- | --- |
| `invite_expired` | `now >= expires_at`. The secret is wiped first, including when a clock later moves backward. |
| `invite_replay` | The invite is already established. |
| `invite_race` | Another redeem of this invite is in the middle of the check on this process. The invite stays pending. |
| `invite_rejected` | Unknown invite, revoked device, or bad MAC. A bad MAC does not consume the invite. |
| `invite_malformed` | The stored v2 record is not a usable pending or terminal state. |

Expiry is decided before the in-process claim, so an expired invite is
`invite_expired` rather than `invite_race`. The claim is process-local. Two
processes serialize on `config.lock`; the loser sees `invite_replay` after the
winner's save. One invite produces one root.

## Session

Every connection, including the one that just finished pairing, runs a new
handshake. Later connections send only this handshake. The root is the MAC key.
Scalars are clamped as in RFC 7748. An all-zero Diffie-Hellman output is
rejected.

```
T   = "riwork/v2/session\0" || I || Ce || De
dh  = X25519(own scalar, peer public)
ikm = root || dh
salt = SHA256(T)
```

HKDF-SHA256 expands `ikm` with that salt to `riwork/v2/c2d`, `riwork/v2/d2c`
and `riwork/v2/hs`. Session id `S` is the first 16 bytes of `salt`.

1. Phone sends `client_hello` (`v:2`): the three UUIDs, `Ce`, and
   `HMAC(root, "riwork/v2/client-hello\0" || I || Ce)`.
2. Desktop sends `server_hello`: `De` and
   `HMAC(hs, "riwork/v2/server-hello\0" || T)`.
3. Phone sends `client_finish`: `HMAC(hs, "riwork/v2/client-finish\0" || T)`.
4. Desktop seals `ready` (`v:1` inside a `v:2` envelope).

A v1 `client_hello` to a v2 device, or a v2 hello to a v1 device, is
`unsupported version` and does not mint keys. Peer control messages stay `v:1`.

Frames match v1 except the label and the envelope version. AAD is
`"riwork/v2/frame\0" || S || direction byte || counter` as 8-byte unsigned
big-endian. The nonce is four zero bytes followed by that counter. Counters,
limits, replay rejection and the RPC bodies are the v1 rules.

## Threat model

Assets are the invite secret, the root key, ephemeral scalars, terminal
plaintext, and the two relay tokens. The desktop user is the trust boundary:
anyone who can run `pair` can mint a device. Pairing is full terminal control.

The phone and the desktop are the only parties that should see plaintext.
The relay is an untrusted forwarder that also checks route tokens. On `wss`
it sees route ids, roles, the registration token, public keys, MACs,
ciphertext, sizes and timing. It does not see plaintext, the invite secret,
the root, or ephemeral private keys, and it does not log payloads. A stolen
relay token can occupy or deny the route. It cannot open an RPC or decrypt.

The invite link is a bearer token until it is redeemed. Whoever completes the
exchange first gets the root. Expiry, a second presentation, and an
overlapping redeem do not create a second root. A wrong proof does not burn
a still-valid invite. After redeem, the root stays on the desktop and in the
phone's keychain. The export file on disk is not wiped by the protocol.

A recorded v2 session stays confidential if the root leaks later and the
ephemeral scalars are gone. The same stolen root can still impersonate either
side in a future handshake: there is no separate identity signature and no
root ratchet. v1 recordings remain readable to anyone who has that device's
PSK. Revocation stops new handshakes; it does not hide traffic already taken
by the relay.

Ephemeral private copies are wiped after the shared secret is mixed in.
Library copies inside the X25519 implementation may outlive that wipe.
`shell.input` is unchanged and is not a cryptographic boundary. The 2026-09-30 direct typing
extension (`shell.keys` and the screen fields of `shell.output`, specified in
[remote-protocol.md](remote-protocol.md)) is independent of the crypto version and
applies to v2 sessions exactly as to v1. So does the 2026-09-30 theme sync extension
(`appearance.get`, read-only, in the same document), and the 2026-09-30 live terminal
extension (`styled`, `if_changed`, `wait_ms` and `hash` on `shell.output`, and the
concurrent handling of a device's requests, in the same document), and the
2026-09-30 deep scrollback extension (`shell.history`, and `history_size` and
`alternate` on `shell.output`, in the same document), and the 2026-10-01 link extension
(`server_ms` on every response, `features` on `ready`, `link.configure`, and deflated
reply frames, in the same document). The deflate marker sits inside the ciphertext, so
the v2 envelope, its AAD and the fixtures are untouched; the compression note in the
threat discussion of that section applies to v2 as it does to v1. It also includes
the 2026-10-01 terminal creation extension (`shell.create` and `shell.close`, in the same document). A
paired device could already start any command by typing it into a shell; creation
adds no authority, but it is still pairing's "full terminal control". The same day's project
creation extension (`project.create` and the error code `already_exists`, in the same
document) is included too: it is independent of the crypto version and applies to v2
sessions exactly as to v1. It makes a folder named by the phone, always in the desktop's default
projects folder (the phone never sends a path), and never touches a folder or project that
exists; like terminal creation it adds no authority a paired device lacks.
The 2026-10-03 activity and recency extension (optional `last_edited_unix`,
`last_activity_unix` and `agents` on `projects.list` entries, and `last_activity_unix`,
`activity`, `activity_since_unix`, `subagents_working` and `subagent_kinds` on `shells.list`
and `orchestrators.list` entries, in the same document) is likewise independent of the crypto
version and applies to v2 sessions exactly as to v1. It is read-only, adds no method and
discloses only the state of agents, and the time of their last output, that the device can
already see and type into.

## Desktop devices (2026-10-02)

A paired device is either a phone (the default) or a *desktop*: another Mac
running RiWork that shows this Mac's shells as terminals. `riwork remote pair
--protocol 2 --kind desktop` mints the invite for a desktop; `--kind desktop`
with `--protocol 1` is refused, because the extra capability (terminal streams,
`pty.*` in [remote-protocol.md](remote-protocol.md), "Desktop terminal
extension") is offered only on a forward-secret session, never on a long-lived
PSK. The kind is a field of the host's own device record (`kind`, absent for a
phone, so an old `devices.json` is read and written back unchanged); the
pairing record, the invite, the handshake, the transcript and the vectors are
the same bytes as for a phone. The desktop redeems its invite the same way, and
keeps the established record as a phone does. Authority, revocation and the 64
device limit are unchanged: a desktop can type into any live shell, as a phone
can, and revoking it ends its streams within a second. An older connector that
reads a `devices.json` holding a desktop device refuses the file (the record has
a field it does not know) instead of treating the desktop as a phone.

## Vectors and tests

`remote/fixtures/generate.py` writes `remote/fixtures/v2.json` and
`ios/Tests/Fixtures/v2.json` with the `cryptography` library, not the Rust or
Swift implementations. Rust, the iOS package tests, and
`swift remote/fixtures/verify.swift remote/fixtures/v2.json` all recompute
those bytes. Isolated continuation uses a temporary `RIWORK_HOME` only.
The 2026-09-29 command log and pass counts are in
[remote-protocol-v2-verification.md](remote-protocol-v2-verification.md).
