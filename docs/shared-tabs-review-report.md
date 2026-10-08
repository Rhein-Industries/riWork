# Shared tabs review corrections

Applied the review in `/tmp/shared-tabs-findings.md` on branch `shared-tabs`. The desktop remains the owner of project membership; selection and pane placement remain local. These corrections make migration conservative, persist worker safety across orphaning, abort destructive close when Hide fails, and identify each store instance independently of its revision.

## Findings

| Finding | Result and verification |
| --- | --- |
| 1: missing wire worker | Fixed. Every root/child emits `worker`. iOS treats either `worker:true` or a non-null parent as a worker, including a contradictory `worker:false` with a parent. Wire and Swift tests cover these cases. |
| 2: failed Hide still exits | Fixed. Store errors, missing shared state and unresolved membership abort Close with a visible error, preserve the local tab and last good list, and send no stop. Missing-entry policy consults session registry/log provenance and defaults unknown provenance to Detach. Workspace regression coverage exercises corrupt store, missing store and unresolved project. |
| 3: history floods migration | Fixed. Saved panes seed visible membership; remaining existing metadata starts hidden, except the project orchestrator, which starts visible and pinned unless previously explicitly detached. Hidden stopped leaf history retains its 200-entry cap. Explicit new-session registration keeps the first newly created user session visible even when it initializes the store. |
| 4: legacy harness disappears | Fixed. A parentless shell in a saved pane records persistent migration user-open provenance, overriding the absent old registry flag. Parent-bearing sessions remain hidden workers. Migration and subsequent-poll tests cover the exception. |
| 5: every shell is user-opened | Fixed. GUI/user handoff uses `for_user`; plain CLI creation checks parent, orchestrator and automation caller markers. Background creation uses `--background`/`for_automation`; these shells record `user_opened=false` and start hidden. Phone user creation clears caller markers. Disposable tmux tests cover first user visibility, automation visibility and orchestrator exclusion from worker policy. |
| 6: one bad chat breaks inventory | Fixed. Per-directory missing, malformed, unreadable or identity-mismatched metadata is skipped with a warning. Other chats remain readable, and existing entries are retained while their directory exists. Whole-directory failures remain errors rather than destructive empty inventories. Migration tests include mid-create and malformed directories. |
| 7: formatting churn | Reverted the unrelated formatting-only hunks in `src/main.rs` against base `9631456`, including reconnect/status/layout/menu/focus code. Formatting was confined to functional changes. Supporting polling-test formatting-only hunks were also restored. |
| 8: dead paths | Removed `refresh_chat_state` and its state-only branch, `shared_tab_children`, `opened_child_views`, the custom JSON depth pre-scan/parser and crate-wide `unbounded_depth` feature. Relay replies use normal bounded serde parsing; the store still caps parent edges at eight. |
| 9: CLI error codes | Fixed. Invalid syntax/keys/titles and policy violations return `invalid_request`; unknown projects/sessions return `not_found`; storage/execution failures return `cli_error`. Classification tests cover malformed keys/titles and unknown projects. |
| 10: functional strip controls | Fixed. The strip uses `WINDOW_CONTROLS_CONTENT_INSET` only when native controls are visible. Right-click or Shift+F10/Menu opens Pin/Unpin, Rename, Close and Move left/right. Close follows the same detach/exit policy; movement respects sibling/pin groups and disables boundary operations. GPUI keyboard tests cover menu focus and Rename activation; pure tests cover movement boundaries. Visual redesign was not undertaken. |
| 11: eviction/reset | Fixed. A persisted insertion queue evicts the oldest of 1,024 deletion tombstones. A UUID `epoch` persists per store and changes on reset. Mac and phone accept a changed epoch at a lower revision; same-epoch revision guards and same-revision status refresh remain. Store and Swift tests cover FIFO order and reset acceptance. |

The requested internally tagged `Update` regression proves each valid variant decodes, then rejects an added unknown field.

## Files and commits

- `97f4a5e`: storage/migration/epoch and wire changes in `src/project_tabs.rs`; tolerant chat metadata and explicit creation in `src/chat/{log,host}.rs`; provenance in `src/sessions.rs`, session fixtures and `src/handoff/target.rs`; CLI classification in `src/cli.rs`; iOS epoch acceptance and tests in `ios/Core/SharedTabs.swift`, `ios/RiWorkRemote/RemoteModel.swift`, `ios/Tests/SharedTabsTests.swift`.
- `ddab32c`: safe desktop close, epoch refresh, formatting reversions and strip controls in `src/main.rs` and new `src/tab_menu.rs`; unused layout paths removed in `src/layouts.rs`; parser/feature cleanup and phone caller environment in `remote/Cargo.toml`, `remote/src/rpc.rs`, `remote/src/rpc/tabs.rs`.
- `fbe78a8`: preserves the orchestrator migration default while retaining explicit legacy detach; adds regression coverage in `src/project_tabs.rs`.
- The documentation commit updates this report and the iOS handoff contract. No push or rebase.

## Final validation

| Check | Result | Log |
| --- | --- | --- |
| Exact required Rust suite | 1,816 passed, 1 known upstream failure, 8 ignored | `/tmp/shared-review-rust-exact-final.log` |
| Complete Rust suite excluding only the known upstream test | 1,878 passed, 0 failed, 8 ignored (1,816 unit + 62 integration) | `/tmp/shared-review-rust-final.log` |
| Separate final Rust integration run | 62 passed, 0 failed | `/tmp/shared-review-rust-integration-final.log` |
| `swift test` in `ios/` | 1,011 passed, 0 failed | `/tmp/shared-review-swift-final.log` |
| Full standalone relay suite | 405 passed, 0 failed, 12 ignored | `/tmp/shared-review-remote-final.log` |
| First-session/automation provenance follow-up | 1 passed | `/tmp/shared-review-provenance-final.log` |
| iOS Simulator build | BUILD SUCCEEDED | `/tmp/shared-review-ios-build.log` |
| Store regression follow-up, including orchestrator migration exception | 27 passed, 0 failed | `/tmp/shared-review-store-confirmed2.log` |
| Whitespace | `git diff --check` passed | Local check |

The complete runs precede the final small orchestrator migration exception; the store follow-up verifies that change. No schedules/update/sessions flaky failures remain in those complete runs.

The upstream failure `text_input::tests::marked_tab_and_shift_tab_keep_child_focus_until_owner_navigation_is_safe` remains unchanged, as requested. Early correction runs exposed test setup and shell registration issues; those were fixed before final validation. The menu test now uses the repository's mounted Kit window fixture, and shell creation uses the same worker predicate as inventory.

## Evidence and open items

No new live desktop screenshot session was started for these review corrections. The previous disposable fixture remains shut down and removed. Existing evidence:

- `/tmp/shared-tabs-dark-strip.png`, `/tmp/shared-tabs-light-strip.png`
- `/tmp/shared-tabs-dark-worker.png`, `/tmp/shared-tabs-light-worker.png`
- `/tmp/shared-tabs-dark-settings.png`, `/tmp/shared-tabs-light-settings.png`
- `/tmp/shared-tabs-dark-close.png`, `/tmp/shared-tabs-light-close.png`

The close-prompt captures show the backdrop but do not visibly establish the panel rendering; that capture/rendering limitation remains open. Live paired Mac–phone timing, carried views across windows and downgrade behavior were not newly exercised visually. The new context menu has headless keyboard coverage, but no live screenshot. See [the original implementation report](shared-tabs-report.md) for earlier desktop evidence and limitations.

[shared-tabs.md](shared-tabs.md) is complete for the iOS chrome worker: list/update/open RPC shapes, nested replies with `worker` and `epoch`, Move with nullable `before`, explicit open/unhide, `RemoteModel` API, capability/refresh rules, `tab_close_behavior` and Ask/Detach/Exit semantics, exact worker/provenance and migration rules.
