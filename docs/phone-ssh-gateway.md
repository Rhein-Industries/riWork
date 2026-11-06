# Phone access through a Somtus desktop

A phone paired with one Somtus desktop can use that desktop's registered SSH
hosts. The phone discovers them after connecting and shows each host with
`via <desktop name>` in the existing host list. Projects, terminals, chats,
uploads and dashboard actions keep the selected host's identity.

The first desktop is the gateway and must stay reachable. This uses the
phone's existing authenticated connection to it, then the desktop's existing
SSH connection over Tailscale. It does not add a hosted Somtus relay, change
phone pairing, distribute SSH keys, or discover arbitrary tailnet machines.
Only hosts explicitly registered on the gateway are exposed. Routing is one
hop; a target's own host registry is not traversed.

## Register a host

Install the updated Somtus remote companion on the gateway and each target,
and use the updated iOS app. SSH login and host-key trust must already work
from the gateway's account. Tailscale supplies network connectivity; Somtus
continues to require authenticated SSH and a pinned Somtus host identity.

From that account on the gateway:

```sh
somtus remote hosts add-ssh --target user@tailscale-host --label 'Studio' \
  --remote-bin /Applications/Somtus.app/Contents/MacOS/somtus-remote \
  --somtus /Applications/Somtus.app/Contents/MacOS/somtus
somtus remote hosts list --json
```

An existing SSH config alias also works as `--target`. For Linux or a custom
installation, use the installed binary paths; `--home` selects a nondefault
remote `RIWORK_HOME`. Registration contacts the target and pins its identity.
It does not install software or accept an unknown SSH host key.

Connect the phone to the gateway as usual. A discovered target does not need
another QR code or pairing secret. Removing its registration with
`somtus remote hosts remove ROUTE_UUID` prevents further forwarded requests,
including requests on an already-open routed session. Revoking the phone on
the gateway also ends its access through the gateway.

## Protocol

The extension is additive to remote protocol v1's inner RPC and works within
the existing authenticated phone session. `ready.features.hosts = 1` announces
support. Older companions remain usable for their own projects and do not
offer gateway discovery.

| Method | Parameters | Result |
| --- | --- | --- |
| `hosts.list` | `{}` | `{v:1, hosts:[{route_id, host_id, label, transport:"ssh"}]}` |
| `hosts.open` | `{route_id, host_id}` | `{session, route_id, host_id, features}` |
| `hosts.call` | `{session, request_id, method, params}` | The target's ordinary RPC result or error |
| `hosts.close` | `{session}` | `{}` |

`route_id` names an existing gateway registry entry. `host_id` is that entry's
pinned execution host, checked again when SSH connects. The phone supplies no
hostname, command, executable path, SSH option, or credentials. Discovery
returns only the four listed metadata fields.

The opaque `session` belongs to one authenticated phone connection and one
exact registered route. Another phone, another connection epoch, a removed
route or a changed target cannot reuse it. At most four routed sessions are
open per phone connection. Gateway loss closes the downstream sessions;
reconnection requires a new open and never automatically repeats mutations.
Repeating `hosts.open` for the same route within that phone connection reuses
its session. Closing one route does not wait behind a command running on it or
close the phone's connection to other hosts. A lost reply to an accepted
command is reported as uncertain.

`request_id` is the original inner request's canonical UUID. The wrapper has
its own fresh RPC UUID. Preserving the inner ID keeps the target's existing
input deduplication intact. The gateway retains the inner request's ordered,
read or long-poll classification. Only normal phone RPCs are forwarded;
recursive `hosts.*`, desktop-only `pty.*` and `link.configure` are refused.
Compression remains a property of the phone's gateway connection.

Each phone receives a separate SSH session. Downstream client identity is
stable for that phone and registered target, while viewport ownership is
isolated by connection. Uploads keep that stable owner so a reconnect can
resume them and completed files retain their normal lifetime after the phone
disconnects. Target RPC validation and its native-method allowlist still apply.
The gateway can read forwarded content, as it already controls the SSH
account; this is not end-to-end encryption between the phone and target with
an untrusted gateway.

Pairing a phone therefore also grants access to the SSH hosts registered on
its gateway, including ones registered later. Merely belonging to the same
Tailscale network grants no Somtus authority.

## Verification on 2026-11-06

Selected validation passed: 133 remote library tests, 32 remote integration
tests, 81 Swift Core connection/protocol/upload tests, 28 iOS app tests and two
mobile UI tests. The gateway integration uses the real encrypted connector
and real account RPC endpoints with an isolated SSH executable shim and
temporary homes. It covers two phones, two targets, cross-phone token refusal,
route removal, reconnect epochs, revocation and closing a route during a slow
mutation without disrupting another target. It does not claim a live physical
phone/Tailscale deployment.

The full Swift Core run executed 1,253 tests and found four assertions failing
in two unchanged `NewChatChoiceTests`: their expected provider lists omit ACP.
Those test and implementation files match the base commit. An existing remote
compression-boundary test also failed once under concurrent build load and
passed in the final serial library run. This is scoped validation, not a claim
that every repository test passes.

Build logs, test results and source fingerprints are under the ignored
`target/phone-gateway-2026-11-06/` directory. The standalone release companion
and the release iOS simulator app are built locally; installing them on the
gateway, target hosts and physical phone is a separate deployment step.
