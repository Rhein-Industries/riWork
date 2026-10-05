---
name: riwork-workspaces
description: Manage RiWork projects, Git worktrees, tasks, and persistent shell sessions through the riwork CLI. Use when organizing work across RiWork worktrees or inspecting a RiWork shell by UUID.
---

# RiWork workspaces

Use the `riwork` CLI to read or change the same project, worktree, task, and shell state shown in the GPUI app. Run `riwork help` for the installed version's command syntax. If the binary is not installed, run `cargo run -- <command>` from the RiWork repository.

Project and task data lives under `RIWORK_HOME`, or `~/.local/share/riwork` by default. Specify `--project ID` when the intended project is not the active one. Project, worktree, and task IDs accept unique UUID prefixes of at least eight characters; shell commands require the full UUID. Add `--json` to read commands when parsing results.

## Projects and worktrees

- `riwork project add PATH [--name NAME]` registers a project and its root worktree.
- `riwork project create PATH [--name NAME] [--no-git]` creates or registers a folder. It initializes Git by default only when no project repositories exist. `--no-git` preserves a plain folder; `project add` remains passive. `riwork project inspect PATH --json` reports the repositories found. A project can contain one repository, several repositories, or none.
- `riwork project list` and `riwork project show [ID]` inspect projects. `riwork project use ID` chooses the default project for subsequent windows and CLI commands; already open windows keep their own project.
- `riwork open PROJECT_OR_PATH` opens a project in a new window. A project selector can be its name, UUID, or unique UUID prefix; a path registers or opens that directory.
- `riwork worktree create BRANCH [--project ID] [--path PATH] [--base REF]` creates a Git worktree. `--base` only sets the start point of a new branch; an existing BRANCH is checked out as is and `--base` is ignored. `riwork worktree list [--project ID | --all]` and `riwork worktree show ID` inspect it.
- Add `--repo PATH_OR_NAME_OR_WORKTREE_UUID` when selecting a repository within a project. Several repositories require an explicit selection. A wrapper folder remains a folder; worktrees belong to the selected contained repository. An empty repository needs a first commit before a Git worktree can be created.
- `riwork worktree forget ID` removes a missing worktree record after its tasks and shells have been cleared. It never deletes the worktree directory.
- `riwork search QUERY` searches projects, worktrees, and tasks.

## Tasks

- `riwork task add TITLE [--project ID] [--details TEXT]` creates a project task. Use `riwork project tasks [ID]` for all tasks in a project, including unassigned tasks; use `riwork worktree tasks ID` for tasks assigned to one worktree.
- `riwork task assign WORKTREE_ID TASK_ID...` assigns one or more tasks in a single batch. Tasks must belong to the worktree's project. Inspect the project and worktree task lists before assigning when the target is ambiguous.
- `riwork task unassign TASK_ID...` removes worktree assignments. `riwork task status TASK_ID todo|in_progress|done` updates progress. `riwork task show ID` and `riwork task list [--project ID | --worktree ID | --all]` inspect tasks.

## Persistent shells

- `riwork shell list [--project ID | --all]` lists project shells. `riwork shell create [--project ID | --worktree ID] [--command CMD]` starts one; a worktree shell starts in that worktree. A worktree can be selected by UUID, unique prefix, branch, or path; when several projects share a branch such as `main`, add `--project ID` to look it up in that project first. Record its full UUID.
- `riwork shell output UUID [--lines N]` reads scrollback and the current screen as text. `riwork shell cwd UUID` shows its current directory; `riwork shell metrics UUID` shows CPU and resident RAM for the shell process tree. Use `--json` if consuming these results programmatically.
- `riwork shell send UUID TEXT` sends the text followed by Return, so use it only when executing that input is intended. `riwork shell attach UUID` prints the tmux attach command.
- Shells run in a dedicated tmux server and survive closing tabs, switching projects, and restarting the UI. `riwork shell close UUID` **ends the shell process**; do not use it merely to dismiss a tab or change views.

## Handing a conversation over

- `riwork handoff --to shell|chat --provider codex|claude|grok [--model M] [--effort E] [--account LABEL_OR_ID] [--mode supervised|auto-edit|full|plan] [--context transcript|summary] [--note TEXT] [--json]` passes a conversation to a new shell or chat in the same project, worktree and directory, with a document of it in `RIWORK_HOME/handoffs/`. Without `--from SHELL_OR_CHAT_ID` it hands off the session it runs in (`RIWORK_SHELL_ID` or `RIWORK_CHAT_ID`), so when the user says "hand this over to a Codex chat with gpt-5 on my Work account", run `riwork handoff --to chat --provider codex --model gpt-5 --account Work`. `--account` is a Codex account's label or id; Claude and Grok have none. Use the default `--context transcript` when handing off yourself: `summary` asks an idle agent, which you are not while you run the command. The source is left as it is.

## Desktop schedules

Use `riwork schedule help` for the current CLI syntax and [scheduling.md](../../docs/scheduling.md#cli-and-mcp-agent-configuration) for examples. `riwork schedule list|show|create|update|pause|resume|delete` and the matching `riwork_schedule_*` MCP tools configure the same schedules shown in the desktop panel. Dispatch only occurs while the desktop app is open. These commands do not launch a session.

Create requires an explicit `app`, `project`, or `workspace` scope, full project/worktree UUIDs where applicable, a full live shell UUID, title, single-line prompt, and exact future RFC 3339 `--at` with timezone. `--every-minutes` is optional, from 5 to 525600 whole minutes. Read the schedule's full UUID and revision with `list --json` or `show --json`; every update/pause/resume/delete must supply both, its explicit scope, and its pinned shell UUID. Update needs a new future `--at`, retains the pinned target, and can clear recurrence with `--once`. A failed or uncertain attempt needs transcript review and a future update before resume. Test only in a fresh child `RIWORK_HOME` fixture.

The UI saves each project's mixed panel/shell tabs, order, split sizes, and active selections. Closing a shell tab keeps its shell alive and detached across restarts; click it in the Shells panel to attach it again. A shell created through the CLI appears in the Shells panel during refresh and joins the saved active pane when that project next opens.

## GPUI views

RiWork creates `~/Documents/riwork` at app startup if missing. New projects default to this parent; `riwork project create --name NAME` creates a folder there, while explicit CLI paths retain their usual meaning. Use **+ PROJECT** or Cmd+N for the compact creation form. Enter a name to create a child folder under the default parent; the folder updates automatically. Editing or browsing the folder selects that explicit location and makes the display name optional. Its Git-init checkbox starts checked when no repository is found and can be disabled. Existing repositories are reused.

The **+** menu opens a shell, Codex, Claude, or Grok in the selected worktree. Cmd+Shift+C, Cmd+Shift+L, and Cmd+Shift+G open normal Codex, Claude, and Grok sessions. Explicit unrestricted presets pass the harness's permission bypass flag.

- `riwork shell create --harness codex|claude|grok [--project ID | --worktree ID] [--unrestricted]` opens a persistent official CLI session and records its harness. Custom `--command` and `--harness` are mutually exclusive.
- Project Settings (Cmd+Option+A) chooses the Codex account for new project Codex sessions and project orchestrators: inherit the app account, explicitly use the system default, or use a saved Orca account. The global orchestrator uses the app account. Running sessions and resumes keep their frozen account. A new plain project shell receives the effective `CODEX_HOME` at creation; its RiWork `codex` launcher resolves the current project choice on each new invocation, and so does a custom command that runs `codex` by name. Only an absolute path to a Codex binary follows its own environment. An unavailable saved account blocks new Codex launches with an error (Settings or Project Settings), but plain shells still open. `codex login`, `logout`, and other credential or configuration commands are refused while `CODEX_HOME` is an Orca-managed account home; manage that account in Orca or run them with your own `CODEX_HOME`.
- `riwork usage [--shell UUID] [--json]` reads provider-reported quota. Without a shell UUID it reads the Codex account of the shell it runs in (`RIWORK_SHELL_ID`): that shell's frozen account, else its project's choice, else the app choice. Claude preset sessions send local statusline telemetry; quota can be unavailable until the first response or on unsupported accounts/versions. For a Grok shell the account allowance is not readable (only Grok's own `/usage` screen shows it), so the command succeeds with no windows and account `unknown`, plus that session's own figures from `grok usage`: a `session` object with `tokens` (`input`, `output`, `cached_read`, `cache_creation`, `reasoning`, `total`), `turns`, `model_calls`, `primary_model`, `models`, `updated_at`, and `session_cost_usd`/`cost_usd` in USD. If the session cannot be identified or read, `session_error` says why and `session` is absent. Treat missing windows, and a usage error for a shell, as unknown.
- The **USAGE** tab shows remaining quota, reset countdowns, and snapshot age. Context percentage and estimated session cost are separate metrics. Quota belongs to the account and is shared across its sessions. Grok tabs are listed with each session's model, tokens, turns and cost, plus a combined total; the Grok allowance is not shown and is only on Grok's `/usage` screen.

Projects, Worktrees, Tasks, and Shells are independent searchable panel tabs. Projects lists all projects; the other panels show the active project. Panel tabs begin in the left pane and can be moved or closed like shell tabs. Every pane's tab strip stays at the top.

- Drag tabs to reorder them, move them onto another pane's strip or center, or drop them on a pane edge to create a split. The outer 12 px of the window docks a tab across the whole workspace.
- Drag the 5 px split dividers to resize panes. Empty strip space or the **⋮** grip touching the window's top edge drags the window; double-click it to zoom. The grip remains reachable when tabs overflow.
- **▤** reopens closed panels, adds a shell, or closes the active tab.
- Cmd+B opens or selects Projects. Cmd+F searches the current panel, opening Projects if a shell tab is active.
- Click a project row to switch the current window. Its **↗** button opens that project in another window; Cmd+Shift+N opens a new project window. Existing windows retain their own project when the CLI default changes.

Native close/minimize/maximize controls occupy a small island; panes use the remaining top and bottom space. The bottom-right status island shows project CPU/RAM, the active shell UUID, **G·ORCH**, and **P·ORCH**.

## Orchestrator

**G·ORCH** or Cmd+Shift+O opens the global orchestrator. **P·ORCH** or Cmd+Alt+O opens the current project's orchestrator. Both are available in the **+ / ▤** menu. The global manager coordinates projects; each project manager coordinates its own tasks, worktrees, and worker sessions. They have independent persistent shells and windows, and no worktree assignment. Worker shell lists exclude orchestrators.

New default Codex orchestrators explicitly load the embedded **riwork-orchestrator** skill in separate contexts, with their global or project scope supplied at startup. Existing sessions can load it once through **LOAD SKILL** or `riwork orchestrator load-skill [--project ID]`; custom commands are unchanged. Loading the skill waits for an objective and does not schedule work.

`riwork orchestrator create [--command CMD]` starts or reuses the global session; add `--project ID` to start or reuse that project's session. `riwork orchestrator list --json` lists all orchestrators. `status`, `output [--lines N]`, `cwd`, `metrics`, `send TEXT`, `load-skill`, and `close` use the global session by default; pass `--project ID` for a project session. For `send`, put scope flags before the one quoted, single-line task argument. Closing one scope preserves the others. Global creation also accepts `--cwd PATH`; project creation uses its registered project root for context.
