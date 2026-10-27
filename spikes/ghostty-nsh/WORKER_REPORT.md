# Phase 2 ENTRY GATE — NO-GO

Task: `b915a54a-3ec6-4491-89b1-428ab28f344a`

Branch: `nsh-p2-teardown`, based on `ea0f2f5`

Date: 2026-10-27

Scope: extend this throwaway spike; no production integration.

**NO-GO for the Phase 2 entry gate.** The detach-before-free protocol completed
20 four-surface batches under active tmux output without hangs or leaked
clients/processes. Every surface disposed in less than 339 ms, meeting the
requested <2 s target. However, observed main-loop gaps reached **156.212 ms**,
so the stricter **100 ms** responsiveness requirement in
[`docs/nsh-contracts.md` §3.7](../../docs/nsh-contracts.md#37-teardown-protocol-frozen-from-the-ghostty-spike-no-go)
is not proved. Runtime experiments stopped at this failure. Thresholds were
not relaxed, and parked/leaked surfaces were not counted as success.

## Implemented protocol

1. Real NSH `when(mounted, …)` removal invokes `NativeComponent::drop`.
   Shutdown marks the view closing, removes it from input/rendering, and obtains
   **that surface's PTY tty** from `ghostty_surface_tty_name`. PTY creation is
   asynchronous, so tty capture happens at release, not `surface_new`.
2. A worker runs `detach_client.py` on the explicit fixture socket. It finds
   exactly one matching tty, records its client PID/login-wrapper PID, validates
   ancestry to the fixture, and executes `tmux detach-client -t <that tty>`.
   It confirms the client PID/tty is absent and the session/pane identity is
   unchanged. There is no detach-all or ambiguous-client fallback.
3. Pending disposal owns the removed NSView while its normal AppKit timer keeps
   pumping `ghostty_app_tick`. Only after the helper reports confirmed detach
   **and** `ghostty_surface_process_exited` returns true does the main thread
   call `ghostty_surface_free`, then app/config free. All 80 surfaces have those
   two prerequisite records before `free-begin` and a matching `free-end`.
4. The worker captures immutable tty/ID values and a raw atomic-result pointer,
   avoiding a final NSView release on the worker. Explicit pending ownership
   ends after main-thread disposal. Window close uses the same protocol, then
   stops/wakes the AppKit event loop after the last surface is freed.

The observed disposal bound is **<2,000 ms per surface and four-surface batch**.
The worker has a bounded detach attempt, and the fixture exits with failure if
detach/child exit is not confirmed by 2 s. It never enters free with a possibly
live child. This is a fail-fast experiment, not the production parking/retry
policy from §3.7. Synchronous native free is not preemptible: these measurements
are an observed bound for the captured runtime, not a universal guarantee.

## Final captured result

Authoritative result:
[cached/verification-summary.json](evidence/phase2/cached/verification-summary.json).
Its only failed checks are responsive-main-loop checks for surfaces **1–4 and
52**: seven recorded timer gaps were ≥100 ms. These are 60 Hz main-run-loop
delivery intervals while closing. They do **not** identify a blocking function
or establish that `ghostty_surface_free` caused the delay; measured native free
was much shorter. No causal profiling or further runtime trial followed NO-GO.

| Check | Result |
| --- | --- |
| Four simultaneously attached surfaces, 20 batches | PASS: 80 created, 80 freed/disposed exactly once |
| Actual NSH drop/unmount/remount | PASS: batches 1–19; four surfaces per component |
| Active native-window close / host quit | PASS: batch 20; process exit 0, zero fixture windows |
| Detach readback and Ghostty child-exit barrier before free | PASS: 80/80 |
| Per-surface full disposal, <2,000 ms | PASS: max **338.653 ms** |
| First request event → last disposed in a batch, <2,000 ms | PASS: max **393.995 ms**; including pre-request prelude, reconstructed <400 ms |
| Synchronous shutdown dispatch, <100 ms | PASS: max **30.670 ms** |
| `ghostty_surface_free`, <100 ms | PASS: max **21.135 ms** |
| Free plus app/config cleanup, <100 ms | PASS: max **25.843 ms** |
| Final timer callback through disposal, <100 ms | PASS: max **55.661 ms** |
| Main-loop delivery gaps, <100 ms | **FAIL: max 156.212 ms**; surface 52 also reached 110.247 ms in batch 13 |
| Hangs / leaked attach clients / leaked child processes | PASS: **0 / 0 / 0**, read back after each close |
| Same session and pane after every close | PASS: `$0 1793134380 %0 42320` throughout |

[cycles.jsonl](evidence/phase2/cached/cycles.jsonl) records four attached clients
before every close, then zero clients, zero fixture descendants and no surviving
old client/wrapper PIDs. The shared server/pane deliberately survive those
closes, including host quit. Each exact-client handshake has its own
`detach-<surface_id>.json`. Native ordering/timing is in
[teardown-events.jsonl](evidence/phase2/cached/teardown-events.jsonl).
Screenshots show the [initial attached fixture](evidence/phase2/cached/attached.png)
and [four attached surfaces before final close](evidence/phase2/cached/final-attached.png).

`pressure.py` writes continuously inside the same pane, nominally 4 MiB/s.
The capture's counters advanced **317,036,544 bytes over 127.031 s**, a measured
mean of **2.380 MiB/s**, including detached periods; pressure progresses across
every attached/closed pair. This is real output, not `tmux send-keys` or a
synthetic surface. See [pressure-summary.json](evidence/phase2/pressure-summary.json).

Native timings use `clock_gettime(CLOCK_MONOTONIC)`; Python capture timings use
`time.monotonic` (`mach_absolute_time` here). The verifier orders each clock
domain separately and correlates explicit surface IDs plus captured
`ready`/`all_disposed` postconditions. It does not apply guessed clock offsets.
Reported disposal includes tty acquisition before the request log, so it is a
conservative duration relative to the request-event interval. Analyzer fixes
for these distinctions preserved the raw observations and the 100 ms failure.

## Exactly which runtime was proved

**Only the cached `1.3.2-dev` archive was executed in the final pressure proof.**
It proves the bounded local detach/free observations above, **not** the complete
entry gate. The rebuilt pin is build-verified only and must not inherit these
runtime results.

| Artifact | SHA-256 / revision | Status |
| --- | --- | --- |
| Cached `libghostty-internal.a` | `75443f9cd0b74a0b41f51b214bbc1cc00c10b94d10710302f5eca9971f157b66` | Final 20×4 runtime capture |
| Final cached fixture | `1c14ab2d8f7e2208d0da3ee3ec9c88f2684ebaf961474d5ff9e3c3b4e53f2e62` | PID 43353, window 6209 |
| Fresh rebuilt `libghostty-internal.a` | `b51367aa565527eddfa180d5ae6304899d903af6fc094b4531671ffc20fb064f` | Built successfully; **not launched** |
| Final rebuilt fixture | `1e1bc2da0fe80967fee7619a6c8c890fa6bb5e5a5e8546689e6682f25c90b291` | Compiled/linked successfully; **not launched** |
| Matching full header | `f2c12e1bb02540d79feed6bd29ee37312c4e0d24391442a63e66f06e1f7f2e4a` | Cache, vendor and rebuilt headers match |
| Rebuilt source | Ghostty `9f0e1719dc918368367d368bfe300f59bb68b5a4` + declared gpui vendor patches | Zig 0.16.0, ReleaseFast, app-runtime=none |

The cache path is
`~/Library/Caches/gpui-libghostty/native/b1b8e8c04734504b2bf3bf7553ee247f/lib`.
Its source/toolchain fingerprint independently recomputes to that key; that is
not an independent attestation of the cached binary's original build. The
rebuild used a private copy of the exact vendor tree and private caches under
`.runtime/pinned`, with the already-installed Zig 0.16 toolchain. No installation
occurred. The fresh archive differs from the cache, despite matching headers.
See [runtime.json](evidence/phase2/cached/runtime.json),
[build provenance](evidence/phase2/runtime/pinned-rebuild-provenance.json),
[cache audit](evidence/phase2/runtime/native-cache-audit.json), and
[evaluation.md](evaluation.md#phase-2-runtime-evaluation-2026-10-27).
Both fixture builds have captured build logs and linker search paths under
`evidence/phase2/runtime/`.

At the 2026-10-27 check, upstream
[#14245](https://github.com/ghostty-org/ghostty/issues/14245) remains open and
[#14444](https://github.com/ghostty-org/ghostty/pull/14444) is open/unmerged with
changes requested, head `8eb96765685e581963889a85b4340fd6e289bcd6`. The revised
proposal drains the app mailbox while joining threads. It was **not applied,
built or tested** here. Neither the old 47 s hang nor these timer gaps are
independently attributed to that upstream root cause.

## Verification environment, excluded work and cleanup

- Final `RIWORK_HOME`:
  `/private/var/folders/bb/kghlxdk94hj1nkcmwsr90r2c0000gn/T/riwork-ghostty-spike.fftNYv`.
  Dedicated `tmux.sock`, `-f /dev/null`, session `ghostty-spike`; no default socket.
- macOS 26.2 (25C56), arm64; Rust 1.99.1; Xcode 26.0.1 (17A400), tmux 3.6a.
- **Fixture app only. RiWork Cua Driver MCP only**, explicit fixture PID and
  window on every input, background delivery. The NSH controls lack AX actions,
  so grounded fixture pixels were used; native window close used its fresh AX
  token. [driver-actions.json](evidence/phase2/cached/driver-actions.json) records
  the 39 input calls and initial read. No live RiWork/orchestrator window was
  targeted. A stale fixture-window read was refused without input.
- Startup preflight found early tty capture was empty and was corrected before
  the counted run. Its dedicated fixture was terminated and isolated session
  cleaned; it is not success evidence. A four-surface exploratory close was
  likewise excluded. Their cleanup readbacks are retained.
- An earlier 20×4 trial freed all surfaces but failed bounded host exit: timer-
  driven `NSApp stop:` lacked a wake event. Its sample shows AppKit idle in
  `nextEventMatchingMask`, not a native-free join. The fixture now posts the same
  application-defined wake event used by NSH `AppView::stop`. That failed trial
  is preserved unchanged under [host-wakeup-trial](evidence/phase2/host-wakeup-trial/note.json)
  and excluded from final success counts. The final source was rebuilt and all
  20 batches repeated after the fix.
- Final native window close exited 0; after every close the session/pane still
  matched. Only after collecting that evidence was the exact fixture session
  killed. [cleanup.json](evidence/phase2/cached/cleanup.json) records fixture,
  server, pane and **all 160 client/wrapper PIDs absent**, session absent, and
  zero remaining fixture windows. Excluded fixture homes are also stopped.

## Commands and outcome

```sh
cargo build --manifest-path spikes/ghostty-nsh/Cargo.toml
# Cached final fixture: exit 0, native bridge rebuilt; 12 existing unused-parameter warnings.
GHOSTTY_LIB_DIR="$PWD/spikes/ghostty-nsh/.runtime/pinned/install/lib" \
GHOSTTY_INCLUDE_DIR="$PWD/spikes/ghostty-nsh/.runtime/pinned/install/include" \
cargo build --manifest-path spikes/ghostty-nsh/Cargo.toml
# Rebuilt-archive final fixture: exit 0, 1.61 s; never launched.
SPIKE_PRESSURE=1 spikes/ghostty-nsh/scripts/prepare.sh
. /private/var/folders/bb/kghlxdk94hj1nkcmwsr90r2c0000gn/T/riwork-ghostty-spike.fftNYv/fixture.env
exec spikes/ghostty-nsh/.runtime/cached/fixture > "$RIWORK_HOME/fixture.log" 2>&1
# PID 43353; exact driver actions and per-cycle readbacks as recorded.
python3 spikes/ghostty-nsh/scripts/record_cycle.py "$RIWORK_HOME" 43353 1 attached
# Driver Close surfaces; record closed; Driver Create surfaces; repeat.
# Batch 20: active native-window close; record closed --expect-exit.
python3 spikes/ghostty-nsh/scripts/verify_teardown.py spikes/ghostty-nsh/evidence/phase2/cached
# EXPECTED exit 1, NO_GO: only five responsive_main_loop checks fail.
cargo fmt --manifest-path spikes/ghostty-nsh/Cargo.toml --check
sh -n spikes/ghostty-nsh/scripts/prepare.sh spikes/ghostty-nsh/scripts/snapshot.sh spikes/ghostty-nsh/scripts/cleanup.sh
# exit 0; all spike Python scripts also parse via ast.parse; git diff --check clean.
```

The frozen gate is **not cleared**. Pinned-runtime pressure verification,
strict <100 ms responsiveness, remote bridge teardown and production timeout/
retry integration remain unproved. No workaround experiments followed this
NO-GO. No merge, push, install, or task-status update was performed; the
orchestrator owns task status.

---

# Historical Phase 1 libghostty / NSH AppKit spike — NO-GO

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
