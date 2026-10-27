# libghostty / NSH AppKit spike — NO-GO

Task: `db4930af-e7a8-455c-8fe7-2de612ae102c`

Branch: `nsh-spike-ghostty` (assigned off `nsh`)

Date: 2026-10-27

Scope: throwaway code only; 60-minute ceiling, stopped on the confirmed teardown blocker.

**NO-GO for adopting this implementation as the NSH terminal host.** Direct
libghostty rendering and input inside Rust/AppKit/NSH are feasible and were
demonstrated without GPUI. However, closing the fixture blocked the AppKit main
thread in `ghostty_surface_free` for more than 47 seconds. It exited only after
an external, explicitly targeted detach of its isolated tmux client. Reliable
native-component disposal is therefore not proven. A real transition between
different display backing scales also remains unverified on this single-display
machine. No production workaround was attempted after the NO-GO decision.

No merge, push, installation, or task-status update was performed. The assignment
has **not** been marked done. The fixture app and its isolated tmux server were
stopped after evidence collection.

## verification_environment

- Worktree: `/Users/dominik/orca/projects/riWork-nsh-spike-ghostty`.
- `RIWORK_HOME=/private/var/folders/bb/kghlxdk94hj1nkcmwsr90r2c0000gn/T/riwork-ghostty-spike.0znRWa`.
- **Isolated RIWORK_HOME + fixture app only.** No live RiWork/orchestrator window
  was inspected or used for testing. No default tmux socket was contacted.
- Dedicated tmux socket: `$RIWORK_HOME/tmux.sock`, config `/dev/null`, session
  `ghostty-spike`; raw Python recorder running in the real tmux pane.
- Fixture binary: `spikes/ghostty-nsh/target/debug/riwork-ghostty-nsh-spike`.
  Successful run PID `26521`, window `6051`; failed-command trial PID `18990`,
  window `6043`. Window title: `RiWork Ghostty NSH SPIKE — isolated fixture`.
- Desktop provider: **RiWork Cua.ai Driver MCP only**. Every input call passed
  the exact fixture PID and window. All driver actions requested background
  delivery. The host used `orderBack`, with no application activation call.
- macOS 26.2 / arm64; Rust 1.99.1; Xcode 26.0.1 (17A400).
- One available display, ID `2`, logical frame `3360×1418`, backing scale `2`.
- NSH data/cache directories were under RIWORK_HOME. Ghostty loaded only fixture
  config files, with shell integration disabled; user Ghostty config was not loaded.
- Details: [verification_environment.json](evidence/verification_environment.json).

## Implementation and embedding decision

The separate Cargo workspace pins `nsh-core` and `nsh-platform-macos` to `0.3.0`.
`AppView::register_component("riwork.terminal", …)` returns a Rust-owned retained
AppKit NSView through `NativeComponent`. `src/main.rs` creates a real AppView and
NSWindow. NSH controls deliver native commands and reactive theme properties.

`native/terminal.m` implements a new `NSTextInputClient`/NSResponder bridge and
calls the full `ghostty_app_*` / `ghostty_surface_*` API. Ghostty supplies the
Metal renderer and PTY. There is no `gpui` crate, `gpui-libghostty` Rust dependency,
GPUI event loop, or GPUI Entity. [cargo-tree.txt](evidence/cargo-tree.txt) records
the dependency graph. The linked native archive is a pre-existing cached
Ghostty `1.3.2-dev` build, also used by the installed crate's native shim; using
that archive is not a claim of a new reproducible Ghostty source build.

The MIT `gpui-libghostty` shim is useful initialization/build reference, but its
view returns `nil` from `hitTest:` and delegates input to GPUI. Forking it still
requires an AppKit input client. This spike instead wrote that thin native host
directly. Ghostling uses VT state plus Raylib, so it would require a different
renderer/PTY integration. Full comparison, pinned source links, artifact hashes,
and ABI risks are in [evaluation.md](evaluation.md); notices are in
[third_party_notices.md](third_party_notices.md).

## Observed results

| Gate | Result | Evidence |
| --- | --- | --- |
| Rust/AppKit NSView hosted by NSH 0.3 AppView | PASS | Compiled and displayed `riwork.terminal`; no GPUI dependencies |
| Real tmux attachment | PASS | Client PID `26816`, `/dev/ttys019`; session `$0`, creation `1793131469`, pane `%0`, recorder/pane PID `14167` |
| Keyboard | PASS, bounded | `NSH-keyboard-123` exactly once; Return `0d`, left arrow `1b5b44`, Ctrl-C `03` reached recorder |
| IME basics | PASS, bounded | Actual current-layout dead key via driver `=` then `a`: marked text, visible preedit, one `â` (`c3a2`) per composition; performed twice |
| Japanese callback plumbing | PASS, synthetic only | `setMarkedText("にほん")`, candidate-rect query, commit `日本語` (`e697a5e69cace8aa9e`); no Japanese input-source/candidate UI test |
| Mouse | PASS | Raw SGR press/release, five drag reports (code 32), twelve wheel reports (code 64), matching decoded recorder events |
| Resize | PASS | Driver changed window `1000×712` to `820×580`; pane changed `108×32` to `88×24`, with intermediate SIGWINCH sizes |
| Theme + live update | PASS | Dark `#101820` → light `#f8f5ec` → dark; screenshot review; same surface and client throughout |
| Content-scale API | PASS, synthetic only | At fixed `800×441` points: scale 1 produced `800×441` pixels; scale 2 produced `1600×882`; rendering recovered |
| Physical display-scale transition | **UNVERIFIED** | Only one 2× display available; no display settings were changed |
| Detach/reattach retained view | PASS | Native NSView removed/re-added; same surface `0x767558000`, same tmux client `26816`, same session/pane/recorder; `REATTACHED` typed afterward |
| Destroy/unmount/close | **FAIL — blocker** | NSH `NativeComponent::drop` → `ghostty_surface_free` → main-thread `pthread_join`; attached client persisted; external detach needed |

Screenshots: [light theme](evidence/theme-light.png),
[resized dark / scale 2](evidence/resized-scale2-dark.png),
[actual dead-key preedit](evidence/dead-key-preedit.png).
Raw bytes/callbacks are in [input-events.jsonl](evidence/input-events.jsonl) and
[native-events.jsonl](evidence/native-events.jsonl). These are fixture-generated
data, not production terminal output. Earlier harness-only tmux `send-keys`
validation used a separate, cleaned-up home and is not counted as GUI proof.

## Blocker and fallback decision

At `2026-10-27T20:07:43.324852Z`, the fixture logged native `shutdown` following
the window close. The sample at `20:07:53Z` shows:

```text
AppView::stop -> Surface::stop -> NativeState::clear
 -> Terminal::drop -> spike_terminal_view_shutdown
 -> ghostty_surface_free -> pthread_join -> __ulock_wait
```

The renderer thread had exited. The I/O thread was in a `killpg` / `wait4` /
sleep loop; the I/O reader was waiting in `__ulock_wait2`. At `20:08:30Z`, the
fixture PID, login wrapper PID `26815`, and tmux client PID `26816` were still
present. See [teardown-sample.txt](evidence/teardown-sample.txt),
[teardown-processes.txt](evidence/teardown-processes.txt), and
[closed.txt](evidence/closed.txt). This is a failed disposal operation, even
though retained-view detach/reattach succeeded.

Recovery used only the fixture socket/client:

```sh
"$SPIKE_TMUX" -S "$SPIKE_TMUX_SOCKET" detach-client -t /dev/ttys019
```

The process then logged `pty fd closed`, `surface closed`, and `SPIKE host
stopped` and exited. [recovered.txt](evidence/recovered.txt) shows zero clients
and the **same** session/pane/recorder identity. External recovery does not make
the native teardown safe, and is not included as a successful close test.

The wait shape corresponds to the open upstream
[libghostty teardown issue #14245](https://github.com/ghostty-org/ghostty/issues/14245).
That issue describes an app-mailbox/reader shutdown interaction and has a
[candidate change #14444](https://github.com/ghostty-org/ghostty/pull/14444).
**Same underlying cause is an inference:** the cached binary is stripped, so
the sampled reader wait was not independently symbolicated to its mailbox.
No upstream patch was applied or verified here.

For the GPUI-host fallback decision: do not switch the production NSH terminal
to this direct host yet. Keeping the established host is the lower-change
option while this lifecycle gate is unresolved. A GPUI host linking the same
archive may share the upstream teardown failure; this spike does not prove
that GPUI fixes it. Require the same active-tmux disposal test for any fallback.
Ghostling is not a small AppKit/Metal fallback. A separately approved follow-up
could validate a fixed pinned Ghostty runtime and safe shutdown protocol.

## Exact build / verification commands and results

Commands ran from the assigned worktree. No `cargo install`, app install, merge,
or push was used. Existing Cargo dependencies were fetched/built locally.

```sh
cargo build --manifest-path spikes/ghostty-nsh/Cargo.toml
# exit 0, initial NSH build 55.79s; final native-input rebuild 3.57s
# build.log: only 12 unused native callback-parameter warnings
cargo tree --manifest-path spikes/ghostty-nsh/Cargo.toml --locked
# exit 0; saved cargo-tree.txt; no "gpui" in output
cargo fmt --manifest-path spikes/ghostty-nsh/Cargo.toml --check
# exit 0
sh -n spikes/ghostty-nsh/scripts/prepare.sh spikes/ghostty-nsh/scripts/snapshot.sh spikes/ghostty-nsh/scripts/cleanup.sh
# exit 0; Python scripts also parsed successfully with ast.parse

spikes/ghostty-nsh/scripts/prepare.sh
# created the isolated home and initial identity: $0 1793131469 %0 14167
. /private/var/folders/bb/kghlxdk94hj1nkcmwsr90r2c0000gn/T/riwork-ghostty-spike.0znRWa/fixture.env
spikes/ghostty-nsh/scripts/snapshot.sh > "$RIWORK_HOME/before-gui.txt"
exec spikes/ghostty-nsh/target/debug/riwork-ghostty-nsh-spike > "$RIWORK_HOME/fixture.log" 2>&1
# run as a persistent command session; verified process 26521
```

First trial had `SPIKE_COMMAND='exec /opt/homebrew/bin/tmux …'`. The cached
runtime itself adds `exec -l`, so it failed with `exec: exec: not found`.
Preparation was corrected to plain `/opt/homebrew/bin/tmux -S … attach-session
-t ghostty-spike`; Menlo replaced an unavailable `monospace` font alias.
The same tmux session was reused. See [failed-command.log](evidence/failed-command.log).
The final `prepare.sh` contains the working command.

Driver calls used `session="ghostty-nsh-spike"`, `pid=26521`, `window_id=6051`.
`list_windows` and `get_window_state` grounded the target first. Input used
`run_steps` with an explicit PID/window in each step. The exact action ledger
is [driver-actions.json](evidence/driver-actions.json). Key calls included:

```text
type_text(text="NSH-keyboard-123")
press_key(key="Return"); press_key(key="ArrowLeft")
press_key(key="c", modifiers=["ctrl"])
click(x=116,y=94)  # Detach, 1100×783 screenshot
click(x=196,y=94)  # Reattach; type_text(text="REATTACHED")
click(x=285,y=94)  # synthetic IME callback probe
press_key(key="="); press_key(key="a")  # actual dead-key composition
drag(from_x=550,from_y=320,to_x=720,to_y=350,steps=5,duration_ms=250)
scroll(x=600,y=350,direction="up",amount=2)
set_window_frame(x=100,y=420,width=820,height=580)
click(x=457,y=112); click(x=559,y=112)  # synthetic 1× then 2×
click(element_token="s0000005a:1")     # native window close; hung
```

An earlier Option-E attempt yielded `€` on the current layout, so it was not
counted as dead-key proof. Driver dispatch alone was not treated as success:
screenshots, native callbacks, and bytes inside tmux supplied the postconditions.

```sh
sample 26521 1 -file "$RIWORK_HOME/teardown-sample.txt"
# exit 0; main thread blocked in ghostty_surface_free / pthread_join
ps -p 26521,26815,26816 -o pid,ppid,pgid,sess,etime,stat,command
# all three still present, saved teardown-processes.txt
python3 spikes/ghostty-nsh/scripts/verify_evidence.py spikes/ghostty-nsh/evidence
# exit 1 (EXPECTED): NO_GO_LIFECYCLE_BLOCKER
# all 13 interaction checks pass; required teardown gate FAIL
"$SPIKE_TMUX" -S "$SPIKE_TMUX_SOCKET" kill-session -t ghostty-spike
# exit 0; subsequent has-session exit 1, no server running
spikes/ghostty-nsh/scripts/cleanup.sh
# corrected, idempotent script: exit 0 with session absence verified
```

[verification-summary.json](evidence/verification-summary.json) is the
machine-readable result. The verifier is read-only and does not operate the GUI
or tmux. Final targeted driver inventory found zero windows for PID `26521`.
Initial `kill-server` returned zero but left the session alive; readback caught
this, and exact-session cleanup above removed it. The script now checks that
postcondition. [cleanup.json](evidence/cleanup.json) records all fixture/server/
recorder PIDs absent and `has-session` exit 1; the earlier contradictory readback
is preserved as [cleanup-initial.json](evidence/cleanup-initial.json). The separate
harness-only `K2Vig8` server/recorder (PIDs `8107`/`8110`) were likewise verified
absent after exact-session cleanup. Temporary evidence directories are retained.

## Production work remaining

1. Resolve and validate bounded main-thread-safe disposal while the PTY is
   active; repeat with multiple surfaces and output pressure. Rebuild/pin the
   Ghostty source and ABI instead of depending on a workstation cache.
2. Verify real 1×/2× display migration and display-link changes. The synthetic
   override exists solely for this experiment.
3. Expand input beyond basics: Japanese/Korean candidate UI and selected ranges,
   keyboard-layout changes, Option-as-Alt, per-side modifier handling, command
   key equivalents, focus/occlusion, mouse exit and scrolling momentum.
4. Integrate RiWork session ownership/error recovery, component unmount/remount,
   clipboard policy, selection/search/link/cursor actions and accessibility.
5. Replace the spike's coalescing 60 Hz timer and synchronous JSONL diagnostics
   with proper host wakeup scheduling; add ownership/thread assertions and
   lifecycle/performance tests. Package Ghostty resources, terminfo, and notices.

The spike deliberately remains throwaway, and its failing shutdown path is
preserved for reproduction rather than hidden by an external-detach workaround.
