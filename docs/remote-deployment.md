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
WebSocket connections (or the smaller `--max-connections` setting), 10-second
registration timeout, 262144-byte frame/message limit, and a 16-message outgoing
queue per socket. An unavailable/full destination fails closed with no replay
buffer. Tungstenite write buffers are also bounded. The example selects 32
connections; at maximum frame/queue occupancy this is roughly 128 MiB of queued
payload, plus socket/HTTP/runtime overhead. Tune the connection cap to host memory
and expected paired devices; two sockets are needed per connected pair. TLS
proxy connection/rate limits can further bound unauthenticated HTTP traffic.

Local desktop `revoke DEVICE_UUID` denies endpoint access immediately (the watcher
closes an existing connection within one second). Removing its relay route and
restarting the relay additionally invalidates routing credentials. Routing hashes
remain offline provisioning data; there is no unauthenticated enrollment endpoint.
Back up desktop config/outcome ledgers with the same care as terminal access,
because the config contains pairing PSKs and the ledger contains submitted lines.
Do not restore an old outcome ledger while retaining a mobile device's pending
requests; that would discard the evidence needed for duplicate suppression.

The v1 PSK protocol does not provide forward secrecy. Its exact security boundary,
handshake/envelope bytes, errors and retry rules are in
[remote-protocol.md](remote-protocol.md). Production transport requires both TLS
and endpoint authentication; a relay token alone never authorizes a CLI request.

References: [Caddy reverse proxy and WebSocket support](https://caddyserver.com/docs/caddyfile/directives/reverse_proxy),
[Caddy automatic HTTPS](https://caddyserver.com/docs/automatic-https),
[Caddy mutually exclusive handle routes](https://caddyserver.com/docs/caddyfile/directives/handle).
