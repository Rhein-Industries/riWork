# nsh-platform-macos 0.3.0, patched for RiWork

The crates.io release with one change: `NSH_HIDDEN_WINDOW=1` keeps the app's window hidden
(transparent, off screen, click-through) and the app in the background, without a Dock icon.
`scripts/pane-shot.sh` sets it, so `nsh test --target macos` can draw and photograph a pane
while someone works. Without the variable it behaves exactly like the release.

Changed: `src/app.rs` (`hidden_window`, `run_app`) and `src/agent.rs` (`activate`). Both
`Cargo.toml` and `.cargo/config.toml` patch crates.io with this folder; drop both lines and the
folder when NSH ships the option itself.
