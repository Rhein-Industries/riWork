# RiWork styled Base behavior contract — source handoff

Project `39832c2e-23a5-476d-aa8f-5ff34a02d314`, task
`d87296de-efe1-483a-98a0-d52a5da00e34`, 2026-10-06.
Verified foundation worktree is on `feat/gpui-kit-behavior-controls`, starting
at deployed baseline `2f96821561105e3737cb019585841bc047cb38d4`, tracked clean.
Existing branches and untracked audits are preserved. Source-only; reviewer owns
all compilation/testing and parent owns all Cua.ai Driver MCP desktop work.

## Stable shared imports and constructor surface

New module: `crate::behavior_controls`. Its public control types wrap the exact
unstyled Base 0.7.1 primitives, forwarding their behavior and style builders.
They are not aliases for `Div` and do not synthesize keyboard/click behavior.

```rust
use crate::{behavior_controls as behavior, controls, theme::Palette};
// Public return types:
// behavior::Button = behavior::Control<gpui_kit::base::Button>
// behavior::Toggle = behavior::Control<gpui_kit::base::Toggle>
// behavior::Switch = behavior::Control<gpui_kit::base::Switch>
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
and owner raw navigation remain. `input::Copy` is unbound only in
`Root && Terminal && !Input`, so Root cannot consume the terminal's native copy
gesture. A focused editor nested under a terminal context still owns input Copy.

Workspace retains one selection scope and four modal container focus handles.
Each existing overlay is wrapped with `behavior::modal_scope(existing_div, scope,
stable_id, &focus_handle)`, delegating both selection scoping and non-editor Tab
containment to Base's `text_selection_scope`/`focus_trap`. No new geometry, key
handler or focus engine is added. Owner modal opening, dismissal and input Tab
navigation stay with their current owners. Focus/restore reuse the same window.
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
state/action tests are planned; parent native AX/pointer/keyboard proof remains
pending. No provider/service/process/GUI or Cargo execution is authorized here.

## Source inventory and mandatory consumer handoffs

`src/behavior_controls.rs` supplies real Base Button, Toggle, Switch, Link,
ToggleGroup and Popup behavior, RiWork/Native palette styles, keyboard focus rings,
stable accessible names and disabled handling. Base 0.7.1 omits the disabled AX flag
(its own `button.rs` disabled accessibility test explicitly asserts the omission).
Our transparent Element forwards identity/layout/prepaint/paint/AX actions and
refines the existing node with `Node::set_disabled`, without a second node/handler.

Nine `main.rs` controls migrate here: `status-current-project`,
`status-current-worktree`, `status-agent-activity`, `copy-active-shell-id`,
`top-orchestrator`, `project-orchestrator`, `status-layout`, `focus-layout-toggle`,
`restore-workspace`. The layout button exposes expanded state; missing session ID
disables its copy control. Existing labels/icons, callbacks and palette overrides
are retained. Custom tab dragging, resizing, terminal chrome and overlays retain
their current specialized paths. Other consumer controls belong to chat/forms
workers; this commit does not edit their source.

The only production Workspace `cx.open_window` factory is `open_workspace_window`.
Ordinary/folder/new-project launch, startup fallback, restored runtime windows,
notifications and focus/restore reach it. Quit layout saving, runtime snapshots,
startup notice and notifications unwrap the retained Workspace via `with_workspace`;
the window ID and Workspace entity are not replaced when toggling focus mode.

Two **mandatory handoffs before integrated acceptance** were found outside this
worker's explicit source ownership:

1. `src/dock_menu.rs:206`: Dock New Window still downcasts directly to Workspace.
   The consumer must iterate front windows and call `crate::with_workspace(handle,
   cx, |workspace, _, cx| { let id = workspace.project_id.clone();
   workspace.open_project_window(&id, cx); })`. Activate and stop on `Ok(Some(()))`;
   skip popup/unrelated windows on `Ok(None)`. Direct Workspace downcast fails after
   the shared Root change. Parent/forms owner must land this narrow compatibility fix.
2. `src/tooltip.rs:505`: the separate non-key `Hint` popup factory still constructs
   a bare Hint. If every native window must mount Root, construct Hint exactly as
   today (including its activation observer), then return
   `cx.new(|cx| gpui_kit::base::Root::new(hint, window, cx))`. Its `is_popup` helper
   at line 366 must also identify the Root's Hint content so cascade counts and
   runtime/Dock inventory still exclude it. Preserve `focus: false`, popup kind,
   existing hide/activation policy and transparent native surface. Parent/forms
   owner must handle this route; no second layer belongs inside Hint's render.

The `src/form_input.rs`, `src/text_input/tests.rs` and
`src/chat_view/editor_tests.rs` window factories are headless test fixtures,
not additional production Workspace routes; consumer selection fixtures needing
the layer should wrap their own content once with Base Root.

Chat copying diagnosis is bounded: this baseline did not mount Base Root, which
is a prerequisite for Base selection participant registration and Root Copy.
Mounting Root does not make plain transcript strings selectable automatically;
chat must register participants/use Base selectable content and remove competing
selection handling. Base Root's default Copy trims leading/trailing whitespace
and propagates empty selection. Exact whole-message/code copy must remain an
explicit owner action when byte-preserving copy is required. No native clipboard
failure reproduction or successful chat copy is claimed in this source-only phase.

## Source evidence and proposed reviewer verification

Exact dependency pins remain unchanged: Kit/Base `=0.7.1`, GPUI `=0.3.8` and
Ghostty `=0.3.1`. Cached pinned source was inspected:

- Base `root.rs`: initialization, `Root::new` hit-test forwarder, retained `view`,
  Copy/Tab/focus-trap handling and first-child `TextSelectionLayer` placement.
- Base `button.rs`, `toggle.rs`, `switch.rs`, `link.rs`: focus tracking, disabled
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

Seven new tests are written in `src/behavior_controls/tests.rs`, **unexecuted**:

1. `base_button_pointer_keyboard_activate_once_and_disabled_does_not_bubble`
2. `root_tab_traversal_uses_base_focus_and_skips_disabled_controls`
3. `root_modal_tab_controls_stay_within_base_focus_trap`
4. `base_toggle_switch_link_share_pointer_and_keyboard_owner_callbacks`
5. `accessible_nodes_keep_names_states_and_only_enabled_click_actions`
6. `root_preserves_content_identity_and_selection_is_window_local`
7. `root_modal_scope_and_input_terminal_keys_do_not_cross_owner_boundaries`

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
cargo check --locked --bin riwork
cargo build --locked --bin riwork
cargo test --locked --bin riwork --no-run
cargo test --locked --bin riwork behavior_controls::tests:: -- --test-threads=1
cargo test --locked --bin riwork text_input::tests:: -- --test-threads=1
```

These are proposals, not receipts. This worker ran zero Cargo/build/test/native
commands; only source reads/edits and Git inspection/commit. `git diff --check`
passed for the source patch. Integrated compile, all seven new cases, root-affected
input regression and mandatory Dock/tooltip handoffs remain pending. Production
activation is not authorized or claimed; no forced host refresh or live shell action.
