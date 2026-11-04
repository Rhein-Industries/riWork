# Hosts, agents and recovery

Open **Global → Agents & hosts** for local and paired-host session metadata.
Filter working agents, sessions needing attention, or unseen completions. Opening
a row routes to its owning host/project. A missing owner never selects a local
project as a fallback.

Keep existing encrypted relay pairings, or add an SSH route with
`somtus remote hosts add-ssh --target SSH_ALIAS`. See [SSH hosts](ssh-hosts.md)
for executable paths and setup. Update both ends to use remote chats and the
versioned host metadata API. [ACP](acp.md) adds compatible structured agents;
[OSC 7501](program-status.md) lets terminal programs explicitly report status.

Each execution host publishes a durable UUID in `RIWORK_HOME/host-identity`.
Connection registry IDs are routing keys; they are separate from execution
identity. The metadata client pins the identity received from each connection,
and refuses a changed identity until the connection is explicitly replaced.
An unreadable identity file is an error, not silently replaced with a new UUID.

Connection health (`connecting`, `online`, `backoff`, `blocked`), data freshness
(`unknown`, `current`, `stale`) and agent execution state are independent. A lost
connection retains the last good metadata and its receive time. It never marks
work completed. The desktop cache writes state and receipt time atomically and
loads it as stale after restart. Routine metadata writes are coalesced over ten seconds; completion acknowledgements persist immediately. Completion acknowledgements compare the exact
completion token and include host identity and session kind, so another host's
same session ID cannot clear attention. They are local to this client.

One process service serves all windows and mods. At most four host reads run at
once, with one request per host. Reads stop when no view subscribes. Local reads
run every two seconds; remote reads every five seconds while observed, with
backoff after failure. The metadata protocol (`Hosts.local`, version 1 over
`native.invoke`) is additive and advertised by the connector. Older hosts show
that they need an update. Snapshot subscriptions carry revisions; slow consumers
receive a replacement snapshot rather than a partial event history. Rows retain their keyed native views during status refreshes.

The metadata wire is bounded to 512 sessions per host and 32 remote hosts, and
marks incomplete inventories. It excludes transcripts, paths, credential data
and approval payloads. Native chats use provider state; terminal programs use
explicit OSC 7501 reports or existing lifecycle hooks. Missing observations stay
unknown. The originating host owns the agent; closing a client view does not stop
it. Native agent integrations remain separate from MCP tool access and terminal
status reporting.

Mods request `hosts.read` and import `hosts.get()` / `hosts.subscribe(listener)`
from their generated `addin-api.ts`. Subscriptions return an unsubscribe function;
closing the view or revoking trust also stops delivery. `workspace.read` alone
does not expose other hosts. See `examples/addins/host-agents` for a complete mod.

Remote desktop chats use host-qualified tab and draft identities, retain event
cursors across reconnects, and send mutations once. A missing command reply is
uncertain; transport recovery never blindly repeats a prompt or approval.
Unsupported remote actions are explicitly unavailable rather than acting on the
local filesystem or account.

## Verification

The November 4, 2026 release passed the targeted host identity/cache, monitoring,
remote chat/recovery, SSH transport, OSC status, ACP lifecycle, mod permission,
headless CLI, UI routing and iOS compatibility checks. The signed desktop release
was reloaded with ten existing windows and all 42 chat/terminal tab references
preserved; window positions and pane layouts were unchanged. The overview was
also inspected in a native fixture through Cua.ai Driver.

The full core suite still has seven failures in pre-existing layout, settings
and theme tests. The same seven failures reproduce on the untouched Git baseline.
SSH coverage uses a simulated login boundary and real remote daemon/RPC/terminal
processes; a real server login has not been exercised. ACP tests use a local
protocol fixture, without paid model calls. See the linked transport and provider
guides for setup requirements and unsupported operations.
