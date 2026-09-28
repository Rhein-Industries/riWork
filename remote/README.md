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
with that phone and remove the export after import. Omitting `--show-link` avoids
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
uses a loopback relay behind Caddy.

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
- Lists/output are bounded to 128 KiB of JSON. Large responses fail clearly;
  lower output line counts where relevant. Polling is request/response, not streaming.
