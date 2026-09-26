# RiWork

**RiWork is in beta.**

A compact cyberpunk GPUI workspace with native Ghostty terminals on macOS. Tabs and split panes attach to shells in a dedicated tmux server, so shells and their output remain available when the UI closes. Projects own shells and worktrees; tasks belong to projects and can be assigned to worktrees.

## Requirements

- macOS with Xcode command line tools
- Rust 1.95 (selected by `rust-toolchain.toml`)
- Zig 0.16 for the Ghostty native build
- tmux (`brew install tmux`) for persistent shells

## Build and run

```sh
ZIG=/path/to/zig-0.16/zig cargo build
ZIG=/path/to/zig-0.16/zig sh scripts/bundle-macos.sh
open target/debug/RiWork.app --args /path/to/project
```

The project path is optional. Without one, RiWork reopens the active project, or registers the current directory on first launch. The bundle includes Ghostty terminfo and shell integration. For an unbundled `cargo run`, set `GHOSTTY_RESOURCES_DIR` to the bundle's `Contents/Resources/ghostty` directory.

To make the CLI available everywhere, link the built executable into a directory on your `PATH`:

```sh
ln -sfn "$PWD/target/debug/riwork" "$HOME/.local/bin/riwork"
```

Run `riwork help` for the full command list. Data defaults to `~/.local/share/riwork`; set `RIWORK_HOME` to use a separate store and tmux server.

RiWork also serves a local MCP connection over stdio with `riwork mcp`. Configure an MCP client to launch that command to give Codex or Claude project, worktree, task, and shell inspection tools. The server uses the same state as the CLI and GPUI app.

## Projects, worktrees, and tasks

```sh
riwork project add /path/to/repo --name my-project
riwork project create /path/to/new-project --name my-project
riwork project create --name my-project
riwork project create /path/to/plain-folder --no-git
riwork project inspect /path/to/project --json
riwork project list
riwork project use PROJECT_ID
riwork open PROJECT_ID
riwork worktree create feature/branch --project PROJECT_ID
riwork worktree create feature/branch --project PROJECT_ID --repo /path/to/project/repo
riwork worktree list --all

riwork task add "Implement search" --project PROJECT_ID --details "Index project tasks"
riwork project tasks PROJECT_ID
riwork worktree tasks WORKTREE_ID
riwork task assign WORKTREE_ID TASK_ID TASK_ID
riwork task status TASK_ID in_progress
riwork search search
```

Projects, Worktrees, Tasks, and Shells each have a searchable panel tab. Projects lists all projects; the other panels show the active project's items. RiWork discovers Git worktrees created outside the CLI during refresh. A removed worktree stays registered so task and shell references keep their UUIDs; its path is marked `[missing]`. Use `riwork worktree forget ID` after its tasks and shells are gone to remove the stale record.

Click a project row to switch the current window, or **↗** to open that project in another window. Cmd+Shift+N opens a new project window; `riwork open PROJECT_OR_PATH` opens one from the CLI. Each window keeps its own project. `riwork project use ID` selects the default for subsequent windows.

## Persistent shells

RiWork creates `~/Documents/riwork` at app startup if needed. Use **+ PROJECT** in Projects or Cmd+N to create a project. Enter a project name to create `~/Documents/riwork/NAME`; the folder field updates automatically. Editing the folder or using Browse selects an explicit location, where the name is optional. `riwork project create --name NAME` also uses this default parent. Explicit CLI paths retain their usual meaning. Projects support a single Git repository, a folder containing several repositories, and plain folders. Creation offers **Initialize Git** checked by default when no repository is found. Uncheck it to keep a plain folder. Existing repositories are reused; a wrapper folder is never initialized around them. `project add` only registers a folder. For projects with several repositories, worktree creation requires choosing a repository with `--repo`; a new repository needs an initial commit before Git can create worktrees.

```sh
riwork shell create --worktree WORKTREE_ID
riwork shell create --worktree WORKTREE_ID --harness codex
riwork shell create --worktree WORKTREE_ID --harness claude
riwork shell create --harness codex --unrestricted
riwork shell list --all --json
riwork shell output SHELL_UUID --lines 100
riwork shell cwd SHELL_UUID
riwork shell metrics SHELL_UUID
riwork shell send SHELL_UUID "pwd"
riwork shell attach SHELL_UUID
```

Every shell has a UUID. `shell output` captures tmux scrollback and the current pane as text, including when the GPUI app is closed. Full screen terminal programs may show only their current rendered screen rather than a complete semantic transcript. Closing a tab or window detaches its shell; `riwork shell close UUID` ends it.

Mouse-wheel and trackpad scrolling use tmux's scrollback. Scrolling up enters copy mode; scroll back to the bottom or press Escape to return to the live terminal. New sessions enable mouse reporting, and existing sessions are upgraded when reattached.

Each project remembers its split layout and sizes, mixed panel/shell tab order, active selections, and selected worktree/task across project switches and UI restarts. Closed shell tabs stay detached; click their shell in the Shells panel to reattach them. Exited shells are skipped during restoration, and new CLI-created shells join the active pane when the project next opens. An intentionally empty workspace stays empty. Layouts are saved separately in `layouts.json` under the same data directory.

The compact bottom-right status island shows CPU and resident RAM for the active project's sessions, the active shell UUID, **G·ORCH**, and **P·ORCH**. The Shells panel shows worker shells with their metrics and current worktree when their working directory matches a known worktree. Click the status UUID to copy it.

## Harness launches and usage

The **+** and **▤** menus launch a shell, Codex, or Claude in the selected worktree. Normal presets use the CLI's usual permissions. Explicit **UNRESTRICTED** presets pass its permission bypass flag. Cmd+Shift+C opens Codex; Cmd+Shift+L opens Claude.

Click the bottom usage readout, or open **USAGE** from the views menu, for remaining quota, reset countdowns, and update times. Codex reads its official app-server account endpoint on a worker with a timeout, refreshing every 15 minutes or on demand. Claude sends documented statusline JSON through settings passed only to that invocation; global Claude settings are unchanged. Claude quota appears after a response on supported Pro/Max accounts and versions. Context usage and estimated session cost appear separately. Missing quota windows stay unavailable; old snapshots are marked stale. Quota is shared by sessions on the same account.

```sh
riwork usage --json
riwork usage --shell SHELL_UUID --json
riwork open PROJECT_NAME
riwork open /path/to/another/project
```

Project windows and independently launched app processes keep their own selected project. Projects, tasks, the shell registry, and layouts share the RiWork data directory. Closing a window preserves its shells. There is one persistent global orchestrator and one persistent orchestrator per project.

## Orchestrator

The global orchestrator coordinates objectives and dependencies across projects. Each project's orchestrator manages its tasks, repositories, worktrees, and Codex/Claude workers. They have separate persistent shells and ordinary workspace tabs; project orchestrators belong to their project and have no worktree, while the global orchestrator has neither project nor worktree ownership.

Click **G·ORCH** or press Cmd+Shift+O for the global orchestrator tab. Click **P·ORCH** or press Cmd+Alt+O for the current project's orchestrator tab. Both are also in the **+ / ▤** menus. Opening a scope selects its existing tab in this window, or attaches its persistent session to the active pane. Orchestrator tabs drag, split, close, and restore like shell tabs. Closing a tab detaches its terminal and preserves the session. No separate orchestrator window is created.

New default orchestrators start the official `codex` CLI, install and explicitly load the embedded **riwork-orchestrator** skill, and receive their scope, project UUID, and project root as appropriate. Global context is `RIWORK_HOME/orchestrator`; project contexts are `RIWORK_HOME/orchestrators/projects/PROJECT_UUID`. Each waits for an objective. The skill covers global coordination, repository selection, task assignment, bounded delegation, progress inspection, and completion checks. Existing sessions show **LOAD SKILL** for a one-time upgrade that preserves their conversation. Custom commands do not receive the skill automatically. Loading the skill does not schedule work.

```sh
riwork orchestrator create --json
riwork orchestrator create --project PROJECT_ID --json
riwork orchestrator list --json
riwork orchestrator status --project PROJECT_ID --json
riwork orchestrator output --project PROJECT_ID --lines 120 --json
riwork orchestrator send --project PROJECT_ID "Implement the requested project objective"
riwork orchestrator load-skill --project PROJECT_ID
```

All orchestrator operations use the global scope when `--project` is omitted. `riwork orchestrator close` ends only the global session; `riwork orchestrator close --project PROJECT_ID` ends only that project's session. Worker shell lists exclude orchestrators. MCP orchestrator status/output tools accept an optional project selector.

## UI controls

| Action | Shortcut |
| --- | --- |
| New shell tab | Cmd+T |
| Open Settings | Cmd+, |
| Create project | Cmd+N |
| New project window | Cmd+Shift+N |
| Split right / down | Cmd+D / Cmd+Shift+D |
| Close tab / pane (shell stays alive) | Cmd+W / Cmd+Shift+W |
| Next / previous tab | Ctrl+Tab / Ctrl+Shift+Tab |
| Search current panel (opens Projects from a shell tab) | Cmd+F |
| Open / select Projects panel | Cmd+B |
| Open global / project orchestrator tab | Cmd+Shift+O / Cmd+Alt+O |
| Focus current tab / restore workspace | Cmd+Shift+F |
| Open Codex / Claude | Cmd+Shift+C / Cmd+Shift+L |

Panel tabs start in a left pane. Every pane's tab strip stays at the top, and every panel and shell tab can move, close, or share a split:

- Drag a tab along a strip to reorder it, or onto another strip or pane center to move it into that pane.
- Drop on a pane edge to create a split. Drop within 12 px of a window edge to dock across the entire workspace.
- Drag the 5 px split dividers to resize panes.
- Drag empty strip space or the **⋮** grip on a strip touching the window's top edge to move the window; double-click that area to zoom. The grip stays reachable when tabs overflow.
- **▤** opens the views menu to reopen panel tabs, add a shell, or close the active tab.
- **⛶** or Cmd+Shift+F focuses the active tab in any pane, hiding tab strips, other panes, and the status island. Centered focus caps the content at 1,100 px and leaves 30% of the window clear below it so terminal input sits higher on screen. The top bar keeps native window controls, **RESTORE**, and a **FILL WINDOW / CENTER FOCUS** toggle visible. Click **RESTORE** or press Cmd+Shift+F again to restore the same split layout and sizes. Ctrl+Tab and Ctrl+Shift+Tab still cycle tabs within the focused pane. Focus mode is temporary and does not rewrite the saved split tree.

The workspace fills the top and bottom of the window around a 78 × 28 px native close/minimize/maximize island and the status island. The installed `riwork-workspaces` skill teaches Codex the CLI workflow for project tasks, batched worktree assignments, and shell inspection.

## Settings

Open **RiWork → Settings…**, press Cmd+, or choose **SETTINGS** from a pane's **+ / ▤** menu. Settings opens as a normal workspace tab: drag, split, close, and restore it like the other panel tabs. Preferences save automatically to `settings.json` under the RiWork data directory and are shared across open windows and app processes.

- **Use RiWork terminal colors** is off by default. Terminals load your Ghostty configuration and palette; turn this on to use RiWork's colors while retaining your other Ghostty preferences. Changing it reconnects terminal display clients and preserves the running shells, Codex/Claude sessions, tabs, and splits.
- **Remember project window size** is on by default. New or reopened project windows restore the last normal window size, constrained to the current display. Maximizing or entering fullscreen does not overwrite it. Switching projects in an existing window keeps that window's size. Older layouts use the default size until opened and saved by this version.
