# Desktop Automations

Open **Automations** from the native application menu, a pane menu, or **Cmd+Shift+S**. Choose a title, a single-line prompt, and a future first run. Run once, hourly, every 24 hours, every seven days, or at an elapsed interval of at least five minutes. Exact times include a UTC offset; recurring runs keep their UTC cadence across daylight-saving changes.

**New project chat** is the default destination. Each occurrence creates an ordinary chat in the selected project's root and submits one prompt. Choose Codex or Claude, optional model/effort/fast settings, and an explicit permission mode; the default is **Supervised**. Codex uses the project-selected account, pinned at save time. An options-only edit keeps that account and destination identity. Switching providers or explicitly choosing a destination binds it again.

**Existing AI shell** selects a specific live Codex or Claude shell. Project scope accepts an ordinary project-root shell without a worktree, or a project orchestrator. Workspace scope requires the exact project/worktree worker. App scope selects its global orchestrator shell. Existing schedules retain their older destination semantics and identities. Shell dispatch preserves the idle, approval, draft, provider/account, creation, pane and directory checks; scheduling never changes the shell's permissions.

The list shows the next run, last outcome, prompt and effective settings. Edit, pause/resume or delete using current revisions; conflicts preserve the draft for review. **Open created chat** opens the recorded result in an app tab. The Project chats list also retains earlier results after another occurrence or schedule deletion.

## CLI and MCP

`automation` and `schedule` are aliases. Existing schedule commands and MCP tool names remain available. Fresh chat creation uses the same schedule service and ledger:

```sh
riwork automation create --new-chat --scope project --project PROJECT_UUID \
  --title 'Daily review' --prompt 'Review the project status' \
  --at FUTURE_RFC3339 --every-minutes 1440 \
  --provider codex --permission supervised --model MODEL --effort high --fast --json

riwork automation create --existing-shell --scope project --project PROJECT_UUID \
  --shell LIVE_SHELL_UUID --title 'Shell review' --prompt 'Review the project status' \
  --at FUTURE_RFC3339 --json
```

Use `--account ACCOUNT_ID` for an explicit saved Codex account. Omit model/effort/fast to use defaults. The fresh-chat `target.shell_id` is a stable destination UUID for the existing revision-checked update/pause/resume/delete commands, not a live shell. The result's `last_run.created_chat_id` identifies the created chat.

`riwork_schedule_create` accepts `destination: "new_chat"`, project scope/project_id, provider, model, effort, fast, permission and codex_account_id; shell_id must be omitted. `destination: "existing_shell"` requires an explicit live shell_id and pins its shell kind. Omitting destination keeps legacy semantics, including project-orchestrator-only shell matching. New fields are additive; old saved schedules need no migration.

## Dispatch and compatibility

RiWork's existing desktop scheduler runs while the app is open. It skips missed occurrences beyond the five-minute grace window, backs off unavailable/busy targets, and admits at most four claimed attempts per minute.

Before claiming a fresh-chat occurrence, the client sends a non-mutating `capabilities` operation to the same host connection that will create the chat. Only an affirmative boolean `identified_create: true` permits creation. Old hosts, absent/false/malformed responses, refusals, disconnects and bounded timeouts defer without Create, Command, a durable dispatch claim or a consumed rate slot. No running host is automatically replaced or restarted.

The dispatch claim persists a caller-owned chat UUID before Create. A compatible host never overwrites an existing chat with that UUID. The returned ID, project/root/account/provider/permission and model/effort/fast must match before Send. An interrupted claim or ambiguous create/send outcome pauses for review and never automatically retries that occurrence. A future reviewed edit can schedule another occurrence. Submitted means the prompt was recorded, not that the agent finished its work.

Unsupported model or provider settings can fail initialization; the recorded chat ID remains available for inspection. The host's actual support, rather than the executable's version on disk, governs fresh-chat availability. Legacy scheduled destinations keep their existing dispatch paths.
