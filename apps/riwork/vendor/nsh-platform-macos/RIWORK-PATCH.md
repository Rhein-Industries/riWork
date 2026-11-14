# nsh-platform-macos 0.5.1, patched for Somtus

The crates.io release (upstream commit `224d9a795c7ac9d10d75750de9351adac5549675`, archive
SHA-256 `274ad01eb629fd822560ae40695af1b181baa44febdd2931b7749cc6f25d3b81`) with one change:
`NSH_HIDDEN_WINDOW=1` keeps the app's window hidden (transparent, off screen, click-through).
`scripts/pane-shot.sh` sets it, so `nsh test --target macos` can draw and photograph a pane
while someone works. Without the variable it behaves exactly like the release.

NSH 0.5 itself now runs test hosts in the background (`AppOptions::background`, the host's
`--background`, which `nsh test --target macos` passes): the app never becomes active, takes
no keyboard and has no Dock icon. That part of the 0.3.0 patch is gone. Upstream still shows
the window, in front of the others, on a second display or, with only one, on the display in
use; hiding it is what remains here.

Changed:

- `src/app.rs`: `hidden_window()`; `run_app` runs a hidden app in the background (so the
  variable also hides a host started without `--background`); `show_launched` makes the
  window transparent, shadowless, click-through and moves it off every screen before ordering
  it in.
- `src/synth.rs`: `shot` draws a hidden window in process (`drawn`, AppKit's
  `cacheDisplayInRect:`, as 0.3.0 always did); the window server has no picture of it.

Only the main window is hidden: a window the app opens itself (`<Window>`) still shows.

The published `tests/` folder is left out: its `host/*.tsx` files would be picked up as part
of the app by `apps/riwork/tsconfig.json` and `nsh test`. The crate is built only as a
dependency, so its `[[test]]` targets are never used.

Both `Cargo.toml` and `.cargo/config.toml` patch crates.io with this folder; drop both lines
and the folder when NSH ships the option itself.
