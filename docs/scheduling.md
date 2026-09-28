# Desktop scheduled prompts

Open **SCHEDULES** from the pane **+ / ▤** menu, **RiWork → Schedules**, or **Cmd+Shift+S**. It is a movable, saved GPUI panel with RiWork's existing compact Menlo controls and palette. Each window shows app schedules plus schedules for its current project.

| Scope | Explicit existing target |
| --- | --- |
| App | Global orchestrator session |
| Project | That project's orchestrator session |
| Workspace | Selected worktree and a worker assigned to it |

Workspace means RiWork worktree context, not a project window, filesystem folder group, or newly created worker. Workspace dispatch also checks that the worker's current directory is still inside that worktree. Selecting a scope never creates, wakes, resumes, closes, or replaces a session. Open the intended harness separately and complete a turn before binding it.

Only Codex and Claude sessions can be scheduled. Grok has no structured completed-turn signal or identity proof yet, so the target picker does not list Grok sessions, and binding, saving, `riwork schedule` and the MCP tools refuse them (`binding_failed`, "grok sessions cannot be scheduled yet"). The rule lives in one place, `HarnessKind::schedulable()`.

Create a title, single-line prompt, target, and first run. Titles and prompts reject control characters, including escape sequences, because text-mode CLI output prints titles as they are stored. Pasting several lines into a field joins them with one space per line break, so words never fuse. Choose **IN 10 MIN**, **IN 30 MIN**, **IN 1 HOUR**, or **IN 1 DAY** without typing a date. Each button selects an elapsed duration from the instant it is clicked; the visible **LOCAL** preview includes the date, time, and UTC offset. New editors initially select ten minutes from opening. **EXACT TIME…** optionally reveals ISO date/time entry with an explicit offset, for example `2026-10-05T09:00:00+02:00`. Editing an existing schedule reveals its exact value; quick choices still work and preserve its target.

Choose **ONCE**, **HOURLY**, **24 HOURS**, **7 DAYS**, or a custom whole-minute interval from 5 minutes to 365 days. Recurrence is elapsed time anchored to the first UTC instant; it does not follow local daylight-saving changes. The first-run **IN 1 DAY** choice means 24 elapsed hours. Displayed first/next/last timestamps include the local UTC offset. Saving retains the selected instant and requires it to remain in the future.

**EDIT** retains the pinned session identity. Clicking a target explicitly binds its current identity. **PAUSE** preserves the schedule; resuming a recurring schedule skips paused overdue occurrences and selects its next future occurrence. A completed or overdue one-time schedule needs a future date through Edit. **DELETE** requires a second confirmation click. Tab/Shift+Tab navigate controls, Enter activates them, Escape cancels editing, and Cmd+S saves. A revision conflict asks the user to reopen the schedule rather than overwriting a concurrent edit or run outcome.

## CLI and MCP agent configuration

`riwork schedule help` lists the full contract. These commands configure the same ledger shown by **SCHEDULES**. They do not launch sessions or dispatch prompts. `riwork mcp` advertises matching `riwork_schedule_list`, `show`, `create`, `update`, `pause`, `resume`, and `delete` tools through `tools/list`.

Find an existing target first: `riwork orchestrator status --json` for the app orchestrator, `riwork orchestrator status --project PROJECT_UUID --json` for a project orchestrator, or `riwork shell list --project PROJECT_UUID --json` for a worktree worker. Use full canonical UUIDs for project, worktree, shell, and schedule IDs. The target must be a live Codex or Claude session in the selected scope with an established provider identity; complete a turn in that session before creating the schedule. A Grok session is refused.

```sh
riwork schedule create --scope app --shell APP_SHELL_UUID \
  --title 'Morning check' --prompt 'Summarize the current queue' \
  --at 2026-10-05T09:00:00+02:00 --json

riwork schedule create --scope project --project PROJECT_UUID \
  --shell PROJECT_ORCHESTRATOR_UUID --title 'Project check' \
  --prompt 'Review open tasks' --at 2026-10-05T09:00:00+02:00 --json

riwork schedule create --scope workspace --project PROJECT_UUID \
  --worktree WORKTREE_UUID --shell WORKER_SHELL_UUID \
  --title 'Workspace check' --prompt 'Review this worktree' \
  --at 2026-10-05T09:00:00+02:00 --every-minutes 60 --json
```

`--at` is a future RFC 3339 instant with seconds and an explicit timezone offset (or `Z`); saved times use UTC epoch seconds. `--every-minutes` is optional, from 5 to 525600 whole minutes. Omit it for a one-time schedule. Update always requires a new future `--at` and retains the pinned target and recurrence unless you use `--every-minutes N` or `--once`. To target a different session, create a new schedule and delete the old one.

Read the full schedule ID and current revision from `riwork schedule list --json` or `riwork schedule show SCHEDULE_UUID --json`. Every later mutation requires both, plus the exact scope and shell identity in the schedule. Add `--project PROJECT_UUID` for project and workspace scope; add `--worktree WORKTREE_UUID` for workspace scope.

```sh
riwork schedule update SCHEDULE_UUID --revision 1 --scope workspace \
  --project PROJECT_UUID --worktree WORKTREE_UUID --shell WORKER_SHELL_UUID \
  --at 2026-10-06T09:00:00+02:00 --title 'Revised check' --json
riwork schedule pause SCHEDULE_UUID --revision 2 --scope workspace \
  --project PROJECT_UUID --worktree WORKTREE_UUID --shell WORKER_SHELL_UUID --json
riwork schedule resume SCHEDULE_UUID --revision 3 --scope workspace \
  --project PROJECT_UUID --worktree WORKTREE_UUID --shell WORKER_SHELL_UUID --json
riwork schedule delete SCHEDULE_UUID --revision 4 --scope workspace \
  --project PROJECT_UUID --worktree WORKTREE_UUID --shell WORKER_SHELL_UUID --json
```

Successful `--json` commands write an object with `schedule`, `items`, or `deleted` and exit 0. Failed commands write `{"error":{"code":"...","message":"...","current":{...}}}` to stdout and exit 2; revision conflicts include the current schedule. MCP successes have the same object in `structuredContent`. MCP tool failures set `isError: true` and put the error under `structuredContent.error`. For example, `riwork_schedule_create` accepts `{"scope":"workspace","project_id":"PROJECT_UUID","worktree_id":"WORKTREE_UUID","shell_id":"WORKER_SHELL_UUID","title":"Workspace check","prompt":"Review this worktree","at":"2026-10-05T09:00:00+02:00","every_minutes":60}`. The other tools use the same field names. Read the new revision after each mutation; a run outcome can also advance it.

A failed or uncertain attempt pauses the schedule and requires reviewing the target transcript. Resume refuses it until an update saves a new future run. A paused overdue recurring schedule resumes on the next future cadence point; an overdue one-time schedule needs an update. Dispatch still requires a RiWork desktop process using the same `RIWORK_HOME` to be open.

## Delivery and lifecycle

The GUI starts one background scheduler per app process, independent of project windows. Schedules run only while a RiWork desktop process using the same `RIWORK_HOME` is open. There is no installed launch agent or background daemon for closed-app execution.

- A due occurrence can start an attempt within five minutes of its due instant. Busy, blocked, unknown, copy-mode, or nonempty-composer targets defer, with checks no more often than every 15 seconds. So does any check that could not be completed: a slow or failing `lsof`, an unreadable session registry, workspace state or hook file, or a tmux failure. Whatever could not be checked is recorded as a **Deferred** run with its reason, and backs off like any deferral, so one broken target never starves the others; an unexpected error escaping the checks (for example the tmux client failing to start) is also logged on stderr. Older occurrences are **Missed** and skipped; that message repeats the last deferral reason. There is no backlog replay.
- Each tick considers one oldest due occurrence. A durable global limit permits at most four claimed attempts per minute, shared across processes; its window restarts if the clock steps backward. Recurring schedules advance to the first future point on their original cadence.
- Targets pin the full RiWork UUID, creation time, command, harness/account home, tmux pane/process identity, and provider session identity. A target that is gone, or whose pinned identity was actually read and differs (a replaced pane, a different Codex thread or Claude session, a worker outside its worktree), becomes **Failed** and pauses. Missing or unreadable evidence is never treated as a difference: it is **Deferred** and retried. There is no implicit retargeting.
- Codex requires a completed turn in its exact bound rollout; Claude requires its structured completed-turn hook. RiWork passes the Claude hooks (`UserPromptSubmit`, `Stop`, `SessionStart`, `SubagentStop`) to each launch through `--settings`; the global Claude configuration is never edited. `SessionStart` rebinds the recorded session after `/clear` or `/resume`, so a schedule pinned to the replaced conversation fails as changed instead of sending into the new one, and `SubagentStop` withdraws a completion left over from a turn whose `UserPromptSubmit` hook was missed. A shell in the pane's foreground (Claude exited) defers, because shell prompts often draw the same `❯`. A missed `UserPromptSubmit` for a turn that runs no subagent cannot be detected from hooks alone. The terminal must additionally show a stable empty composer at its initial cursor. Busy, trust, login, approval, and draft surfaces fail the gate. Completed conversation above the composer is ignored, including replies discussing thinking, login, approvals or quoted UI messages. Additional textual vetoes inspect current interactive/status controls below the composer; ongoing tasks are refused by structured lifecycle evidence. The dimmed default Codex placeholder is distinguished from ordinary typed text.
- Custom Codex notification hooks are preserved. On macOS, Codex identity is proven by inspecting only the selected pane's own process group (the exec'd harness and, for the npm launcher, its child) with bounded `lsof -nP` (2 seconds, output drained and capped). The pane's foreground command must be `codex`, or `node` (the npm launcher), and in either case that group must hold exactly one open rollout under its pinned account home whose thread matches the binding and whose metadata is a primary interactive thread. A binding recorded earlier is never enough by itself: after Codex exits to a shell, or under any wrapper or shared server without descriptor proof, the identity stays unproven and the attempt defers instead of typing into the pane. A Codex started by typing `codex` into a shell pane runs in a different process group from the pane's, so it has no proof either; use a directly launched RiWork Codex pane. It never finds a target by directory similarity or scans other sessions' content. Ambiguous or inaccessible evidence fails closed as a deferral; only a rollout that is provably a different thread or account fails the target. Persistent per-target rollout cursors catch up large logs in bounded reads.
- One completed provider turn can supply readiness for only one scheduled attempt. This prevents multiple jobs from consuming the same stale idle signal before a harness records the next start.
- Scheduled input shares the existing `SessionManager::send` submission path and its per-session input lock. The registry lock is held for the whole attempt, so RiWork close/respawn cannot race the submission. The session, pane, provider-identity and workspace-directory checks run under the registry lock just before the input lock is taken; the readiness sample (lifecycle token, stable empty composer, foreground process, and a repeated provider-identity check) and the claim run inside the input lock, immediately before the paste. It uses one settled bracketed paste and one Return.

**Submitted** means terminal submission completed, not that the agent finished or achieved the requested objective. **Deferred** is a pre-input refusal. **Failed** identifies a target that needs repair. **Uncertain** means delivery may have been partial or its owner died after claiming it. Failed and uncertain schedules pause. Review the target transcript, then Edit and save a future run to acknowledge the outcome. Uncertain delivery is never automatically retried. External terminal input and provider state can change after the readiness sample; this is not an exactly-once provider execution protocol.

## Persistence and ownership

Schedules live in `RIWORK_HOME/schedules.json`, separate from the workspace state schema so older workspace writers cannot drop them. `schedules.lock` serializes CRUD and dispatch across windows/processes. Files use private permissions, bounded sizes/counts, fsync, unique temporary files, atomic rename, and directory sync, matching RiWork's store conventions.

Before terminal mutation, the scheduler atomically persists the consumed readiness token, rate-limit state, next occurrence, and **Dispatching** claim. The lock stays held through submission. If a process exits before saving its final outcome, the next owner changes Dispatching to Uncertain and pauses it. Store failures are visible in the scheduling panel. No prompts are logged by the readiness fallback; configured schedule prompts are persisted locally.

## Verification

Use Zig 0.16 and this worktree's own Cargo target directory:

```sh
PATH=/Users/dominik/.cache/uv/archive-v0/wTsWUKqQ1AgbZM4gXoYXu/ziglang:$PATH cargo build --offline
PATH=/Users/dominik/.cache/uv/archive-v0/wTsWUKqQ1AgbZM4gXoYXu/ziglang:$PATH scripts/check-schedules.sh schedules
PATH=/Users/dominik/.cache/uv/archive-v0/wTsWUKqQ1AgbZM4gXoYXu/ziglang:$PATH cargo test --offline --test schedule_interfaces
PATH=/Users/dominik/.cache/uv/archive-v0/wTsWUKqQ1AgbZM4gXoYXu/ziglang:$PATH scripts/check-schedules.sh
```

`check-schedules.sh` gives the child test process a fresh isolated `RIWORK_HOME`; it leaves the caller's inherited value intact. Unit and real tmux fixtures use full UUIDs and remove only their own files/sessions. They cover timing boundaries, fixed recurrence, missed runs/rewind, deferral, pause/edit/delete, revisions, corruption, concurrent threads/processes, interrupted claims, rate limiting, stale readiness and wrong/missing targets. Real deterministic tmux fixtures cover all three scope routes. Readiness tests cover completed-history acceptance, both harness composers, busy/trust/login/approval/draft refusal, and dimmed Codex placeholders. A scheduler-level regression reads a rollout larger than two poll budgets across retained ticks: initial reads defer, catch-up submits once to the same target, and no startup-history notice or duplicate appears. Quick first-run tests verify exact future instants and preserved editor fields.

`schedule_interfaces` spawns the real CLI and MCP stdio binary with a fresh child `RIWORK_HOME`, three inert fixture shells, and fixture-only provider identities. It checks all three scopes, persistence across processes, JSON and MCP structured results/errors, `tools/list`, the full mutation lifecycle, invalid requests, stale and exited targets, and a two-process revision race. It never opens a GUI or delivers a scheduled prompt.

For optional live provider verification, provision three new disposable Codex sessions in a fresh child `RIWORK_HOME`, complete their startup turns, and create its `qa-info.json` fixture marker. Supply only their full UUIDs in app/project/workspace order. Never point this check at production sessions or an existing schedule ledger:

```sh
RIWORK_HOME="$fixture_home" \
RIWORK_SCHEDULE_LIVE_HOME="$fixture_home" \
RIWORK_SCHEDULE_LIVE_IDS="$app_shell_uuid,$project_shell_uuid,$workspace_shell_uuid" \
cargo test --offline -- --exact schedules::tests::isolated_live_provider_acceptance --ignored --test-threads=1 --nocapture
```

The opt-in test refuses a nonempty schedule ledger, verifies a new provider completion and exact reply for each scope, and confirms that pane identities and one-time outcomes remain unchanged on a later tick. The ignored concurrency helper is invoked automatically by its parent test; do not run all ignored tests indiscriminately.

Native screenshots and interactions must use RiWork Cua Driver. GPUI custom controls use screenshot-grounded pixel input and keyboard navigation; the driver's native AX tree may contain only the window/menu chrome. The acceptance record and screenshots are in [scheduling-verification.md](scheduling-verification.md).
