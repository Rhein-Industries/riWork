# Remote access

[Documentation index](../README.md) · [Project overview](../../README.md)

Repository paths and commands below are relative to the checkout root.

For SSH routes and phone access through a paired desktop, see [SSH hosts](../ssh-hosts.md) and [phone gateway setup](../phone-ssh-gateway.md).

- [Encrypted iOS access](#encrypted-ios-access)
- [Controlling another Mac](#controlling-another-mac)

## Encrypted iOS access

The standalone `somtus-remote` relay and outbound desktop connector let paired
mobile devices inspect existing projects/tasks/worktrees, read persistent shell
and orchestrator output, submit a line to an explicitly selected existing
session, start or close a terminal (a shell, Codex, Claude, Grok or Muse) in a
project or worktree, start the global or a project's orchestrator (a terminal
or a chat, as this Mac's Settings say; an orchestrator that runs as a chat is
followed and driven with the chat methods), and create a new project in the default projects folder
(the phone names it and never chooses a location). A running Somtus window adds
a tab for a terminal started this way (or by `somtus shell create`) behind its
current one, without taking focus, and lists a project made this way (or by
`somtus project create`) in its Projects panel within a couple of seconds. A phone can
also send a photo or a file to a terminal, whose program gets its path as from a drop on
the terminal, or to a chat, whose message gets its path (see [Files from the phone](terminals.md#files-from-the-phone)). Each device has independent endpoint secrets and revocable access; the
relay routes encrypted frames without session content or pairing secrets.

The [iPhone and iPad app](../../ios/README.md) opens with an **All hosts** dashboard for
its paired desktops: work in progress, decisions waiting for you, projects and
open tasks. Each row names its host, and choosing a desktop opens that host's
dashboard. Unavailable hosts retain their last successful read with an age;
partial totals say how many hosts were read. Looking at a dashboard creates no
sessions or projects. A decision opens its original approval choices or question,
then checks the exact host, chat and current request again before sending your
explicit response. An uncertain response is never retried automatically.

**Haiku dashboard summaries** is an optional desktop setting beside the saved
Anthropic API key. It is off by default and uses separately billed API requests
to `claude-haiku-5-5` to summarize observed activity. The desktop and phone show
these summaries with their freshness; actionable decisions still come from
provider requests. Unchanged evidence is reused, requests are at least one minute
apart, and each host is limited to 120 requests per day. Missing keys or API
failures leave the underlying dashboard available. See
[dashboard decisions](../../docs/dashboard-decisions.md).

For a connected Clicks keyboard with the software keyboard hidden, the phone puts
project tabs at the bottom and treats physical **Tab** as a shortcut prefix:
**Tab Q** sends a normal Tab, **Tab E** sends Escape, **Tab W/A/S/D** moves with
arrows, and **Tab 1–9** switches tabs. Hold Tab for one second to see the full map.
The Standard profile and touch Tab keep their usual behavior. Choose the Clicks
profile in **Display → Keyboard** if iOS reports a generic keyboard name; see
[keyboard controls](../../ios/README.md#project-and-terminal-tabs).

```sh
cargo build --locked --release --manifest-path remote/Cargo.toml
export RIWORK_REMOTE_BIN="$PWD/target/release/somtus-remote"
somtus remote --help
somtus remote start --somtus /absolute/path/to/somtus
```

Build the standalone binary before bundling to include it beside the desktop
CLI. `RIWORK_HOME` is retained. Stopping the transport preserves tmux and harness
state. Mobile tabs can temporarily resize the existing tmux grid to their visible
terminal area; disconnect restores desktop sizing and keeps the same process.
Follow [pairing and test instructions](../../remote/README.md), the
[frozen mobile protocol](../../docs/remote-protocol.md), and
[TLS relay deployment](../../docs/remote-deployment.md). v1 supports captured terminal
text and single-line input, with persistent request deduplication and explicit
unknown-outcome errors.

Open the native iPhone/iPad project in [ios/RiWorkRemote.xcodeproj](../../ios/RiWorkRemote.xcodeproj).
See [iOS setup and verification](../../ios/README.md) for building, pairing and continuing
in the compact project and terminal-tab interface.
## Controlling another Mac

A second Mac running Somtus can open this Mac's projects and shells as real terminals, with the fidelity of a local one (colors, mouse, full-screen programs, scrollback). The shells stay on the Mac they belong to, in its own tmux; a tab on the other Mac only attaches to one, so closing the tab, closing the window or losing the connection never ends the shell. The two Macs talk through the same encrypted relay as the phone companion, and the app needs the `somtus-remote` binary next to it, as `somtus remote` does.

**Pairing.** On the Mac whose shells you want to reach (the host), open Settings → **Remote** → **PAIR ANOTHER MAC**. Name the other Mac, give the relay's `wss://…/v1/ws` URL and the relay's routes file (it starts at `RIWORK_HOME/remote/relay-routes.json`). Somtus runs `somtus-remote pair --protocol 2 --kind desktop` and shows a single-use link that expires after about ten minutes, with **COPY LINK**. Add the route `pair` appended to your relay's routes and restart the relay, exactly as for a phone. The link is a secret: it is held only while the dialog is open, is never written to disk or a log, and is shown cut short. On the other Mac, Settings → Remote → **ADD HOST**, paste the link, and optionally name the host; Somtus runs `somtus-remote hosts add --link -` and hands it the link on standard input, never on the command line. Settings lists every paired Mac with its link state; **REMOVE** (click twice) forgets one, and it must send a new link to be paired again. A paired Mac has the same authority as a phone, which includes typing into any terminal, so pair only Macs you control. Revoke one on the host with `somtus remote revoke DEVICE_UUID`.

**A paired Mac is a folder.** In the Projects panel each paired Mac appears below your own folders, named after it, with a small dot: cyan for online, gold for connecting, a hollow grey one for **Offline (or access revoked)**, which is all Somtus can tell about a sleeping Mac and one that revoked this one. Its projects are listed inside as ordinary project rows with the same figures (trees, done/all tasks, live shells), ordered and searched like the others (a Mac has no file times to sample, so **Last edited** follows the date a project was added). The folder opens and closes like the others, but it cannot be renamed, moved or removed here. The rows are read from the Mac only while the Projects panel shows the folder, and the figures a few projects at a time; a Mac that cannot be reached keeps listing what it listed last, dimmed. That list is kept in `remote-ui.json` in the Somtus data directory (mode 600), so a restart shows it before the Mac answers. **+** on the folder asks for a name and creates the project on that Mac with `project.create`, in its default projects folder. It is sent once and never retried: if no answer comes, the folder says the project may exist and asks for its list again, and trying again is up to you.

**Selecting a remote project** works like selecting a local one. It becomes the window's project, under its own layout, and the window title and status bar name it with its Mac, as in `Somtus · app (on Studio)`. Its ids are kept as `remote:{host}:{project}` wherever the window stores a project, so they can never be looked up in, or collide with, this Mac's store; nothing local is created for it, and selecting it does not change the active project that `somtus` commands use. The panels follow:

- **Worktrees**, **Tasks** and **Shells** list what the Mac reports (`worktrees.list`, `tasks.list`, `shells.list` and `orchestrators.list`) while they are on screen. Tasks are read-only, as the Mac offers no task changes. Choosing a worktree does what it does locally: it becomes the one new terminals start in, and a tab already open on one of its shells is shown, else its newest live shell is opened, else a plain shell is started in it on the Mac with `shell.create` and opened, in the pane that is selected. That shell is made once; a second click while it is being made does nothing, and nothing is ever sent again unasked. Choosing a shell opens it as a tab.
- **New Tab** (the **+** menu, Cmd+T and the Codex, Claude, Grok and Muse shortcuts) asks the Mac for a Shell, Codex, Claude, Grok or Muse in the selected worktree or the project's root with `shell.create`, then opens it. An agent is unrestricted or not as **Agent terminals run unrestricted** in this Mac's Settings says, and the request says which, so the other Mac's own setting does not decide. Like a project, it is sent once and never retried. The orchestrator entries open the Mac's own orchestrator if it has one running in a terminal; one that runs as a chat there has no terminal to attach to, and a notice says to open it on that Mac or in the iOS app. A split starts empty, as nothing is made on the other Mac unasked.
- **Files**, **Preview**, **Project settings**, **Schedules** and **Usage** have nothing behind them for another Mac's project yet, and say *Not available for a project on HOST* instead of showing this Mac's data. The status bar shows the project, the worktree, the live shells and the active tab's id; agent activity and resources show a dash, and usage and the Codex account are left out, as they are this Mac's own.

A window opens on the same remote project after a restart or a reload. If the Mac is offline then, the folder shows the kept list, dimmed, and the connection is made in the background.

**Tabs.** A remote tab is titled `⇄ HOST · …` and the host mark is drawn in magenta. Its terminal runs `somtus-remote attach --desktop ID --shell UUID`, a bridge to the host's tmux client through that host's client daemon (`somtus-remote client ensure` starts it on demand), so hiding, releasing and re-attaching a terminal work as for a local shell: the host's tmux repaints the screen. Remote tabs are saved in the remote project's layout as `remote_shell` tabs and come back with the window; a build without this feature keeps them untouched. While the daemon's link to the host is down, a **RECONNECTING · HOST** strip sits above the terminal. When the shell ends on the host, the bridge exits and the strip says **Ended on HOST** until the tab is closed; **RECONNECT** starts the bridge again in case it was only the connection that ended.
