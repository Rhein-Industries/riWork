# Desktop scheduled prompts

Open **SCHEDULES** from the pane **+ / ▤** menu, **RiWork → Schedules**, or **Cmd+Shift+S**. It is a movable, saved GPUI panel with RiWork's existing compact Menlo controls and palette. Each window shows app schedules plus schedules for its current project.

| Scope | Explicit existing target |
| --- | --- |
| App | Global orchestrator session |
| Project | That project's orchestrator session |
| Workspace | Selected worktree and a worker assigned to it |

Workspace means RiWork worktree context, not a project window, filesystem folder group, or newly created worker. Workspace dispatch also checks that the worker's current directory is still inside that worktree. Selecting a scope never creates, wakes, resumes, closes, or replaces a session. Open the intended harness separately and complete a turn before binding it.

Create a title, single-line prompt, target, and first run. Enter an ISO date/time with an explicit offset, for example `2026-10-05T09:00:00+02:00`. The initial value uses the local offset. Choose **ONCE**, **HOURLY**, **24 HOURS**, **7 DAYS**, or a custom whole-minute interval from 5 minutes to 365 days. Recurrence is elapsed time anchored to the first UTC instant; it does not follow local daylight-saving changes. Displayed next/last timestamps include the local UTC offset.

**EDIT** retains the pinned session identity. Clicking a target explicitly binds its current identity. **PAUSE** preserves the schedule; resuming a recurring schedule skips paused overdue occurrences and selects its next future occurrence. A completed or overdue one-time schedule needs a future date through Edit. **DELETE** requires a second confirmation click. Tab/Shift+Tab navigate controls, Enter activates them, Escape cancels editing, and Cmd+S saves. A revision conflict asks the user to reopen the schedule rather than overwriting a concurrent edit or run outcome.

## Delivery and lifecycle

The GUI starts one background scheduler per app process, independent of project windows. Schedules run only while a RiWork desktop process using the same `RIWORK_HOME` is open. There is no installed launch agent or background daemon for closed-app execution.

- A due occurrence can start an attempt within five minutes of its due instant. Busy, blocked, unknown, copy-mode, or nonempty-composer targets defer, with checks no more often than every 15 seconds. Older occurrences are **Missed** and skipped. There is no backlog replay.
- Each tick considers one oldest due occurrence. A durable global limit permits at most four claimed attempts per minute, shared across processes. Recurring schedules advance to the first future point on their original cadence.
- Targets pin the full RiWork UUID, creation time, command, harness/account home, tmux pane/process identity, and provider session identity. Exited, replaced, moved, or mismatched targets become **Failed** and pause. There is no implicit retargeting.
- Codex requires a completed turn in its exact bound rollout; Claude requires its structured completed-turn hook. The terminal must additionally show a stable empty composer at its initial cursor. Busy, trust, login, approval, and draft surfaces fail the gate. The dimmed default Codex placeholder is distinguished from ordinary typed text.
- Custom Codex notification hooks are preserved. On macOS, the fallback inspects only the selected pane's own Codex PID with bounded `lsof -nP`, requires one open rollout under its pinned account home, and validates its primary interactive thread metadata. It never finds a target by directory similarity or scans other sessions' content. Ambiguous or inaccessible evidence fails closed. Persistent per-target rollout cursors catch up large logs in bounded reads.
- One completed provider turn can supply readiness for only one scheduled attempt. This prevents multiple jobs from consuming the same stale idle signal before a harness records the next start.
- Scheduled input shares the existing `SessionManager::send` submission path and its per-session input lock. Identity/readiness checks run under that lock and the registry lock; RiWork close/respawn cannot race the submission. It uses one settled bracketed paste and one Return.

**Submitted** means terminal submission completed, not that the agent finished or achieved the requested objective. **Deferred** is a pre-input refusal. **Failed** identifies a target that needs repair. **Uncertain** means delivery may have been partial or its owner died after claiming it. Failed and uncertain schedules pause. Review the target transcript, then Edit and save a future run to acknowledge the outcome. Uncertain delivery is never automatically retried. External terminal input and provider state can change after the readiness sample; this is not an exactly-once provider execution protocol.

## Persistence and ownership

Schedules live in `RIWORK_HOME/schedules.json`, separate from the workspace state schema so older workspace writers cannot drop them. `schedules.lock` serializes CRUD and dispatch across windows/processes. Files use private permissions, bounded sizes/counts, fsync, unique temporary files, atomic rename, and directory sync, matching RiWork's store conventions.

Before terminal mutation, the scheduler atomically persists the consumed readiness token, rate-limit state, next occurrence, and **Dispatching** claim. The lock stays held through submission. If a process exits before saving its final outcome, the next owner changes Dispatching to Uncertain and pauses it. Store failures are visible in the scheduling panel. No prompts are logged by the readiness fallback; configured schedule prompts are persisted locally.

## Verification

Use Zig 0.16 and this worktree's own Cargo target directory:

```sh
PATH=/Users/dominik/.cache/uv/archive-v0/wTsWUKqQ1AgbZM4gXoYXu/ziglang:$PATH cargo build --offline
PATH=/Users/dominik/.cache/uv/archive-v0/wTsWUKqQ1AgbZM4gXoYXu/ziglang:$PATH scripts/check-schedules.sh schedules
PATH=/Users/dominik/.cache/uv/archive-v0/wTsWUKqQ1AgbZM4gXoYXu/ziglang:$PATH scripts/check-schedules.sh
```

`check-schedules.sh` gives the child test process a fresh isolated `RIWORK_HOME`; it leaves the caller's inherited value intact. Unit and real tmux fixtures use full UUIDs and remove only their own files/sessions. They cover timing boundaries, fixed recurrence, missed runs/rewind, deferral, pause/edit/delete, revisions, corruption, concurrent threads/processes, interrupted claims, rate limiting, stale readiness and wrong/missing targets. Real deterministic tmux fixtures cover all three scope routes. Readiness tests cover both harness composers, busy/trust/login/approval/draft refusal, and dimmed Codex placeholders.

For optional live provider verification, provision three new disposable Codex sessions in a fresh child `RIWORK_HOME`, complete their startup turns, and create its `qa-info.json` fixture marker. Supply only their full UUIDs in app/project/workspace order. Never point this check at production sessions or an existing schedule ledger:

```sh
RIWORK_HOME="$fixture_home" \
RIWORK_SCHEDULE_LIVE_HOME="$fixture_home" \
RIWORK_SCHEDULE_LIVE_IDS="$app_shell_uuid,$project_shell_uuid,$workspace_shell_uuid" \
cargo test --offline -- --exact schedules::tests::isolated_live_provider_acceptance --ignored --test-threads=1 --nocapture
```

The opt-in test refuses a nonempty schedule ledger, verifies a new provider completion and exact reply for each scope, and confirms that pane identities and one-time outcomes remain unchanged on a later tick. The ignored concurrency helper is invoked automatically by its parent test; do not run all ignored tests indiscriminately.

Native screenshots and interactions must use RiWork Cua Driver. GPUI custom controls use screenshot-grounded pixel input and keyboard navigation; the driver's native AX tree may contain only the window/menu chrome. The acceptance record and screenshots are in [scheduling-verification.md](scheduling-verification.md).
