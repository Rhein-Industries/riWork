# RiWork

**RiWork is in beta.**

A compact, themeable GPUI workspace with native Ghostty terminals on macOS. Tabs and split panes attach to shells in a dedicated tmux server, so shells and their output remain available when the UI closes. Projects own shells and worktrees; tasks belong to projects and can be assigned to worktrees.

## Requirements

- macOS with Xcode command line tools
- Rust 1.95 (selected by `rust-toolchain.toml`)
- Zig 0.16 for the Ghostty native build
- tmux (`brew install tmux`) for persistent shells
- Vim for editing files in persistent RiWork tabs (included with macOS)
- macOS 14 or later for Cua.ai desktop control; RiWork setup installs Cua Driver

## Build and run

```sh
ZIG=/path/to/zig-0.16/zig cargo build --release
ZIG=/path/to/zig-0.16/zig sh scripts/bundle-macos.sh release
open target/release/RiWork.app --args /path/to/project
```

The project path is optional but must be an existing directory. Without one, RiWork reopens the active project, or registers the current directory on first launch. Any other unrecognised argument, such as a misspelled command, prints an error and exits instead of opening a window. The bundle includes Ghostty terminfo and shell integration, and copies the named theme catalog from `/Applications/Ghostty.app` when installed. Set `GHOSTTY_THEMES_DIR` while bundling to use another catalog directory. Custom themes in your Ghostty configuration directory also work. For an unbundled `cargo run`, set `GHOSTTY_RESOURCES_DIR` to the bundle's `Contents/Resources/ghostty` directory. The bundler signs `RiWork.app` (ad hoc by default) under its bundle identifier `dev.riwork.shell` and verifies the signature; set `CODESIGN_IDENTITY` to a certificate name to sign with it instead.

Use the release build for normal use. For development, `cargo build` and `sh scripts/bundle-macos.sh debug` create artifacts under `target/debug`; `cargo run -- /path/to/project` starts an unbundled development window.

To make the CLI available everywhere, link the built executable into a directory on your `PATH`:

```sh
ln -sfn "$PWD/target/release/riwork" "$HOME/.local/bin/riwork"
```

Run `riwork help` for the full command list and `riwork --version` for the installed version. Data defaults to `~/.local/share/riwork`; set `RIWORK_HOME` to use a separate store and tmux server. An empty `RIWORK_HOME` or `HOME` counts as unset. A newly created data directory is owner-only, and `state.json` and `sessions.json` are written with owner-only permissions.

## Encrypted iOS access

The standalone `riwork-remote` relay and outbound desktop connector let paired
mobile devices inspect existing projects/tasks/worktrees, read persistent shell
and orchestrator output, and submit a line to an explicitly selected existing
session. Each device has independent endpoint secrets and revocable access; the
relay routes encrypted frames without session content or pairing secrets.

```sh
cargo build --locked --release --manifest-path remote/Cargo.toml
export RIWORK_REMOTE_BIN="$PWD/remote/target/release/riwork-remote"
riwork remote --help
riwork remote start --riwork /absolute/path/to/riwork
```

Build the standalone binary before bundling to include it beside the desktop
CLI. `RIWORK_HOME` is retained. Stopping the transport preserves tmux and harness
state. Mobile tabs can temporarily resize the existing tmux grid to their visible
terminal area; disconnect restores desktop sizing and keeps the same process.
Follow [pairing and test instructions](remote/README.md), the
[frozen mobile protocol](docs/remote-protocol.md), and
[TLS relay deployment](docs/remote-deployment.md). v1 supports captured terminal
text and single-line input, with persistent request deduplication and explicit
unknown-outcome errors.

Open the native iPhone/iPad project in [ios/RiWorkRemote.xcodeproj](ios/RiWorkRemote.xcodeproj).
See [iOS setup and verification](ios/README.md) for building, pairing and continuing
in the compact project and terminal-tab interface.

## Update and reload

```sh
riwork update                 # Build release, install it, and reload all open RiWorks
riwork update --debug         # Explicit development build
riwork reload                 # Reload all open windows using the installed build
riwork instances --json       # Inspect running apps and their windows
riwork reload --session       # Also resume this RiWork-hosted Codex conversation with Cua
```

Reloads preserve each window's project, layout, position, and persistent shells. The old app exits only after its replacement has restored the windows. Running agents stay attached to their existing tmux sessions. Apps opened before reload support was installed need one normal quit and reopen first. A running app relaunches only as its own executable or the build recorded by `riwork update`; it refuses any other path found in a reload request. A running app whose registration this build cannot read (an incompatible version) is reported as a warning instead of being silently skipped, and counts as `unreadable_registrations` in `--json` output. `reload` and `update` still reload every readable app, then exit non-zero because that process keeps running its old build; quit and reopen it, or run `riwork reload` from the build that started it.

`update` builds and packages your local source in a staging directory, including `riwork-remote` when the checkout contains its manifest, then installs the executable and app together. A companion failure preserves the previous installation. It defaults to an optimized release build, including when run from a debug CLI. Use `--source /path/to/riWork` to choose a checkout, `--debug` for a development build, or `--no-reload` to install without reopening windows. `--release` selects the default explicitly; it cannot be combined with `--debug`. The command does not fetch or change Git history. Later `open` and `reload` commands use the latest successfully installed app, including when its profile or source checkout differs from the CLI's. Build failures keep the installed app running and retain a diagnostic log.

Before anything is replaced, `update` checks the staged bundle's code signature and starts both the staged executable and the one inside the bundle with `riwork help`. A build that compiles but fails to start is rejected, and the installed build stays as it was. The build being replaced is kept next to the new one as `RiWork.app.previous` and `riwork.previous` in `target/<profile>` until the next successful update replaces them. `update` prints the `.previous` location and repeats it if the reload fails. To roll back after a bad update:

```sh
cd target/release   # or target/debug
mv RiWork.app RiWork.app.bad && mv RiWork.app.previous RiWork.app
mv riwork riwork.bad && mv riwork.previous riwork
riwork reload
```

Ctrl-C, closing the terminal, or SIGTERM stops the running cargo, rustc, or bundler process group and removes the staging directory; the installed build is untouched. Each update begins, under its lock, by removing staging directories left by interrupted runs and all but the five most recent `target/riwork-update-*.log` files. A staging directory kept after a failed install is retained for a week.

`--session` applies to the current RiWork Codex pane. It waits for the active turn to finish, then resumes the same conversation UUID through RiWork's Cua launcher. Other agents keep running. New RiWork launches bind tool commands to their own terminal even when Codex shares a backend. For an older launch without that binding, use `riwork reload --session --shell SHELL_UUID`, selecting the conversation's terminal UUID from RiWork. Use it after installing a new harness integration; reopening the app alone cannot replace the tools of an already running agent.

## Cua.ai setup and computer use

RiWork uses [Cua.ai's Cua Driver](https://cua.ai/cua-driver) for computer use across Codex, Claude, Grok, and the global and project orchestrators. On a machine without the driver, the app opens Settings with **SET UP CUA**. Setup downloads Cua's official stable installer over HTTPS, installs the signed `CuaDriver.app`, and prepares the shared connection. You can also run:

```sh
riwork setup
riwork cua status --json
riwork cua permissions
```

Select **GRANT MACOS ACCESS**, then enable **CuaDriver** in macOS **Privacy & Security → Accessibility** and **Screen & System Audio Recording**. These permissions belong to CuaDriver and need to be enabled once in System Settings. Approve CuaDriver's direct screen capture prompt when macOS shows it. Settings reports the grants and capture verification, and offers **CHECK AGAIN** after enabling them. RiWork launches its driver in standard permission mode. The explicit grant action restarts the shared service with Cua's permission onboarding so updated grants are read by a fresh process.

RiWork runs Cua's installer with a minimal environment: `PATH`, `HOME`, `TMPDIR`, the locale, proxy and certificate settings, and Cua's telemetry variables. Tokens and API keys in your shell, such as `GH_TOKEN`, are not passed to it. After installing, and before it launches or runs the driver, RiWork checks that `/Applications/CuaDriver.app` carries Cua AI, Inc.'s Developer ID signature (bundle identifier `com.trycua.driver`, Team ID `YCK386LBJ7`) and that macOS accepts it as notarized (`codesign --verify` and `spctl --assess`). An app that fails is not launched; the error explains how to reinstall it. RiWork remembers a successful check until the app on disk changes, so normal startup does not repeat it. The driver must live inside that app; a `cua-driver` elsewhere on `PATH` is not used. An installer that times out is stopped with SIGTERM so it can release its lock, and RiWork removes a leftover lock under `RIWORK_HOME/cua/package/packages/.install.lock.d` when the process recorded in it is gone. A slow first start of the driver after boot is retried and never triggers a reinstall.

**Telemetry.** Cua's installer states that telemetry defaults to enabled for new installations, and that it collects no prompts, tool arguments, screen contents, or file paths. RiWork captures the installer's output, so you do not see that notice, and it does not change the default. To opt out, run `RIWORK_HOME/cua/bin/cua-driver telemetry disable` after setup (for the default state directory, `~/.local/share/riwork/cua/bin/cua-driver telemetry disable`), which is the command Cua's installer prints. RiWork also forwards `CUA_TELEMETRY_ENABLED` and `CUA_DRIVER_RS_TELEMETRY_ENABLED` from the environment of `riwork setup` to the installer, but the command changes the driver's own persistent setting.

New RiWork Codex, Claude, and Grok launches receive the same `cua-driver` MCP connection and computer-use instructions. Codex's OpenAI computer-use feature is disabled for these launches. Existing model, account, and harness permission preferences are retained. New plain shell tabs include RiWork's Codex, Claude, and Grok launchers on their `PATH`, so starting any CLI from a shell tab uses the same integration. An absolute path to a separately installed CLI bypasses those launchers. RiWork runs the CLI under the name it finds it by, so version-manager shims such as mise's and Volta's keep working, and it skips a file that is not executable. When the app is opened from the Dock or Finder its `PATH` lacks the directories that nvm, pnpm, bun, pyenv and similar tools add. If a CLI is not found on the app's own `PATH`, RiWork asks your login shell (`$SHELL`, else `/bin/zsh`) once per process, without a terminal and within three seconds, for the `PATH` a terminal would have, and gives Codex, Claude, and Grok tabs that `PATH` after the launcher directory. A shell that fails or hangs is ignored. Existing running harnesses need to be restarted to load the new connection; closing and reopening a terminal tab only reattaches its running session.

The MCP connection runs through `riwork cua mcp`, which resolves the installed driver without depending on the agent's `PATH`. For a custom driver installation or isolated tests, set `RIWORK_CUA_DRIVER` to its executable (an empty value is ignored). This developer override skips the signature and notarization check, since it names a different binary on purpose, and is passed to each harness's MCP server so `riwork cua mcp` uses the same driver. New tmux sessions receive it in their environment too, so a `codex`, `claude`, or `grok` typed in a plain shell tab uses the same driver. When several agents start together against a stopped desktop service, the later ones wait up to 30 seconds for the first to start it instead of failing. If the RiWork binary a launcher points to has moved or been deleted, the launcher warns and runs the next `codex`, `claude`, or `grok` on `PATH` outside RiWork's launcher directory until the app regenerates them. Setup keeps its managed launchers under `RIWORK_HOME/cua` and does not edit your global Codex, Claude, or Grok configuration or shell startup files. Grok receives a session-scoped agent definition from `RIWORK_HOME/cua/grok-agent-*.md`; its model and login still come from Grok's own profile, while RiWork's agent definition replaces Grok's default agent selection for that session. Each launch refreshes the definition it uses and removes ones that no launch has used for 30 days. Before a Grok tab or a `grok` typed in a RiWork shell starts, RiWork starts Cua Driver when its desktop service is down and completes an MCP handshake. That work finishes before Grok's 30-second MCP startup limit begins. When neither `GROK_MCP_STARTUP_TIMEOUT_SECS` nor `MCP_TIMEOUT` is already set, that Grok process receives `GROK_MCP_STARTUP_TIMEOUT_SECS=120`, the same startup budget Codex is given. A value you already set is forwarded unchanged. When both are set, Grok applies `MCP_TIMEOUT` (milliseconds) over `GROK_MCP_STARTUP_TIMEOUT_SECS`. A per-server `startup_timeout_sec` in your own `config.toml` still wins, and RiWork does not edit that file or put a timeout in Grok's agent definition, which has no effect on the handshake limit. Grok still owns that limit. On 1.0.44 a headless first prompt can be sent before the session has folded this handshake into the turn, even when the server answers in well under a second, and a slow handshake is only waited on for about two seconds before that prompt proceeds. The tools are registered on the session for a later prompt. If the check fails, Grok is not started and the message names the cause, how long the check took, and `RIWORK_HOME/cua/driver.log`. RiWork's zsh tabs forward your startup files through a session directory and restore the launcher prefix after their PATH changes. Custom shells, aliases, and nested shells can override that PATH; direct RiWork harness launches always receive the connection.

RiWork also serves a local MCP connection over stdio with `riwork mcp`. Configure an MCP client to launch that command to give Codex, Claude, or Grok project, worktree, task, shell, orchestrator, and schedule tools. Many tools only read state, but others change it: they register projects and worktrees, start shells (a `command` runs as an arbitrary program), send input to shells, and edit schedules whose prompts RiWork later types into an agent. Each tool's `annotations` mark its read-only and destructive behavior accordingly; for example `riwork_worktree_list` is not read-only because refreshing writes newly found worktrees to RiWork's state. Every tool rejects arguments outside its `inputSchema` instead of ignoring them, and failed schedule calls set `isError` without `structuredContent`. The server accepts JSON-RPC batches and answers malformed input with a JSON-RPC error. It uses the same state as the CLI and GPUI app.

## Projects, worktrees, and tasks

Settings → **Import from Orca** offers a one-time preview and import of local
projects and worktrees through the installed Orca CLI. RiWork finds it through
`ORCA_CLI_COMMAND`, then the Orca.app bundle, then `PATH`, `/usr/local/bin`, and
`/opt/homebrew/bin`; Codex account discovery uses the same lookup. Keep Orca
running while previewing and importing. Existing paths retain their RiWork names
and grouping; the import records completion and is safe to invoke again. An
import with nothing to add and nothing skipped, such as while Orca is still
starting, records nothing, so you can import again once Orca lists its projects.
Settings then says there is nothing to import yet instead of offering a finish
button.
The current Orca CLI does not export folder names or nesting. You can also use:

```sh
riwork import orca --preview --json
riwork import orca --json
```

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

`riwork worktree create --base REF` chooses the start point of a new branch only; if the branch already exists it is checked out as is and `--base` is ignored (the MCP `base` argument behaves the same). Projects, Worktrees, Tasks, and Shells each have a searchable panel tab. Projects lists all projects; the other panels show the active project's items. RiWork discovers Git worktrees created outside the CLI during refresh. A removed worktree stays registered so task and shell references keep their UUIDs; its path is marked `[missing]`. Use `riwork worktree forget ID` after its tasks and shells are gone to remove the stale record.

Use the Projects sort selector beside the project count to choose **Last edited**, **Name**, **Date added**, or **Live sessions**. The arrow reverses the order. Projects sort within their virtual folders while the folder hierarchy stays intact; search and collapsed folders keep working. The preference is saved and shared across windows. Last edited defaults to newest first and reads the latest source-file modification time across each project's roots and registered worktrees, respecting Git ignores and excluding common build and dependency directories. Background scans refresh every 30 seconds. Each root is scanned within its own limits, and a project shows the latest edit among the roots whose scan completed, so one very large or unreadable root does not hide the others. Unavailable dates sort last; commit dates, deleted files, and task activity are not used as file-edit timestamps.

Press **Cmd+Shift+E**, or choose **FILES** under **Views** in a pane's **…** actions menu, to browse the current worktree. The explorer follows the focused shell's worktree, including moving into another registered worktree with `cd`. Select a regular file and click **Edit in Vim**, double-click its row, press Enter in the tree, or press **Cmd+E**. RiWork opens Vim in a dedicated persistent terminal tab in that worktree; existing agent shells keep running. Vim's normal editing, `:w` save, `u` undo, and `/` search work in the tab. The editor tab restores with the project layout while its tmux session is alive. Return to Files and select the file to edit it again; a live Vim tab is reused even after saving, and whether you open the file from the project root or its primary worktree. Symlinks and files outside the worktree cannot be opened for editing from Files.

Single-click also shows a read-only preview of text, code, Markdown source, raster images (rotated by their EXIF orientation, shrunk to 1800 px if larger and never enlarged), or PDF pages in the Files pane. Moving the selection with the arrow keys previews after a short pause, and the previous preview stays up until the next one is ready. PDFs are parsed only on request: click the file, press Enter, or use **Preview PDF** in the placeholder. Drag the divider to resize the tree and preview; narrow panes stack them vertically. The file actions sit in the preview header, to the right of the file's name; in a narrow preview the name is shortened before the actions move to a line of their own, and any message about an action appears directly under the header. With **Icons instead of labels** on, they are compact icon buttons with tooltips. Text previews stop at 1 MiB and image or PDF previews at 24 MiB; unsupported or binary files show a message. A folder lists at most 5,000 entries, followed by a row counting the rest, and collapsed folders are not rescanned until expanded again. **Open Externally** uses the default application, but refuses symbolic links, executables, and applications, scripts, or installers such as `.app` and `.command` with an inline message; use **Reveal** for those. **Reveal** and **Copy Path** act on the selection. **Copy Contents** puts the selected UTF-8 text file's exact contents on the clipboard, unclipped and with its line endings intact, and stays disabled for binary files, images, PDFs and files over 1 MiB rather than copying part of one. **Refresh** updates the tree and preview, **Hidden** includes dotfiles, and **Cmd+F** filters files in directories already loaded. Use arrow keys in the tree, Tab to reach the preview and controls, Left/Right on a PDF preview to change pages, and **Cmd+O** to open externally. Files tabs restore with each project's layout.

Each pane has a **lock** control. Locked panes keep their tabs, selected tab, and surrounding split sizes across project switches; panel contents still follow the selected project. The left navigation pane is locked by default. Unlock it to let that region use each project's saved layout. Locked shell tabs keep their original shell session and project context. A locked pane's tabs cannot be closed and the pane itself cannot be closed (see below); unlock it first.

Projects and Worktrees show Codex activity: **working** during an active turn, **done** after its completion, and **waiting** while idle. **Unknown** means an exact conversation binding or complete lifecycle read is not available yet. New managed Codex sessions bind on their first completion; exact resumed sessions can be tracked immediately. Large Codex logs are read from their first record and recent tail, so a long-running conversation does not need a full replay. Existing notification settings are preserved.

The **bell** beside each project controls macOS completion alerts. Alerts are off by default: a cyan bell means on, and a muted crossed-out bell means off. While RiWork is open, completed Codex turns and completed turns in newly launched RiWork Claude sessions send one alert per turn across all windows and processes. Only the packaged `RiWork.app` posts and claims alerts, so an unbundled development build sharing the same data never consumes them. Click the notification or **Open agent** to open that project's completed agent.

Initial **Done** states, historical completions, aborted turns, and ordinary shell exits do not alert. Existing Codex sessions with an exact conversation binding also work; start a new Claude session to load its completion hooks. macOS asks for permission when you first enable a bell, and **System Settings → Notifications** controls delivery. Native alerts require the packaged RiWork app; unbundled `cargo run` launches cannot send them.

Click a project row to switch the current window, or **↗** to open that project in another window. Cmd+Shift+N opens a new project window; `riwork open PROJECT_OR_PATH` opens one from the CLI. Each window keeps its own project. `riwork project use ID` selects the default for subsequent windows.

Use **+ FOLDER** (a folder-with-plus icon when **Icons instead of labels** is on) to create virtual folders in Projects, or a folder's **+** to create a subfolder. Drag a project onto a folder to file it there. Drag a folder onto another folder to nest it, or onto **UNFILED** to move it to the top level. Dropping a project onto **UNFILED** removes its folder assignment. Folder headers expand or collapse their whole subtree; **✎** renames a folder and **×** removes it, promoting its projects and subfolders to its parent. Virtual folders organize the browser without moving files.

Click a project's **⚙** to open its **PROJECT SETTINGS** tab, where you can edit its name, select a folder by its full breadcrumb, and inspect its root and repository paths. **SAVE PROJECT**, Enter, or Cmd+S applies name and folder changes. Settings tabs restore with each project's layout. App-wide themes, Cua, and window preferences remain in **SETTINGS**.

```sh
riwork project folder create "Personal"
riwork project folder create "Tools" --parent "Personal"
riwork project update PROJECT_ID --folder "Personal / Tools"
riwork project folder move "Personal / Tools" --root
riwork project folder rename "Personal" "Side projects"
riwork project update PROJECT_ID --ungrouped
```

## Persistent shells

RiWork creates `~/Documents/riwork` at app startup if needed. Use **+ PROJECT** (a box-with-plus icon when **Icons instead of labels** is on) in Projects or Cmd+N to create a project. Enter a project name to create `~/Documents/riwork/NAME`; the folder field updates automatically. Default names cannot start with a dot. Editing the folder or using Browse selects an explicit location, where the name is optional; a typed folder must be an absolute path (`/…` or `~/…`), and the form shows the resolved folder. If creation fails, folders it made that are still empty are removed. `riwork project create --name NAME` also uses this default parent. Explicit CLI paths retain their usual meaning. Projects support a single Git repository, a folder containing several repositories, and plain folders. Creation offers **Initialize Git** checked by default when no repository is found. Uncheck it to keep a plain folder. Existing repositories are reused; a wrapper folder is never initialized around them. `project add` only registers a folder. For projects with several repositories, worktree creation requires choosing a repository with `--repo`; a new repository needs an initial commit before Git can create worktrees.

```sh
riwork shell create --worktree WORKTREE_ID
riwork shell create --worktree WORKTREE_ID --harness codex
riwork shell create --worktree WORKTREE_ID --harness claude
riwork shell create --worktree WORKTREE_ID --harness grok
riwork shell create --harness codex --unrestricted
riwork shell list --all --json
riwork shell output SHELL_UUID --lines 100
riwork shell cwd SHELL_UUID
riwork shell metrics SHELL_UUID
riwork shell send SHELL_UUID "pwd"
riwork shell attach SHELL_UUID
```

Every shell has a UUID. `shell output` captures tmux scrollback and the current pane as text, including when the GPUI app is closed. Full screen terminal programs may show only their current rendered screen rather than a complete semantic transcript. Closing a tab or window detaches its shell; `riwork shell close UUID` ends it.

Mouse-wheel and trackpad scrolling use tmux's scrollback. Scrolling up enters copy mode; scroll back to the bottom or press Escape to return to the live terminal. New sessions enable mouse reporting, and existing sessions are upgraded when reattached. A new session keeps up to 100,000 lines of scrollback; tmux applies that limit only to panes created after it is set, so sessions started by older builds keep tmux's default of 2,000 lines.

RiWork's tmux server always lives in tmux's default socket directory: an inherited `TMUX_TMPDIR` is ignored, so the app, the CLI, and a terminal that sets it all reach the same server. A project folder whose name tmux would misread (`C#Tools`, `#{…}`, or a name ending in `;`) starts its shells in that folder; if tmux still cannot start a shell in the requested folder, creation fails with an error instead of opening in your home directory. Variables that name a CLI profile (`CODEX_HOME`, `CLAUDE_CONFIG_DIR`, `GROK_HOME`) and `RIWORK_CUA_DRIVER` are cleared from the server's environment when the app was started without them, so a value left by an earlier launch cannot reach a new session, and your own zsh startup files remain free to set them.

`shell send` pastes the text verbatim, whatever its length or trailing characters such as `;`, and presses Return once. Every tmux call is limited to five seconds, so an unresponsive tmux server produces an error instead of freezing the app. A Vim editor session ends when Vim quits, and RiWork then removes its entry from the Shells panel; exited agent and plain shells keep theirs.

Each project remembers its split layout and sizes, mixed panel/shell tab order, active selections, and selected worktree/task across project switches and UI restarts. Closed shell tabs stay detached; click their shell in the Shells panel to reattach them. Exited shells are skipped during restoration, and new CLI-created shells join the active pane when the project next opens. An intentionally empty workspace stays empty. Layouts are saved separately in `layouts.json` under the same data directory. A project whose saved layout cannot be read, such as one written by a newer RiWork with panel types this build lacks, opens with a default layout and a notice; other projects are unaffected. Unreadable entries and unknown fields are written back unchanged and never replaced, so an older build cannot destroy a newer build's layouts (changes to that project's layout are not saved while it stays unreadable). A `layouts.json` that is not valid JSON is kept as `layouts.corrupt-*.json` when layouts are next saved, and a file with a newer schema version is left alone.

The full-width bottom status bar shows the current project on the left and live sessions, CPU, resident RAM, usage, Codex account, the active shell UUID, **G·ORCH**, and **P·ORCH** on the right by default. The account item shows the focused Codex session's email when Orca's public account metadata supplies one; otherwise it says **Email unknown**. When another tab is focused, it says **DEFAULT** and identifies whether the project inherits the app choice, selects the system default, or selects a saved account. Click the account item to open Project Settings, or press Cmd+Option+A. Click the status UUID to copy it. Configure status items in **Settings → Status bar**. The Shells panel shows worker shells with their metrics and current worktree when their working directory matches a known worktree.

## Harness launches and usage

The **New Tab** section of a pane's **…** actions menu launches a shell, Codex, Claude, or Grok in the selected worktree. Normal presets use the CLI's usual permissions. Explicit **unrestricted** presets pass each CLI's permission bypass flag. Cmd+Shift+C opens Codex; Cmd+Shift+L opens Claude; Cmd+Shift+G opens Grok. Grok scheduling, RiWork activity and completion alerts, and RiWork quota display are not yet supported.

Click the bottom usage readout, or open **USAGE** under **Views** in a pane's **…** actions menu, for remaining quota, reset countdowns, and update times. Codex reads its official app-server account endpoint on a worker with a timeout, refreshing every 15 minutes or on demand. Claude sends documented statusline JSON through settings passed only to that invocation; global Claude settings are unchanged. Claude quota appears after a response on supported Pro/Max accounts and versions. Context usage and estimated session cost appear separately. Missing quota windows stay unavailable; old snapshots are marked stale. Quota is shared by sessions on the same account. `riwork usage` without `--shell` reports the account of the shell it runs in (`RIWORK_SHELL_ID`): that shell's frozen Codex account, else its project's choice, else the app choice.

```sh
riwork usage --json
riwork usage --shell SHELL_UUID --json
riwork open PROJECT_NAME
riwork open /path/to/another/project
```

Project windows and independently launched app processes keep their own selected project. Projects, tasks, the shell registry, and layouts share the RiWork data directory. Older and newer builds can share it after `riwork update` leaves an older window or `riwork` on `PATH`: a build keeps the fields and shell entries it does not understand, such as a session for a harness it predates, when it rewrites `state.json` or `sessions.json`; it hides those entries instead of failing. A store schema newer than the build is still refused. Closing a window preserves its shells. There is one persistent global orchestrator and one persistent orchestrator per project.

## Orchestrator

The global orchestrator coordinates objectives and dependencies across projects. Each project's orchestrator manages its tasks, repositories, worktrees, and Codex, Claude, or Grok workers. They have separate persistent shells and ordinary workspace tabs; project orchestrators belong to their project and have no worktree, while the global orchestrator has neither project nor worktree ownership.

Click **G·ORCH** or press Cmd+Shift+O for the global orchestrator tab. Click **P·ORCH** or press Cmd+Alt+O for the current project's orchestrator tab. Both are also under **New Tab** in a pane's **…** actions menu. Opening a scope selects its existing tab in this window, or attaches its persistent session to the active pane. Orchestrator tabs drag, split, close, and restore like shell tabs. Closing a tab detaches its terminal and preserves the session. No separate orchestrator window is created.

New default orchestrators start the official `codex` CLI, install the embedded **riwork-orchestrator** skill in their context folder, and receive their scope, project UUID, and project root as appropriate. The startup message names the installed `SKILL.md` for Codex to read instead of carrying the skill text, which keeps the launch command far below tmux's limit of about 16 KB. Global context is `RIWORK_HOME/orchestrator`; project contexts are `RIWORK_HOME/orchestrators/projects/PROJECT_UUID`. Each waits for an objective. The skill covers global coordination, repository selection, task assignment, bounded delegation, progress inspection, and completion checks. Existing sessions show **LOAD SKILL** for a one-time upgrade that preserves their conversation; it pastes the complete skill into the running Codex. Custom commands do not receive the skill automatically. Loading the skill does not schedule work.

Default project orchestrators launch Codex in unrestricted mode, with approval prompts and sandboxing disabled. This mode is saved with the session and preserved when its conversation is reloaded. Existing project orchestrators keep their startup mode; close and recreate them to use the new default. Global orchestrators retain the selected Codex account's permission and approval settings. Custom commands run as supplied. On a global orchestrator's first launch, Codex may ask you to trust its context folder; answer that prompt in the terminal before the startup skill loads.

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
| Close tab / pane (shell stays alive; refused in a locked pane) | Cmd+W / Cmd+Shift+W |
| Next / previous tab | Ctrl+Tab / Ctrl+Shift+Tab |
| Search current panel (opens Projects from a shell tab) | Cmd+F |
| Open / select Projects panel | Cmd+B |
| Open global / project orchestrator tab | Cmd+Shift+O / Cmd+Alt+O |
| Project Settings (Codex account) | Cmd+Option+A |
| Focus current tab / restore workspace | Cmd+Shift+F |
| Open Codex / Claude / Grok | Cmd+Shift+C / Cmd+Shift+L / Cmd+Shift+G |

While the Create project or folder dialog is open, these shortcuts are ignored. Focusing a terminal, by click or by opening a tab, ends a Cmd+F search so typing reaches the shell only.

Panel tabs start in a left pane. Every pane's tab strip stays at the top, and every panel and shell tab can move, close (unless its pane is locked), or share a split:

- Drag a tab along a strip to reorder it, or onto another strip or pane center to move it into that pane.
- Drop on a pane edge to create a split. Drop within 12 px of a window edge to dock across the entire workspace.
- Drag the 5 px split dividers to resize panes.
- Drag empty strip space on a strip touching the window's top edge to move the window; double-click that area to zoom. A blank drag area beside the pane controls stays reachable when tabs overflow.
- The **…** actions menu groups **New Tab** launches, **Views** for panel tabs, and **Pane** actions for splitting or closing the pane. The tab's **X** closes only that tab; the labelled **Close pane** action closes its region. Both preserve shell sessions. Closing the active pane focuses the neighbouring pane that takes its space.
- The lock icon keeps a pane across project switches and keeps its tabs open. In a locked pane the tabs show no **X**, the **Close pane** row is left out of the **…** menu, and Cmd+W and Cmd+Shift+W do nothing but show "Unlock the pane to close its tabs" (or "…to close it"). Unlocking brings all of these back at once. Dragging, reordering, and selecting tabs keep working, tabs can still be dragged out to another pane, and a shell that exits, a project switch, or a restore is not affected. The focus icon or Cmd+Shift+F focuses the active tab in any pane, hiding tab strips, other panes, and the status bar. Narrow panes keep lock and focus actions in the **…** menu. Centered focus caps the content at 1,100 px and leaves 30% of the window clear below it so terminal input sits higher on screen. The top bar keeps native window controls, **RESTORE**, and a **FILL WINDOW / CENTER FOCUS** toggle visible. Click **RESTORE** or press Cmd+Shift+F again to restore the same split layout and sizes. Ctrl+Tab and Ctrl+Shift+Tab still cycle tabs within the focused pane. Focus mode is temporary and does not rewrite the saved split tree.

The workspace fills the window around a 78 × 28 px native close/minimize/maximize island and the full-width status bar. In macOS full screen, the custom island is hidden and the native controls appear when the menu bar drops down; maximized windows keep the island. The installed `riwork-workspaces` skill teaches Codex the CLI workflow for project tasks, batched worktree assignments, and shell inspection.

## Settings

Open **RiWork → Settings…**, press Cmd+, or choose **SETTINGS** under **Views** in a pane's **…** actions menu. Settings opens as a normal workspace tab: drag, split, close, and restore it like the other panel tabs. Preferences save automatically to `settings.json` under the RiWork data directory and are shared across open windows and app processes. A value this build does not recognize, for example a theme from a newer build, reads as that setting's default and stays in the file until you change that setting; an invalid Codex account selection still blocks new Codex launches instead of falling back to the system account, while plain shells keep opening.

- **Codex accounts** detects the current Codex profile and saved accounts from Orca. The current profile is `~/.codex` or the `CODEX_HOME` from your own environment, and its row shows the resolved path; an account home that RiWork exported into a session is never mistaken for it, so a shell inside an account-bound session still starts system-default sessions on your own profile. Use **REFRESH ACCOUNTS** to check again, then select an app default for new Codex sessions; a project can override it in **Project Settings**. Existing sessions and resumed conversations keep their original account. Quota is read separately for each account and follows the active Codex session's saved account.
- **Project Settings → Codex Account** selects **Inherit app default**, **System default**, or an available saved Orca account. The choice saves immediately and applies to new Codex sessions and new project orchestrators. Older projects inherit the app setting. Global orchestrators always use the app setting. An explicit saved account that later becomes unavailable (for example when Orca is removed or the account is deleted) remains selected and prevents new Codex launches, including the `codex` launcher in a shell, with an error until it is restored or changed. Plain shells still open; they just start without an injected `CODEX_HOME`. RiWork reads only Orca's public account list and its own metadata cache; it never reads account credentials.
- New plain project shells start with `CODEX_HOME` set to the project's effective choice. The RiWork `codex` launcher in a plain shell resolves the current project choice each time a new Codex invocation begins; a resume or managed child keeps its original account. A custom command that runs `codex` by name goes through the same launcher and resolves the project choice too; only an absolute path to a Codex binary bypasses it and uses its own environment. Utility commands such as `codex login` skip account binding, so RiWork's launcher refuses `login`, `logout`, and other credential or configuration changes (`mcp add`, `plugin`, …) while `CODEX_HOME` is an Orca-managed account home: manage that account in Orca, or point the command at your own profile with `CODEX_HOME=~/.codex codex login`. A plain shell shows a Codex account (**A1**, status bar) only while Codex is running in it. Changing a default never changes a running process or its saved account label. When multiple live Codex account homes exist in one project, tabs show **A1**, **A2**, and so on beside the session title; the focused account uses the same number in the status bar. Numbers derive from the sorted frozen homes within each project and may change when the set of live accounts changes.
- **Status bar** lets you show or hide the entire bar, choose visible items, move each item left or right, and change its order with the arrow buttons. The current project appears on the left by default. Worktree and agent activity are optional; **RESET DEFAULTS** restores the original arrangement. Preferences apply across windows.
- **Theme** defaults to **Follow Ghostty**: panels, tabs, menus, and terminals share your Ghostty palette. RiWork uses Ghostty's own configuration loader, including recursive files and custom themes. It checks those files every couple of seconds and re-reads the configuration only when one changed, so palette edits appear within a few seconds. **RiWork**, **Catppuccin Mocha**, **Tokyo Night**, and **Gruvbox Light** apply matching workspace and terminal colors while retaining your other Ghostty preferences. Preferences are shared across windows and app processes. Selecting a RiWork theme does not edit the standalone Ghostty app's configuration.
- **Use RiWork terminal colors** remains available under Follow Ghostty for existing preferences and is off by default. Turn it on to keep RiWork's original terminal palette. Theme changes preserve running shells, Codex/Claude/Grok sessions, tabs, and splits; returning to Ghostty's colors reconnects only terminal display clients.
- **Icons instead of labels** is off by default. Turn it on to show a small icon instead of the name on the built-in panel tabs (Projects, Worktrees, Files, Tasks, Shells, Usage, Settings, Project Settings, and Schedules) and on toolbar buttons: **+ FOLDER** and **+ PROJECT** in Projects, the create-folder button in Project Settings, and the Files actions (Edit in Vim, Copy Path, Copy Contents, Reveal, Open Externally). Hover an icon for its full name and shortcut; a button that is unavailable says why. Shell, agent, orchestrator, and editor tabs keep their titles, and the focus-mode bar still shows the full name. It applies to all open windows at once and is saved as `panel_tab_icons` in `settings.json`; the key keeps that name from when the setting only covered tabs, and a build without the setting leaves it alone.
- **Remember project window size** is on by default. New or reopened project windows restore the last normal window size, fitted to the current display's visible area, and later windows in the cascade stay on that display. Only a size you set by resizing is saved: zooming (the green button or a titlebar double-click), entering fullscreen, or the smaller size a window is clamped to on a smaller display does not overwrite it. Switching projects in an existing window keeps that window's size. Layouts without a saved size use the default size until the window is resized.

The embedded Ghostty adapter currently uses the light variant of a paired `light:…,dark:…` theme and does not follow macOS appearance changes. A single Ghostty theme or an explicit RiWork theme applies consistently.

## Scheduled prompts

Open **SCHEDULES** from the pane **+ / ▤** menu, **RiWork → Schedules**, or **Cmd+Shift+S**. App schedules target the existing global orchestrator, project schedules target their existing project orchestrator, and workspace schedules target an explicitly selected worker in a worktree. The compact native panel supports quick future first-run choices with local timezone previews, optional exact date/time entry, one-time or elapsed-time recurring prompts, edit/pause/delete, and visible next/last outcomes.

Scheduling runs while RiWork is open. Busy or blocked targets defer for up to five minutes; older occurrences are skipped. There is no overdue replay or automatic retry of uncertain delivery. Targets remain pinned across windows and restart; failed or uncertain schedules pause for review. See [timing, lifecycle, persistence, and verification](docs/scheduling.md).

Agents can configure these schedules with `riwork schedule list|show|create|update|pause|resume|delete` or the discoverable `riwork_schedule_*` tools in `riwork mcp`. Run `riwork schedule help` for UUID, scope, revision, and time requirements; [CLI and MCP examples](docs/scheduling.md#cli-and-mcp-agent-configuration) cover all three scopes.
