# RiWork styled Base behavior contract — source handoff

Project `39832c2e-23a5-476d-aa8f-5ff34a02d314`, task
`d87296de-efe1-483a-98a0-d52a5da00e34`, 2026-10-06.
Verified foundation worktree is on `feat/gpui-kit-behavior-controls`, starting
at deployed baseline `2f96821561105e3737cb019585841bc047cb38d4`, tracked clean.
Existing branches and untracked audits are preserved. Source-only; reviewer owns
all compilation/testing and parent owns all Cua.ai Driver MCP desktop work.
This follow-up starts at immutable `87e30022f03b2b4d0f22a4370c85c58f2db7d8b4`.
Reviewer PRE-01–06 and the forms main.rs handoff were inspected. Parent owns
integration with chat `84d6775` and Dock compatibility `a20465c`.

## Stable shared imports and constructor surface

**2026-10-06 follow-up, parent-approved consumer API (source only):**
`button_content(id, accessible_name, content) -> Button`,
`toggle_content(id, name, content, pressed) -> Toggle`,
`switch_content(id, name, content, checked) -> Switch`, and
`radio_content(id, name, content, checked) -> Radio = Control<base::Radio>`.
These constructors install no hover, padding, background, corners, focus-visible
style or extra visible label/mark. Caller content and styling remain intact;
Base owns control geometry; the render-time RiWork style policy preserves
inherited line height when the caller supplied none, and restores leading
alignment when the caller explicitly chose flex without a justify override.
Explicit line height/justification always wins. `.disabled(bool)` and the
existing builders/AX disabled refinement apply to all four. Radio emits `true`
only for an unchecked choice; activating the checked choice cannot deselect it.

Content/chat capture should use `behavior::activate_content_scope(scope, window,
cx)` instead of calling Base activation directly. Workspace calls
`sync_modal_scope(Some(modal_scope)/None, window, cx)`; ordinary rerenders write
no scope. The window registry remembers the last content scope for modal-close
restoration and ignores background scope activation while modal. This is a
single-line chat-owner handoff, not a chat source edit by foundation. Current
direct Base chat activation survives ordinary rerenders but cannot be remembered
for restoration because pinned Base exposes no active-scope getter.
Call `retire_content_scope(scope, window, cx)` before removing/replacing the
registered content scope, so modal close cannot restore a retired scope. Registry
entries are removed on window close. A new window starts with Base's default
scope, without writing it on every content render.

New module: `crate::behavior_controls`. Its public control types wrap the exact
unstyled Base 0.7.1 primitives, forwarding their behavior and style builders.
They are not aliases for `Div` and do not synthesize keyboard/click behavior.

```rust
use crate::{behavior_controls as behavior, controls, theme::Palette};
// Public return types:
// behavior::Button = behavior::Control<gpui_kit::base::Button>
// behavior::Toggle = behavior::Control<gpui_kit::base::Toggle>
// behavior::Switch = behavior::Control<gpui_kit::base::Switch>
// behavior::Radio = behavior::Control<gpui_kit::base::Radio>
// behavior::Link = behavior::Control<gpui_kit::base::Link>
// behavior::Segments = gpui_kit::base::ToggleGroup
// behavior::Popup = gpui_kit::base::Popup

behavior::button(id, label, controls::Button::Secondary, colors) // -> Button
    .disabled(unavailable)
    .on_click(cx.listener(|owner, event, window, cx| { /* one domain action */ }));
behavior::symbol(id, accessible_name, icon_element, colors) // -> Button
    .disabled(unavailable)
    .on_click(cx.listener(/* existing owner action */));
behavior::disabled_button(id, label, colors) // -> Button, behavior disabled
behavior::action(id, accessible_name, colors) // -> Button, owner supplies visible content
    .child(existing_content).on_click(cx.listener(/* one owner action */));
behavior::toolbar_button(id, sf_symbol, accessible_name, enabled, colors) // -> Button
    .on_click(cx.listener(/* one owner action */));
behavior::toggle(id, label, pressed, colors) // -> Toggle
    .on_change(move |next, event, window, cx| { /* update controlled state */ });
behavior::switch(id, accessible_name, checked, colors) // -> Switch
    .on_change(move |next, event, window, cx| { /* update controlled state */ });
behavior::segments(id, accessible_name, colors) // -> Segments
    .child(behavior::segment(segment_id, label, selected, colors) /* -> Toggle */);
behavior::link(id, label, colors) // -> Link
    .href(url)
    .open_with(|href, event, window, cx| { /* explicit owner navigation */ });
behavior::popup(id, trigger) // -> Popup; content only when owner open state is true
    .content(existing_menu_content);
behavior::button_content(id, name, existing_content) // -> Button; no preset styles
    .hover(owner_hover).focus_visible(owner_focus).on_click(owner_action);
behavior::toggle_content(id, name, existing_content, pressed) // -> Toggle
    .on_change(owner_change);
behavior::switch_content(id, name, existing_content, checked) // -> Switch
    .on_change(owner_change);
behavior::radio_content(id, name, existing_content, checked) // -> Radio
    .set_position(position, choice_count).on_change(owner_change);
```

IDs must be stable and unique in their scope. Labels are meaningful accessible
names; a symbol's name is required independently of its visible glyph. All
control wrappers implement `Styled`, `ParentElement`, `InteractiveElement`,
`StatefulInteractiveElement` and `IntoElement`. Existing RiWork palette/layout/
hover helpers remain usable. Use `controls::Button` only for visual variants;
`Disabled` maps to actual Base disabled behavior. Changing `.disabled(bool)`
must update both Base behavior and native accessibility disabled metadata.
Use `.accessibility_label(name)` to override a dynamic label and `.tab_stop(false)`
only when an existing owner explicitly excludes that control from traversal.
`controls::toolbar_button_on` accepts these controls as well as existing Divs.

Button `.on_click` delegates to Base's one activation callback for pointer,
Enter/Space and AX Click. Toggle/Switch `.on_change` delegates to Base and
reports the proposed next boolean; the owner retains controlled state. Do not
add a second click/mouse/key handler around the same standard control. Do not
use a plain div with a role as a substitute. Retain custom drag/resize/native
terminal interaction where it is genuinely not a standard button.

`cx.listener` has the Button's three callback arguments; it does **not** have
Toggle/Switch's four arguments. For controlled booleans use an owner weak entity:

```rust
let owner = cx.entity().downgrade();
behavior::switch("show-details", "Show details", self.show_details, colors)
    .on_change(move |next, _, _, cx| {
        let _ = owner.update(cx, |state, cx| {
            state.show_details = next;
            cx.notify();
        });
    });
```

Link has no implicit navigation side effect: supply `.open_with` for href
navigation, or `.on_activate` for an owner action, without combining two
callbacks that perform the same action. Popup owns measured anchoring/deferred
paint/window-edge positioning only; existing owner open/close, modal focus and
domain policy remain explicit. Segments use controlled Base Toggle children
with pressed accessibility state, not a custom click engine.

## Window root and selection ownership

Foundation owns the single `open_workspace_window` factory and all Workspace
root discovery in `main.rs`. The factory mounts **one Base Root** around
the retained Workspace entity. Base Root already places **one TextSelectionLayer
as its first child** before content/overlays and owns root Copy/Tab behavior.
Workers must not add another Root or TextSelectionLayer to ChatView, panels,
forms, modal children, focus-mode content, or restore paths. Window and project/
shell/chat identity remain the same; Workspace entity discovery will unwrap
Base Root's `view()` for existing runtime snapshots, notifications and quit.

Selectable content uses Base selection participants and stable semantic scopes.
An active modal must exclude background participants; focus mode/restore must
reuse this same window root. Base input editors own their own copy/drag engine;
native terminal input/selection remains terminal-owned. `behavior::init` runs
after `text_input::init` and targets only `root::Tab`/`root::TabPrev` with GPUI
`Unbind` in `Root && (Terminal || Input)`. Thus adding Root does not move focus
before an editor owner's marked-Tab check; input indentation/navigation bindings
and owner raw navigation remain. Pinned Input registers IndentInline/OutdentInline
handlers only for multiline mode (`input/base/state.rs:4480–4484`); a single-line
Tab can therefore reach its existing owner navigation after the Root traversal
unbind. The fixture checks marked Tab and ShiftTab preserve composition/focus,
then ordinary Tab and ShiftTab navigate once after unmarking. `input::Copy` is unbound only in
`Root && Terminal && !Input`, so Root cannot consume the terminal's native copy
gesture. A focused editor nested under a terminal context still owns input Copy.

The native Edit menu installs `Cut`, `Copy`, `Paste`, `Select All` using exactly
`base::input::{Cut, Copy, Paste, SelectAll}` and their standard `OsAction`s. No
new global shortcut is installed. Pinned Ghostty's raw key callback forwards
keys but does not stop propagation; GPUI macOS can subsequently offer unhandled
Command keys to menu equivalents (`window.rs:2637–2762`). Workspace's bubble
handler calls `protect_terminal_edit_menu_fallback` only for unmodified Command
X/C/V/A in `Terminal && !Input`, **after** Ghostty has received the unbound key.
The existing bound `PasteInTerminal` file/picture action runs first; its propagated
plain-text key still reaches Ghostty. Modified application shortcuts, Tab and
nested editor actions are excluded. This is not a global action swallow or a
second terminal copy engine. The headless terminal proxy proves this routing
boundary; actual AppKit key-equivalent/Ghostty clipboard acceptance remains pending.

Workspace retains one modal selection scope and four modal container focus handles.
Each existing overlay is wrapped with `behavior::modal_scope(existing_div, scope,
stable_id, &focus_handle)`, delegating both selection scoping and non-editor Tab
containment to Base's `text_selection_scope`/`focus_trap`. No new geometry, key
handler or focus engine is added. Owner modal opening, dismissal and input Tab
navigation stay with their current owners. Focus/restore reuse the same window.
`sync_modal_scope` writes scope only on opening/closing a modal, preserving
non-default chat selection through ordinary parent paints. A tracked content
scope is restored on close; Base clears the modal selection on the transition.
The four modal entrypoints already reject another open modal. `FocusReturn`
captures the invoker before opening, restores it on cancel/close, and forgets it
on successful domain transitions; missing invoker falls back to active content.
It does not implement Tab traversal. Main uses `restore_within(&workspace.focus,
window, cx)` to reject an invoker absent from the rendered owner tree (for example
a menu row removed when opening a modal); rejected restoration falls back to active
content. Switching between pane/layout menus preserves the original invoker.
Native lifecycle coverage remains pending.
Chat owner owns replacing transcript selection/copy behavior and participant
registration; no chat source is edited here. Forms owner inventories its
controls and hands main.rs needs to the parent/foundation without overlapping
main.rs edits.

## Native accessibility boundary and verification

GPUI 0.3.8's Window constructor initializes AccessKit unless accessibility was
explicitly force-disabled; the native macOS adapter initializes there. Base init
is already reached once through `text_input::init`; it must not be duplicated.
Base Root installs the same macOS hit-test forwarder already called by RiWork's
window factory. Partial/occluded production captures showing only window/chrome
do not prove descendants absent. Source audit and meaningful headless role/name/
state/action tests are written and unexecuted; parent native AX/pointer/keyboard proof remains
pending. No provider/service/process/GUI or Cargo execution is authorized here.

## Source inventory and mandatory consumer handoffs

`src/behavior_controls.rs` supplies real Base Button, Toggle, Switch, Radio, Link,
ToggleGroup and Popup behavior, RiWork/Native palette styles, keyboard focus rings,
stable accessible names and disabled handling. Base 0.7.1 omits the disabled AX flag
(its own `button.rs` disabled accessibility test explicitly asserts the omission).
Our transparent Element forwards identity/layout/prepaint/paint/AX actions and
refines the existing node with `Node::set_disabled`, without a second node/handler.
Pinned Radio also omits disabled pointer suppression; the wrapper suppresses
only a disabled Radio's left mouse-down so a nested parent control cannot activate.
Enabled/checked behavior remains Base-owned. Checked Radio exposes selected/
toggled state and no Click action, since clicking it cannot deselect it.
Pinned `FocusTrapContainer` omits its inner AX metadata: `focus_scope` preserves
the original role/name/state on that same trapped element and advertises its
tracked Focus capability. There is no extra accessibility node or event engine.

The original nine `main.rs` controls are retained: `status-current-project`,
`status-current-worktree`, `status-agent-activity`, `copy-active-shell-id`,
`top-orchestrator`, `project-orchestrator`, `status-layout`, `focus-layout-toggle`,
`restore-workspace`. The layout button exposes expanded state; missing session ID
disables its copy control. Existing labels/icons, callbacks and palette overrides
are retained. Custom tab dragging, resizing, terminal chrome and overlays retain
their current specialized paths. This follow-up also migrates every remaining
standard main.rs `on_click` family in the forms inventory: remote reconnect,
tab selection and nested tab close, orchestrator skill load, layout-menu rows,
account/usage chips, both refresh-usage variants, pane symbols, main-pane marker,
and pane-menu rows. Base owns focus and keyboard/AX activation; the existing
domain callbacks and visual tokens remain. Tabs expose Tab/TabList and selected
state; pane Main/Lock menu rows expose MenuItemCheckBox state. The disabled layout
row and pending refresh now have real disabled behavior. Focus/restore use the
unstyled content API, each with exactly one caller hover and a caller focus ring.
Other consumer controls belong to chat/forms workers; no consumer source is edited.
Pinned Base `Tab` explicitly lacks keyboard focus/navigation (`tabs.rs:14–20`),
so these draggable tabs use actual Base Button activation with Tab semantics,
not the incomplete Tab primitive or a new key engine. Existing Next/Previous Tab
domain shortcuts remain; no new arrow-key/roving tab-list policy is claimed.

Pane/layout menu rows use actual Base Button behavior and a Base focus-trapped
Menu container, with one remembered invoker and outside/Escape restore. Existing
absolute anchoring and terminal-snapshot open/close ownership remain intentional:
substituting Popup's deferred positioning here would change native terminal overlay
timing/placement. Thus these two menus use Base focus/activation without claiming
Base Popup owns their geometry. Menu domain actions retain their existing focus
destination. Ghostty, native window/tab drag, split resize and native overlays
retain their specialized engines.

The only production Workspace `cx.open_window` factory is `open_workspace_window`.
Ordinary/folder/new-project launch, startup fallback, restored runtime windows,
notifications and focus/restore reach it. Quit layout saving, runtime snapshots,
startup notice and notifications unwrap the retained Workspace via `with_workspace`;
the window ID and Workspace entity are not replaced when toggling focus mode.

Parent has already committed Dock Root lookup compatibility as `a20465c` in
integration. This worktree does not edit Dock; combined acceptance must include
that commit. The separate `src/tooltip.rs` Hint factory is an explicit approved
**bare-root exception**: display-only, non-key, no chat/editor content. Its popup
classification, activation observer, transparent surface and focus=false remain.
Do not broaden native Hint behavior by adding Root or another selection layer.

The `src/form_input.rs`, `src/text_input/tests.rs` and
`src/chat_view/editor_tests.rs` window factories are headless test fixtures,
not additional production Workspace routes; consumer selection fixtures needing
the layer should wrap their own content once with Base Root.

Chat copying diagnosis is bounded: the original baseline did not mount Base Root, which
is a prerequisite for Base selection participant registration and Root Copy.
Mounting Root does not make plain transcript strings selectable automatically;
chat must register participants/use Base selectable content and remove competing
selection handling. Base Root's default Copy trims leading/trailing whitespace
and propagates empty selection. Exact whole-message/code copy must remain an
explicit owner action when byte-preserving copy is required. Parent chat commit
`84d6775` handles Copy at the bubble and preserves outer whitespace. The fixture
tests this same action priority, but does not prove actual chat virtualized source
projection or stream-cache correctness (PRE-03/05 remain chat-owned). No native
clipboard reproduction or successful chat copy is claimed in this source-only phase.

## Source evidence and proposed reviewer verification

Exact dependency pins remain unchanged: Kit/Base `=0.7.1`, GPUI `=0.3.8` and
Ghostty `=0.3.1`. Cached pinned source was inspected:

- Base `root.rs`: initialization, `Root::new` hit-test forwarder, retained `view`,
  Copy/Tab/focus-trap handling and first-child `TextSelectionLayer` placement.
- Base `button.rs`, `toggle.rs`, `switch.rs`, `radio.rs`, `link.rs`: focus tracking, disabled
  callback suppression, native keyboard/AX Click dispatch and role/name/state.
- Base `focus_trap.rs`, `text_selection.rs`, `toggle_group.rs`, `popup.rs`:
  container containment, active scope/registration, toolbar/pressed semantics,
  measured deferred anchoring.
- GPUI `window.rs:1615` initializes AccessKit unless force-disabled; native
  `gpui-pre-macos/src/window.rs:2365` installs `SubclassingAdapter::for_window`.
  AX activation requests subsequent refresh/tree frames; a bounded occluded capture
  with only chrome cannot establish whether descendants are missing. Adding Root
  is a selection prerequisite, not a replacement for this existing AX bootstrap.
- GPUI `element.rs`/`elements/div.rs` and `keymap.rs`: existing-node metadata/action
  dispatch and targeted `Unbind` resolution, rather than swallowing all keys.

Fifteen tests are written in `src/behavior_controls/tests.rs`, **all unexecuted
by this worker**. Seven original cases remain, with fixtures updated coherently:

1. `base_button_pointer_keyboard_activate_once_and_disabled_does_not_bubble`
2. `root_tab_traversal_uses_base_focus_and_skips_disabled_controls`
3. `root_modal_tab_controls_stay_within_base_focus_trap`
4. `base_toggle_switch_link_share_pointer_and_keyboard_owner_callbacks`
5. `accessible_nodes_keep_names_states_and_only_enabled_click_actions`
6. `root_preserves_content_identity_and_selection_is_window_local`
7. `root_modal_scope_and_input_terminal_keys_do_not_cross_owner_boundaries`

Seven follow-up cases:

8. `parent_rerender_preserves_nondefault_transcript_scope_after_pointer_selection`
9. `two_visible_transcript_scopes_switch_on_first_pointer_gesture`
10. `modal_transition_restores_content_scope_and_retired_scope_stays_inactive`
11. `single_line_owner_tab_and_shift_tab_preserve_ime_and_navigate_once`
12. `native_edit_menu_uses_base_actions_and_preserves_editor_priority`
13. `content_controls_keep_caller_hover_nested_isolation_and_radio_exclusivity`
14. `content_adapter_ax_names_disabled_radio_and_focus_scope_metadata`
15. `content_control_typography_and_row_bounds_match_legacy_at_scale_and_palette`

The scope regression selects by pointer under a distinct chat scope, then forces
the actual parent render path before Copy/assertions. It covers both current
direct Base capture and the registered helper. The two-pane fixture switches on
the first gesture; modal close is tested with child activation disabled, and
retiring the content scope prevents stale restoration. The native Edit fixture
uses the menu's actual action boxes, tests whitespace, editor SelectAll/Copy/Cut/
Paste priority and raw terminal delivery before the narrow menu-fallback guard.
Content fixtures allow exactly one caller hover, exercise nested Close and disabled
Radio suppression, controlled Toggle/Switch, and Radio's no-deselect rule. Modal
Tab/cancel verifies containment, invoker restoration and rejection of a disabled
invoker no longer present in the focus tree, not real form dismissal.

Fixtures use actual Base controls/Root/Input states, fixed selection registrations,
synthetic clipboard/events and a Terminal key-context proxy, never Workspace,
Ghostty, a shell/service/provider or credentials. Action effects are drained between
turns before owner counter/focus assertions. AX tests inspect actual rendered
Element nodes and enabled Click capability; they do not simulate OS AX action
delivery. The test text system remains headless/Noop; real font glyph hit testing,
native AX descendants/actions, foreground pointer/keyboard and copy proof are
still parent-owned Cua.ai Driver MCP acceptance work.

Reviewer-only proposed commands after parent authorizes its isolated slot, with
private child `RIWORK_HOME`, `RIWORK_RUNTIME_DIR` and its existing private target
cache (never overwrite inherited home):

```sh
# Reviewer supplies already isolated child directories in these task-specific vars.
env RIWORK_HOME="$review_child_home" RIWORK_RUNTIME_DIR="$review_child_runtime" CARGO_TARGET_DIR="$review_target" cargo check --locked --bin riwork
env RIWORK_HOME="$review_child_home" RIWORK_RUNTIME_DIR="$review_child_runtime" CARGO_TARGET_DIR="$review_target" cargo build --locked --bin riwork
env RIWORK_HOME="$review_child_home" RIWORK_RUNTIME_DIR="$review_child_runtime" CARGO_TARGET_DIR="$review_target" cargo test --locked --bin riwork --no-run
env RIWORK_HOME="$review_child_home" RIWORK_RUNTIME_DIR="$review_child_runtime" CARGO_TARGET_DIR="$review_target" cargo test --locked --bin riwork behavior_controls::tests:: -- --test-threads=1
env RIWORK_HOME="$review_child_home" RIWORK_RUNTIME_DIR="$review_child_runtime" CARGO_TARGET_DIR="$review_target" cargo test --locked --bin riwork text_input::tests:: -- --test-threads=1
```

These are proposals, not receipts. This worker ran zero Cargo/build/test/native
commands; only source reads/edits and Git inspection/commit. `git diff --check`
passed with exit 0 for this source patch. Integrated compile, all fifteen
cases and root-affected input regressions remain reviewer-owned. Chat helper/
retirement integration and the parent Dock commit are acceptance dependencies;
Hint requires no migration. Production activation is not authorized or claimed;
no forced host refresh or live shell action.

## Focused correction following the early reviewer receipts — 2026-10-06

Starts at `d92cbb9bfc13e62d5be9ebd09b95ea9568bd6bb8`. Reviewer report line 927
records immutable `6c1f5da944de019d550affeebd14a874853a44c4`: 24 exact cases,
14 passed / 10 failed (foundation 4/7, selection 2/9, existing editor 8/8).
The parent's later `6192c94` message-fixture correction reached selection 7/9.
These are inherited receipts from different candidates, not this patch's results.
The early run did not cover d92's seven added cases. No new execution here.

Only parent 6c1's control-test hunks were inspected/incorporated: stable Harness
ID before on_click; the FocusHandle clone before window.focus was already present
in d92 and remains. No chat fixture file, branch cherry-pick or consumer source edit.

Focused changes and exact affected fixture names:

- `base_toggle_switch_link_share_pointer_and_keyboard_owner_callbacks`: Base Link
  lacks test_support in the pinned source. A transparent Element probe forwards all
  phases/AX metadata and captures the **actual Link** bounds/node after prepaint.
  The fixture targets those coordinates via native synthetic pointer events and
  checks Base's supplied focus handle before Enter/Space. No mirrored Link role,
  proxy behavior or replacement hitbox; disabled activation assertions retained.
- `accessible_nodes_keep_names_states_and_only_enabled_click_actions` and
  `content_adapter_ax_names_disabled_radio_and_focus_scope_metadata`: controls now
  mount in a retained NodeFixture under one Root; RenderOnce runs during GPUI drawing
  with a current view. Refined AX metadata is collected during actual prepaint, then
  read after the completed frame. All role/name/disabled/state/Click assertions
  remain, including both checked Radio states and Menu Focus metadata.
- `root_modal_scope_and_input_terminal_keys_do_not_cross_owner_boundaries` and
  `single_line_owner_tab_and_shift_tab_preserve_ime_and_navigate_once`: focus asserted
  immediately after click, after marking, after a frame, before/after Tab, and after
  effects flush, with active contexts in failure diagnostics. The click-count probe
  is now an untracked outer div; the normal domain focus container is a separate
  keyed child with no on_click. Thus observation cannot synthesize its own focused
  Enter/Space click. Both 6c and d92 already have the shared input padding handler's
  window.prevent_default(), so that source was left intact. Workspace's production
  tracked root already has no on_click. The early receipt does not establish the
  exact loss phase; these stronger assertions retain that diagnosis for reviewer
  execution rather than asserting a proven native fix.
- `content_control_typography_and_row_bounds_match_legacy_at_scale_and_palette`:
  renders real legacy Div and Base Button/Toggle text rows, compares descendant
  inherited text styles and actual row/label bounds at 0.85/1/1.5 scale, Menlo/
  colorful and system-face/Native tokens, default/inherited/explicit line height,
  and leading/explicit centered alignment (24 configurations, 48 control comparisons).
  No hover/padding/background/corners are added by the constructor. This is a
  headless layout/style comparison, not native font/glyph acceptance.

All existing scope/menu tests remain. The same Base Edit actions and Terminal
guard are unchanged. `native_edit_menu_uses_base_actions_and_preserves_editor_priority`,
`parent_rerender_preserves_nondefault_transcript_scope_after_pointer_selection` and
`two_visible_transcript_scopes_switch_on_first_pointer_gesture` dispatch menu actions
in separate `turn` calls, which leave update_window and drain effects before clipboard,
model and counter assertions. No clipboard assertion occurs in the action-dispatch turn.

**Forms/main integration boundary:** forms owns its actual Base Dialog states and
keyboard containment. Once those modal modules are assembled, main's backdrop must
use selection scoping only for the Dialog-owned families, avoiding redundant outer
modal_scope focus traps and competing invoker-restoration policies. Parent must
assemble that scoped main change together with the forms Dialog commit; removing
the existing main traps in this earlier branch would leave its current old modal
modules uncontained. This correction edits no main/form/chat source and does not
claim the combined Dialog ownership has been verified. One trap and one restoration
owner per active modal is the acceptance rule; no new keyboard policy is added.

Reviewer proposal: compile once in the already approved private child/cache, then
run the five corrected names above plus the new style case, and the retained scope/
menu cases needed for integration. All 15 remain available under the existing
behavior_controls::tests:: filter. No broader suite or native execution is requested
by this worker. Source `git diff --check` is the only new check receipt.
