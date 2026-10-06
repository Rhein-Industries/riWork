# Foundation correction — source-only receipt

2026-10-06 · Project `39832c2e-23a5-476d-aa8f-5ff34a02d314` · Task `d87296de-efe1-483a-98a0-d52a5da00e34`.

Own branch/worktree verified at `6bdf2029a119759ab606c391ec1f0cc65f5bf1e5`. Read actual assembled source at `/private/tmp/rw2-2jhb4co_/src/src/` and the five exact `test-behavior_controls-tests-<name>.log` files under that runner's `logs/`. Immutable candidate: `5e73c1cabf223efcd91bebfc6e0ec2f3fa2fcee5`. Parent reports normal check/harness passed, 50 exact cases executed with 41 pass / 9 fail, foundation 10/15; these are inherited results, not new verification.

## Narrow corrections and evidence

* `behavior_controls::init`: use `Root > (Terminal || Input)` for Root Tab/TabPrev exclusions and `(Root > Terminal) && !Input` for Root Copy. Pinned GPUI 0.3.8 `keymap/context.rs:277–319` evaluates positive `&&` operands in the same last context; `>` expresses ancestry, and outer `!Input` excludes Input anywhere in the stack. The old predicates therefore missed separate `[Root, Input]` / `[Root, Terminal]` contexts. `window.rs:6008–6039` dispatches bound actions before raw capture, explaining the immediate marked-Tab focus loss and missing terminal raw Copy/Tab receipts; this is a source diagnosis, not a fresh runtime reproduction or deferred-focus workaround.
* `text_input::frame`: capture Base `IndentInline`/`OutdentInline` only for editable marked composition. Pinned Base `input/editor/indent.rs:200–259` does not guard marked text before indentation; a composing textarea must not be edited, and a single-line composing key must not fall through to parent navigation. Ordinary Base indentation, owner navigation, Return/Escape, disabled/read-only behavior and custom bindings retain their existing owners. No second editor engine or focus restoration added.
* `Harness::render` / Link fixture: Base `link.rs:135–140,179–184` owns a private keyed handle and overwrites supplied `track_focus` on render. Remove the misleading fixture handle and observe the library's actual focus on mouse-up, after native mouse-down focus. GPUI `window.rs:5820` bubbles mouse listeners in reverse registration order, so reading from a custom mouse-down observer would be too early. Keep actual laid-out Link role/bounds and strict focus plus three activation / disabled counts; no proxy or injected focus operation.
* `ContentControls::render`: the nested Close callback now stops its click propagation, as actual main tab controls and forms `panels::project_control` already do (`main.rs` nested tab Close callback; forms `panels.rs:1866–1868`). Base intentionally bubbles enabled clicks. Isolation belongs to this command owner, not a blanket shared click swallow or second activation engine. No mouse-down stop is added to the fixture: it must retain Base default focus before keyboard activation.
* `NodeFixture::render`: manually incorporated only parent `332920173ff8bd3d99c47e3ba2e64ff97674fdd1`'s stable `node-menu-scope` ID hunk. No other parent source copied.

## Exact corrected fixtures, pending reviewer execution

All below remain under `behavior_controls::tests::`:

1. `base_toggle_switch_link_share_pointer_and_keyboard_owner_callbacks`
2. `root_modal_scope_and_input_terminal_keys_do_not_cross_owner_boundaries`
3. `single_line_owner_tab_and_shift_tab_preserve_ime_and_navigate_once`
4. `native_edit_menu_uses_base_actions_and_preserves_editor_priority`
5. `content_controls_keep_caller_hover_nested_isolation_and_radio_exclusivity`

Expected callback counts, immediate/drained focus, marked-navigation zero counts, plain-navigation exact counts and terminal raw `[x,c,v,a,tab,tab]` / four menu-guard assertions are unchanged. Added drained marked-range/bytes checks for both Tab directions in single-line and textarea; the existing unmarked textarea indentation assertion remains. The menu fixture also verifies nested Input Copy/Paste beats Root and the exact production `cmd-v` → `PasteInTerminal` / `Terminal` binding, installed after shared init. Its inert action observer propagates text paste and performs no terminal/file/provider work. Native menu action assertions remain after deferred effects drain. Existing AX/style machinery and all fifteen fixture names remain unchanged; the keyed NodeFixture also supports the two existing AX fixtures.

## Limits and commands

`git diff --check` passed. Source syntax was parsed without rewriting unrelated formatting:

```sh
/Users/dominik/.rustup/toolchains/1.95.0-aarch64-apple-darwin/bin/rustfmt --edition 2024 --config skip_children=true --emit stdout src/behavior_controls.rs src/behavior_controls/tests.rs src/text_input.rs > /dev/null
```

No Cargo/build/test/list/runtime/GUI/provider/process/tmux/service command ran. This patch changes no main, Ghostty, other worker source, production bindings or file/picture handlers; previous audits remain preserved. The sole reviewer must compile and rerun the five exact names in an approved private environment; old failing receipts remain until then. Real native IME, terminal delivery, AX and clipboard acceptance are pending parent control.

Two source boundaries deserve parent follow-up without expanding this patch: main declares/binds `PasteInTerminal`, and `terminal_drop::terminal_paste` implements file/picture handling, but no attached action listener/call site for that action was found in the assembled source. This receipt does not claim end-to-end file paste acceptance. Also, actual nested consumer mouse-down propagation stops can run before GPUI default focus (reverse bubble order); their native keyboard-focus acceptance is not established by the corrected fixture's click isolation. No competing focus engine or other owner edits were introduced to hide either limit.
