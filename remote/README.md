# RiWork encrypted remote access

This standalone Rust crate builds the blind relay and the outbound desktop
connector without GPUI/Ghostty native dependencies. The mobile client uses the
[frozen v1 protocol](../docs/remote-protocol.md) and shared fixtures. Desktop
session content is encrypted end to end with a random per-device pairing secret;
relay authentication uses independent role tokens.

## Build

```sh
cargo build --locked --release --manifest-path remote/Cargo.toml
```

Use `remote/target/release/riwork-remote` directly. With a rebuilt desktop CLI,
`riwork remote ...` forwards arguments unchanged to a sibling `riwork-remote`,
the companion inside an adjacent `RiWork.app`, or `RIWORK_REMOTE_BIN`. PATH
symlinks to `target/{debug,release}/riwork` resolve that packaged companion too:

```sh
export RIWORK_REMOTE_BIN="$PWD/remote/target/release/riwork-remote"
riwork remote --help
```

`riwork update` builds the companion from `remote/Cargo.toml` into its disposable
staging directory, requires it in the bundle, then installs the executable/app
atomically. A companion build or validation failure keeps the previous install.
The direct macOS bundling script includes the standalone binary when built in
`remote/target/{debug,release}` before bundling. Existing installed desktop CLIs
can use the standalone binary directly until the orchestrator installs a new
build. No GUI window is needed for the connector. `RIWORK_HOME` is inherited by
all CLI subprocesses and is never changed by forwarding/start.

## Pair, provision and start

Run these from the desktop as its normal RiWork user. Pairing grants access to
all existing projects and sessions in that desktop's `RIWORK_HOME`, and because
`shell.input` types into any live shell, unrestricted harness or editor tab, a
paired phone can run arbitrary commands as you. Each phone
gets an independent device UUID, PSK, route UUID and role tokens. Pair once per
phone; do not share a pairing export between devices.

```sh
RIWORK_PAIR_DIR=$(mktemp -d "${TMPDIR:-/tmp}/riwork-pair.XXXXXX")
riwork remote pair --relay wss://relay.example.com/v1/ws --name 'My iPhone' \
  --out "$RIWORK_PAIR_DIR/phone.pairing.json" \
  --relay-routes "$RIWORK_PAIR_DIR/relay-routes.json" --show-link
```

Import `phone.pairing.json` in the iOS app or open/scan the printed
`riwork://pair?v=1&data=...` link. The export/link contains secrets; share it only
with that phone and remove the export after import. `--protocol 2` mints a
single-use invite (`riwork://pair?v=2&data=...`) that expires (`--ttl-seconds`,
30 to 3600, default 600) and is replaced by a forward-secret session root. The
default remains protocol 1, and an existing v1 device is left as-is. The v2
contract and threat model are in [remote-protocol-v2.md](../docs/remote-protocol-v2.md).
Omitting `--show-link` avoids
printing secrets to terminal scrollback. Both JSON files are created with mode
600; connector state is in `$RIWORK_HOME/remote` (or the default data directory's
`remote`) with mode 700. Existing files are never overwritten for pairing exports.
`--relay-routes` appends the new route to a mode-600 provisioning manifest; keep a
single manifest when adding devices so existing routes are preserved. `pair` is
all-or-nothing: if it fails, the export and the route it added are removed, so a
retry does not leave orphan routes.

Copy **only `relay-routes.json`** to the relay operator's protected config and
start/restart the relay with that manifest. It contains hashes of relay tokens,
never the endpoint PSK or session content. [TLS deployment](../docs/remote-deployment.md)
uses a loopback relay behind Caddy. The desktop connector installs rustls's ring
CryptoProvider and checks that certificate against the operating-system root store.

```sh
riwork-remote relay --routes /etc/riwork-relay/routes.json --bind 127.0.0.1:8787
riwork remote start --riwork /absolute/path/to/RiWork.app/Contents/MacOS/riwork
riwork remote devices
riwork remote revoke DEVICE_UUID
```

`start` is a foreground process with automatic reconnects; leave it running while
remote access is needed. If forwarded through `riwork remote`, `--riwork` normally
can be omitted because forwarding supplies `RIWORK_CLI`. The connector logs to
stderr when it starts serving a device, when a device authenticates (flagging its
first time) and why a connection dropped; `devices` prints, per device, JSON
including `paired_at_unix`, `first_authenticated_unix` and `last_authenticated_unix`
(Unix seconds, `null` when unknown). Stopping the connector,
relay or mobile client does not terminate tmux or a coding harness. Output is
captured from existing tmux panes. Mobile requests select full shell UUIDs;
project/worktree/task list requests use explicit full project UUIDs.

Mobile project tabs select existing terminals by UUID, one terminal per tab.
`shell.resize` pins the tmux grid to validated mobile cell counts;
`shell.resize.clear` restores sizing without clearing content or sending input.
One connection owns one override; another owner gets `viewport_busy`. Peer loss,
revoke and shutdown clear sizing, with a separate 12-second crash lease as fallback.
The connector renews the lease every three seconds, but only while it has heard an
authenticated request from the phone in the last 20 seconds (the iOS app polls every
three seconds), so a phone that vanished stops pinning the terminal. This affects
the grid rendered by Ghostty, not the physical macOS window geometry. The CLI
commands are:

```sh
riwork shell resize SHELL_UUID --columns 43 --rows 17 --owner DEVICE_UUID --lease CONNECTION_UUID --json
riwork shell resize-clear SHELL_UUID --owner DEVICE_UUID --lease CONNECTION_UUID --json
```

The connector supplies owner/connection UUIDs internally; mobile RPCs never supply
them. Local CLI callers must retain their same UUIDs to clear the override, or
allow its unrenewed lease to expire. Do not use these commands on an unrelated
terminal. Input uses a per-shell lock for the entire bracketed paste, 500-ms
settling interval and single Return. It queues paired devices and local CLI sends
so their text/Return cannot interleave. The delay exceeds the
[Codex composer's paste suppression window](https://github.com/openai/codex/blob/main/codex-rs/tui/src/bottom_pane/paste_burst.rs).
Empty text sends exactly one Return without a paste buffer or settling delay,
under the same shell lock and input-mode checks.
`sent` confirms terminal submission, not a model/server acknowledgment. No second
Return is sent automatically if an outcome is uncertain.

`shell.keys` types into the shell as it happens, without a line or a Return of
its own: an ordered batch of literal `text` and named `key` items (the contract is
in [remote-protocol.md](../docs/remote-protocol.md)), delivered by
`riwork shell keys SHELL_UUID -- t:TEXT k:KEY...` under the same per-shell lock as
`shell.input`. Each batch UUID is recorded in a write-ahead ledger of the 4096 most
recent batches per device (`keys-DEVICE_UUID.json`, mode 600, separate from the
input outcomes), so a retry with a new request ID answers `duplicate` or
`uncertain` instead of typing twice. `shell.output` also reports the cursor, pane
size and copy-mode state that place the phone's cursor. Like `shell.input`, it
is arbitrary command execution as the desktop user.

`shell.output` can also follow a terminal live (the contract is in
[remote-protocol.md](../docs/remote-protocol.md)). `styled` keeps the colors and
attributes as SGR sequences and removes every other escape (the CLI filters, and the
connector refuses to pass on anything else). `if_changed` with a `hash` from an
earlier result and `wait_ms` (0 to 10000) makes the one `riwork shell output ...
--if-changed=HASH --wait-ms N` process capture again when tmux says the pane changed
(about every 80 ms where it cannot say) and answer `{"shell_id","unchanged":true,"hash"}` if nothing changed in time. That
call may take ten seconds, so the connector no longer handles a device's requests one
at a time: `lanes.rs` lets one ordered request (`shell.keys`, `shell.input`,
`shell.resize`, `shell.resize.clear`, `shell.create`, `shell.close`, `project.create`, `orchestrator.create`, `chat.create`, `chat.command`, `chat.stop`, `shell.paste`, in arrival order) and three others run at once,
at most two of them waits (a `shell.output` with `if_changed` and `wait_ms`, or a `chat.events` with `wait_ms` above 0), queues the rest in arrival order, and the connection loop
alone seals and sends the responses (out of order by request, in order by counter). A
wait ends, and its CLI process is killed, when the connection closes, the phone goes
offline or the device is revoked (checked every 250 ms while requests are pending).

`shell.history` reads scrollback above the screen a page at a time (the contract is in
[remote-protocol.md](../docs/remote-protocol.md)): `end` lines skipped above the
screen, then `lines` (1 to 5000; 1000 until 2026-10-01) older ones, optionally `styled`, answered as
`{"shell_id","output","line_count","history_size","complete"}` from
`riwork shell history SHELL_UUID --end N --lines M [--styled] --json`. The connector
validates the request before anything runs, checks the shell like `shell.output`
(`not_found` for an unknown or dead one) and refuses a CLI page that does not match
what it asked for, or whose styled text holds anything but SGR. It never waits, so
`lanes.rs` counts it with the other reads: one of the three shared slots, never the
ordered slot of typing and resizing and never a wait slot. A page too big for one
response is `response_too_large` and the phone asks for fewer lines. `shell.output`
also passes on `history_size` and `alternate` from the CLI, in its `unchanged` answer
too.

The link extension (the contract is in [remote-protocol.md](../docs/remote-protocol.md), "Link
extension") is in `src/link.rs` and the sealing path of `connector.rs`. Every response gains
`server_ms`, the time from decrypting the request to having its reply ready. `ready` announces
`features` (`deflate`, `history_max_lines`). `link.configure` is answered by the connection loop
itself, in arrival order, because it changes the connection and not the desktop. After `deflate`,
a reply of 2 KiB or more is sent as `0x01 || u32 length || raw deflate` (level 6, `flate2`'s Rust
backend) when that is smaller, with the closing `server_ms` field deflated after the body so the
number includes the compression; bodies over 128 KiB are deflated on a blocking thread. The CLI
may then write up to 2 MiB to the pipe and `Rpc::handle_shared_up_to` checks the response against
the session's limit; a reply that does not fit one frame even deflated is replaced by
`response_too_large`. `shell.history` pages of up to 5000 lines are accepted; the first time the installed CLI refuses a page for being too long (`--lines needs an integer from 1 to N`, as builds from before 2026-10-01 do at 1000) the connector remembers N, refuses longer pages itself and announces N in later `ready` frames. `tests/link.rs` runs a real relay and the real connector binary against a
stand-in CLI; `fixtures/link.json` (see `fixtures/generate_link.py`) is shared with the iOS tests.

`appearance.get` returns the colors the desktop published, so the phone can match its
theme (the contract is in [remote-protocol.md](../docs/remote-protocol.md)). It runs
`riwork appearance --json` (no shell selection, no ledger), re-validates the output
(version 1, lowercase `#rrggbb` colors, exactly 16 terminal colors, the optional
`native` and `mic` flags as booleans and passed on only when true, at most 16 KiB)
and answers `not_found` "appearance not published" when the desktop app has not
published a usable `appearance.json` yet. Its tests use a stub CLI and compile the
desktop's `src/appearance_file.rs` to keep the two validators identical.

`shell.create` starts a terminal in a project or worktree of the desktop (a plain shell,
Codex, Claude or Grok, optionally with the agent's `unrestricted` flag or a command for
a plain shell) and `shell.close` ends one (the contract is in
[remote-protocol.md](../docs/remote-protocol.md)). An agent whose request leaves
`unrestricted` out gets `--as-settings`, so the desktop's **Agent terminals run
unrestricted** decides, when `riwork capabilities --json` says `"shell_create_as_settings":
true` (asked with the chat and orchestrator questions, announced as
`features.shell_create_as_settings`); an older CLI starts it restricted, as before. The connector validates every field
before anything runs, builds the argument vector of `riwork shell create ... --json`
itself (one argument per value, nothing through a shell string), maps the CLI's
"No project matches", "No worktree matches" and "... is not installed or is not on PATH"
sentences to `not_found` and `harness_unavailable`, and refuses a session that is not
the one asked for. Both run in the ordered lane, which a phone that drops does not cut short, and the CLI of a
creation runs in a task of its own, so no connection ending can kill it between tmux starting
the session and the CLI registering it. A creation first looks the id up with
`project show` / `worktree show` and requires that exact id back (the CLI would also resolve names,
branches and paths). They are not idempotent and not deduplicated. Their tests use a stub CLI (`tests/shell_create.rs`); the root crate's
`tests/shell_create_cli.rs` pins the CLI sentences and output the connector relies on,
and an ignored test drives the real CLI with `RIWORK_TEST_CLI`.

`project.create` makes a new project in the desktop's default projects folder (the contract is in
[remote-protocol.md](../docs/remote-protocol.md)). The phone sends a `name` and optionally `git`
and never a path. The connector validates both before anything runs (one folder name of at most 100
characters and 255 bytes, no control characters, no `/` or `\`, no leading `.` or `-`, no
whitespace at either end), builds the argument vector of `riwork project create --name NAME
[--no-git] --exclusive --json` itself (the name is one argument, nothing goes through a shell
string), and only sends `--exclusive` after `riwork capabilities --json` said
`project_create_exclusive` (an older CLI would read the flag as a PATH). `--exclusive` makes the CLI
refuse a name or folder that exists instead of registering it, and the connector turns the CLI's
`already_exists: project …` / `already_exists: folder …` into the code `already_exists` with a
sentence of its own that names no path. It refuses a project that is not the one asked for, projects
the result like `projects.list`, runs in the ordered lane like `shell.create`, and the CLI runs in a
task of its own so no connection ending can kill it between making the folder and registering it. It
is not idempotent and not deduplicated: a repeat answers `already_exists`. Its tests use a stub CLI
(`tests/project_create.rs`); the root crate's `tests/project_create_cli.rs` pins the CLI sentences
and output the connector relies on (in a throwaway `HOME` and `RIWORK_HOME`), and an ignored test
drives the real CLI with `RIWORK_TEST_CLI` the same way.

`chats.list`, `chat.create`, `chat.events`, `chat.command` and `chat.stop` let the phone follow and drive the
desktop's Codex and Claude chats (the contract is in [remote-protocol.md](../docs/remote-protocol.md), "Chat
extension"; the code is `src/rpc/chat.rs`). Each is one call of the installed CLI (`riwork chat list|new|events|
command|stop ... --json`) with an argument vector built from validated values; the chat JSON itself is the
desktop's `src/chat/model.rs`, passed on as printed, after the connector checked its envelope (a chat has a
canonical id, a known provider and a state; a page of events has `{seq, event}` entries in rising order after
`since`). A project or worktree is looked up by its exact id first, like `shell.create`. `riwork capabilities
--json` says whether the CLI has chats (`"chat":true`): it decides `features.chat` in `ready` (asked once per
connection while the answer is no, and remembered once it is yes) and each method refuses with "update RiWork"
without it. `chat.create`, `chat.command` and `chat.stop` are in the ordered lane; a creation's CLI runs in a
task of its own, like `shell.create`'s. `chat.events` is a long poll of up to 25 seconds: the CLI subscribes to the
chat host from `since`, waits for the first event, collects for 50 ms and prints a page. The connector asks it for
at most the reply limit less 1 KiB of JSON (128 KiB, or 2 MiB for a session that deflates) and then halves the
page until the reply fits one sealed frame the way `connector.rs` will seal it (`link::encode_reply`), setting
`more` and `next` for the cut (one event that is more than a frame by itself has its long strings cut, as the CLI cuts them for a page). `tests/chat.rs` runs the RPCs against a stub CLI (an ignored test drives the real
CLI and its chat host in a throwaway home with `RIWORK_TEST_CLI`); `tests/chat_link.rs` runs a real relay and the
real connector binary for `features.chat`, the lanes and the sealing of a page.

`upload.begin`, `upload.chunk`, `upload.finish`, `upload.cancel` and `shell.paste` take a photo or a file
from the phone and give it to a shell or a chat (the contract is in [remote-protocol.md](../docs/remote-protocol.md),
"File upload extension"; the code is `src/upload.rs` and `src/rpc/upload.rs`). The connector writes the chunks
itself (no CLI per chunk) into `remote/uploads/DEVICE/UPLOAD.part`, checks the size and SHA-256 at `upload.finish`
and only then links the file, whole, into `RIWORK_HOME/uploads/TARGET/` under a name it makes from the phone's
(ASCII letters, digits, `-` and `_`, eight random hex digits, the extension), never replacing a file. Every
limit is the connector's: 50 MiB a file, four uploads under way and 200 MiB per device (the oldest complete
uploads make room), 90 KiB of data per chunk, 16 files per paste. `ready` announces them as `features.upload`.
A per-device ledger (`uploads-DEVICE.json`, mode 600) says what each device sent; the sweep (at start, then
hourly, and for a device before each new upload) removes partial uploads idle for an hour, complete ones after a
day, everything of a device that is revoked or gone (also `revoke` itself and a running connector within a tick)
and day-old files no ledger knows. The desktop removes a shell's inbox when the shell is closed and a chat's when
it is deleted. A begin for a shell checks first that the CLI has `shell paste` (`"shell_paste": true` in
`riwork capabilities --json`, remembered once it says yes) and that the shell is alive. `shell.paste` runs
`riwork shell paste SHELL -- FILE...` for this device's finished uploads for that shell, with the batch ledger,
locks and `sent|duplicate|uncertain` answers of `shell.keys`, and the same reading of the CLI's error tokens. The
upload steps are reads in `lanes.rs` (a shared slot each, so an upload never holds up typing); `shell.paste` is
ordered. Nothing logs, opens or runs a file. The tests are `tests/upload.rs` (a stub CLI: validation, traversal
names, resume, damaged files, limits, exactly-once paste, sweep and revocation) and, in the root crate,
`tests/shell_paste_cli.rs` (the real CLI and a real tmux in a throwaway `RIWORK_HOME`).

`projects.list`, `shells.list` and `orchestrators.list` also carry what the desktop knows about
recency and agent activity (the contract is in [remote-protocol.md](../docs/remote-protocol.md),
"Activity and recency extension"). The connector computes none of it: the CLI answers
`riwork project list --json`, `shell list --json` and `orchestrator list --json` with the optional
`last_edited_unix`, `last_activity_unix` and `agents` (projects) and `last_activity_unix`,
`activity`, `activity_since_unix`, `subagents_working` and `subagent_kinds` (shells), and
`PROJECT_FIELDS` and `SESSION_FIELDS` in `src/rpc.rs` let them through. Unlike the older fields, each of these is checked for its shape
before it is passed on (non-negative integers, the five activity words, `agents` with
`working` and `waiting` and optionally `done`, at most eight short kind names), and one that
fails is left out as if the CLI had not answered it, so a damaged or newer answer cannot reach a
phone that decodes them strictly. An older CLI has none of them and the answers are byte for byte
what they were. The tests are `tests/activity_fields.rs` (a stub CLI) and, in the root crate,
`tests/agent_activity_cli.rs`, which pins the CLI side with real hook events, Codex rollouts and a
real tmux in a throwaway `RIWORK_HOME`.

An orchestrator can run as a chat of the chat host instead of a tmux terminal ("Chat orchestrators"
in the protocol document). `orchestrator list --json` then also carries `mode` (`terminal` or
`chat`) and, for a chat, `chat_id` (the entry's own `id`) and `provider` (`codex` or `claude`); they
are in `SESSION_FIELDS` with the same shape checks, and `chat_id` and `provider` are passed on only for
an entry whose own `mode` is `chat` (`session_fields`). The `shell.*` methods refuse a chat
orchestrator's id with `invalid_request` before any `riwork shell ...` runs: `Rpc::selected` looks
the session up and stops at `mode` `chat`; where the CLI checks shells itself (no lookup first, for
speed), `Rpc::explain` looks only after that CLI refused the id as unknown. `tests/chat_orchestrator.rs`
pins it against a stub CLI.

`orchestrator.create` (`src/rpc/orchestrator.rs`) makes the global orchestrator or a project's, or
returns the one that exists: `riwork orchestrator create [--project ID] --json`, which prints a list
entry plus a boolean `created`. The connector forwards optional `mode` as `--mode terminal|chat`; omitted mode uses the desktop's "Orchestrator runs as" setting, and its chat provider is retained, looks the project up by exact id first like `shell.create`, runs in the
ordered lane with the CLI in a task of its own, and refuses a `created` that is not a boolean or an
entry of another scope. `"orchestrator_create": true` in `riwork capabilities --json` decides
`features.orchestrator_create` in `ready`; the same answer serves `features.chat`. The tests are
`tests/orchestrator_create.rs` (a stub CLI) and `tests/chat_link.rs` (the real connector).

## Pairing another Mac (desktop devices)

A second Mac running RiWork can show this Mac's shells as real terminals (the contract is
in [remote-protocol.md](../docs/remote-protocol.md), "Desktop terminal extension"). It is paired as a
*desktop* device, on protocol 2 only:

```sh
riwork remote pair --relay wss://relay.example.com/v1/ws --protocol 2 --kind desktop \
  --name 'Studio Mac' --out "$RIWORK_PAIR_DIR/studio.pairing.json" \
  --relay-routes "$RIWORK_PAIR_DIR/relay-routes.json" --show-link
riwork remote devices   # each device now says "kind": "mobile" or "desktop"
```

`--kind` is `mobile` (the default) or `desktop`; `--kind desktop` without `--protocol 2` is refused.
Import the printed `riwork://pair?v=2&data=...` link on the other Mac, then provision the relay route
and start the connector exactly as for a phone. The kind is stored in the host's `devices.json` only
(the field is left out for a phone, so a config of phones is unchanged byte for byte), and only a
desktop on a v2 session is told in `ready` (`features.pty`) that terminal streams exist. Revoke it
like any device; its streams end within a second. Do not downgrade `riwork-remote` once a desktop is
paired: an older build refuses a `devices.json` with a `kind` it does not know, rather than treating
the desktop as a phone.

`pty.open` runs `riwork shell attach SHELL_UUID --exec [--ignore-size]` (which needs a `riwork` that
reports `shell_attach_exec` in `riwork capabilities --json`) on a pseudo-terminal the connector owns
(`src/pty.rs`: `openpty`, a session of its own with the terminal as controlling terminal, an
allowlisted environment, and the process killed and reaped when the stream, the session or the
connector ends). The connector's `lanes.rs` gives `pty.open` a lane of its own (one at a time) and
`pty.read` another (up to 12 parked), and the connection loop answers `pty.write`, `pty.resize` and
`pty.close` itself, in arrival order, because they never wait. The tests are `tests/pty.rs` (a
stand-in CLI, through the RPC layer and through a real relay and connector) and, in the root crate,
`tests/shell_attach_exec_cli.rs`.

Revocation stops live endpoint access within one second and removes its local
PSK/tokens. Remove that route from the relay manifest and restart the relay to
invalidate relay tokens too. Other paired devices keep their endpoint secrets;
a relay restart reconnects them with fresh session keys. An input already in
flight cannot be undone.

## Using another Mac as a client

Another Mac running RiWork can control this Mac's shells the way a phone does, through the same
blind relay and the same end-to-end encryption, and show them in real terminals. The shell stays
a tmux session on the host; the client runs a bridge process whose child is a Ghostty surface, and
the bytes of a `tmux attach` that the host runs in a pseudo-terminal travel over the `pty.*` RPCs
(`pty.open`, `pty.read`, `pty.write`, `pty.resize`, `pty.close`). Scrollback, the alternate screen,
mouse and the repaint after a reconnect therefore come from tmux and the terminal, not from a
re-drawing of captured text. A Mac pairs with `--protocol 2 --kind desktop` and has the same
authority as a phone: full terminal control, revocable with `revoke` like any device. A phone's
pairing is refused `pty.*` ("unsupported RPC method").

```sh
# On the host (the Mac whose shells are shown); see "Pair, provision and start".
riwork remote pair --protocol 2 --kind desktop --relay wss://relay.example.com/v1/ws \
  --name 'My MacBook' --out "$RIWORK_PAIR_DIR/macbook.pairing.json" \
  --relay-routes "$RIWORK_PAIR_DIR/relay-routes.json" --show-link
# On the client, within the invite's lifetime, with the host's connector and relay running:
riwork-remote hosts add --link 'riwork://pair?v=2&data=...' --label 'Studio'   # or --link - to read it from stdin
riwork-remote hosts list [--json]
riwork-remote status --desktop DESKTOP_ID [--watch] [--json]
riwork-remote call --desktop DESKTOP_ID projects.list
riwork-remote attach --desktop DESKTOP_ID --shell SHELL_UUID [--ignore-size]
riwork-remote hosts remove DESKTOP_ID
```

**Registry.** `hosts add` redeems the invite at once (`pair_hello`, `pair_accept`, `pair_finish` of
[remote-protocol-v2.md](../docs/remote-protocol-v2.md)) and stores the established record in
`$RIWORK_HOME/remote/hosts.json` (mode 600, mode-700 directory, written atomically with the same
helpers as `devices.json`) before it sends the first RPC. The invite secret is gone from the record;
what stays is the root key and the relay token, so the file is a secret and nothing prints it:
`hosts list` shows the id (the host's desktop id, which `--desktop` takes), label, relay and route,
and `hosts add --json` the same for the new host. A host is added once; to pair it again, remove it first.
`--allow-insecure-loopback` is the development switch of `pair`, for a `ws://127.0.0.1` relay. Add the
host from a link only; the link, the export file and any terminal scrollback that held it are as
secret as the invite until it is redeemed (`--link -` reads it from standard input, which keeps it out of
the process list).

**One process per host.** The relay lets one socket per role hold a route, so `riwork-remote client serve
--desktop ID` owns the connection (role "mobile", the v2 handshake, deflate when the host offers it, a
ping and a probe every 10 s, reconnects with backoff of 0.5 to 15 s) and everything else on this Mac
talks to it over a Unix socket. `client ensure --desktop ID` starts it detached unless one answers
(safe to repeat or run at once; it logs to `run/*.log`), `client socket --desktop ID` prints the
socket's absolute path without starting anything, and it exits after five minutes without a client.
`hosts remove` ends it. The socket is `$RIWORK_HOME/remote/run/<12 hex>.sock`, mode 600 in a mode-700
directory, with a hashed name because a macOS socket path may hold 104 bytes (a longer `RIWORK_HOME` is
refused with that advice). A connection from another user is dropped (`getpeereid` on macOS). The
daemon is a fixed list of operations, one JSON request per line, answered with JSON lines:

| Request | Answer |
| --- | --- |
| `{"op":"call","id","method","params","timeout_ms"}` | `{"id","ok":true,"result","server_ms"}` or `{"id","ok":false,"error":{"code","message"},"server_ms"}`. Many calls may run at once on a connection. While the first connection is still being made a call waits for it; once the host is known to be away it fails at once with `offline`. The daemon's own codes are `offline`, `timeout` and `invalid_request`. |
| `{"op":"status"}` | `{"state":"connecting"\|"online"\|"offline","rtt_ms"?,"since"?,"reason"?,"label"}` (`since` is Unix seconds; the reason of a host that is away is "host offline (or access revoked)") |
| `{"op":"watch"}` | that line now and again at every change, until the client closes |
| `{"op":"shutdown"}` | `{"ok":true}`, then the daemon quits |
| `{"op":"attach","shell_id","columns","rows","term","ignore_size"}` | the connection becomes binary frames |

An attached connection carries frames `u8 type || u32 big-endian length || payload` (at most 1 MiB):
`D` terminal data in both directions, `R` `{"columns","rows"}` from the bridge, `S` a status line
as above and `E` `{"reason"}` from the daemon (`exited`, `closed`, `limit`, or the host's refusal such as
`not_found: ...`), after which the daemon hangs up. The daemon `pty.open`s the stream (with TERM
`xterm-ghostty` or `xterm-256color`), parks one `pty.read` per stream and files its reply by `seq`
(the host's `max_reads` are counted over all attached terminals, and a terminal waits for a free one;
the relay closes a socket whose queue passes 16 messages, so more would only risk that), and pipelines
`pty.write` in chunks of at most `max_write` with a running `seq`, at most four in flight. A host that is
behind answers a write `pty_limit` without advancing the offset, and the writes already sent behind it
skip bytes; the daemon lets the window drain and sends them again, in order and at the same offsets,
after 100 ms. A chunk that starts with CR carries `gap_ms`, the pause the person made before it (at most
150 ms) less what the two writes are apart anyway, so that a Return typed after a pause costs no delay and
one squeezed against its text by a full window keeps the pause that Codex's paste detection looks for.
Window changes are folded into the latest size, with one `pty.resize` in flight. When the link drops the bridge gets `S`
offline, the daemon waits for the next session and opens the stream again with the current window
size (`S` online follows), whether the host was away for a minute or for a moment. Keys typed while
the stream is down are dropped, never replayed. Closing the bridge sends `pty.close`; the host also
drops a session's streams when the session ends.

**The bridge.** `attach` runs `client ensure`, puts its terminal in raw mode (every key, Ctrl-C
included, goes to the host; the mode is restored when it exits, also on SIGTERM and SIGHUP), attaches
with the terminal's size and `xterm-ghostty` if `TERM` says Ghostty (otherwise `xterm-256color`),
copies keys and output, and sends `R` on SIGWINCH. On `S` offline it freezes the screen and draws one
dimmed line on the last row; on `S` online after an interruption it first writes a reset (`ESC [ ! p`,
the alternate screen left, mouse, focus and bracketed-paste modes off, the cursor shown) so that
whatever mode the frozen frame left on is gone before tmux repaints. On `E` it prints the reason and
exits 0. A host that cannot attach this Mac says why: "unsupported RPC method" means it was paired as a
phone (pair again with `--kind desktop`).

## Local development

Production pairing rejects plaintext. The explicit dev option accepts only
literal loopback URLs; it cannot be used from a physical phone on another host.
Use an iOS simulator on the relay host, or TLS for a physical device.

```sh
RIWORK_PAIR_DIR=$(mktemp -d "${TMPDIR:-/tmp}/riwork-pair.XXXXXX")
remote/target/release/riwork-remote pair --relay ws://127.0.0.1:8787/v1/ws \
  --allow-insecure-loopback --name 'Simulator' \
  --out "$RIWORK_PAIR_DIR/simulator.pairing.json" \
  --relay-routes "$RIWORK_PAIR_DIR/routes.json"
remote/target/release/riwork-remote relay --bind 127.0.0.1:8787 \
  --routes "$RIWORK_PAIR_DIR/routes.json"
# In another terminal, retaining the same RIWORK_HOME:
remote/target/release/riwork-remote start --riwork /absolute/path/to/riwork
```

For isolated manual testing, explicitly set a temporary `RIWORK_HOME` on **each**
command in a separate terminal. Existing production `RIWORK_HOME` is not altered
by any automated test below.

## Verification

```sh
cargo test --locked --manifest-path remote/Cargo.toml
cargo clippy --locked --manifest-path remote/Cargo.toml --all-targets -- -D warnings
swift remote/fixtures/verify.swift remote/fixtures/v1.json
RIWORK_TEST_CLI=/absolute/path/to/installed/riwork \
  cargo test --locked --manifest-path remote/Cargo.toml --test isolated_e2e \
  -- --ignored --nocapture
```

The opt-in integration tests start a real relay and connector and invoke a newly
rebuilt RiWork CLI with temporary `RIWORK_HOME` directories. They create only their
own project, task, zsh shell and zsh-backed project orchestrator, submits only to
their recorded UUIDs, and closes those sessions at cleanup. It checks list/output
RPCs, input exactly once across reconnect/restart, preserved shell variables,
unknown outcomes, tamper closure and active revocation. The viewport/PTY test
proves 120x40 ->43x17 ->120x40 with the same UUID/window/pane/PID and actual PTY
cells, ownership denial, tab switch, reconnect, revoke, renewal and connector
SIGKILL recovery within 15 seconds. A raw terminal composer reproduces the old
immediate-Return failure, then verifies complete 3500-character submissions,
exactly one Return, cached retry and two-device serialization. It calls no model
and launches no coding worker. Default tests cover
independent crypto vectors, replay/gaps/wrong device, routing authentication,
health, duplicate sockets, unavailable peers, frame/socket/queue limits,
unauthenticated-socket budget, ping/idle liveness and stale-registration
replacement, connector lease renewal, transactional pairing, private
config, RPC allowlist and durable outcome-cache behavior. Root forwarding is
compiled and exercised by the standalone tests too.
The small default terminal-control test also requires `tmux` (and uses its own
temporary server). Root input/viewport modules are compiled here with strict
Clippy independently of GPUI.

The client tests (`tests/client*.rs`) run in temporary homes with short paths and touch no real
state. `client.rs` pairs a real relay and the real connector by a v2 link and drives the real binary
(`hosts`, `call`, `status`, the client process, host restart and revocation). `client_attach.rs`,
`client_link.rs` and `client_bridge.rs` use a host stand-in (`tests/client_support`) that does the
real v2 handshake and answers `pty.*` as the design says, so the stream, the reconnect and the bridge
(in a real pseudo-terminal: raw mode, SIGWINCH, the reset, the restored terminal) are tested
independently of the host's `pty.*`. `client_real_host.rs` checks the same against the real connector;
its attach test is ignored until the host's `pty.*` and `pair --kind desktop` exist
(`cargo test --test client_real_host -- --ignored`).

Fixtures were generated independently with Python `cryptography`, checked in
Rust, and verified against native Swift CryptoKit. To regenerate:

```sh
uv run --with cryptography python remote/fixtures/generate.py
```

## Current limits

- v1 uses a PSK handshake without forward secrecy. Protect/rotate pairing exports.
- Pairing credentials never expire and any local process of the desktop user can
  pair a device; watch the connector log and `devices` output, and `revoke` strays.
- Relay sees route IDs, role tokens during registration, timing and frame sizes.
  It cannot decrypt session content or authenticate as an endpoint with a role token.
- For a phone, terminal support is captured text and one control-free physical line followed
  by Return (or `shell.keys`). It is not a remote PTY or a semantic harness API. A paired
  desktop (`pair --kind desktop`, above) is the exception: it gets a real terminal stream.
- Outcome ledger is durable and never evicted, up to 4096 inputs per device.
  At capacity, review outcomes and pair a new device. Pending/uncertain sends
  return `outcome_unknown`; inspect the shell before any deliberate manual retry.
- Desktop supports up to 64 active paired devices. New pairing releases revoked
  configuration slots while retaining their outcome ledgers for review.
- Relay provisioning is operator-managed and takes effect at relay restart.
  There is no public registration/admin API, cloud account service or automatic
  connector startup installation.
- Lists/output are bounded to 128 KiB of JSON, or, for a phone that asked for compression
  (`link.configure`), to what fits 128 KiB once deflated (at most 2 MiB of JSON). Large
  responses fail clearly; lower output line counts where relevant. Polling is
  request/response, not streaming.
