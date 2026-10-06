# Automations implementation report

Completed the desktop engine task `c2250f68-0cd7-4d5d-8cd0-6bb86ba2e025` and native UI/interfaces task `91dc5719-45da-461a-9650-1d6c6f96bdac` for project `39832c2e-23a5-476d-aa8f-5ff34a02d314` in worktree `807d1ec7-2baa-46d0-b9af-21bfd0c16558` at `/Users/dominik/orca/projects/riWork-feat-automations`.

## Frozen candidate and commits

- Branch: `feat/automations`; base: `edcaf5799038de8769d985700858323a47fe6b1e`.
- Frozen implementation candidate: **`a38ea39ec5a0de607698e8cab88adad0c6b17680`** — `Implement desktop Automations with guarded fresh chats and explicit AI shells`.
- The subsequent commit adds only this report. The frozen handoff commit is the report commit at branch HEAD; its exact hash is returned to the parent in the final handoff. Implementation, tests, build receipts and GUI artifacts are unchanged from the implementation candidate above.
- No main merge, push, deployment, production host replacement, connector restart or OTA restart occurred. Independent acceptance review is pending.

## Result

The existing scheduler/ledger now supports fresh ordinary project chats and explicitly selected existing AI shells. New project chat is the native editor default with Codex, Supervised permission, omitted model/effort and fast=false. Each admitted occurrence creates a new ordinary chat at its pinned project root/account, verifies its returned identity and options, and attempts exactly one prompt submission. It never replaces or creates an orchestrator as a substitute.

The durable claim contains the preassigned created-chat UUID and is synced **before Create**. Compatible hosts serialize creation and refuse duplicate UUIDs, including IDs already present on disk after a restart. Failed startup, refused/broken creation, refused/broken Send, missing message proof and interrupted dispatch all keep the recorded ID; claimed uncertain outcomes pause for review without automatic replay. Existing missed-run, revision/conflict, shared locking, five-minute grace, deferral backoff and four-attempts/minute logic remain in the original scheduler.

The E1/E2 findings from `/Users/dominik/orca/projects/riWork-review-manu-20261005/AUTOMATIONS_REVIEW_REPORT.md` are addressed:

1. **Old-host preflight:** a dedicated non-mutating `capabilities` operation must affirm boolean `identified_create: true` on the same connection used for Create. Unknown-op/refused/broken/timeout, missing/false/malformed/unreadable/wrong-correlation responses are Deferred before a claim, Create or Command. They consume no dispatch rate slot. The check is bounded to two seconds in normal scheduling. The old running host PID78989 is not contacted by verification, restarted or replaced. When such an old host is used by the candidate, fresh-chat scheduling safely defers and eventually uses existing missed-run rules; legacy destinations retain their original paths. Returned-ID/root/project/account/provider/permission **and model/effort/fast** guards remain after affirmative support.
2. **Ordinary project shells:** explicit shell targets persist additive `shell_kind`. Project scope accepts the specifically chosen `ShellKind::Project` shell with the exact project, canonical project root and no worktree; workspace requires the exact project/worktree worker; orchestrator shell scopes still work. Missing shell_kind on old serialized targets retains the old project-orchestrator-only matching. Binding and dispatch use the same marked semantics and preserve creation/command/harness/account-home/pane/provider identity and all existing readiness/input guards.
3. **Options editing:** the native save path applies model/effort/fast/permission changes to the pinned fresh-chat target rather than discarding them or selecting another account. Options-only editing preserves the destination UUID, project root and saved account. Explicit provider/destination selection rebinds. Defaults and cleared options are tested.

## Native UI and interfaces

Automations is discoverable from the native application menu, pane menus and Cmd+Shift+S. Saved panel enum/layout identifiers (`schedules`), panel IDs and key context remain unchanged. The editor includes title/prompt, destinations, provider/options, explicit permission, one-time and existing recurring time controls, useful project/account context, empty state and scrolling. Existing edit/pause/resume/delete revision/conflict behavior is retained. Rows show last/next/outcome/configuration and Open created chat; ordinary project chats remain listed after later occurrences or schedule deletion. Opening a result selects an existing same-ID tab or creates and persists an app chat tab.

The original schedule CLI/MCP names and omitted-destination behavior remain. `automation` is a CLI alias. CLI fresh creation uses `--new-chat`; explicit shell creation uses `--existing-shell`. MCP `riwork_schedule_create` adds `destination=new_chat|existing_shell` and fresh options through the same service. Explicit shell marking survives revision-checked lifecycle operations; omission preserves legacy semantics. No second scheduler or ledger was added. No iOS files changed.

## Files

- Engine/serialization: `src/schedules.rs`, `src/schedule_chat.rs`, `src/chat/wire.rs`, `src/chat/client.rs`, `src/chat/host.rs`, `src/chat/mod.rs`.
- Shared interfaces: `src/schedule_service.rs`, `src/cli.rs`, `src/mcp.rs`.
- Native UI/layout: `src/schedule_panel.rs`, `src/main.rs`, `src/layouts.rs`.
- Tests/fixtures: `src/schedule_chat/fresh_tests.rs`, `src/schedules_tests.rs`, `src/chat/testing.rs`, `src/chat_view/testing.rs`, `tests/schedule_interfaces.rs`, `scripts/automations-ui-fixture.py`.
- Documentation/artifacts: `docs/automations.md`, `docs/verification/automations/README.md`, all screenshot/JSON/log files under `docs/verification/automations/`, and this report.

The implementation commit contains 37 files. `git show --stat a38ea39ec5a0de607698e8cab88adad0c6b17680` gives the exact manifest.

## Build and focused verification

All final checks below passed. Used **`CARGO_TARGET_DIR=/tmp/riwork-automations-target`** and installed Zig via PATH prefix **`/Users/dominik/.local/share/riwork/toolchains/zig-0.16.0`** (binary `/Users/dominik/.local/share/riwork/toolchains/zig-0.16.0/zig`). Inherited RIWORK_HOME was preserved; `scripts/check-schedules.sh` overrides home/runtime only in child test processes. No authenticated model/provider test ran.

| Command after the environment prefix | Result | Original log | Committed receipt |
| --- | --- | --- | --- |
| `scripts/check-schedules.sh --bin riwork schedule` | 74 passed, 0 failed, 2 existing opt-in tests ignored | `/tmp/riwork-automations-engine.log` | `docs/verification/automations/test-logs/engine.log` |
| `scripts/check-schedules.sh --test schedule_interfaces` | 6 passed | `/tmp/riwork-automations-interfaces.log` | `docs/verification/automations/test-logs/interfaces.log` |
| `scripts/check-schedules.sh --test cli_mcp_contract` | 12 passed | `/tmp/riwork-automations-cli-mcp.log` | `docs/verification/automations/test-logs/cli-mcp.log` |
| `scripts/check-schedules.sh --bin riwork chat::host::tests` | 39 passed | `/tmp/riwork-automations-chat-host.log` | `docs/verification/automations/test-logs/chat-host.log` |
| `scripts/check-schedules.sh --bin riwork workspace_tab_tests::icon_only_panel_tabs_name_their_panel_in_the_tooltip` | 1 passed | `/tmp/riwork-automations-native-label.log` | `docs/verification/automations/test-logs/native-label.log` |
| `cargo build --offline` | Native desktop debug build passed | `/tmp/riwork-automations-build.log` | `docs/verification/automations/test-logs/build.log` |
| `cargo fmt --all -- --check`; `git diff --check` | Passed | Final tool receipts | No formatting/diff errors |

Fake-driver lifecycle coverage includes Codex/Claude ordinary fresh creation, one prompt per occurrence, concurrent ticks, persistent IDs across host/scheduler restart, failed startup, create/send refusal or broken exchange, missing log proof, positive capability with mismatched ID/root/project/account/permission/model/effort/fast, interrupted claims and old serialized schedules. The creation stand-in asserts the on-disk Dispatching claim and created_chat_id before handling Create. Direct UUID creation races permit only one winner and refuse reuse after host restart.

Capability stand-ins reject or omit support, return false/malformed/unreadable/wrong IDs, disconnect or time out. They assert zero Create/Command calls, zero chats, no consumed fresh rate slot/claim, repeated/reloaded Deferred checks, eventual Missed and non-starvation of a due legacy schedule. Ordinary root Codex/Claude shell fixtures use inert deterministic harnesses and real guarded input code, verify one dispatch, legacy-marker absence semantics and changed scope/kind/creation/account/root rejection. Existing shell readiness/races and old schedules remain covered. CLI and real MCP stdio tests cover fresh/shell requests, defaults, invalid destination/project/account/permission, stale revisions and retained markers through update/pause/resume/delete.

## Cua verification and honest limits

Only RiWork's **Cua.ai Driver MCP** was used. Its descriptions/current state were read; driver 0.30.4 reported Accessibility and Screen Recording granted. No fallback provider or production UI session was used. Native fixtures used copied, uniquely identified bundles and child-only private RIWORK_HOME/runtime; no real provider prompt was submitted.

Proven GUI observations/actions:

- Cmd+Shift+S opened the Automations tab; screenshots show the empty-state guidance and native editor destination/provider/Supervised defaults.
- Earlier fixture screenshot verifies a 640×900 logical editor with wrapping/scrolling. It predates the final title/prompt placement and is retained as source evidence, not a final narrow acceptance claim.
- Final candidate rendered the paused inert result, next/last/outcome, configuration, Open created chat and retained Project chats; its edit form rendered populated title/prompt and future time.
- A final native editor Cmd+S changed the fixture ledger **revision 2→3**, retaining `paused=true`, title and prompt. The fixture had been paused through its CLI setup. This proves editor save/paused retention; it does **not** prove a GUI resume/re-pause cycle. Pause/resume/edit/delete/conflicts are proven separately by the passing service/CLI/MCP tests.
- Opening the seeded result has semantic proof in `fixture-receipt.json`: the app saved and selected chat **`ebd0815a-696d-4bc5-a907-9bac5e4da60a`** in its project layout. The inert chat remains Stopped with no user-message/provider prompt. The final screenshot retained a previously painted panel frame and Project chats focus, so it is not presented as a ChatView screenshot.

Unverified GUI limits: final title/prompt replacement, reliable mouse hit delivery, complete narrow Tab/Shift-Tab/scroll behavior, GUI pause/resume/delete/conflict paths and final chat-content paint. Tool input acknowledgements alone were not treated as success. Off-Space AX binding/frame operations and session escalation intermittently refused or lagged despite granted permissions; further GUI exploration stopped as requested. Rechecking RiWork's Cua setup/driver window binding is the appropriate path for those remaining interactive acceptance checks.

Artifact interpretation is recorded precisely in `docs/verification/automations/README.md`. Source screenshots and semantic receipts are committed. The fixture script supports `--seed-result` to prepare a paused inert result without starting a provider.

## Cleanup and operational limits

Cua ownership ended. Fixture GUI PIDs **37032, 55868, 93223** are closed. The final fixture host **PID1877**, whose exact command was the uniquely copied `5e472dab` fixture binary's `chat serve`, was terminated after preserving the receipt. Only uniquely owned fixture bundles `f351330d`, `8723414c`, `5e472dab` were removed. The paused final fixture home `/private/tmp/rwa-ui-dudv4q4g/home` and metadata/receipts are retained. `cleanup-receipt.json` confirms production native host **PID78989** still running its original command. DomiMax HTTPS OTA and connectors were untouched.

Dispatch still requires the desktop app to be open. Unsupported provider model/effort/fast settings may fail initialization and remain inspectable through the recorded chat. Fresh-chat automations safely defer against an old running host; adoption of the candidate host belongs to a later authorized deployment, not this task. The two original ignored tests require separately provisioned disposable live providers or are concurrency child helpers; no real-model acceptance was attempted.

Both implementation tasks are delivered as committed desktop code and focused passing checks. Independent frozen-candidate review and the listed final interactive GUI checks remain explicit acceptance limits. No further edits or GUI ownership are retained after handoff.
