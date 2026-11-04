# Somtus hosts over SSH

SSH is an alternative route to an existing Somtus installation. It uses your SSH
configuration, agent and known-host keys. Somtus does not install software, copy
credentials, accept new SSH host keys, or start a listening service on the host.
The remote account must already have compatible `somtus` (or `somtus-cli`) and
`somtus-remote` executables, and the CLI must expose `Hosts.local`.

First establish normal SSH access and trust the host outside Somtus. Noninteractive
login must work: the transport runs `ssh -T` with `BatchMode=yes` and
`StrictHostKeyChecking=yes`. SSH config aliases are supported. Password prompts and
interactive host-key confirmation are deliberately unavailable in this route.

Register from the client:

```sh
somtus remote hosts add-ssh --target my-server --label 'Build server'
```

When the remote executables are not on the noninteractive login PATH, give their
absolute paths. For a macOS app bundle, for example:

```sh
somtus remote hosts add-ssh --target my-mac --label 'Studio' \
  --remote-bin '/Applications/Somtus.app/Contents/MacOS/somtus-remote' \
  --somtus '/Applications/Somtus.app/Contents/MacOS/somtus'
```

Use `--home /absolute/remote/state/directory` only when the remote account uses a
nondefault `RIWORK_HOME`. Paths are quoted as literal POSIX shell arguments; `~`
and environment-variable expansion inside path options are not performed. This
route therefore requires a POSIX-compatible remote login shell. The standalone
`somtus-remote hosts add-ssh` command accepts the same options.

`somtus remote hosts list --json` includes `transport: "ssh"` and the pinned
`host_id`. Its `id` is the route's registry identifier; `host_id` is the stable
installation identity returned by `Hosts.local`. A relay route and an SSH route
can refer to the same installation without changing that identity. Existing
encrypted relay pairings and their registry records remain supported.

The shared per-host client daemon reconnects SSH automatically. Each connection
starts `somtus-remote connect` over SSH standard input/output and verifies the
pinned identity before publishing the connection or sending application requests.
If an SSH alias starts reaching a different Somtus installation, requests are
refused. After deliberately replacing a host, remove its old route with
`somtus remote hosts remove ROUTE_ID`, then add it again to establish the new pin.
Host-key changes still require the normal SSH trust workflow.

The connection carries the existing bounded JSON RPC methods: native module
calls, chat history/events/commands and terminal streams use the same handlers as
the relay. SSH authenticates the operating-system account; it does not create a
relay pairing or grant a phone any additional authorization. Stream frames and
in-flight queues have size limits. Keepalives detect silent failures, and the
client uses bounded backoff to reconnect.

Requests are never replayed after reconnect. A request that reached the host can
finish even if its answer is lost; a timeout is not proof it did not run. Queued
work is discarded once disconnect is observed, while already accepted ordered
work finishes. Bounded admission can delay EOF observation when the queue is
full. Disconnect closes that connection's terminal viewers; host tmux programs
and persistent chat sessions remain owned by the host. The UI reattaches viewers
on a new connection, and chat event offsets support catching up without replaying
commands. Reconnecting is not evidence that an agent completed its task.

Verification uses a fake SSH executable at the process boundary, the real Somtus
remote client daemon and stdio server, a fixture CLI, and an isolated real tmux
server. Tests cover chat reads, terminal writes/reads, reconnection, retained host
programs, changed-identity refusal before mutation, cancellation before dispatch,
frame limits and existing relay registry compatibility. They do not test a real
SSH server's login or host-key configuration.

## Mac-to-Mac over Tailscale

The current path is normal SSH over the tailnet. A direct authenticated Somtus
network listener and automatic host discovery are not implemented. Real tailnet
connectivity has not yet been tested; the integration fixture described above
tests the application protocol after the SSH process boundary.

1. Install or update both Macs to the same Somtus build, including the bundled
   `somtus-remote`. On the execution Mac, set up tmux and the providers you want to
   run, and verify local Somtus sessions work under the account you will SSH into.
   Projects, provider logins and sessions remain on that Mac.
2. Connect both Macs to Tailscale and permit SSH traffic between them in your
   tailnet policy. On the execution Mac, enable **System Settings → General →
   Sharing → Remote Login**, restricting access to the intended account with
   **Only these users**. This uses macOS SSH; enabling Tailscale's separate SSH
   server is unnecessary. See [Apple's Remote Login instructions](https://support.apple.com/guide/mac-help/allow-a-remote-computer-to-access-your-mac-mchlp1066/mac).
3. Identify that account's short username and the Mac's MagicDNS name, for example
   `alice@studio-mac`. A full `machine.tailnet.ts.net` name or Tailscale IP also
   works when reachable. See [MagicDNS](https://tailscale.com/docs/features/magicdns).
   Configure SSH key authentication, with any required key unlocked in an agent
   available to the Somtus process. First connect interactively and verify the
   host fingerprint through a trusted source before accepting it:

   ```sh
   ssh alice@studio-mac
   ```

   Exit that session, then confirm unattended login from the client Mac:

   ```sh
   ssh -T -o BatchMode=yes -o StrictHostKeyChecking=yes alice@studio-mac true
   ```

4. On the client Mac, register the execution Mac using the installed bundle paths:

   ```sh
   somtus remote hosts add-ssh --target alice@studio-mac --label 'Studio Mac' \
     --remote-bin '/Applications/Somtus.app/Contents/MacOS/somtus-remote' \
     --somtus '/Applications/Somtus.app/Contents/MacOS/somtus'
   somtus remote hosts list --json
   ```

   Adjust paths if the app is elsewhere. If `somtus` is not on the client PATH,
   invoke `/Applications/Somtus.app/Contents/MacOS/somtus` instead. Select the
   registered Mac in Somtus's host selector to open its projects and sessions;
   the Agents view also includes its reported activity. Registration currently
   uses the CLI; the existing pairing-link form is for relay hosts.

Tailscale-specific limitations: Somtus invokes `ssh`, not `tailscale ssh`. A
userspace-only Tailscale client or unavailable OS MagicDNS needs working SSH
proxy/DNS configuration; there is no automatic wrapper fallback. The wrapper's
additional capabilities are documented in the [Tailscale CLI reference](https://tailscale.com/docs/reference/tailscale-cli#ssh).
If using [Tailscale SSH check mode](https://tailscale.com/docs/features/tailscale-ssh),
complete browser reauthentication outside Somtus first. Somtus currently discards
SSH stderr, so it cannot display the authentication URL, and its application
handshake times out after 15 seconds. Policies requiring a fresh check on every
connection do not fit unattended reconnects. For failures, run SSH interactively
or with `-v`; Somtus's current generic errors do not distinguish DNS, host-key,
authentication, tailnet-policy or missing-executable problems.
