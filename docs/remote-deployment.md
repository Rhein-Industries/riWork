# Deploying the RiWork blind relay

The relay is a standalone Rust binary. It never runs the RiWork CLI and never
receives endpoint pairing secrets. Run it under a dedicated OS account, with
operator-managed routing-token hashes, behind a TLS reverse proxy. Desktop and
iOS both make outbound `wss://HOST/v1/ws` connections. The relay itself accepts
plaintext **only on loopback**, including in production behind the proxy.

Build on the deployment host from the reviewed source/lockfile:

```sh
cargo build --locked --release --manifest-path remote/Cargo.toml
```

Install `remote/target/release/riwork-remote` as `/usr/local/bin/riwork-remote`.
Create the `riwork-relay` service account and `/etc/riwork-relay` with mode 700,
owned by it. Install the **route hash manifest** exported by desktop pairing as
`/etc/riwork-relay/routes.json`, mode 600, also owned by `riwork-relay`. Transfer
neither desktop `devices.json` nor the mobile pairing export to the relay.

A Linux systemd unit (`/etc/systemd/system/riwork-relay.service`):

```ini
[Unit]
Description=RiWork opaque WebSocket relay
After=network.target

[Service]
Type=simple
User=riwork-relay
Group=riwork-relay
ExecStart=/usr/local/bin/riwork-remote relay --bind 127.0.0.1:8787 --routes /etc/riwork-relay/routes.json --max-connections 32
Restart=on-failure
RestartSec=2
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
UMask=0077
LimitNOFILE=256
MemoryMax=512M

[Install]
WantedBy=multi-user.target
```

This is a deployment example, not installed or enabled by repository commands.
The binary reads the routing manifest at startup. Restart the service after
adding/removing routes. Service restart drops transports only; the desktop's
persistent sessions survive and connectors derive fresh reconnect keys.

Caddy example on the **same host** (`/etc/caddy/Caddyfile`):

```caddyfile
{
    servers {
        timeouts {
            read_header 5s
        }
    }
}

relay.example.com {
    @relay path /v1/ws /healthz
    handle @relay {
        reverse_proxy 127.0.0.1:8787
    }
    handle {
        respond 404
    }
}
```

Replace the hostname and point its DNS at the server. Caddy handles HTTPS
certificates and WebSocket upgrades. This example intentionally does not enable
access logging; the relay never logs registration bodies, bearer tokens,
ciphertexts or decrypted payloads. Do not enable reverse-proxy body tracing or
capture WebSocket payloads. Firewall public access to 443 (and 80 if used by
certificate issuance/redirect); do not expose port 8787. For another TLS proxy,
forward WebSocket upgrades at exactly `/v1/ws`, preserve frame order, disable
payload logging, and allow long-lived connections. There is no application-layer
compression requirement or alternate WebSocket subprotocol.

**Put connection and rate limits at the proxy.** The relay only sees the proxy's
loopback address, cannot tell clients apart, and `axum` gives it no HTTP header
timeout, so per-client limits belong in front of it. It bounds unauthenticated
sockets (16 at once, 3 seconds each, oldest dropped first), but a sustained flood of
new connections can still crowd out real registrations. The Caddy example above sets
a header timeout only: stock Caddy has no per-address connection or rate limiter
(third-party modules such as `caddy-ratelimit` add one, or use the host firewall).
nginx has both built in; a complete alternative for the same host:

```nginx
# http { } context
limit_conn_zone $binary_remote_addr zone=riwork_conn:1m;
limit_req_zone  $binary_remote_addr zone=riwork_req:1m rate=5r/s;

server {
    listen 443 ssl;
    server_name relay.example.com;
    # ssl_certificate / ssl_certificate_key managed as usual
    client_header_timeout 5s;

    location = /v1/ws {
        limit_conn riwork_conn 16;                  # sockets per address
        limit_req  zone=riwork_req burst=10 nodelay; # new connections per address
        proxy_pass http://127.0.0.1:8787;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_read_timeout 120s;                    # relay pings every 20 s
    }
    location = /healthz { proxy_pass http://127.0.0.1:8787; }
    location / { return 404; }
}
```

Each connected pair uses two sockets, and a home or office NAT shares one address
across every desktop and phone behind it, so size `limit_conn` for that.

Health checks:

```sh
curl --fail http://127.0.0.1:8787/healthz
curl --fail https://relay.example.com/healthz
```

`ok` confirms HTTP/router health, not device authentication or desktop availability.
Desktop pairing must use `wss://relay.example.com/v1/ws`. Production clients use
normal certificate verification; no certificate bypass is provided. Local
`ws://127.0.0.1` pairing requires the explicit `--allow-insecure-loopback` switch.

Limits are enforced before routing: at most 128 configured routes, at most 256
**authenticated** WebSocket connections (or the smaller `--max-connections`
setting), 262144-byte frame/message limit, and a 16-message outgoing queue per
socket. Sockets that have not registered yet are not counted against
`--max-connections`: they have their own fixed budget of 16 and 3 seconds to send a
valid registration, and a newcomer beyond the budget drops the oldest, so idle
sockets cannot lock out desktops and phones. A relay already holding
`--max-connections` authenticated sockets closes further valid registrations.
An unavailable/full destination fails closed with no replay
buffer. Tungstenite write buffers are also bounded. The example selects 32
connections; at maximum frame/queue occupancy this is roughly 128 MiB of queued
payload, plus socket/HTTP/runtime overhead. Tune the connection cap to host memory
and expected paired devices; two sockets are needed per connected pair.
`LimitNOFILE` must cover the cap, the 16 unauthenticated sockets and the health
checks (256 is ample for the example).

Liveness: the relay pings each socket about every 20 seconds and closes any that has
sent nothing (pongs count) for 60 seconds. After a network switch or a Mac sleep
the dead registration would otherwise reject the reconnect as a duplicate; a
registration that authenticates with the same route token replaces one silent for
30 seconds. The connector applies the same 60-second deadline to the relay. The
relay logs to stderr (journald under the example unit): registration rejections
(unknown route, bad token, duplicate, relay full, timeout, at most 20 lines per
10 seconds), replaced registrations and the reason an authenticated socket closed.
Route IDs and roles appear for authenticated sockets; tokens, registration bodies
and payloads never do.

Local desktop `revoke DEVICE_UUID` denies endpoint access immediately (the watcher
closes an existing connection within one second). Removing its relay route and
restarting the relay additionally invalidates routing credentials. Routing hashes
remain offline provisioning data; there is no unauthenticated enrollment endpoint.
Back up desktop config/outcome ledgers with the same care as terminal access,
because the config contains pairing PSKs. The ledger does not contain submitted
lines: per request it stores the request UUID, an unsalted SHA-256 digest of the
request JSON (which includes the line) and the cached response (shell ID and
status). The UUID sits in the clear beside its digest, so it adds no secrecy;
digests of short or predictable lines can be confirmed by brute force. Treat the
ledger as sensitive anyway.
Do not restore an old outcome ledger while retaining a mobile device's pending
requests; that would discard the evidence needed for duplicate suppression.

The v1 PSK protocol does not provide forward secrecy. Its exact security boundary,
handshake/envelope bytes, errors and retry rules are in
[remote-protocol.md](remote-protocol.md). Production transport requires both TLS
and endpoint authentication; a relay token alone never authorizes a CLI request.

## What a paired device can do, and how to watch it

A paired phone can submit a line, followed by Return, to **every live project shell
and orchestrator** of the desktop: plain shells, unrestricted (approval-free)
harness sessions and editor tabs. That is arbitrary command execution as the desktop
user, even though the v1 API has no create/close/schedule methods. Pair only devices
you would give a terminal on that machine.

Pairing credentials never expire; revocation is the only end. Any process running as
the desktop user can run `riwork remote pair`, and a running connector adopts the
new device within about 250 milliseconds, so the desktop account is the trust
boundary. To make that visible:

- The connector logs (stderr) `Remote device ID (NAME) enabled.` when it starts
  serving a device and `authenticated for the first time.` the first time that
  device completes a handshake, then `authenticated.` on later ones.
- `riwork remote devices` prints JSON per device including `paired_at_unix`,
  `first_authenticated_unix` and `last_authenticated_unix` (Unix seconds; `null`
  when unknown, e.g. pairings made before these were recorded, or never connected).
  A device that appears which you did not pair, or connects when you did not expect
  it, should be revoked with `riwork remote revoke DEVICE_UUID`.

References: [Caddy reverse proxy and WebSocket support](https://caddyserver.com/docs/caddyfile/directives/reverse_proxy),
[Caddy automatic HTTPS](https://caddyserver.com/docs/automatic-https),
[Caddy mutually exclusive handle routes](https://caddyserver.com/docs/caddyfile/directives/handle),
[Caddy server timeouts](https://caddyserver.com/docs/caddyfile/options#timeouts),
[nginx limit_conn](https://nginx.org/en/docs/http/ngx_http_limit_conn_module.html),
[nginx limit_req](https://nginx.org/en/docs/http/ngx_http_limit_req_module.html).
