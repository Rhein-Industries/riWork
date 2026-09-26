---
name: riwork-orchestrator
description: Coordinate RiWork work with a global orchestrator and one orchestrator per project. Use to delegate project objectives, manage project tasks across repositories and worktrees, inspect persistent Codex or Claude CLI sessions, and verify completed tasks.
---

# RiWork orchestration

RiWork has one global orchestrator and one orchestrator per project. Each is a persistent Codex session with no worktree. The global orchestrator coordinates objectives and dependencies across projects. A project orchestrator owns its project's task decomposition, worktree assignments, and worker sessions.

Read your startup scope and `RIWORK_ORCHESTRATOR_SCOPE`. A project orchestrator also receives `RIWORK_PROJECT_ID` and the registered project root in its startup instructions. Keep that explicit project UUID for all project operations; a different window may change the CLI default. Use the installed `riwork` CLI and retain `RIWORK_HOME`. Run `riwork help` when checking syntax. Start by reading state, then act on the user's requested objective. Loading this skill alone does not start work.

## Global coordination

- Read `riwork project list --json` and `riwork orchestrator list --json` to discover projects and their managers. Global orchestrator commands omit `--project`; project orchestrator commands include `--project PROJECT_ID`.
- Reuse a live project orchestrator, or start one with `riwork orchestrator create --project PROJECT_ID --json`. Read its `status` and `output --lines 120 --json` with the same explicit scope before sending input.
- Submit a bounded project objective with `riwork orchestrator send --project PROJECT_ID "ASSIGNMENT"`. Include the project UUID, outcome, constraints, dependencies, acceptance checks, and expected report. Use one physical line and quote it as one shell argument. Confirm it is at a task prompt first; do not submit while it is busy or answer trust/login/approval prompts as tasks.
- Let each project orchestrator coordinate its workers. Check its report against project tasks and relevant evidence; avoid assigning duplicate work directly to its workers. Coordinate dependencies between projects and report their combined progress.
- A project orchestrator works within its supplied project. Report cross-project dependencies to the global orchestrator or user instead of taking over another project's work.

## Find the working context

- Read `riwork project list --json`, then `riwork project show PROJECT_ID --json`, `riwork worktree list --project PROJECT_ID --json`, `riwork project tasks PROJECT_ID --json`, and `riwork shell list --project PROJECT_ID --json` as needed.
- Always use an explicit project or worktree selector for mutations. Another window may change the CLI's default project. Store the full project, worktree, task, and shell UUIDs in your plan; names and paths are supporting context.
- A project can be one repository, a folder containing several repositories, or a plain folder. Inspect its repository roots before choosing where to work. A wrapper folder is not itself necessarily a Git repository. For several repositories, select the intended repository with `riwork worktree create BRANCH --project PROJECT_ID --repo REPOSITORY_PATH`.
- `riwork project inspect PATH --json` inspects a folder. `riwork project create PATH --name NAME` creates/registers one and initializes Git only when no project repository exists; `--no-git` preserves a plain folder. Passive `project add` never initializes Git. Create projects or repositories when the user's task calls for it.
- A newly initialized repository can have no commits. Do not invent an initial commit or change the user's files just to create a worktree; use its root for work until a suitable commit exists.

## Project delegation

1. Translate the objective into project tasks with a clear outcome and acceptance checks. Read existing tasks first so repeated check-ins do not duplicate work.
2. Choose or create the correct repository's worktree. Batch assignments with `riwork task assign WORKTREE_ID TASK_ID...`; then inspect `riwork worktree tasks WORKTREE_ID --json`.
3. Reuse an appropriate live harness when possible. Otherwise create one with `riwork shell create --worktree WORKTREE_ID --harness codex --json` or `--harness claude`. Normal presets use the harness's configured permissions. Do not choose `--unrestricted` unless the user requests it.
4. Read `riwork shell cwd SHELL_UUID --json` and `riwork shell output SHELL_UUID --lines 120 --json` before sending input. Confirm the session is in the intended worktree and at an input prompt. If it is still working, inspect progress instead of submitting another task. A terminal capture is its rendered screen, not a guaranteed complete conversation transcript.
5. Use `riwork shell send SHELL_UUID TEXT` to submit one clear assignment. It presses Return. Include task IDs, repository/worktree path, scope, constraints, acceptance checks, and the expected report. Use one physical line of text (join sections with spaces) and quote the whole prompt as one shell argument: embedded newlines are terminal key input and can submit fragments. Do not send input to a plain shell as if it were a harness, or answer a trust/login/approval prompt as if it were a task prompt.
6. Mark delegated tasks `in_progress` after successful submission. Give agents separate worktrees for overlapping edits, and state any dependencies before starting dependent work.

## Check progress and finish

- Re-read task state and each assigned shell's output on a check-in. `riwork shell metrics SHELL_UUID --json` and `riwork usage --shell SHELL_UUID --json` add resource/quota context; missing usage means unknown. Sessions on one account share quota.
- Keep a compact mapping of task IDs to worktree IDs and shell UUIDs. When resuming, reconcile it with current state rather than assuming a previous submission completed.
- Treat a harness's completion report as evidence to inspect. Review the relevant diff and the requested verification result before marking a task `done`. If a check fails, give the same harness a focused correction when it is ready for input.
- Report completed work, remaining tasks, and concrete blockers. Do not merge, publish, send messages, or broaden the objective merely because implementation finished; follow the user's authorization for those actions.
- Closing a UI tab/window leaves its shell alive. `riwork shell close UUID` ends the process, so use it only when ending that session is intended. Project switching also preserves all sessions. `riwork orchestrator close` ends only the global manager; `riwork orchestrator close --project PROJECT_ID` ends only that project's manager.

The skill coordinates check-ins when invoked. Schedules and recurring triggers must be explicitly configured; loading the skill does not create a background schedule.
