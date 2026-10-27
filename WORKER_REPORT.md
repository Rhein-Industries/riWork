# Task 24ab1d55-508b-473b-af92-f0aa26503792 — Phase 2a local terminals: PASS

Integrated native `riwork.terminal` on every NSH AppView, using the exact pinned
Ghostty 9f0e171 GO C archive with no GPUI/Ghostty Rust crates. Local tabs now
attach, accept input, resize, detach/reopen the same tmux session, update themes
and resolve links. Teardown confirms exact-tty detach and PTY-child exit before
main-thread free; normal close/quit drains disposals and preserves sessions.

`verification_environment`: isolated fixtures only. Final fixture
`/private/tmp/rwnsh-p2-upf_p6zi`, PID `69604`, two windows observed read-only with
RiWork Cua.ai Driver and explicit PID. **20 cycles, 84 surfaces freed, zero
leaked clients/attach descendants, all four sessions survived quit.** Maximum
native free 5.840 ms, total disposal 40.868 ms, teardown heartbeat gap 16.644 ms;
zero global gaps at or above 100 ms. Broad Cargo check: 1,728 passed; final
terminal/desktop check: 42 passed; final NSH suite: 16 passed. Counts overlap.

[Full report, exact commands, environment, hashes, screenshots and limits](crates/riwork-nsh-terminal/WORKER_REPORT.md).
Paste/drop, subshells, remote and Vim tabs remain next slice. No live state or
windows used, no merge/push/install, and no task-status change. The orchestrator
owns status. Earlier reports below are preserved as historical evidence.

---

# Historical task b915a54a-3ec6-4491-89b1-428ab28f344a — Phase 2 ENTRY GATE: NO-GO

Implemented off-main-thread exact-client detach, tmux readback, and Ghostty
PTY-child-exit confirmation before main-thread `ghostty_surface_free`.
The cached `1.3.2-dev` runtime completed **20 batches × 4 attached surfaces**
under continuous output: **zero hangs, zero leaked clients/processes, the same
tmux session/pane after every close**, and per-surface disposal below 339 ms.

**The gate remains NO-GO:** measured main-loop gaps reached **156.212 ms**,
exceeding frozen contract §3.7's 100 ms responsiveness limit. Native free itself
peaked at 21.135 ms. This establishes a responsiveness-proof failure, without
attributing the gaps to a particular blocking function. Runtime experiments
stopped at this failure; no threshold relaxation or surface-leak workaround.

The pinned Ghostty source and final fixture were rebuilt successfully, but that
runtime was **not launched or proved** after the NO-GO. Upstream #14245 and
#14444 remain open; the candidate fix was not applied. Exact runtime hashes,
commands, failed checks, excluded trial, cleanup and limits are in the
[full spike report](spikes/ghostty-nsh/WORKER_REPORT.md).

Verification used isolated `RIWORK_HOME`, fixture PID `43353` only, and RiWork's
Cua Driver MCP with explicit PID/window on every input. No live RiWork or
orchestrator windows were used. Fixture/server/pane and all 160 tracked attach
client/wrapper PIDs are absent after cleanup. No merge, push, install, or task
status change; status remains for the orchestrator.

---

# Historical task db4930af-e7a8-455c-8fe7-2de612ae102c — NO-GO

Direct Rust/AppKit libghostty hosting as NSH 0.3 `riwork.terminal` worked without
GPUI for rendering, keyboard/dead-key basics, mouse, resize, live themes and
retained-view detach/reattach. **Native teardown hung in `ghostty_surface_free`
until the isolated tmux client was externally detached.** Physical display-scale
migration remains unverified; synthetic 1×/2× probes passed.

`verification_environment`: isolated `RIWORK_HOME` under
`/private/var/folders/bb/kghlxdk94hj1nkcmwsr90r2c0000gn/T/riwork-ghostty-spike.0znRWa`,
fixture app only, RiWork Cua.ai Driver with explicit fixture PID. No live
RiWork/orchestrator windows tested. Fixture app/server cleaned up.

See [full report, exact commands, evidence and production remainder](spikes/ghostty-nsh/WORKER_REPORT.md).
No merge, push, install, or task-status change. Task not marked done.
