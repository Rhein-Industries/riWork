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
`shell.resize`, `shell.resize.clear`, `shell.create`, `shell.close`, in arrival order) and three others run at once,
at most two of them waits, queues the rest in arrival order, and the connection loop
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
(version 1, lowercase `#rrggbb` colors, exactly 16 terminal colors, at most 16 KiB)
and answers `not_found` "appearance not published" when the desktop app has not
published a usable `appearance.json` yet. Its tests use a stub CLI and compile the
desktop's `src/appearance_file.rs` to keep the two validators identical.

`shell.create` starts a terminal in a project or worktree of the desktop (a plain shell,
Codex, Claude or Grok, optionally with the agent's `unrestricted` flag or a command for
a plain shell) and `shell.close` ends one (the contract is in
[remote-protocol.md](../docs/remote-protocol.md)). The connector validates every field
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

Revocation stops live endpoint access within one second and removes its local
PSK/tokens. Remove that route from the relay manifest and restart the relay to
invalidate relay tokens too. Other paired devices keep their endpoint secrets;
a relay restart reconnects them with fresh session keys. An input already in
flight cannot be undone.

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
- Terminal support is captured text and one control-free physical line followed
  by Return. It is not a remote PTY or a semantic harness API.
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
