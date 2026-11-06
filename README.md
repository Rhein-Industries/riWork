# SOMTUS

**Somtus is in beta.**

Somtus is the new name for RiWork. The logo and wordmark use **SOMTUS**; the public command is `somtus`, with `somtus-cli` and `somtus-remote` as headless companions. The supplied mark lives in `somtus-icon-export/` and is packaged for macOS and iOS.

Existing data stays in `~/.local/share/riwork`, and existing `RIWORK_*` environment variables, bundle identifiers, Keychain entries, and wire formats remain compatible. Cargo package names and source directories keep their existing names. New pairing links use `somtus://`; existing `riwork://` links are still accepted. See the [build and CLI setup guide](docs/guides/development.md#build-and-run) to switch a local installation to the new name.

A compact native macOS workspace built with NSH and Ghostty terminals. Tabs and split panes attach to shells in a dedicated tmux server, so shells and their output remain available when the UI closes. Projects own shells and worktrees; tasks belong to projects and can be assigned to worktrees.

The Global window’s **Agents & hosts** view combines local and remote agent status, pending attention and connection freshness. See [Multi-host agents](docs/multi-host-agents.md), [ACP agents](docs/acp.md), and [terminal status reporting](docs/program-status.md).

Project-window layouts, command bars, add-in panels and shortcuts can be customized live. See [Customizing Somtus](docs/customization.md) for the user files, extension API and reset shortcut.

## Requirements

- macOS with Xcode command line tools; macOS 14 or later for Cua.ai desktop control.
- Rust 1.99.1, selected by `rust-toolchain.toml`.
- NSH CLI 0.3.0, matching the pinned Rust NSH crates. Set `NSH_CLI` to its absolute executable path.
- Zig 0.16 for Ghostty, tmux (`brew install tmux`), and Vim for file editing.

Keep Zig in a persistent directory. See [toolchain requirements](docs/guides/development.md#requirements) and [iOS setup](ios/README.md) for platform-specific preparation.

## Rust workspace

Core models and persistence live in `riwork-core`; sessions, providers and scheduling in `riwork-engine`; CLI/MCP commands in `riwork-cli`; the AppKit/NSH desktop in `riwork-desktop`; and encrypted connections in `riwork-remote`. The desktop UI is `apps/riwork`, and the mobile UI is `apps/riwork-mobile`.

See the [package map and architecture](docs/guides/development.md#rust-workspace), [desktop NSH host](crates/riwork-desktop/README-NSH.md), and [iOS migration boundary](docs/nsh-ios-migration.md).

## Build and run

From the checkout root, set the paths to your installed tools and build the release app:

```sh
export ZIG=/path/to/zig-0.16/zig
export NSH_CLI=/path/to/nsh-0.3.0/bin/nsh
export CARGO_TARGET_DIR="$PWD/target"
(
set -eu
cargo build --locked --release -p riwork-desktop --bin somtus
riwork_ui_stage=$(mktemp -d "$PWD/target/release/.riwork-ui.XXXXXX")
trap 'rm -rf "$riwork_ui_stage"' 0
"$NSH_CLI" build apps/riwork --release --out "$riwork_ui_stage/ui"
rm -rf target/release/riwork-ui
mv "$riwork_ui_stage/ui" target/release/riwork-ui
sh scripts/bundle-macos.sh release
open target/release/Somtus.app --args /path/to/project
)
```

The project path must name an existing directory and may be omitted. Build the UI after changing TypeScript, native bridge APIs or imported assets. The packaged app includes its UI and runs without a source checkout or NSH CLI.

The [development guide](docs/guides/development.md#build-and-run) covers debug builds, signing, Ghostty resources, CLI installation, isolated TSX reloads and testing. Focused backend checks use `cargo test -p riwork-core`, `cargo test -p riwork-engine`, or `cargo test -p riwork-cli`; remote verification is documented in [remote/README.md](remote/README.md). Run `somtus help` for commands. Set `RIWORK_HOME` to use a separate data store and tmux server.

## Encrypted iOS access

The iPhone and iPad app connects to paired desktops for projects, dashboards, decisions, chats and terminals. Each device has revocable access. A paired desktop can also expose its explicitly registered SSH hosts to the phone, shown as **via desktop name**.

See [iOS build and pairing](ios/README.md), [phone access through an SSH gateway](docs/phone-ssh-gateway.md), and the [mobile access guide](docs/guides/remote-access.md#encrypted-ios-access).

## Controlling another Mac

Open another host's projects, chats and persistent terminals through an authenticated connection. The execution host owns its processes and data; closing a client view preserves its shells.

See [SSH hosts](docs/ssh-hosts.md), [multi-host agents and recovery](docs/multi-host-agents.md), and [paired Mac setup and behavior](docs/guides/remote-access.md#controlling-another-mac).

## Update and restart

```sh
somtus update            # Build release, install, and restore open windows
somtus restart           # Restore windows using the installed build
somtus instances --json  # Inspect running apps and windows
```

Updates build local source without fetching or changing Git history. Restart preserves persistent shells and window layouts. See [update options, diagnostics and rollback](docs/guides/development.md#update-and-restart).

## Cua.ai setup and computer use

Somtus setup installs Cua.ai Driver and connects supported agents to its `cua-driver` MCP server. Run `somtus setup`, then use `somtus cua status --json` and `somtus cua permissions` to inspect setup and macOS access.

See [driver setup, permissions and agent integration](docs/guides/computer-use.md).

## Projects, worktrees, and tasks

Projects own tasks, shells and worktrees. Windows retain their selected project and layout, while the underlying store and persistent sessions are shared.

See [project navigation, worktrees, task assignment and activity](docs/guides/projects-and-tasks.md).

## App preview tabs

App previews run supported desktop apps or iOS simulator builds in a tab, with explicit start/stop and target ownership.

See [preview modes and adapters](docs/guides/previews.md) and [preview CLI/MCP controls](docs/preview-control.md).

## Persistent shells

Ghostty surfaces attach to persistent tmux sessions. Detaching or closing a window preserves the shell; explicit exit ends it. File editing, uploads, terminal input, output and history use the same session identities.

See [terminal lifecycle, files, input and scrollback](docs/guides/terminals.md) and [program status reporting](docs/program-status.md).

## Harness launches and usage

Launch installed agents in terminal tabs and inspect the usage information their supported interfaces expose. Permissions and account choices are controlled by settings.

See [launch behavior and usage](docs/guides/chat.md#harness-launches-and-usage), [ACP providers](docs/acp.md), and [subscription balancing](docs/subscription-balancing.md).

## Chat host

The background chat host owns provider processes and durable conversation logs. Tabs follow those chats and can reconnect after their views close.

See [chat-host commands, providers and recovery](docs/guides/chat.md#chat-host), [event recovery](docs/chat-events.md), and [chat notices](docs/chat-notices.md).

## Orchestrator

Global and project orchestrators coordinate work in their own scopes, with settings for provider, model, permissions and delegation.

See [orchestrator operation and controls](docs/guides/orchestration.md) and [durable native chat delegation](docs/chat-delegation.md).

<a id="chats"></a>

## Chat tabs

Chat tabs provide transcripts, tool details, approval and question controls, attachments, drafts and conversation navigation.

See [chat-tab workflows](docs/guides/chat.md#chat-tabs) and the [chat improvement roadmap](docs/chat-improvement-roadmap.md), which distinguishes delivered behavior from planned work.

## Handoff

Hand off a terminal or chat conversation to another agent with transcript or summary context, preserving the source session.

See [handoff commands, context and outcomes](docs/guides/handoff.md).

## UI controls

Split panes, select and move tabs, lock panes across project switches, and enter focus mode using the workspace controls and shortcuts.

See [controls and keyboard shortcuts](docs/guides/interface.md) and [customizing layouts, commands and add-ins](docs/customization.md).

## Settings

Configure appearance, terminals, agent permissions, accounts, orchestrators, delegation and project-specific preferences.

See the [settings reference](docs/guides/settings.md).

## Scheduled prompts

Open **Automations** from a pane menu, the application menu or **Cmd+Shift+S**. Create scheduled project chats or select existing supported destinations. Uncertain delivery pauses for review and is never automatically retried.

See [Automations UI and CLI/MCP examples](docs/automations.md) and [scheduler timing, persistence and legacy targets](docs/scheduling.md).

## Documentation and ongoing work

The [documentation index](docs/README.md) separates current guides, contracts and active work from [historical implementation and verification records](docs/archive/README.md).

The [performance handover](docs/performance-audit-handover-2026-11-06.md) records pass10 as the last completed pass and pass11 as unfinished; [deployment evidence](docs/performance-audit-deployment-2026-11-06.txt) records the installed scope. Historical acceptance reports retain their original limitations and do not establish current acceptance.

The [phone SSH gateway](docs/phone-ssh-gateway.md) has been built and tested; deployment remains pending.
