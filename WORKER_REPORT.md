# Task db4930af-e7a8-455c-8fe7-2de612ae102c — NO-GO

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
