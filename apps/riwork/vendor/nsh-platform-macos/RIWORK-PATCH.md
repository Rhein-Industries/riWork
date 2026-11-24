# nsh-platform-macos 0.6.1, patched for Somtus

The crates.io release (upstream commit `2f51d8611677c8a6bbad937b39756e8d0cc45350`, archive
SHA-256 `8fe81366570211925b77823c8210868d0943976da55944bb327eaa27f215d49e`) with one change:
`NSH_HIDDEN_WINDOW=1` keeps the app's window hidden (transparent, off screen, click-through).
`scripts/pane-shot.sh` sets it, so `nsh test --target macos` can draw and photograph a pane
while someone works. Without the variable it behaves exactly like the release.

NSH itself runs test hosts in the background since 0.5 (`AppOptions::background`, the host's
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
