# Scheduler reference

Use [Desktop Automations](automations.md) for the current editor, **New project chat** and **Existing AI shell** destinations. Open **Automations** from **Somtus → Automations**, a pane menu, or **Cmd+Shift+S**. This reference covers delivery, persistence, verification and the compatible existing-target CLI/MCP interface.

## Existing-target compatibility

CLI/MCP creation that omits a destination keeps the original scope rules:

| Scope | Existing target when destination is omitted |
| --- | --- |
| App | Global orchestrator session or chat |
| Project | That project's orchestrator session or chat |
| Workspace | Selected worktree and a worker assigned to it |

Explicit **Existing AI shell** creation also accepts an ordinary project-root shell; its directory checks and the fresh-chat workflow are described in [Automations](automations.md). Workspace means Somtus worktree context, not a project window, filesystem folder group, or newly created worker. Workspace dispatch checks that the worker's current directory remains inside that worktree. Selecting an existing target does not create, wake, resume, close or replace a session.

Existing terminal targets support Codex and Claude. Grok and Muse terminals lack the required scheduling identity/readiness contract and are refused by `HarnessKind::schedulable()`. Open the intended terminal harness separately and complete a turn before binding it. This terminal restriction does not limit the providers offered for **New project chat**.

Titles and prompts are single-line and reject control characters, including escape sequences. Text-mode CLI output escapes control characters, line breaks and bidirectional overrides in older saved titles as `\u{…}`; `--json` output is unchanged. The editor joins pasted line breaks with spaces.

Recurrence is elapsed time anchored to the first UTC instant; it does not follow local daylight-saving changes. Intervals range from 5 to 525600 whole minutes. First/next/last timestamps include the local UTC offset. Saving requires a future first run. Pausing preserves the schedule; resuming a recurring schedule skips paused overdue occurrences and selects its next future cadence point. A completed or overdue one-time schedule needs a future date through Edit. Revision conflicts preserve the draft for review instead of overwriting a concurrent edit or run outcome.

## CLI and MCP agent configuration

`somtus schedule help` lists the full contract; `automation` is an alias of `schedule`. These commands configure the same ledger shown by **Automations**. They do not launch sessions or dispatch prompts. `somtus mcp` advertises matching `riwork_schedule_list`, `show`, `create`, `update`, `pause`, `resume`, and `delete` tools through `tools/list`.

Find an existing target first: `somtus orchestrator status --json` for the app orchestrator, `somtus orchestrator status --project PROJECT_UUID --json` for a project orchestrator, or `somtus shell list --project PROJECT_UUID --json` for a worktree worker. Use full canonical UUIDs for project, worktree, shell, and schedule IDs. A terminal target must be a live Codex or Claude session in the selected scope with an established provider identity; complete a turn in that session before creating the schedule. Grok and Muse terminal targets are refused. An orchestrator that runs as a chat (`"mode":"chat"`) is bound by its `id`, which is its chat's id, in `--shell`; it needs no completed turn first, because a chat has no pane or provider session to prove, and the chat's own identity is pinned instead.

```sh
somtus schedule create --scope app --shell APP_SHELL_UUID \
  --title 'Morning check' --prompt 'Summarize the current queue' \
  --at FUTURE_RFC3339 --json

somtus schedule create --scope project --project PROJECT_UUID \
  --shell PROJECT_ORCHESTRATOR_UUID --title 'Project check' \
  --prompt 'Review open tasks' --at FUTURE_RFC3339 --json

somtus schedule create --scope workspace --project PROJECT_UUID \
  --worktree WORKTREE_UUID --shell WORKER_SHELL_UUID \
  --title 'Workspace check' --prompt 'Review this worktree' \
  --at FUTURE_RFC3339 --every-minutes 60 --json
```

Replace `FUTURE_RFC3339` with a future RFC 3339 instant with seconds and an explicit timezone offset (or `Z`); saved times use UTC epoch seconds. `--every-minutes` is optional, from 5 to 525600 whole minutes. Omit it for a one-time schedule. Update always requires a new future `--at` and retains the pinned target and recurrence unless you use `--every-minutes N` or `--once`. To target a different session, create a new schedule and delete the old one.

Read the full schedule ID and current revision from `somtus schedule list --json` or `somtus schedule show SCHEDULE_UUID --json`. Every later mutation requires both, plus the exact scope and shell identity in the schedule. Add `--project PROJECT_UUID` for project and workspace scope; add `--worktree WORKTREE_UUID` for workspace scope.

```sh
somtus schedule update SCHEDULE_UUID --revision 1 --scope workspace \
  --project PROJECT_UUID --worktree WORKTREE_UUID --shell WORKER_SHELL_UUID \
  --at FUTURE_RFC3339 --title 'Revised check' --json
somtus schedule pause SCHEDULE_UUID --revision 2 --scope workspace \
  --project PROJECT_UUID --worktree WORKTREE_UUID --shell WORKER_SHELL_UUID --json
somtus schedule resume SCHEDULE_UUID --revision 3 --scope workspace \
  --project PROJECT_UUID --worktree WORKTREE_UUID --shell WORKER_SHELL_UUID --json
somtus schedule delete SCHEDULE_UUID --revision 4 --scope workspace \
  --project PROJECT_UUID --worktree WORKTREE_UUID --shell WORKER_SHELL_UUID --json
```

Successful `--json` commands write an object with `schedule`, `items`, or `deleted` and exit 0. Failed commands write `{"error":{"code":"...","message":"...","current":{...}}}` to stdout and exit 2; revision conflicts include the current schedule. MCP successes have the same object in `structuredContent`. MCP tool failures set `isError: true` and omit `structuredContent`, because clients validate it against each tool's `outputSchema`; the text content is the same `{"error":{...}}` object as the CLI prints, so `code` and a conflict's `current` schedule are still available. For example, `riwork_schedule_create` accepts `{"scope":"workspace","project_id":"PROJECT_UUID","worktree_id":"WORKTREE_UUID","shell_id":"WORKER_SHELL_UUID","title":"Workspace check","prompt":"Review this worktree","at":"FUTURE_RFC3339","every_minutes":60}`. The other tools use the same field names. Read the new revision after each mutation; a run outcome can also advance it.

A failed or uncertain attempt pauses the schedule and requires reviewing the target transcript. Resume refuses it until an update saves a new future run. A paused overdue recurring schedule resumes on the next future cadence point; an overdue one-time schedule needs an update. Dispatch still requires a Somtus desktop process using the same `RIWORK_HOME` to be open.

## Delivery and lifecycle

The GUI starts one background scheduler per app process, independent of project windows. Schedules run only while a Somtus desktop process using the same `RIWORK_HOME` is open. There is no installed launch agent or background daemon for closed-app execution.

- A due occurrence can start an attempt within five minutes of its due instant. Busy, blocked, unknown, copy-mode, or nonempty-composer targets defer, with checks no more often than every 15 seconds. So does any check that could not be completed: a slow or failing `lsof`, an unreadable session registry, workspace state or hook file, or a tmux failure. Whatever could not be checked is recorded as a **Deferred** run with its reason, and backs off like any deferral, so one broken target never starves the others; an unexpected error escaping the checks (for example the tmux client failing to start) is also logged on stderr. Older occurrences are **Missed** and skipped; that message repeats the last deferral reason. There is no backlog replay.
- Each tick considers one oldest due occurrence. A durable global limit permits at most four claimed attempts per minute, shared across processes; its window restarts if the clock steps backward. Recurring schedules advance to the first future point on their original cadence.
- Targets pin the full Somtus UUID, creation time, command, harness/account home, tmux pane/process identity, and provider session identity. A target that is gone, or whose pinned identity was actually read and differs (a replaced pane, a different Codex thread or Claude session, a worker outside its worktree), becomes **Failed** and pauses. Missing or unreadable evidence is never treated as a difference: it is **Deferred** and retried. There is no implicit retargeting.
- Codex requires a completed turn in its exact bound rollout; Claude requires its structured completed-turn hook. Somtus passes the Claude hooks (`UserPromptSubmit`, `Stop`, `SessionStart`, `SubagentStart`, `SubagentStop`) to each launch through `--settings`; the global Claude configuration is never edited. `SessionStart` rebinds the recorded session after `/clear` or `/resume`, so a schedule pinned to the replaced conversation fails as changed instead of sending into the new one. A subagent hook withdraws a completion left over from an earlier turn, because the main agent is then mid-turn even if its `UserPromptSubmit` was missed; the turn's own `Stop` completes it again, and a subagent hook never completes a turn. One rule decides which subagent hooks count, and the app's activity display uses the same one: a `SubagentStop` counts when its `agent_id` pairs with a `SubagentStart` seen for this conversation, or when it names a prompt the cursor never saw begin, and a `SubagentStart` counts when it names such a prompt (one for the finished turn's own prompt is ignored). A `SubagentStop` that is neither is ignored. Claude 2.1.288 sends one, sometimes two, 3 to 5 seconds after some replies (tool-using or not): no start came before it, its `agent_type` is empty and it carries the finished turn's own prompt (seven of seven observed), so it must not block a schedule. The unseen-prompt condition needs no `SubagentStart`, so it is also the only evidence a Claude launched before that hook was registered can give. Such a session sends only unpaired stops, and a stray one is told apart from a real one by its prompt. A Claude version that sends no `prompt_id` on its hooks cannot be told apart this way, so an unpaired stop from it is ignored; the terminal checks below still apply. A shell in the pane's foreground (Claude exited) defers, because shell prompts often draw the same `❯`. A missed `UserPromptSubmit` for a turn that runs no subagent cannot be detected from hooks alone. The terminal must additionally show a stable empty composer at its initial cursor. Busy, trust, login, approval, and draft surfaces fail the gate. Completed conversation above the composer is ignored, including replies discussing thinking, login, approvals or quoted UI messages. Additional textual vetoes inspect current interactive/status controls below the composer; ongoing tasks are refused by structured lifecycle evidence. The dimmed default Codex placeholder is distinguished from ordinary typed text.
- Custom Codex notification hooks are preserved. On macOS, Codex identity is proven by inspecting only the selected pane's own process group (the exec'd harness and, for the npm launcher, its child) with bounded `lsof -nP` (2 seconds, output drained and capped). The pane's foreground command must be `codex`, or `node` (the npm launcher), and in either case that group must hold exactly one open rollout under its pinned account home whose thread matches the binding and whose metadata is a primary interactive thread. A binding recorded earlier is never enough by itself: after Codex exits to a shell, or under any wrapper or shared server without descriptor proof, the identity stays unproven and the attempt defers instead of typing into the pane. A Codex started by typing `codex` into a shell pane runs in a different process group from the pane's, so it has no proof either; use a directly launched Somtus Codex pane. It never finds a target by directory similarity or scans other sessions' content. Ambiguous or inaccessible evidence fails closed as a deferral; only a rollout that is provably a different thread or account fails the target. Persistent per-target rollout cursors catch up large logs in bounded reads.
- **A chat orchestrator** (the app or project orchestrator created with Settings set to Chat) is delivered to through the chat host, by `schedule_chat`, next to the tmux path above and with the same claim. The target pins the chat's id, creation time, agent (`harness`) and Codex account, and its scope, and the chat host is asked for the chat each time: a chat that is gone, or is another chat of the scope than the one pinned, becomes **Failed** and pauses, as a replaced pane does. A chat that is **Idle** or **Stopped** (a message resumes it) takes the prompt; one that is **Starting**, **Running**, **Waiting** for an approval or an answer, or **Failed** (open it and send a message to restart it) defers, with the reason, and is asked again no more often than every 15 seconds. A host that cannot be reached defers too. A claimed occurrence is submitted at most once, and confirmation comes from the chat's log: the log's length is noted, the scheduler's claim (rate limit, next occurrence and **Dispatching** state, on disk) is taken, and only then is the prompt sent to the host as a `Send`. A host that refuses the `Send` did nothing and the schedule is **Failed**; an exchange that breaks may or may not have reached the host and the schedule is **Uncertain**; a `Send` the host acknowledged is **Submitted** once the log shows a user message with exactly the prompt's text after the noted position, and **Uncertain** if it does not within 10 seconds. A claimed attempt is never retried, and a process that dies after the claim leaves **Uncertain** as for a terminal. Because the chat's state is the host's own and is read just before, there is no stale idle signal to consume: the readiness token of a terminal turn has no counterpart here. The ledger keeps such a target as the same fields plus a `chat` object (the pinned Codex account) and empty `pane_identity` and `provider_session`; a terminal target has no `chat` key, so older builds read the ledger as before, and a build from before chat delivery that finds a chat target takes it for a session that is gone, fails it and pauses it.
- One completed provider turn can supply readiness for only one scheduled attempt. This prevents multiple jobs from consuming the same stale idle signal before a harness records the next start.
- Scheduled input shares the existing `SessionManager::send` submission path and its per-session input lock. The registry lock is held for the whole attempt, so Somtus close/respawn cannot race the submission. The session, pane, provider-identity and workspace-directory checks run under the registry lock just before the input lock is taken; the readiness sample (lifecycle token, stable empty composer, foreground process, and a repeated provider-identity check) and the claim run inside the input lock, immediately before the paste. It uses one settled bracketed paste and one Return.

**Submitted** means terminal submission completed, not that the agent finished or achieved the requested objective. **Deferred** is a pre-input refusal. **Failed** identifies a target that needs repair. **Uncertain** means delivery may have been partial or its owner died after claiming it. Failed and uncertain schedules pause. Review the target transcript, then Edit and save a future run to acknowledge the outcome. Uncertain delivery is never automatically retried. External terminal input and provider state can change after the readiness sample; this is not an exactly-once provider execution protocol.

## Persistence and ownership

Schedules live in `RIWORK_HOME/schedules.json`, separate from the workspace state schema so older workspace writers cannot drop them. `schedules.lock` serializes CRUD and dispatch across windows/processes. Files use private permissions, bounded sizes/counts, fsync, unique temporary files, atomic rename, and directory sync, matching Somtus's store conventions.

Before terminal mutation, the scheduler atomically persists the consumed readiness token, rate-limit state, next occurrence, and **Dispatching** claim. The lock stays held through submission. If a process exits before saving its final outcome, the next owner changes Dispatching to Uncertain and pauses it. Store failures are visible in the Automations panel. No prompts are logged by the readiness fallback; configured schedule prompts are persisted locally.

## Verification

Use the repository-pinned Rust 1.99.1 toolchain and Zig 0.16, available as `zig` on `PATH` or selected with `ZIG`. See [development requirements](guides/development.md#requirements) and [build and run](guides/development.md#build-and-run) for the current NSH UI setup. Run these commands from the repository root with cached dependencies for `--offline`, using this worktree's own Cargo target directory:

```sh
cargo build --locked --offline -p riwork-desktop --bin somtus
scripts/check-schedules.sh --locked schedules
scripts/check-schedules.sh --locked schedule_chat
cargo test --locked --offline -p riwork-cli --test schedule_interfaces -- --test-threads=1
```

The broader `scripts/check-schedules.sh --locked` command runs the core, engine, CLI and desktop test suites. The current panel's UI fixtures are in `apps/riwork/schedules/Automations.test.tsx`; they use an in-memory API and do not dispatch prompts. With the pinned NSH CLI configured, run `"$NSH_CLI" check apps/riwork` and `"$NSH_CLI" test apps/riwork --filter P6B-SCHED-UI` for the UI checks.

`check-schedules.sh` gives the child test process a fresh isolated `RIWORK_HOME`; it leaves the caller's inherited value intact. Unit and real tmux fixtures use full UUIDs and remove only their own files/sessions. They cover timing boundaries, fixed recurrence, missed runs/rewind, deferral, pause/edit/delete, revisions, corruption, concurrent threads/processes, interrupted claims, rate limiting, stale readiness and wrong/missing targets. Real deterministic tmux fixtures cover all three scope routes. Readiness tests cover completed-history acceptance, both harness composers, busy/trust/login/approval/draft refusal, and dimmed Codex placeholders. A scheduler-level regression reads a rollout larger than two poll budgets across retained ticks: initial reads defer, catch-up submits once to the same target, and no startup-history notice or duplicate appears. Quick first-run tests verify exact future instants and preserved editor fields.

Chat delivery is covered by `schedule_chat` tests against a chat host with fake drivers on a throwaway home: an idle chat gets the prompt once and its log shows it; a running, waiting, starting or failed chat defers without sending and is delivered to once it is idle; a stopped chat is resumed by the delivery; recurring schedules deliver each occurrence once however often they are asked, and racing schedulers deliver once; a refused `Send` fails after the claim with nothing sent; a broken exchange, and an acknowledged `Send` the log never shows (both against a stand-in that speaks the wire protocol by hand), are uncertain and never retried; a deleted, replaced or re-accounted chat fails instead of being retargeted; and an unreachable host defers.

`schedule_interfaces` spawns the real CLI and MCP stdio binary with a fresh child `RIWORK_HOME`, three inert fixture shells, and fixture-only provider identities. It checks all three scopes, persistence across processes, JSON and MCP structured results/errors, `tools/list`, the full mutation lifecycle, invalid requests, stale and exited targets, and a two-process revision race. It never opens a GUI or delivers a scheduled prompt.

For optional live provider verification, provision three new disposable Codex sessions in a fresh child `RIWORK_HOME`, complete their startup turns, and create its `qa-info.json` fixture marker. Supply only their full UUIDs in app/project/workspace order. Never point this check at production sessions or an existing schedule ledger:

```sh
RIWORK_HOME="$fixture_home" \
RIWORK_SCHEDULE_LIVE_HOME="$fixture_home" \
RIWORK_SCHEDULE_LIVE_IDS="$app_shell_uuid,$project_shell_uuid,$workspace_shell_uuid" \
cargo test --locked --offline -p riwork-engine --lib -- --exact schedules::tests::isolated_live_provider_acceptance --ignored --test-threads=1 --nocapture
```

The opt-in test refuses a nonempty schedule ledger, verifies a new provider completion and exact reply for each scope, and confirms that pane identities and one-time outcomes remain unchanged on a later tick. The ignored concurrency helper is invoked automatically by its parent test; do not run all ignored tests indiscriminately.

Native screenshots and interactions must use Somtus Cua Driver and explicitly target the disposable fixture app's PID. The current desktop panel uses NSH (`apps/riwork/schedules/Automations.tsx`). [Scheduling verification](scheduling-verification.md) preserves the original September 2026 GPUI acceptance record and screenshots; it is historical evidence, not a description of the current UI stack.
