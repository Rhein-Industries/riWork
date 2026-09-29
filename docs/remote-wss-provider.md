# WSS CryptoProvider

Run on 2026-09-29 in
`/Users/dominik/orca/projects/riWork-feature-remote-protocol-v2`, parent
`30a4febba9b4cf4926f41a6bf3ac09be38bfbd3c`. Toolchain: rustc 1.95.0
(59807616e 2026-04-14), cargo 1.95.0 (f2d3ce0bd 2026-03-21). The protocol v2
counts in [remote-protocol-v2-verification.md](remote-protocol-v2-verification.md)
stay as recorded there.

## Cause

`riwork-remote start` against `wss://` builds a rustls `ClientConfig` inside
tokio-tungstenite 0.28 feature `rustls-tls-native-roots`. rustls 0.23.45 was
compiled with `default-features = false` and with `ring` and `aws-lc-rs` both
absent, so `CryptoProvider::get_default_or_install_from_crate_features`
panicked:

```
Could not automatically determine the process-level CryptoProvider from Rustls crate features.
Call CryptoProvider::install_default() before this point to select a provider manually,
or make sure exactly one of the 'aws-lc-rs' and 'ring' features is enabled.
```

The device worker captured that text from the connector log. tokio-tungstenite
connects TCP and then builds the rustls config; the panic is in that build,
before the handshake finishes. Loopback `ws://` skips the config build.

## Change

`remote/Cargo.toml` depends on rustls 0.23 with `default-features = false` and
features `ring`, `std`, and `tls12`. `aws-lc-rs` stays off. Enabling both
providers makes `CryptoProvider::from_crate_features` return `None`, and the
panic comes back. `connect_registered` calls
`rustls::crypto::ring::default_provider().install_default()` before
`connect_async_tls_with_config`. A second install loses the race and rustls
keeps the first provider.

With a `None` connector, tokio-tungstenite still loads the operating-system
root store through `rustls-native-certs`. That is the shipping path. Unix
`cargo test` can place one extra root in a process slot that `Drop` clears.
The slot is compiled out of non-test builds.

`Cargo.lock` lists `ring` under rustls 0.23.45. The same lock update added
rcgen 0.13.2 (feature `ring`), time 0.3.55, time-core 0.1.9, yasna 0.5.2,
deranged 0.5.8, num-conv 0.2.2, and powerfmt 0.2.0. `cargo tree --offline -i
rustls -e features` shows rustls features `ring`, `std`, and `tls12`. The lock
contains no `aws-lc-rs` package.

v1 and v2 registration and session frames are unchanged. Relay close codes are
unchanged. The relay still binds plaintext loopback.

## Regression

`connector::tests::wss_rejects_an_untrusted_cert_and_carries_v1_and_v2_when_trusted`

A CA valid from one hour before the test clock until two hours after it signs
a leaf whose only subjectAltName is IP `127.0.0.1`. A TLS listener on
`127.0.0.1` and `[::1]` copies decrypted bytes to the plaintext loopback relay.

- With the test root unset, `connect_registered` to
  `wss://127.0.0.1:<port>/v1/ws` returns an error containing
  `invalid peer certificate` and `UnknownIssuer`. Registration does not complete.
- With that CA installed for the test, `wss://localhost:<port>/v1/ws` returns
  an error containing `invalid peer certificate` and `not valid for name`.
  `localhost` is a DNS name and the leaf has only the IP SAN.
- With the CA installed, `run_device_with` and an in-process peer complete a v1
  handshake. `projects.list` returns `ok` and an empty project list.
- A second device on the same relay completes a v2 invite, `projects.list`, a
  stored-root resume, and a second `projects.list`.

Aborting the v1 connector task makes the relay log a WebSocket reset on that
desktop socket. The v2 device then authenticates, including the resume. The
temporary home is a `tempfile` directory.

## Command

```sh
cargo test --manifest-path remote/Cargo.toml --offline -- --test-threads=8
```

Exit 0. 54 passed, 0 failed, 3 ignored.

| Suite | Passed | Failed | Ignored |
|---|---:|---:|---:|
| `src/lib.rs` | 7 | 0 | 0 |
| `src/main.rs` | 0 | 0 | 0 |
| `tests/cli_forward.rs` | 3 | 0 | 0 |
| `tests/config_security.rs` | 3 | 0 | 0 |
| `tests/crypto_security.rs` | 4 | 0 | 0 |
| `tests/isolated_e2e.rs` | 0 | 0 | 3 |
| `tests/pairing_state.rs` | 7 | 0 | 0 |
| `tests/protocol_v2.rs` | 5 | 0 | 0 |
| `tests/rpc_safety.rs` | 5 | 0 | 0 |
| `tests/terminal_control.rs` | 12 | 0 | 0 |
| `tests/transport.rs` | 8 | 0 | 0 |
| doc-tests | 0 | 0 | 0 |

The new library test is
`wss_rejects_an_untrusted_cert_and_carries_v1_and_v2_when_trusted`. The first
offline test run updated `remote/Cargo.lock` from the local registry cache.
The suite above ran after that lock was written.

`~/.local/share/riwork` mtime was `1790644436` immediately before this command
and `1790644436` immediately after. The command did not receive that directory
as `RIWORK_HOME`. The mtime had already moved from `1790643733` earlier in the
session, before this command.

## Swift

An isolated Swift continuation over this certificate was not run.
`URLSessionConnector` builds an ephemeral `URLSession` on the system trust
store, and this private CA is absent from that store. The shipping connector
is unchanged, and the login keychain was left untouched.
`ios/scripts/local-smoke.py` was not pointed at the TLS proxy. The
continuation that ran is `connect_registered` (the function `riwork-remote
start` calls), `run_device_with`, and an in-process peer.

## Limitations

- ring's default key-exchange groups are X25519, secp256r1, and secp384r1.
  `prefer-post-quantum` requires `aws-lc-rs`, which this crate leaves disabled,
  so the TLS handshake has no post-quantum group. Application pairing still
  uses its own X25519.
- The relay process speaks plaintext on loopback. Production TLS terminates at
  the reverse proxy in [remote-deployment.md](remote-deployment.md).
- The trusted success path uses the test-only root. The shipping binary passes
  `None` and verifies against the operating-system roots. The untrusted case
  above is that path rejecting the private CA.
- `remote/tests/isolated_e2e.rs` stays ignored unless `RIWORK_TEST_CLI` is set.
- Duplicate mobile registration remains close 1005, as the device worker
  recorded. `remote/src/relay.rs` is unchanged in this commit.
