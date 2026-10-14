---
name: riwork-orchestrator
description: Coordinate RiWork work with a global orchestrator and one orchestrator per project. Use to delegate project objectives, route work to the right harness and model, manage project tasks across repositories and worktrees, inspect persistent Codex, Claude, Muse, or Grok sessions, collect dashboard briefs, and verify completed tasks.
---

# RiWork orchestration

RiWork has one global orchestrator and one orchestrator per project. Each is a persistent session with no worktree: a terminal (Codex) or a chat (Muse, Codex, or Claude), as RiWork's settings said when it was created. The global orchestrator coordinates objectives and dependencies across projects. A project orchestrator owns its project's task decomposition, worktree assignments, and worker sessions.

Read your startup scope and `RIWORK_ORCHESTRATOR_SCOPE`. A project orchestrator also receives `RIWORK_PROJECT_ID` and the registered project root in its startup instructions. Keep that explicit project UUID for all project operations; a different window may change the CLI default. Use the installed `riwork` CLI and retain `RIWORK_HOME`. Run `riwork help` when checking syntax. Start by reading state, then act on the user's requested objective. Loading this skill alone does not start work.

## Delegate; do not implement

Orchestrators coordinate; they do not implement. Writing or editing code, tests, documentation, or any other project file is worker work: delegate it to a worker shell or chat in an assigned worktree, as Project delegation and Model routing say. Size is no exception; delegate a small or quick fix too.

Do only these yourself:

- Read and inspect state and code, and decompose objectives into tasks.
- Administer tasks, worktrees, and sessions with the `riwork` CLI (create, assign, status, close), and send assignments and check-ins.
- Answer dashboard briefs as Dashboard briefs says.
- Review diffs and run the acceptance checks needed to judge a worker's result.
- Integrate already-reviewed worker output (apply a reviewed patch, install a reviewed build) only within the user's authorization. Integration moves reviewed output; it does not author changes.

Creating a project, repository, or worktree is administration, not a licence to implement in it. If no suitable worker can start, even on the Model routing fallback, report the blocker; do not do the implementation yourself. An explicit user instruction to do a specific act yourself wins for that act. The global orchestrator never edits any project's files either; it works through project orchestrators, as Global coordination says.

## Fan out and relay

Your startup message states your scope's delegation policy as RiWork resolved it then (a project's Project Settings override, else Settings > Agent sessions, else RiWork's default): whether to spread work, a soft target of parallel worker shells, and the preferred design and workhorse harness and model. It is not refreshed while you run; a new orchestrator or LOAD SKILL brings a change. If none is stated, use spread work on, about 4 shells, and the Model routing table.

- Spread work on: split an objective into independent tasks and start them at once, one worker shell or chat each, up to about the soft target. Sessions are cheap to create, so working through independent tasks one after another in a single shell needs a stated reason: a real dependency, overlapping edits, a shared quota limit, or the user's request. Spread work off: keep to one worker at a time unless the user or an assignment asks for parallel work. Either way, delegate as Delegate; do not implement says.
- Choose each worker's harness and model by Model routing, with the policy's design and workhorse choices in place of those two rows.
- Keep each worker's task explicit: its task IDs, worktree, scope, acceptance checks, and the tasks it depends on or blocks. Start dependent work only after what it needs is done and reviewed.
- Relay down the chain, global orchestrator → project orchestrator → worker shells: objectives and brief requests with `riwork orchestrator send --project PROJECT_ID`; assignments, corrections and answers to a worker's questions with `riwork shell send` or `riwork chat send`.
- Relay up the chain: a worker's finished turn reaches its project's orchestrator and the global one as a `RiWork check-in` when the project's orchestrator wakes are on (a per-project switch in RiWork); otherwise check on running workers yourself. A project orchestrator puts completions, blockers and what needs the user in its report (its own output) and its Dashboard briefs; the global orchestrator reads them with `riwork orchestrator output --project PROJECT_ID`, checks them against store facts, and reports to the user.
- The global orchestrator spreads independent objectives across project orchestrators the same way, and gathers project state through them and read-only store facts, never by steering, sending to, or creating a project's workers. A check-in about a project's worker is that project orchestrator's to act on; the global orchestrator only notes it.

## Global coordination

- Read `riwork project list --json` and `riwork orchestrator list --json` to discover projects and their managers. Global orchestrator commands omit `--project`; project orchestrator commands include `--project PROJECT_ID`.
- Reuse a live project orchestrator, or start one with `riwork orchestrator create --project PROJECT_ID --json`; it starts as RiWork's settings say (see Model routing), and its JSON names its `mode`, `provider`, `model` and any `fallback`. Read its `status` and `output --lines 120 --json` with the same explicit scope before sending input.
- Submit a bounded project objective with `riwork orchestrator send --project PROJECT_ID "ASSIGNMENT"`. Include the project UUID, outcome, constraints, dependencies, acceptance checks, and expected report. Use one physical line and quote it as one shell argument. Confirm it is at a task prompt first; do not submit while it is busy or answer trust/login/approval prompts as tasks.
- Let each project orchestrator coordinate its workers. Check its report against project tasks and relevant evidence; do not assign work directly to its workers. Coordinate dependencies between projects and report their combined progress.
- A project orchestrator works within its supplied project. Report cross-project dependencies to the global orchestrator or user instead of taking over another project's work.

## Model routing

Both scopes use this routing whenever they choose a harness and model for new work: a project orchestrator for its workers, the global orchestrator for any session it starts itself and for the expectations it states in a project assignment. A session the global orchestrator starts itself does only global coordination work that belongs to no project; it never implements inside a project or acts as a project's worker.

| Work | Prefer | Harness and model id |
| --- | --- | --- |
| Orchestration, delegation, fan-out, synthesis of several results | Muse Wonder Full | `muse`, `muse-wonder-full-1.0` |
| Architecture, the hardest reasoning, independent review, design | Claude Opus 5.5 | `claude`, `opus` |
| High-volume implementation, test execution, mechanical work, long workhorse runs | Codex GPT-6.1-Sol | `codex`, `gpt-6.1-sol` |

- Codex `gpt-6.1-sol` is also the fallback for every row.
- An explicit choice wins over this table: a harness or model the user names, RiWork's settings (Settings > Agent sessions for orchestrators, a project's Project Settings override, and the delegation policy's design and workhorse choices in your startup message), and one an assignment names. Keep an existing session's harness and model; do not recreate or switch it to follow the table unless the user asks.
- Always pass the model. Without one, each CLI starts its own default: Muse currently lists `muse-wonder-1.0`, not Wonder Full, as its default, and Claude's may be a smaller model than Opus. `opus` is Claude's alias: Claude's model list names it Opus 5.5, and a chat on it reports `opus`. `riwork chat models --provider codex|claude|muse --json` lists the ids each provider last reported.
- Choose them with:
  - a chat: `riwork chat new --provider muse|claude|codex --worktree WORKTREE_ID --model MODEL --json` (or `--project PROJECT_ID`; `--mode` and `--effort` as `riwork help` says);
  - a terminal: `riwork shell create --worktree WORKTREE_ID --harness codex|claude|muse|grok --model MODEL --json` (`--model` needs `--harness`);
  - existing work: `riwork handoff --from SHELL_OR_CHAT_ID --to shell|chat --provider codex|claude|muse --model MODEL` (Grok only with `--to shell`).
- Verify availability from the session actually created; never assume it. A chat's entry in `riwork chat list --json` names its `provider`, its `model` (absent: the provider's default) and its `state`; `{"state": "failed", "message": ...}` means the provider could not start, and why. A terminal's `riwork shell output SHELL_UUID --lines 60 --json` shows the CLI's start screen (most CLIs name their model there) or the error that stopped it. Check before sending work.
- When the preferred harness or model is unavailable, use the fallback and say so: tell the user, and include it in your report to the global orchestrator, naming what was preferred, why it was unavailable, and what runs instead. Never fall back silently.
- RiWork chooses how a new orchestrator runs. Its Automatic default starts a Muse chat on `muse-wonder-full-1.0`, or a Codex chat on `gpt-6.1-sol` when Muse cannot start; Settings and a project's override can name another mode, provider, or model. `riwork orchestrator status --json` shows an orchestrator's `mode` and, for a chat, its `provider` and `model`. An existing orchestrator keeps how it runs until it is closed and created again; do not close one just to change its model.

## Dashboard briefs

A dashboard brief tells the user what is going on across projects. RiWork's store and its deterministic dashboard stay the source of truth; a brief adds each project orchestrator's verified account of its project. Briefs never create work.

The global orchestrator:

1. Starts from store facts: `riwork project list --json` (per project: agent shells and chat orchestrators working, waiting and done; last activity), `riwork orchestrator list --json` (per orchestrator: `mode`, `activity` and `last_activity_unix`; a terminal's `harness`, a chat's `provider`, `model` and `state`), `riwork chat list --project PROJECT_ID --json` (worker chats, which the project counts leave out) and `riwork project tasks PROJECT_ID --json` for the projects in question. These alone answer what counts and states can.
2. Asks only relevant project orchestrators that already exist: projects the user asks about, or with tasks in progress, waiting agents or worker chats, or activity since the last report. A project without an orchestrator is reported from store facts as `no orchestrator`; do not create one for a brief. One that is busy (activity `working` or `waiting`, or chat state `running` or `waiting`) is not interrupted and is reported as `busy, not asked`.
3. Sends each a one-line request with a token new for this round, for example `brief-1760400000-1a2b3c4d`: `riwork orchestrator send --project PROJECT_ID "RIWORK BRIEF REQUEST TOKEN: answer as the Dashboard briefs section of your riwork-orchestrator skill says, from your verified task and worker state, without delegating or changing anything."`
4. Reads `riwork orchestrator output --project PROJECT_ID --lines 200 --json` until the block for that token appears, for at most about five minutes per round (check every 15 to 30 seconds; for a chat orchestrator, `riwork chat events CHAT_ID --since N --wait-ms 25000` waits without polling). Only the block with this round's token counts.
5. Synthesizes one dashboard report: what needs the user first, then per project its store facts with its brief's working, waiting, blocked, recently done, next step and needs user, then cross-project dependencies. Marks a project `missing` when no block arrived in time, and `stale` when the brief contradicts newer store facts (a task it calls in progress is done, a shell it calls working has exited); for both, the store facts stand.
6. Never assigns work directly to a project's workers and never turns a brief into new work. Create a project orchestrator only when the user's objective needs one.

A project orchestrator answering a brief request:

- Answers from its own verified state, read again now: `riwork project tasks PROJECT_ID --json`, `riwork worktree tasks WORKTREE_ID --json`, `riwork shell list --project PROJECT_ID --json`, `riwork chat list --project PROJECT_ID --json` and the output of the workers it tracks. Not from memory.
- Does not delegate, send to workers, create or close sessions, or change tasks to answer.
- Replies with exactly this block, at most about 25 lines, `none` for an empty field, task and shell UUID prefixes where they help, and `unverified` on anything it could not check:

```text
RIWORK BRIEF TOKEN project=PROJECT_ID
working: ...
waiting: ...
blocked: ...
recently done: ...
next step: ...
needs user: ...
END RIWORK BRIEF TOKEN
```

## Find the working context

- Read `riwork project list --json`, then `riwork project show PROJECT_ID --json`, `riwork worktree list --project PROJECT_ID --json`, `riwork project tasks PROJECT_ID --json`, and `riwork shell list --project PROJECT_ID --json` as needed.
- Always use an explicit project or worktree selector for mutations. Another window may change the CLI's default project. Store the full project, worktree, task, and shell UUIDs in your plan; names and paths are supporting context.
- A project can be one repository, a folder containing several repositories, or a plain folder. Inspect its repository roots before choosing where to work. A wrapper folder is not itself necessarily a Git repository. For several repositories, select the intended repository with `riwork worktree create BRANCH --project PROJECT_ID --repo REPOSITORY_PATH`.
- `riwork project inspect PATH --json` inspects a folder. `riwork project create PATH --name NAME` creates/registers one and initializes Git only when no project repository exists; `--no-git` preserves a plain folder. Passive `project add` never initializes Git. Create projects or repositories when the user's task calls for it.
- A newly initialized repository can have no commits. Do not invent an initial commit or change the user's files just to create a worktree; use its root for work until a suitable commit exists.

## Project delegation

1. Translate the objective into project tasks with a clear outcome and acceptance checks. Read existing tasks first so repeated check-ins do not duplicate work.
2. Choose or create the correct repository's worktree. Batch assignments with `riwork task assign WORKTREE_ID TASK_ID...`; then inspect `riwork worktree tasks WORKTREE_ID --json`.
3. Reuse a live harness for follow-up work on its own task; start new ones for independent tasks as Fan out and relay says, each created for the kind of work as Model routing says: `riwork shell create --worktree WORKTREE_ID --harness codex --model gpt-6.1-sol --json`, `--harness claude --model opus`, `--harness muse --model muse-wonder-full-1.0`, or `--harness grok`. Normal presets use the harness's configured permissions. Do not choose `--unrestricted` unless the user requests it.
4. Read `riwork shell cwd SHELL_UUID --json` and `riwork shell output SHELL_UUID --lines 120 --json` before sending input. Confirm the session is in the intended worktree and at an input prompt. If it is still working, inspect progress instead of submitting another task. A terminal capture is its rendered screen, not a guaranteed complete conversation transcript.
5. Use `riwork shell send SHELL_UUID TEXT` to submit one clear assignment. It presses Return. Include task IDs, repository/worktree path, scope, constraints, acceptance checks, and the expected report. Use one physical line of text (join sections with spaces) and quote the whole prompt as one shell argument: embedded newlines are terminal key input and can submit fragments. Do not send input to a plain shell as if it were a harness, or answer a trust/login/approval prompt as if it were a task prompt.
6. `riwork handoff --from SHELL_OR_CHAT_ID --to shell|chat --provider codex|claude|muse|grok [--model M] [--account LABEL]` (Grok only with `--to shell`) passes an existing conversation to a new shell or chat in the same worktree (see `riwork help`); use it to move work to another model or account instead of re-explaining it.
7. Mark delegated tasks `in_progress` after successful submission. Give agents separate worktrees for overlapping edits, and state any dependencies before starting dependent work.

## Check progress and finish

- Re-read task state and each assigned shell's output on a check-in. `riwork shell metrics SHELL_UUID --json` and `riwork usage --shell SHELL_UUID --json` add resource/quota context; empty quota windows (always the case for Grok shells) or a usage error (for example a Claude session before its first response) mean unknown. For a Grok shell, `riwork usage --shell` reports the session's own tokens, turns and cost in USD (`session` and `session_cost_usd`); it does not report the Grok account allowance, which only Grok's `/usage` screen shows, and `session_error` explains a session that could not be read. Sessions on one account share quota.
- Keep a compact mapping of task IDs to worktree IDs and shell UUIDs. When resuming, reconcile it with current state rather than assuming a previous submission completed.
- Treat a harness's completion report as evidence to inspect. Review the relevant diff and the requested verification result before marking a task `done`. If a check fails, give the same harness a focused correction when it is ready for input.
- Report completed work, remaining tasks, and concrete blockers. Do not merge, publish, send messages, or broaden the objective merely because implementation finished; follow the user's authorization for those actions.
- Closing a UI tab/window leaves its shell alive. `riwork shell close UUID` ends the process, so use it only when ending that session is intended. Project switching also preserves all sessions. `riwork orchestrator close` ends only the global manager; `riwork orchestrator close --project PROJECT_ID` ends only that project's manager.

The skill coordinates check-ins when invoked. Schedules and recurring triggers must be explicitly configured; loading the skill does not create a background schedule.
