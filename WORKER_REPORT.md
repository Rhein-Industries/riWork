# Task b915a54a-3ec6-4491-89b1-428ab28f344a — Phase 2 ENTRY GATE: NO-GO

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
