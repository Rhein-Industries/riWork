# Protocol v2 verification

Run on 2026-09-29 against `8a9de8633191f86d1f35fe1f2b7252ae41cc3b31` in
`/Users/dominik/orca/projects/riWork-feature-remote-protocol-v2`. No code defect
turned up, so this record does not change the protocol. Commands were run from
that worktree. Continuation homes were temporary directories created by
`ios/scripts/local-smoke.py`. The production data directory
`~/.local/share/riwork` was not passed to any command; its `stat` mtime stayed
`1790643191` from before the suite through fixture removal.

The contract and threat model are in [remote-protocol-v2.md](remote-protocol-v2.md).

## Rust full suite

```sh
cargo test --manifest-path remote/Cargo.toml --offline -- --test-threads=8
```

Exit 0. 53 passed, 0 failed, 3 ignored.

| Suite | Passed | Failed | Ignored |
|---|---:|---:|---:|
| `src/lib.rs` | 6 | 0 | 0 |
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

`protocol_v2` passed `published_vectors_match_rust_for_invite_and_session`,
`different_ephemeral_keys_cannot_open_a_recorded_frame`,
`bad_proof_does_not_consume_expiry_wipes_and_replay_loses`,
`ttl_bounds_and_v1_records_stay_free_of_invite_fields`, and
`one_winner_replay_and_overlapping_race`. The library tests include
`crypto::v2::tests::zero_diffie_hellman_is_rejected`,
`crypto::v2::tests::recorded_session_key_is_not_a_function_of_the_root_alone`,
and `connector::tests::v2_relay_rejects_race_replay_and_old_hello_then_continues`.
`crypto_security` still matches the frozen v1 fixtures.

The three ignored tests are `mobile_viewport_ownership_restoration_and_long_turns_across_devices`,
`real_relay_connector_persistent_shell_retry_restart_and_revocation`, and
`return_only_and_long_inputs_are_serialized_in_disposable_cli_session`. Each is
marked `requires RIWORK_TEST_CLI` and was not given that variable. The
relay–desktop–Swift continuation below is the end-to-end check for this protocol.

## Swift package and vectors

```sh
swift test --package-path ios
swift remote/fixtures/verify.swift remote/fixtures/v1.json
swift remote/fixtures/verify.swift remote/fixtures/v2.json
```

`swift test` exited 0. XCTest executed 32 tests with 0 failures:
`ProtocolTests` 16 passed, 0 failed, including
`testPublishedV2InviteAndSessionVectors` and `testPublishedRustSwiftVector`;
`RelayClientTests` 16 passed, 0 failed. The Swift Testing runner that follows
reported 0 tests in 0 suites. `remote/fixtures/v2.json` and
`ios/Tests/Fixtures/v2.json` are the same file bytes, produced by
`remote/fixtures/generate.py` with Python `cryptography`.

`verify.swift` exited 0 for both files. v1 printed `PASS: native CryptoKit UUID
bytes, handshake MACs, HKDF keys, nonce/AAD and ChaChaPoly fixtures`. v2 printed
`PASS: CryptoKit v2 invite, X25519 session, HKDF and ChaChaPoly fixtures`.

## Signed simulator app tests

```sh
xcodebuild -project ios/RiWorkRemote.xcodeproj -scheme RiWorkRemote \
  -destination 'platform=iOS Simulator,id=45D942B2-ABE6-4C4F-8D13-E252AF668880' \
  -only-testing:RiWorkAppTests test CODE_SIGN_IDENTITY=-
```

Exit 0. **TEST SUCCEEDED** at 2026-09-29 02:54:37 on the booted iPhone 17 Pro
simulator `45D942B2-ABE6-4C4F-8D13-E252AF668880`. `RiWorkAppTests` executed 23
tests with 0 failures: `CommandFieldTests` 3 passed, `StateTests` 20 passed,
including `testV2ConnectStoresTheRootAndDropsTheInviteSecret`. Signing used
`CODE_SIGN_IDENTITY=-`. `CODE_SIGNING_ALLOWED=NO` is not a valid stand-in: it
compiles, and Keychain then returns `-34018`.

## Isolated continuation

Project and shell commands used
`/Users/dominik/orca/projects/riWork/target/debug/riwork`. Pair, relay, revoke,
and the connector used this worktree's `remote/target/debug/riwork-remote`.
The phone was `ios/.build/debug/riwork-ios-smoke`.

```sh
python3 ios/scripts/local-smoke.py \
  --relay-binary remote/target/debug/riwork-remote \
  --riwork /Users/dominik/orca/projects/riWork/target/debug/riwork \
  --smoke-binary ios/.build/debug/riwork-ios-smoke \
  --protocol 2
```

Exit 0. The script printed:

- `Authenticated; protocol 2; projects: 1`
- `PASS: exactly one executed line; same persistent session alive after Swift reconnect`
- `PASS: consumed v2 invite is rejected for a second process`
- `Authenticated; protocol 2` again from a new process using the stored root
- `PASS: a new process continued with the stored root and did not replay the invite`
- `PASS: revoked device cannot authenticate/reconnect`
- `Fixture processes and sessions cleaned up`

The fixture shell was `94b06107-31e6-451f-8f98-da3358651edd`. Replay presented
the original invite file. The stored-root file had `invite_state` established,
a root, and no invite secret; the harness checks that before the second
process. Revoke was checked with that stored root, so a consumed invite could
not masquerade as a successful revocation.

```sh
python3 ios/scripts/local-smoke.py \
  --relay-binary remote/target/debug/riwork-remote \
  --riwork /Users/dominik/orca/projects/riWork/target/debug/riwork \
  --smoke-binary ios/.build/debug/riwork-ios-smoke \
  --protocol 1
```

Exit 0. The script printed `Authenticated; protocol 1; projects: 1`,
`PASS: exactly one executed line; same persistent session alive after Swift reconnect`,
and `PASS: revoked device cannot authenticate/reconnect`. The fixture shell was
`e1f0752e-c186-44f5-b69c-89b7aa1030c1`. Protocol 1 has no single-use invite.
The same long-lived PSK reconnects inside the first smoke process, and the
pairing file is rejected only after `revoke`.

## Cleanup

`local-smoke.py` closes its relay, connector, and fixture shells, then leaves
the temporary directory on disk. After both runs, `shell list --json` under
each fixture `RIWORK_HOME` returned 0 shells, and `tmux -L` for each fixture
socket reported `no server running`. No process command line referenced
`riwork-swift-interop-`. These directories were then removed:

| Directory suffix | Role |
|---|---|
| `riwork-swift-interop-zclb6i4m` | protocol 2 run above |
| `riwork-swift-interop-yuj8j69o` | protocol 1 run above |
| `riwork-swift-interop-uid385v9` | protocol 2 run from the implementation pass |
| `riwork-swift-interop-_twr8h09` | protocol 1 run from the implementation pass |

Each path was under `/var/folders/bb/kghlxdk94hj1nkcmwsr90r2c0000gn/T/`. A
following existence check printed `removed` for all four. The production
directory mtime was still `1790643191`.

## Limitations

These are properties of the protocol, confirmed by the tests above rather than
failures of this run.

- `pair` defaults to protocol 1. A v1 PSK still decrypts recordings of that
  device. Moving a phone to v2 is revoke plus a new pairing.
- The invite link is a bearer token until the first successful redeem. Expiry,
  a second presentation, and an overlapping redeem do not create a second root.
  A wrong proof returns `invite_rejected` and leaves the invite pending.
- The desktop stores the consumed invite before it sends `pair_accept`. A crash
  in between burns the invite.
- The phone export file is not rewritten. After redeem it still holds
  `invite_secret` and `relay_token`. Replaying the secret fails. The token can
  still occupy the route. Delete the export.
- A stolen root does not decrypt a finished session whose ephemeral scalars are
  gone. The same root can impersonate either side in a later handshake. There
  is no separate identity signature and no root ratchet.
- Same-process overlap uses an in-memory claim, so the loser sees `invite_race`
  while that claim is held, or `invite_replay` after commit. Separate processes
  do not share the claim set; `config.lock` serializes the commit and the loser
  sees `invite_replay`. macOS `flock` does not serialize threads in one process.
- An expired invite is wiped before the in-process claim, so expiry wins over a
  race and a clock rollback does not revive it.
- Copies of the ephemeral scalars in this code are zeroized after the shared
  secret is mixed. `x25519-dalek` and CryptoKit may retain their own copies.
  An all-zero Diffie-Hellman result is rejected.
- The relay sees route ids, roles, registration tokens, public keys, MACs,
  ciphertext, sizes, and timing. It does not see plaintext, the invite secret,
  the root, or ephemeral private keys, and it does not log payloads.
- `shell.input` remains arbitrary command execution as the desktop user.
- `remote/tests/isolated_e2e.rs` stays ignored unless `RIWORK_TEST_CLI` is set.
  This run's real continuation is `ios/scripts/local-smoke.py`.
- The signed simulator run covers `RiWorkAppTests`. The Xcode `RiWorkCoreTests`
  target was not launched; those sources ran under `swift test`. No physical
  device was installed.
