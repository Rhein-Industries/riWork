# Shared project tabs implementation report

Implemented on `shared-tabs`, based on `9631456`, without pushing, rebasing, or creating another branch/worktree. The old implementation was used as a reference and adapted to the current Kit controls and chat feed lifecycle.

## Design and files

The desktop owns locked, atomically replaced project membership with durable revisions. Shared order, pins, titles and visibility are independent of each device's selection and pane layout. Reconciliation preserves explicit hides and worker opens; hidden history retirement prevents resurrection. Project orchestrators start pinned. Workers retain their detach-only policy after their parent disappears.

- `src/project_tabs.rs`, `src/chat_tabs.rs`: store, inventory, title/parent reconciliation, creation coordination and worker provenance.
- `src/chat/{model,host,client,launch,log}.rs`, `src/sessions.rs`, `src/cli.rs`, `src/layouts.rs`: session registration, inherited parents, shared CLI operations and migration.
- `src/main.rs`, `src/chat_view.rs`, `src/tab_close_dialog.rs`, `src/settings.rs`: shared strip, two-second refresh, worker opening from Shells, close policy, Base/Kit dialog and local setting. The strip reserves space for macOS window controls. Existing pane bars still represent local placement.
- `remote/src/rpc/tabs.rs`, `remote/src/{rpc,connector,lanes}.rs`, `remote/tests/tabs.rs`: capability, validated list/update/open forwarding and transport tests.
- `ios/Core/SharedTabs.swift`, `ios/Core/{Chat,Entities,LinkTiming,RelayClient}.swift`, `ios/Tests/SharedTabsTests.swift`: wire models, transport, validation and capability defaults.
- `ios/RiWorkRemote/RemoteModel*.swift`, `RemoteViews.swift`, `DisplaySettings.swift`, Xcode project: shared APIs, four-second refresh including focus mode, minimal worker picker, close sheet and device-local preference. Global orchestrators remain available outside project membership.
- Supporting chat/session fixtures and callers gained the new metadata fields. The final desktop commit also contains formatting of touched Rust callers/tests.
- `scripts/shared-tabs-ui-fixture.py`: disposable bundle/state preparation with a unique bundle identifier. It uses a real executable, no LaunchServices registration or launcher bundle; launch with the explicit clean fixture environment.

Worker close hides without stopping or prompting. User-session close defaults to Ask, with Detach/Exit and Cancel; Exit hides and stops the provider or destructively closes the shell, retaining chat history. The existing destructive `shell.close` API and remote Mac tabs retain their semantics.

## Validation

| Check | Result | Evidence |
| --- | --- | --- |
| Latest exact `ZIG=/opt/homebrew/bin/zig cargo test -- --test-threads=1` | 1,808 passed, 1 known upstream failure, 8 ignored | `/tmp/shared-tabs-rust-last.log` |
| Earlier complete Rust tests excluding only the upstream failure | 1,870 passed, 0 failed, 8 ignored (1,808 unit + 62 integration) | `/tmp/shared-tabs-rust-complete.log` |
| Latest `swift test` in `ios/` | 1,009 passed, 0 failed | `/tmp/shared-tabs-swift-last.log` |
| Full standalone remote suite | 405 passed, 0 failed, 12 ignored | `/tmp/shared-tabs-remote2.log` |
| Added remote open-forwarding test | 1 passed (406 passing tests covered across the full run and this follow-up) | `rpc::tabs::tests::tabs_open_unhides_an_existing_session_without_process_commands` |
| Focused chat tests after feed adoption fix | 130 passed | `/tmp/shared-tabs-chat-rerun2.log` |
| Shared store tests | 23 passed | `/tmp/shared-tabs-store-tests.log` |
| Workspace open-child regression | 1 passed | `/tmp/shared-tabs-main-rerun.log` |
| iOS Simulator build | BUILD SUCCEEDED | `/tmp/shared-tabs-ios-build-final.log` |
| Desktop build after spacing fix | Passed | `/tmp/shared-tabs-build-spacing.log` |
| Whitespace check | `git diff --check` passed | Local check |

The sole latest Rust failure is `text_input::tests::marked_tab_and_shift_tab_keep_child_focus_until_owner_navigation_is_safe`. The user confirmed it fails on plain origin/main after upstream Tab behavior changed. It was also reproduced against the unchanged base sources, which were restored afterward; the test was not fixed. The exact full command stops after that unit failure, so integration coverage is reported separately from the earlier complete run.

Initial port verification exposed deferred activation replacing recording test feeds and an old assumption that child sessions automatically open. The feed guard and explicit child opening fixed those failures. No schedules/update/sessions flaky failures remained in the final exact run. Remote socket checks required an unrestricted test run; that run passed.

## Desktop evidence and limitations

Cua Driver inspected only the disposable app's confirmed pid/window. `open -n` initially returned `kLSNoExecutableErr`; direct execution of the isolated bundle worked. A temporary alias was removed by the user. No launcher bundle or further `lsregister` operation was used in the final check. The fixture app was quit, its shell was closed through its exact fixture session id, the user stopped the remaining fixture chat host, and the fixture bundle/home/runtime were removed. Screenshots remain outside the deleted state and were downscaled with `sips -Z 1600`.

- Dark strip with user chat and pinned project orchestrator: `/tmp/shared-tabs-dark-strip.png`.
- Dark worker opened from Shells: `/tmp/shared-tabs-dark-worker.png`.
- Dark close setting: `/tmp/shared-tabs-dark-settings.png`.
- Light strip: `/tmp/shared-tabs-light-strip.png`.
- Light worker opened from Shells: `/tmp/shared-tabs-light-worker.png`.
- Light close setting: `/tmp/shared-tabs-light-settings.png`.
- Close-prompt capture attempts: `/tmp/shared-tabs-dark-close.png`, `/tmp/shared-tabs-light-close.png`.

The close-prompt images show the modal backdrop but do **not** visibly show its panel. Cua's fresh accessibility state exposed Cancel, Detach and Exit, and Escape removed the prompt without closing the tab. This establishes the interaction state, but does not establish successful visual rendering of the panel; the capture/rendering issue remains an open item. Visual work was stopped under the user's time box. Worker close was observed to remove its tab without a prompt while the disposable shell remained live. The close setting is functional and accessibility exposes its selection, but the current simple radio presentation could use clearer selected-state styling.

Live Mac-to-phone end-to-end sync with a paired physical phone was not exercised. Store, transport, decoding and reconciliation tests cover the contract, and the iOS app compiles. iOS chrome design remains with the other worker; only a minimal functional picker and confirmation sheet were added.

## iOS handoff contract

The complete contract is [shared-tabs.md](shared-tabs.md). It specifies `tabs.list`, `tabs.update`, `tabs.open`, authoritative nested replies, status/revision handling, **Move with a nullable `before` key**, explicit open/unhide, `RemoteModel` APIs, capability gating, and refresh/generation rules. It defines the device-local `tab_close_behavior` key (`ask`/`detach`/`exit`, default Ask), worker-forced Detach and user-session Exit semantics. Workers are inherited-parent sessions or unsolicited harness shells without explicit user-open provenance; opened workers remain workers even after orphaning.

Local implementation commits: `6d815ae` (port), `6dd83b9` (visibility/feed/API verification), `98e3c71` (desktop controls and window spacing). The documentation/fixture commit follows this report. Nothing was pushed.
