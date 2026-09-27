# Scheduling acceptance record

Verified on macOS, 2026-09-27, in `feature/desktop-schedules`. No production schedules or sessions were used. The inherited parent `RIWORK_HOME` remained `/Users/dominik/.local/share/riwork`; commands ran in fresh child homes. Native artifacts used this worktree's APFS-cloned Cargo target, never the root worktree's target. Zig: `0.16.0` from the requested executable.

## Checks

- `cargo build --offline`, with the requested Zig directory at the front of the child command's PATH: native build passed.
- `scripts/check-schedules.sh schedules`: 15 passed, two opt-in/helper tests excluded. The concurrent-process parent explicitly launches its excluded child helper with a fresh fixture home.
- `scripts/check-schedules.sh`: 223 passed, two excluded. Tests run serially in a fresh child `RIWORK_HOME` and `RIWORK_RUNTIME_DIR`.
- `cargo fmt --check` and `git diff --check`: passed.
- Live Codex acceptance: the exact opt-in test below passed, 1/1, using three new disposable sessions and the existing account configuration. Each received one scheduled prompt, returned its exact marker, completed a fresh turn, and retained its pane/process identity. A later tick did not send again.

The live fixture project was `eb22a2ef-6e5c-44d4-81ea-238a0763fbc3`; workspace/worktree `64948b03-8ff7-4fef-98f9-1fd7e4433c35` represented this checkout in the disposable registry.

| Scope | Full disposable shell UUID | Provider reply |
| --- | --- | --- |
| App / global orchestrator | `ef7e58e2-c7d1-49e3-b1f5-eda2584fc6c0` | `SCHEDULE_DISPATCH_SCOPE_0` |
| Project / project orchestrator | `96bc8fed-873b-46d2-aa04-584b3479ef07` | `SCHEDULE_DISPATCH_SCOPE_1` |
| Workspace / explicitly selected worker | `13d218f0-af24-40ed-8dc5-f23e88c58bae` | `SCHEDULE_DISPATCH_SCOPE_2` |

The executed child command was:

```sh
RIWORK_HOME="$fixture_home" \
RIWORK_SCHEDULE_LIVE_HOME="$fixture_home" \
RIWORK_SCHEDULE_LIVE_IDS="ef7e58e2-c7d1-49e3-b1f5-eda2584fc6c0,96bc8fed-873b-46d2-aa04-584b3479ef07,13d218f0-af24-40ed-8dc5-f23e88c58bae" \
cargo test --offline -- --exact schedules::tests::isolated_live_provider_acceptance --ignored --test-threads=1 --nocapture
```

The fixture used injected due times `1000`, `1001`, and `1002` (hence the 1970 timestamps in screenshots) while delivery went to real Codex 0.157.1 processes. No provider tools ran. Temporary registry/home/app bundle and all three shells were removed afterward; only the new provider transcripts and sanitized screenshots remain.

## Native desktop verification

The iOS worker `bd609639-9b49-4970-a54b-fe0e5fc66b9c` was read first and left alone while active. Its final completion and idle prompt were confirmed before desktop interaction. Only RiWork Cua Driver MCP was used, under `riwork-desktop-schedules`; only that named session was explicitly ended. The driver's launch tool uses its transport session, which had to be revived after earlier teardown. No other named controller was ended.

A separate `dev.riwork.schedules.qa` bundle launched with a disposable `RIWORK_HOME` and runtime registry. Verified the scheduling shortcut, Tab/Return target selection, native title/prompt text input, Cmd+S creation, next run, pause, edit to custom five-minute recurrence, retained paused state and identity, and two-click deletion. Model readback confirmed each mutation, then the GUI window's closure was verified independently.

Window geometry was set and independently read back at 1220 × 780 and 740 × 760 points. Screenshots show the panel's compact styling, wrapping labels, and usable narrow editor. GPUI's custom body lacks native AX controls; screenshot-grounded foreground input was needed after background/off-Space delivery refusals. Text focus was verified in a separate click before typing. No alternate desktop provider was used.

![Compact editor with custom repeat interval](verification/schedules-editor.png)

![Narrow app-scope editor](verification/schedules-narrow.png)

![Submitted outcomes for all three scopes and a paused future UI fixture](verification/schedules-outcomes.png)

## Review findings and limits

A preserved custom Codex notify hook initially left the new pane without an activity binding. The verified direct-PID rollout fallback now establishes exact identity without replacing that hook or guessing from a CWD. The dim default Codex placeholder also required explicit recognition; typed lookalikes and other unknown composer shapes fail closed.

A deterministic fixture initially advertised completion before its Python terminal had finished starting. The safe blank-screen refusal exposed that test race. Fixtures now wait for their own prompt with a bounded poll before dispatch assertions. The final full run passed.

Final review added regression checks for custom zero-minute input and for a completed replacement provider thread. Custom intervals cannot silently become one-time schedules; readiness tokens must belong to the pinned provider, and the provider identity is checked again inside the serialized input gate before claiming delivery.

Live provider acceptance covered Codex. Claude was verified with real isolated tmux input and structured UserPromptSubmit/Stop fixtures, including no delivery before completion; an authenticated live Claude account was not invoked. Unknown lifecycle/prompt variants and indirect/shared-server configurations without exact binding remain deferred or unavailable. Recurrence is elapsed UTC time; there is no calendar cron, closed-app launch daemon, or automatic retry after uncertain input. Submitted denotes terminal submission, not completed agent work. External manual input can race after a readiness sample.
