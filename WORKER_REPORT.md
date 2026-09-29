# iOS device acceptance — bd88af67-1afc-43df-9fc9-3eaf9770963e

Project `39832c2e-23a5-476d-aa8f-5ff34a02d314`. Worktree branch `test/ios-device-acceptance`, base `25f00364fd163ad6b44e4cff4338d92d294f3a61`. Protocol v2 task `a5622e56-4ad9-45ad-9b45-407ad7ec5f15` was left untouched at `30a4feb` on `feature/remote-protocol-v2` (clean worktree at report time). No merge, no push, no App Store upload.

A DomiMax relay session was not established. Simulator checks below are labeled as simulator checks.

## Device and signing

DomiMax, UDID `00008150-000E34861EDB401C`, iOS 26.6, devicectl id `88D3F2B2-D2EB-5747-862F-5402E08D981A`, iPhone 17 Pro Max (`iPhone18,2`). It was paired, available, and unlocked (`passcodeRequired` false). LAN address seen for `DomiMax.local` was `192.168.178.166`. This Mac’s `en0` was `192.168.178.175`, hostname `Dominiks-MacBook-Pro`. The firewall was off. Device support for this model and OS was installed.

Disposable bundle installed and later removed: `com.riwork.accept.bd88af67.RiWorkRemote` (framework `com.riwork.accept.bd88af67.RiWorkCore`). That id is not in `ios/project.yml`. `com.riwork.remote` was not installed on DomiMax. The only other third-party app noted on the phone was riScan Capture; it was not opened or removed.

Device build succeeded with `CODE_SIGN_STYLE=Automatic` and `DEVELOPMENT_TEAM=ZR7A22CNVY`. The identity that signed it is SHA1 `E7FA63D9B395990A9D37B4A9D8EDE5E4E3199CF9`, “Apple Development: Dominik Gstoehl (K94D56Z2AA)”, valid through 15 Jul 2027. An older certificate with the same common name (SHA1 `699D297E…`, notAfter 5 Apr 2026) is expired; `security find-certificate -c` returns that one first, while `security find-identity -v` lists the valid identity. The profile used was the Xcode-managed wildcard “iOS Team Provisioning Profile: *”, UUID `a8f2687c-1dde-4a46-8a54-0c83bfab5cc9`, app id `ZR7A22CNVY.*`, expiring 2027-09-09, GetTaskAllow, and it includes this UDID. Setting `PROVISIONING_PROFILE_SPECIFIER` failed because that profile is Xcode-managed and because `RiWorkCore` does not take a provisioning profile.

Debug `Info-Debug.plist` has `NSAllowsLocalNetworking` only. No Info.plist key accepts a self-signed certificate, so a disposable ATS exception would not have made the phone trust the fixture CA. The fixture CA was not installed into the user or system trust store.

## What was fixed on this branch

Reproducible UIText input and scanner-lifecycle bugs:

- `CommandField` turns off inline prediction, math-expression completion, and writing tools, in addition to the existing smart-punctuation and autocapitalization settings.
- Pairing JSON is entered in `PairingCodeField`, a UIKit text view with those same traits, data detectors off, and accessibility label “Pairing JSON or deep link”. Paste still assigns the string directly.
- `ScannerStartController` calls `startScanning` from `viewDidAppear` (it throws if started from `makeUIViewController` before the controller is in a window), stops on disappear, and on a start failure shows “Camera could not start. Paste the pairing code instead.” without marking itself scanning.
- `riwork-ios-smoke` accepts optional `--idle-seconds N` and fails if the socket drops during that wait. The flag is opt-in.

A DEBUG-only launch probe used during the run was removed before this commit. It is not in the tree.

Focused re-run after that removal, iPhone 17 Pro simulator `45D942B2-ABE6-4C4F-8D13-E252AF668880` (iOS 26.0):

```
xcodebuild test -project ios/RiWorkRemote.xcodeproj -scheme RiWorkRemote \
  -destination 'platform=iOS Simulator,id=45D942B2-ABE6-4C4F-8D13-E252AF668880' \
  -only-testing:RiWorkAppTests/CommandFieldTests \
  -derivedDataPath /tmp/riwork-accept-cmdfield-dd \
  -resultBundlePath /tmp/riwork-accept-cmdfield.xcresult
```

`CommandFieldTests` 6 tests passed at 2026-09-29 02:58:55. ** TEST SUCCEEDED **. Exit 0.

Earlier the same day, the full `RiWorkRemote` scheme on that simulator passed 31 core tests and 25 app tests, 0 failures. Result bundle: `/tmp/riwork-ios-dd/Logs/Test/Test-RiWorkRemote-2026.09.29_02-33-03-+0200.xcresult`. The three new app tests are in that 25.

## Acceptance checks

Isolated `RIWORK_HOME`, relay, project, and shell were used for every desktop command. The user home `/Users/dominik/.local/share/riwork` was not the target. Pairing files and pair URLs were mode 600 under temp directories and are not copied here.

### Pairing sheet, QR, and paste

On DomiMax this did not complete.

- A `ws://` relay on the LAN, with the development switch set in the launch environment, was rejected by `Pairing.validate`. App text: `Invalid pairing: Use a secure wss:// relay. Local ws:// requires the development switch.` That matches the protocol: cleartext is only `127.0.0.1`, `localhost`, `[::1]`, or `::1`.
- A `wss://192.168.178.175` fixture (short-lived CA, server SAN for that IP and `Dominiks-MacBook-Pro.local`, notAfter 1 Oct 2026, byte-pipe TLS proxy, relay on loopback) did not open a session. App text: `The Internet connection appears to be offline.` Proxy log: `handshake SSLEOFError`. The relay logged no mobile registration. `devicectl` cannot accept the local-network permission dialog or aim the camera. A person at the phone has to allow local network for this disposable app before any further `wss` attempt is meaningful. `devicectl device process launch --timeout` then exits because the SwiftUI process does not quit; the `ACCEPT` line above is the app result, and the command itself reported a 25s timeout.
- QR aim and the on-phone pairing confirmation were not performed. There is no devicectl screenshot, and no host-side tool here can tap the phone.

On the iPhone 17 Pro simulator (not DomiMax), `ws://127.0.0.1` with the development switch is the supported path. Cua driver session `ios-accept` drove Simulator.app only. Observed:

- The `riwork://pair` link opened the sheet “A link asked to pair this device” with the text view labeled “Pairing JSON or deep link”.
- Scan QR opened “Scan desktop code” / “Camera unavailable” / “QR scanning requires a supported iPhone or iPad. Paste the pairing code on this device.” Close returned to the sheet. DataScanner is unsupported on this simulator, so the appear-to-start host was not on screen.
- “Allow local development relay” went from off to on and showed relay `127.0.0.1` (local, unencrypted), device “Sim fixture”, desktop `a54bd098`, and “Pair with 127.0.0.1”.
- The confirmation alert named that device and desktop. Pair reached PROJECTS / Connected and the fixture project path. Opening it showed terminal `77c6ec0a-52c1-4f95-87b6-d585ff2a5993` at 53 columns by 41 rows. Connector: device `66c6bdf7-67ea-499a-b95c-93669d471134` enabled, then authenticated.

### One persistent shell

Simulator UI session: tmux pane `%0`, PID `96217`, shell `77c6ec0a-52c1-4f95-87b6-d585ff2a5993`. The viewport resize and the later restore kept that pane and PID.

`ios/scripts/local-smoke.py --viewport` against a separate isolated home: `PASS: real PTY 43x17; desktop restored to 100x30; same pane %0 and PID 95342`. One executed line. The same session stayed alive. A revoked device could not authenticate. Evidence directory `riwork-swift-interop-n1ocmmj_` was removed at cleanup after that shell list was empty.

DomiMax did not open a shell.

### Network change and duplicate registration

No interface change was performed on the phone, because there was no device session.

On the Mac, against an isolated loopback relay:

- A second live mobile registration exited 1. Client text, from close code 1005: “The relay closed the connection. This device may no longer be authorized, another connection for it may already be open, or the relay could not deliver a message.”
- After SIGSTOP of the first client for 32s, the new client exited 0, authenticated, and held a 1s idle. Relay log: `relay: replacing silent mobile socket on route 1a42447e-ecee-4ede-b6e9-91979e9ec1fb`.

`remote/src/relay.rs` uses `replace_after` of 30 seconds. The duplicate close has no distinct reason. That is reported to the parent below and was not edited.

### Keepalive

macOS `URLSession` smoke client: `IDLE_EXIT 0 ELAPSED 45.84` and `Idle keepalive held for 45s`, then `PASS`.

iOS Simulator app process, same client stack: `ACCEPT idle held 40s`, then `ACCEPT done`.

`URLSessionConfiguration` `timeoutIntervalForRequest` stays 30. The hypothesis that control-frame pings fail to reset that idle timer was not reproduced. DomiMax keepalive was not measured.

### Background and resume

Simulator, same shell as the UI pairing. Before the Home button, the pane was `53 41`, PID `96217`, pane `%0`. The Cua press of the simulator Home button returned with the pane already `100 30`, same PID and pane. Restore finished within that click round trip, which is the background viewport release, not the vanished-phone path (connector `mobile_active` 20s plus the 12s viewport lease).

Resume: `simctl launch` of the disposable bundle printed pid `82800`. The accessibility tree then showed heading RIWORK, “NO DESKTOP CONNECTED”, and “Pair a desktop”. That is the empty-library overlay. It is not “SAVED PAIRINGS UNAVAILABLE”. The pane stayed `100 30` / PID `96217` / `%0` because nothing reattached.

`RemoteModel.add` persists before the sheet dismisses, and the in-session project list had already appeared, so the write returned success in that process. The next process’s `loadLibrary` saw a missing item or an empty library (`errSecItemNotFound` becomes an empty library; other keychain errors set the failure view). This was not reproduced a second time and was not changed. Keychain service remains `com.riwork.remote.desktops`, no access group, `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`. A later disposable-bundle rerun is recorded at the end of this report. It also failed to reproduce the empty library, and no keychain change was made.

### Duplicate send

Simulator app process, shell `77c6ec0a-52c1-4f95-87b6-d585ff2a5993`, input id `11111111-1111-4111-8111-111111111111`. The marker file was one byte (`x`). The shell contained the `printf` command once (`PRINTF_X_COUNT 1`). The connector authenticated, disconnected, authenticated again, and the second send with the same id was the cached desktop result. Uncertain outcomes are not auto-resent; that path was not changed.

### Cleanup

```
xcrun simctl terminate 45D942B2-ABE6-4C4F-8D13-E252AF668880 com.riwork.accept.bd88af67.RiWorkRemote
xcrun simctl uninstall 45D942B2-ABE6-4C4F-8D13-E252AF668880 com.riwork.accept.bd88af67.RiWorkRemote
xcrun devicectl device uninstall app --device 00008150-000E34861EDB401C com.riwork.accept.bd88af67.RiWorkRemote --timeout 60
```

Simulator `get_app_container` for that bundle then failed with `NSPOSIXErrorDomain` code 2. Device uninstall JSON listed that bundle under `uninstalledApplications`. A later launch returned CoreDeviceError 10002: the application is not installed (OSStatus -10814).

The simulator fixture holder was SIGTERM’d. Its `finally` closed shell `77c6ec0a-52c1-4f95-87b6-d585ff2a5993`. `riwork shell list --all --json` on that temporary `RIWORK_HOME` returned `[]`. The relay and connector processes for `127.0.0.1:56559` were gone. Cua session `ios-accept` was ended.

Removed this run’s temp homes and secret files: `riwork-dev-4d__u016`, `riwork-idle-_j_k6afu`, `riwork-net-3y065tq3`, `riwork-net-45ww0b5n`, `riwork-net-v3qidxum`, `riwork-sim-qxx0gf5x`, `riwork-swift-interop-n1ocmmj_`, and `/tmp/riwork-accept` (pair URL, pairing JSON, TLS key and certificate, accessibility dumps). Older `riwork-*` temp directories were left alone. `com.riwork.remote` was not uninstalled.

User home name-only mtimes, contents not read. No `remote/` directory and no `devices.json` at the start or the end. Every `riwork` invocation in this task set `RIWORK_HOME` to a temp directory. These epoch mtimes moved by more than one second while the task ran, which matches a desktop app rewriting its own state: the directory `1790641963` → `1790643475`, `layouts.json` `1790641764` → `1790643475`, `sessions.json` `1790641928` → `1790643423`, `state.json` `1790642113` → `1790643451`, `terminal-control` `1790641962` → `1790642502`. User shells were not listed.

## For the protocol v2 parent

Not changed on this branch. v2 already edits the files a fix would touch.

1. Duplicate mobile registration has no stable close reason. Measured: a live second registration is rejected as close 1005, and `ios/Core/JSONValue.swift` maps 1005 to one sentence covering unauthorized, duplicate, and queue-full. After the first socket is silent for 32 seconds the relay replaces it (`replace_after` is 30s) and the new client authenticates. A distinct close code or reason would let the client wait for replacement. Retrying every connect failure for 35 seconds would also stall a revoked device. `remote/src/relay.rs` was not edited.

2. `riwork-remote start` against `wss://` panics before TCP. Captured from the device-fixture connector log:

```
thread 'tokio-rt-worker' panicked at .../rustls-0.23.45/src/crypto/mod.rs:249:14:
Could not automatically determine the process-level CryptoProvider from Rustls crate features.
Call CryptoProvider::install_default() before this point to select a provider manually,
or make sure exactly one of the 'aws-lc-rs' and 'ring' features is enabled.
```

`tokio-tungstenite` is pinned to feature `rustls-tls-native-roots`. The rustls 0.23 crates are built without `aws-lc-rs` and without `ring`, so `CryptoProvider::get_default_or_install_from_crate_features` panics. Loopback `ws://` does not use that path and worked. A wire-compatible fix is to enable exactly one of those rustls features. v2 commit `8a9de86` already adds `x25519-dalek` and `zeroize` to `remote/Cargo.toml` and expands `Cargo.lock`, `connector.rs`, and the crypto modules. At `30a4feb` the tungstenite feature list is still only `rustls-tls-native-roots`, so this panic is still present on that branch. It was left for that task.

On the failed DomiMax `wss` attempt the phone aborted during the handshake (`SSLEOFError`) and the connector panicked on its own `wss` connect, so neither side completed registration.

Merge notes when v2 lands on this branch:

- `ios/RiWorkRemote/PairingViews.swift`: this branch replaces the pairing `TextEditor`; v2 adds an invite row in `PairingDetails`. The hunks are in different places.
- `ios/Smoke/main.swift`: both sides edit the usage string and the block before `disconnect`. Keep this branch’s `--idle-seconds` and v2’s `--write-established` plus the connect call that returns the established pairing.
- Do not take this branch’s `RelayClient.connect` signature over v2’s.

## Follow-up rerun: empty pairing library

The empty-library observation above was not reproduced. No product change was made. `KeychainStore` and `RemoteModel.loadLibrary` stay as committed in `f5aeec7`. No regression test was added: a test of the current read path would not lock a failure this rerun could not produce. `remote/` was not edited. The protocol branch was not edited. The WSS rustls panic stays with that task.

The rerun used a new disposable bundle, `com.riwork.accept.kc88af67.RiWorkRemote` (framework `com.riwork.accept.kc88af67.RiWorkCore`). That id is not in `ios/project.yml`. Build was Debug, Sign to Run Locally:

```
xcodebuild -project ios/RiWorkRemote.xcodeproj -scheme RiWorkRemote -configuration Debug \
  -destination 'platform=iOS Simulator,id=45D942B2-ABE6-4C4F-8D13-E252AF668880' \
  -derivedDataPath /tmp/riwork-kc-dd \
  PRODUCT_BUNDLE_IDENTIFIER='com.riwork.accept.kc88af67.$(PRODUCT_NAME:rfc1034identifier)' build
```

Simulator: iPhone 17 Pro, UDID `45D942B2-ABE6-4C4F-8D13-E252AF668880`, iOS 26.0. Cua session `kc-rerun` pressed only the Simulator Home button (Simulator pid `46980`, window `83004`). No `RIWORK_HOME` was created. The relay was not started. DomiMax was not used. The library body was the existing unit-test pairing vector written through `KeychainStore.write`, not a pairing-sheet confirm against a live relay.

A DEBUG probe logged `KC op=… t_ms=… status=… bytes=… service=…` and was removed before this commit. For `op=task` and `op=phase-*`, `status` is `1` or `0` for `UIApplication.shared.isProtectedDataAvailable`, and `bytes` on `task` / `reload-*` is the in-memory desktop count. `load-failure` status `0` means no failure string. SecItem status `0` is `errSecSuccess`. `-25300` is `errSecItemNotFound`. The attribute value logged as `aku` is `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`. `RemoteModel.init` calls `loadLibrary` before the probe’s `.task`, so the `read` at `t_ms=0` is that first `SecItemCopyMatching`. Protected-data samples start in `.task` and on `scenePhase`, after that first read. No pairing material was printed.

Launches used `xcrun simctl launch --console-pty` on that bundle with `--keychain-probe`, plus `--terminate-running-process` except for the one relaunch called out below.

1. Delete of a fixture item left by an earlier uncaptured write in this same install: first read `t_ms=0 status=0 bytes=484`, task desktops `1`, load-failure `0`, protected `1`. Deletes of the probe marker service, the data-protection probe service, and `com.riwork.remote.desktops` each returned status `0`. A second delete of the data-protection item with `kSecUseDataProtectionKeychain` returned `-25300`. `done` at `t_ms=551`.
2. Empty read, new process: `read t_ms=0 status=-25300` on the default service. The same not-found status on the marker at `488`, `541`, `697`, `1498`, and `2541` ms, and again on the default service at `2541`. Task at `t_ms=488` had protected `1` and desktops `0`. `phase-active` at `t_ms=503` had protected `1`. load-failure `0`. `reload-final` desktops `0`. Not-found did not become success.
3. Fast write, new process: first read `t_ms=0 status=-25300`, desktops `0`. Marker `update` `-25300` then `add` status `0` (7 bytes). Data-protection add status `0` (4 bytes). Library `update` `-25300` then `add` status `0` (484 bytes). Immediate reread status `0` bytes `484`. `reload-after-write` desktops `1` at `t_ms=495`.
4. Five following cold launches with `--terminate-running-process`. Each first read was `t_ms=0 status=0 bytes=484`, task desktops `1`, load-failure `0`, protected `1` on the first `.task` sample, marker read status `0` bytes `7` from about `500` ms through about `2600` ms, and attribute queries (marker, data-protection item with the flag, and the same item without the flag) status `0` with `aku`. `reload-final` desktops `1`. No sample moved from not-found to success.
5. One further launch with the write flag found the item already present: `SecItemUpdate` status `0` for the 7-byte marker and the 484-byte library, no `SecItemAdd`, reload desktops `1`. The cold launch after that again read status `0` bytes `484` at `t_ms=0`.
6. Stay process, `--terminate-running-process --keychain-stay`, pid `41216`. First read `t_ms=0 status=0 bytes=484`, task at `t_ms=515` desktops `1`, load-failure `0`, `reload-final` desktops `1` at `t_ms=2619`. Cua Home (`AXPress`, effect unverifiable) logged `phase-inactive` at `t_ms=31144` and `phase-background` at `t_ms=31786`, both protected `1`. `simctl launch` without `--terminate-running-process` returned the same pid `41216`. The probe then logged `phase-inactive` at `t_ms=45511` and `phase-active` at `t_ms=45765`, both protected `1`. The accessibility tree showed heading Desktops and the fixture row “KC probe, example.com”, with “Pair a desktop” still present as the add action. It did not show “NO DESKTOP CONNECTED”.
7. A second Cua Home logged `phase-inactive` at `t_ms=177394` and `phase-background` at `t_ms=178040`, both protected `1`. While that process was still backgrounded, `simctl terminate` exited `0` and pid `41216` was gone. The next `--terminate-running-process` cold read was `t_ms=0 status=0 bytes=484`, task at `t_ms=559` protected `1` and desktops `1`, load-failure `0`, `phase-active` at `t_ms=567` protected `1`, `reload-final` desktops `1` at `t_ms=2655`.

`errSecInteractionNotAllowed` (`-25308`) and `errSecMissingEntitlement` (`-34018`) did not appear. On the simulator, queries with and without `kSecUseDataProtectionKeychain` both returned the probe item once it had been added.

What this rules out on this simulator, for this bundle, is a cross-process loss of a `KeychainStore` write, and a first read that misses the item while protected data is unavailable and then finds it. The non-terminating launch resumed the same pid and kept the library. A real new process, started after the app had been backgrounded with protected data still available, also read the item at `t_ms=0`.

Limit: one simulator and one fresh bundle. The save was `KeychainStore.write` of the unit-test vector, not the original deep-link pairing sheet against the live loopback relay. The matrix was not run on DomiMax. Protected data was not sampled at the instant of the `t_ms=0` read; every later sample in these processes was available, and none was unavailable. The original empty overlay on pid `82800` remains a single unreproduced observation.

Cleanup: `--keychain-delete` removed the marker, the data-protection item, and `com.riwork.remote.desktops` (each status `0`); the flagged second delete returned `-25300`. `simctl uninstall` of `com.riwork.accept.kc88af67.RiWorkRemote` exited `0`. `get_app_container` for that bundle then failed with `NSPOSIXErrorDomain` code `2`. `com.riwork.remote` was still installed and was not uninstalled. Cua session `kc-rerun` was ended. The probe edits to `ios/Core/KeychainStore.swift` and `ios/RiWorkRemote/RiWorkRemoteApp.swift` were reverted. `/tmp/riwork-kc-dd`, `/tmp/riwork-kc-build.log`, `/tmp/riwork-kc-stdout.txt`, and `/tmp/riwork-kc-stderr.txt` were removed. This follow-up did not invoke `riwork` and did not read the user home. Name-only mtimes there moved again after the earlier section (directory `1790643475` → `1790643733`, `layouts.json` `1790643475` → `1790643485`, `state.json` `1790643451` → `1790643733`); `sessions.json` stayed `1790643423` and `terminal-control` stayed `1790642502`.
