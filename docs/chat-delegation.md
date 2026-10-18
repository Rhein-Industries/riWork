# Durable native chat delegation

RiWork's MCP server exposes three tools for an agent running inside a native
RiWork parent chat:

| Tool | Arguments | Behavior |
| --- | --- | --- |
| `riwork_delegate_task` | `request_id`, `prompt`; optional `title`, `provider`, `model`, `effort`, `approval_mode`, `wait_ms` | Reserves one task and creates a native child chat for its assignment. |
| `riwork_delegated_task_status` | `task_id`; optional `wait_ms` | Reads the saved task, receipts, and child log. |
| `riwork_delegated_task_cancel` | `task_id` | Records cancellation and requests a stop for this assignment. |

`wait_ms` defaults to zero and is limited to 30,000. The parent is resolved
from the invocation context, never from a model-supplied parent selector.
New assignments require an active parent turn. Existing shell-worker tools
continue to work independently.

Use a stable `request_id` for one assignment. Repeating it with the same
arguments observes the original task, including after a host restart. Changing
the arguments under that ID is a conflict. Do not change IDs to retry an
uncertain launch: it could create a second worker.

The child inherits the parent's project, worktree, working directory, and
permission ceiling. The default provider is the parent's provider; applicable
model/account settings follow it when the provider is unchanged. An explicit
approval mode can restrict the child, but cannot grant broader permissions.
The parent identity, turn, scope, and current permission ceiling are checked
when the host admits the assignment. Cancellation is bound to the assignment's
input operation and turn, so it cannot stop a later unrelated child turn.

## Results and recovery

Replies include stable task, parent-chat, parent-turn, and child-chat IDs,
assignment, status, and timestamps. A completed result records its turn ID,
completion sequence, outcome, text, and whether the text was truncated. Result
text is limited to 64 KiB. A saved result remains available after the child
chat is deleted. The result belongs to the assignment's first turn; later
manual messages in that child do not replace it.

Statuses are `running`, `waiting`, `completed`, `interrupted`, `failed`,
`cancelling`, `cancelled`, and `uncertain`. `waiting` can mean the child needs
an approval or answer; normal RiWork decision controls handle that interaction.
No approval is granted automatically. Status reads and bounded waits never
start a host or provider. Uncertain dispatches and unavailable providers are
reported for inspection; they are not automatically retried or resumed.
Child logs are attributed to an assignment only after its Send receipt is
accepted. An uncertain or rejected assignment cannot claim another input's
result merely because it appears first in that child's history.

Task records live under `delegated-tasks/` in the RiWork home. Reservation is
synced before creation or sending. Updates use per-task filesystem locks and
atomic replacement. A delayed Send acknowledgment cannot overwrite a saved
result or cancellation request.

## Command receipts

The chat host also supports optional operation IDs for identified commands.
Receipts live under `chat-command-receipts/` and bind the operation ID to the
chat, canonical command digest, and dispatch guards. They retain no command
text. Reusing the ID for different input is an error.

The host syncs an `uncertain` reservation before dispatch, then records
`accepted` or a known pre-dispatch `rejected` result. `accepted` means the host
dispatched the command successfully; it does not mean the provider completed
the requested work. A crash between dispatch and receipt settlement remains
`uncertain`. Repeating that operation observes its receipt and never dispatches
it again. Receipts survive chat deletion. Commands without operation IDs keep
their existing behavior.

Clients probe support before identified dispatch. Guarded commands use their
own wire operation so an older host cannot silently ignore the guard. An
unsupported host returns an error instead of falling back to an ordinary Send
or Stop.

This service does not provide workspace checkpoints, automatic retries,
scheduled recovery, or a general dependency graph.
