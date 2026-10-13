# Idle-work improvements — 2026-10-12

Three small cuts to steady idle churn, found by auditing the periodic
timers after the earlier sweeps (desktop idle-work, phone request path,
`docs/performance-native-theme.md`). The large costs were already handled —
one shared tmux/`ps` sample per tick, change-gated redraws, FSEvents
project recency, the cached Settings view. These remove what was left.

## 1. `state.json` and `settings.json` are parsed only when they change

`Store::snapshot()` took a file lock, read the whole file, and parsed it on
every call — synchronously on the UI thread every 2 s in each project
window and in the global window, plus once a second from the schedules
panel. `SettingsStore::load()` did the same. Both stores now keep the last
parse behind a file stamp (length, mtime, inode — every writer here
replaces the file via temp + rename, so an equal stamp means equal bytes):

- Unchanged stamp: the cached value is cloned back; no lock, read, or parse.
- Changed stamp: the old locked read runs, and the result is cached only if
  the stamp stood still across the read.
- The stores' own writes (`transaction_if_changed`, `update`) seed the
  cache with what they just wrote, so writers never re-parse either.

The cache is shared by every clone of a store (`Arc<Mutex<…>>`) and needs
no API change, so all existing callers benefit. What remains per tick is a
`stat` and, in the windows, the in-memory `State` equality check.

Code: `crates/riwork-core/src/store.rs` (`FileStamp`, `snapshot`),
`crates/riwork-core/src/settings.rs` (`load`).
Tests: `snapshot_reuses_the_last_parse_until_the_state_file_changes`
(`store_tests.rs`), `load_reuses_the_last_parse_until_the_settings_file_changes`
(`settings.rs`) — including a same-length/same-mtime rewrite that must be
served from cache, plus change, removal, and recreation being seen at once.

## 2. The schedules panel stops refreshing while its tab is covered

Once opened, the panel entity lives in the workspace for the window's
life, and its 1 s loop ran a store snapshot, the session sample, the
schedule list, and a full chat-info read every second even when another
tab covered it. (The redraw was already change-gated; the background work
was not.) The workspace now reports whether the Schedules tab is showing,
following the same pattern as the project-settings panel:

- Covered: the loop still ticks but does no reads.
- Coming forward — via the workspace tick or the panel taking focus — it
  refreshes immediately, so nothing waits up to a second to appear.

Code: `crates/riwork-desktop/src/schedule_panel.rs` (`on_screen`,
`set_on_screen`), wired from `Workspace::refresh` in
`crates/riwork-desktop/src/main.rs`.
Test: `hidden_panel_skips_its_refresh_until_it_comes_forward` — a real
panel over a temp store: hidden, it misses an external change across
three loop ticks; shown again, it has the change without waiting.

## 3. Invisible windows skip heartbeat redraws

Both window kinds redrew every fifth tick (10 s) to keep relative ages
("3m ago") moving, and project windows also redrew every tick while the
Files/Project-settings tab was active in a pane — regardless of whether
the window itself was visible. Those redraws now require
`window_visible`; a hidden window catches up on the first tick after it
shows again. Real content changes still notify as before.

Code: `crates/riwork-desktop/src/main.rs` (`Workspace::refresh`),
`crates/riwork-desktop/src/global_window.rs` (dashboard heartbeat).

## Deliberately not changed

- The 80 ms tmux output polling (`OUTPUT_POLL` in
  `crates/riwork-engine/src/sessions.rs`) is a safety mitigation for a
  tmux control-mode crash; see `GPUI_KIT_WATCH_POLLING_MITIGATION.md`.
- Release-profile tuning (LTO, `codegen-units`, stripping the desktop
  binary) should be measured with the frame procedure in
  `docs/performance-native-theme.md` before adopting, against its build
  time cost. Not attempted here.

## Verification

- `cargo test -p riwork-core --lib`: 240 passed (includes the 2 new tests,
  which failed before the change).
- `cargo test -p riwork-engine --lib -- schedules:: notifications:: project_tabs::`
  (the main `snapshot()` consumers): 59 passed.
- Desktop: new panel test, `schedule_panel::kit_control_tests`, and
  `global_window::` tests pass; `cargo check -p riwork-remote --all-targets`
  clean; `cargo fmt --all --check` clean.
- Known sandbox limit while verifying: chat-host tests that bind a Unix
  socket (`TestHost`) fail with `Operation not permitted` in this
  environment on the untouched tree as well — confirmed by running one
  (`cli::chat_tests::ensure_prints_the_socket_to_connect_to`) with these
  changes stashed. They are expected to pass where socket binding is
  allowed; no behavior they cover was changed.
