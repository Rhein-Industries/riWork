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

2026-10-07 audit correction: the earlier missing-terminal-listener finding was incorrect; the source search omitted the nested module. `terminal_links/ui.rs:417–418` attaches the real `PasteInTerminal` listener and calls `workspace.terminal_paste(pane_id, tab_id, cx)`. Its blob is `daa5ae07ce013385723b90861701a6c674088b6b`, identical at foundation HEAD and deployed baseline `2f96821561105e3737cb019585841bc047cb38d4`. No terminal wiring correction is needed or made. Native end-to-end file paste acceptance is outside these fixtures. Actual nested consumer mouse-down stops can still precede GPUI default focus; the separately committed `8a7278b` addresses the owned Workspace tab-close seam without adding a focus engine.

## 2026-10-07 — Last sealed failure: textarea fixture mode

Inherited reviewer receipt at immutable `9c1663411987a548955e6fd211cfc208605b3125`: fresh check/harness/list passed; 50 exact cases, 49 passed / 1 failed; foundation 14/15, selection 16/16, editor 8/8, forms 11/11. Read the actual failure log at `/private/tmp/rw2-uwzcymqf/logs/test-behavior_controls-tests-root_modal_scope_and_input_terminal_keys_do_not_cross_owner_boundaries.log`: line 496 expected two spaces from ordinary Notes Tab but received empty text. The later marked-textarea checks were unreached. No new execution or passing result is claimed here.

Pinned Base separates type and layout: `TextareaMode::MULTI_LINE` is true (`input/base/kind.rs:343–344`), while `TextareaState::auto_grow` chooses `LayoutMode::AutoGrow` (`input/base/state.rs:10758–10760`). `input/editor/indent.rs:58–63,253–255` permits indentation only for editable multiline PlainText/CodeEditor layouts and propagates otherwise. RiWork's ordinary `multiline` helper intentionally selects AutoGrow. The failing two-space assertion therefore used the wrong fixture mode; shared production textarea policy and composition guards remain unchanged.

Only `behavior_controls::tests::root_modal_scope_and_input_terminal_keys_do_not_cross_owner_boundaries` is extended. Ordinary Notes now checks real positive frame bounds, pointer focus, immediate and drained focus across unmarked Tab/ShiftTab, empty text, no marked range and no owner navigation. It then checks marked Tab/ShiftTab preserves exact `日本` bytes, `Some(0..2)` composition and focus, before the original terminal raw-key assertions.

The original strict two-space text and clipboard assertions remain on a separate actual Base `TextareaState::new(...).tab_size(TabSize::default())`: this explicitly retains the default PlainText layout and does **not** call auto_grow. It is mounted under the same Terminal context/shared frame, reached by a real pointer click with measured bounds/focus, and clipboard actions drain in separate turns. The optional state is created only within this exact fixture; all other Harness cases leave it absent. No substitute editor, altered global textarea mode, weakened counts/clipboard expectations, main/other worker source or assembly edit.

`git diff --check` and syntax parsing with the approved rustfmt `--emit stdout src/behavior_controls/tests.rs > /dev/null` passed. No Cargo/build/test/list/runtime/GUI/provider/process-control/service/tmux command ran. Reviewer should compile the assembled source and execute this exact qualified name in its approved private environment. The old 49/50 receipt remains the acceptance boundary until then; native IME, clipboard and geometry proof remain parent-owned and pending. Previous artifacts and terminal source are preserved.
