# Build, development and updates

[Documentation index](../README.md) · [Project overview](../../README.md)

Repository paths and commands below are relative to the checkout root.

- [Requirements](#requirements)
- [Rust workspace](#rust-workspace)
- [Build and run](#build-and-run)
- [Update and restart](#update-and-restart)

## Requirements

- macOS with Xcode command line tools
- Rust 1.99.1 (selected by `rust-toolchain.toml`)
- NSH CLI 0.3.0 for the TypeScript UI build, matching the pinned Rust NSH crates. Set `NSH_CLI` to that version's absolute executable path when another version is on your `PATH`. `somtus update` requires an absolute path for this override and does not download a CLI automatically.
- Zig 0.16 for the Ghostty native build. `somtus update` uses the one unpacked as `toolchains/zig-0.16*` in the Somtus data directory (`~/.local/share/riwork` by default) when `ZIG` is not set; keep it out of `/tmp`, whose periodic cleanup deletes its standard library
- tmux (`brew install tmux`) for persistent shells
- Vim for editing files in persistent Somtus tabs (included with macOS)
- macOS 14 or later for Cua.ai desktop control; Somtus setup installs Cua Driver
## Rust workspace

The Rust workspace separates the backend from the native desktop host and UI adapters:

| Package | Location | Responsibility |
| --- | --- | --- |
| `riwork-core` | `crates/riwork-core` | Stored models, paths, configuration, and persistence |
| `riwork-engine` | `crates/riwork-engine` | Sessions, chat providers, scheduling, and orchestration |
| `riwork-cli` | `crates/riwork-cli` | CLI and MCP commands, updater, and the headless `somtus-cli` executable |
| `riwork-desktop` | `crates/riwork-desktop` | AppKit windows, the NSH host, native controls, and the public `somtus` executable |
| `riwork-nsh-native` | `crates/riwork-nsh-native` | Native APIs exposed to the TypeScript UI |
| `riwork-nsh-terminal` | `crates/riwork-nsh-terminal` | Ghostty terminal surfaces |
| `riwork-nsh-workspace` | `apps/riwork` | Workspace layout and host coordination; the TypeScript UI lives beside it |
| `riwork-remote` | `remote` | Encrypted relay, desktop connector, and remote client |

Core and engine have no GPUI or Ghostty dependency. The desktop calls the same CLI implementation before starting its UI, preserving the `somtus` command. The default workspace member is desktop; use `cargo build -p riwork-cli` or `cargo build -p riwork-remote` to build either companion independently. All Rust packages share the root lockfile and target directory. The iOS app under `ios/` now starts in NSH, with its mobile screens in `apps/riwork-mobile` and its existing native transport, chat and terminal surfaces retained during [migration](../../docs/nsh-ios-migration.md). The desktop speech helper remains under `macos/speech/`.
## Build and run

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

Run these commands from the checkout root. The NSH build refreshes the native bridge declarations and emits `bundle.js`, its source map and bytecode, app/capability metadata, and imported assets. Its `--release` option minifies JavaScript and strips bytecode debug information. The native description helper is built automatically as part of this step; no separate declaration-generation command is needed. Building into a fresh temporary directory and replacing `target/release/riwork-ui` only after success preserves the previous UI when compilation fails and removes assets that an older build no longer uses.

The bundle copies that entire directory into `Somtus.app/Contents/Resources/riwork-ui`. Bundling refuses a missing or empty UI entry; it never substitutes an old development cache. The installed app loads its packaged UI and assets without a source checkout, Cargo, or the NSH CLI. Rebuild the UI after changing TypeScript, native bridge APIs, or imported assets.

The project path is optional but must be an existing directory. Without one, Somtus reopens the active project, or registers the current directory on first launch. Any other unrecognised argument, such as a misspelled command, prints an error and exits instead of opening a window. The bundle includes Ghostty terminfo and shell integration, and copies the named theme catalog from `/Applications/Ghostty.app` when installed. Set `GHOSTTY_THEMES_DIR` while bundling to use another catalog directory. Custom themes in your Ghostty configuration directory also work. For an unbundled `cargo run`, set `GHOSTTY_RESOURCES_DIR` to the bundle's `Contents/Resources/ghostty` directory. Both the bundler and `somtus update` sign `Somtus.app` under its bundle identifier `dev.riwork.shell` with a Developer ID Application or Apple Development identity from your keychain, preferring Developer ID. Set `CODESIGN_IDENTITY` to a certificate name or hash to choose one explicitly; `CODESIGN_IDENTITY=-` selects ad-hoc signing, also used when no certificate is available. An ad-hoc signature changes with every build, so macOS may ask again for folder access after each update. Running ad-hoc and certificate-signed copies with the same bundle identifier can also repeatedly invalidate each other's permission record. Use the same certificate for your local builds so folder-access decisions remain attached to the app.

Use the release build for normal use. Both `somtus update` and `scripts/bundle-macos.sh` default to release; choose debug explicitly for development. Keep the same environment variables and build both parts without `--release`:

```sh
(
set -eu
cargo build --locked -p riwork-desktop --bin somtus
riwork_ui_stage=$(mktemp -d "$PWD/target/debug/.riwork-ui.XXXXXX")
trap 'rm -rf "$riwork_ui_stage"' 0
"$NSH_CLI" build apps/riwork --out "$riwork_ui_stage/ui"
rm -rf target/debug/riwork-ui
mv "$riwork_ui_stage/ui" target/debug/riwork-ui
sh scripts/bundle-macos.sh debug
open target/debug/Somtus.app --args /path/to/project
)
```

For an unbundled development run, point directly at the UI you just built. This also makes the choice explicit when a sibling `Somtus.app` exists:

```sh
RIWORK_NSH_BUNDLE="$PWD/target/debug/riwork-ui/bundle.js" cargo run -- /path/to/project
```

`RIWORK_NSH_BUNDLE` must be an absolute path. Keep the rest of the UI output beside `bundle.js`, including its `assets` directory. This developer override is unnecessary for the packaged app.

For automatic TSX reloads in a separate development window, use the pinned NSH 0.3.0 CLI:

```sh
NSH_CLI=/absolute/path/to/nsh-0.3.0/bin/nsh python3 scripts/dev-ui.py
```

The launcher builds the desktop with `dev-reload`, starts `nsh dev --server-only`, and opens this checkout using separate data and runtime directories under `target/dev-ui`. `--project /path/to/project` chooses the project and `--home /path/to/dev-data` chooses a separate persistent development home. It leaves the installed app running. Saving TSX rebuilds the UI and reloads every development window without rebuilding Rust. Build failures keep the current UI. Before a reload, the host flushes drafts and layouts, stops the old UI engines, and waits for terminal clients to detach; persistent shells and chats remain owned by their backend services. The new views restore their current project and layout. NSH 0.3's public embedding API remounts the whole view, so local component state, focus and scroll position reset. Rust changes still require stopping and rerunning the command. Ctrl-C stops the development window and server.

The optional host connection is selected by `RIWORK_NSH_DEV_URL=ws://127.0.0.1:PORT/ws` and exists only in builds with `--features dev-reload`; ordinary packaged builds have no dev client.

To make the CLI available everywhere, link the built executable into a directory on your `PATH`:

```sh
ln -sfn "$PWD/target/release/somtus" "$HOME/.local/bin/somtus"
```

**Tests.** `ZIG=/path/to/zig-0.16/zig cargo test --workspace` runs the fast, deterministic tests across the Rust packages: formats and protocols, data safety, permissions and the chat, session and schedule state machines, with no real tmux server, agent process or timing. Use `cargo test -p riwork-core`, `cargo test -p riwork-engine`, or `cargo test -p riwork-cli` for focused backend work without the desktop UI dependency. Tests that drive real tmux, real child processes or wall-clock timing are marked `#[ignore = "slow: …"]`; run them before a release, on an otherwise idle machine, with `cargo test --workspace -- --include-ignored`. The phone connector can also be selected with `cargo test -p riwork-remote` (its ignored end-to-end tests need the setup in `remote/README.md`). Main is kept `cargo fmt`-clean (`cargo fmt --all --check` before committing), so formatting never arrives as a repo-wide diff that every open branch then conflicts with.

Run `somtus help` for the full command list and `somtus --version` for the installed version. Data defaults to `~/.local/share/riwork`; set `RIWORK_HOME` to use a separate store and tmux server. An empty `RIWORK_HOME` or `HOME` counts as unset. A newly created data directory is owner-only, and `state.json` and `sessions.json` are written with owner-only permissions.
## Update and restart

```sh
somtus update                 # Build release, install it, and reload all open windows
somtus update --debug         # Explicit development build
somtus restart                # Quickly restart all open windows using the installed build
somtus reload                 # Compatibility alias for restart
somtus instances --json       # Inspect running apps and their windows
somtus restart --session      # Also resume this Somtus-hosted Codex conversation with Cua
```

`restart` restores every open Somtus app and window using the latest installed build,
without rebuilding. Use `update` to build and install the current source and restore all
windows in one command. `reload` remains an alias with the same behavior; `--all` is
accepted for compatibility because both commands always include all running apps.

Reloads preserve each window's project, layout, position, and persistent shells. The old app exits only after its replacement has restored the windows. If the replacement has not confirmed within 25 seconds, the old app keeps its windows, stops the replacement it launched (SIGTERM, then SIGKILL after 3 seconds, sent to the replacement's own process group only after its pid and start time are checked) and records why in the reload report, including, when known, the terminal session the replacement was stuck attaching and the diagnostic file under `~/Library/Caches/riwork/runtime/diagnostics/` to read. A replacement that no one is left to stop ends itself 40 seconds after starting to restore, unregistering first. Stopping a replacement closes only the terminal clients attached to it; the tmux sessions and their agents keep running. Running agents stay attached to their existing tmux sessions. Apps opened before reload support was installed need one normal quit and reopen first. A running app relaunches only as its own executable or the build recorded by `somtus update`; it refuses any other path found in a reload request. A running app whose registration this build cannot read (an incompatible version) is reported as a warning instead of being silently skipped, and counts as `unreadable_registrations` in `--json` output. `reload` and `update` still reload every readable app, then exit non-zero because that process keeps running its old build; quit and reopen it, or run `somtus reload` from the build that started it.

`update` builds and packages your local source in a staging directory, including `somtus-remote` when the checkout contains its manifest, then installs the executable and app together. A companion failure preserves the previous installation. It defaults to an optimized release build, including when run from a debug CLI. Use `--source /path/to/riWork` to choose a checkout, `--debug` for a development build, or `--no-reload` to install without reopening windows. `--release` selects the default explicitly; it cannot be combined with `--debug`. The command does not fetch or change Git history. Later `open` and `reload` commands use the latest successfully installed app, including when its profile or source checkout differs from the CLI's. Build failures keep the installed app running and retain a diagnostic log.

For an NSH checkout, `update` also builds the UI into a fresh staging directory and packages all its assets with the executable. It requires NSH CLI 0.3.0; use `NSH_CLI=/absolute/path/to/nsh` to select it. A missing or incompatible CLI, or a failed UI build, leaves the installed app untouched. Updating a legacy GPUI checkout keeps its original build flow.

Before anything is replaced, `update` checks the staged bundle's code signature and starts both the staged executable and the one inside the bundle with `somtus help`. A build that compiles but fails to start is rejected, and the installed build stays as it was. The build being replaced is kept next to the new one as `Somtus.app.previous` and `somtus.previous` in `target/<profile>` until the next successful update replaces them. `update` prints the `.previous` location and repeats it if the reload fails. To roll back after a bad update:

```sh
cd target/release   # or target/debug
mv Somtus.app Somtus.app.bad && mv Somtus.app.previous Somtus.app
mv somtus somtus.bad && mv somtus.previous somtus
somtus reload
```

Ctrl-C, closing the terminal, or SIGTERM stops the running cargo, rustc, or bundler process group and removes the staging directory; the installed build is untouched. Each update begins, under its lock, by removing staging directories left by interrupted runs and all but the five most recent `target/riwork-update-*.log` files. A staging directory kept after a failed install is retained for a week.

`--session` applies to the current Somtus Codex pane. It waits for the active turn to finish, then resumes the same conversation UUID through Somtus's Cua launcher. Other agents keep running. New Somtus launches bind tool commands to their own terminal even when Codex shares a backend. For an older launch without that binding, use `somtus reload --session --shell SHELL_UUID`, selecting the conversation's terminal UUID from Somtus. Use it after installing a new harness integration; reopening the app alone cannot replace the tools of an already running agent.
