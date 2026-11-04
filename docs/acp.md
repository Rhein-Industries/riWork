# ACP chat providers

Somtus can run an installed Agent Client Protocol agent alongside the native Codex,
Claude and Muse adapters. Configure the agent on the host where it will execute:

```sh
somtus chat acp configure --command /absolute/path/to/agent --arg --acp
somtus chat acp show
somtus chat new --provider acp --project PROJECT_ID
```

Use the arguments required by that agent. Each `--arg` supplies one literal argument,
including spaces or text that looks like shell syntax. Somtus does not evaluate a
shell command. Configuration writes `acp.json` in the Somtus state directory and
does not install, authenticate, or start the agent. Sign in through the agent's own
CLI first. The desktop's new-chat provider picker also offers **ACP**; an unconfigured
host shows the setup command.

An already-running chat backend keeps its existing agents alive through an app
update. If it predates ACP, it explicitly refuses ACP creation until that backend
has restarted with the updated build; updating the UI does not interrupt active
chats to force this change.

There is one default ACP executable configuration per host. Every new ACP chat pins
its executable and arguments in its own `acp-launch.json`; changing the default
affects new chats. Agents retain their own settings, credentials, sandbox and tool
configuration. Separate configurations are not an account-isolation guarantee.

The adapter negotiates stable ACP v1, starts a native session, streams text,
reasoning, plans and tool updates, and displays permission requests. Advertised
models become available in Somtus's model catalog; models from a different executable
configuration are not mixed in. Client filesystem and terminal capabilities are
disabled: the agent executes its own tools. Somtus does not infer child-agent
lineage or execution status from tool names.

Supervised mode asks about every permission request the agent sends. Auto-edit
accepts one-time edit requests; Full accepts one-time requests. These choices do
not change the agent's native sandbox. Somtus preserves native permission option
IDs and never turns an approval into an agent's persistent `allow_always` rule.
Cancel answers outstanding permissions and sends `session/cancel`; completion is
reported when the agent acknowledges the original prompt. An unresponsive agent is
stopped after a bounded wait.

After a disconnect or app/host restart, the next message loads the same native
session only if the agent advertised session loading. The saved Somtus transcript
remains visible; the agent's load replay is not appended a second time. Interrupted
prompts are never automatically resubmitted. An unsupported or missing saved session
produces an error instead of silently starting an empty conversation.

This adapter currently supports text prompts. Attachments, steering an active
prompt, reasoning-effort/fast controls, enforced Plan mode, native conversation
forks, compaction and scheduled ACP targets are explicitly unavailable. ACP v2 draft
is not claimed as supported. Protocol and lifecycle tests use a fake local agent,
including a real Somtus chat-host restart; they do not make paid model calls.

Protocol references: [initialization](https://agentclientprotocol.com/protocol/v1/initialization),
[session setup](https://agentclientprotocol.com/protocol/v1/session-setup),
[prompt lifecycle](https://agentclientprotocol.com/protocol/v1/prompt-turn), and
[tool permissions](https://agentclientprotocol.com/protocol/v1/tool-calls).
