# Shared GPUI Kit input contract

Foundation owns dependencies, `src/text_input.rs`, and startup/theme integration.
Chat and forms owners retain their field migrations. Rebase/cherry-pick the foundation
commits before compiling those migrations; do not change dependency pins independently.

## Released versions

Use crates.io `gpui-kit = =0.7.1` with `default-features = false`,
`gpui-base = =0.7.1` (exact transitive pin), `gpui-pre = =0.3.8`, and
`gpui-pre-platform = =0.3.8`. Base requires `gpui-pre-macros = =0.3.8`.
Keep `gpui-libghostty = =0.3.1`: it has no GPUI dependency; its application-bound
`bind_gpui!(gpui)` compiles and links with 0.3.8; the headless smoke exercises its
native-window precondition without spawning a terminal process.
Only dev dependencies enable Kit `test-support`; no styled components, assets,
speech, decimal, tree-sitter, git dependency, or substitute editing engine.

Source: published 0.7.1 Cargo.toml and Base input source, plus
[official input docs](https://gpui-kit.com/component/input/) and
[textarea docs](https://gpui-kit.com/component/textarea/).

## Stable `crate::text_input` surface

```rust
pub use gpui_kit::base::input::{InputEvent, InputState, TextareaState};
pub enum EnterBehavior { Submit, Newline }
pub fn single_line(value: impl Into<SharedString>, placeholder: impl Into<SharedString>,
                   window: &mut Window, cx: &mut App) -> Entity<InputState>;
pub fn multiline(value: impl Into<SharedString>, placeholder: impl Into<SharedString>,
                 min_rows: usize, max_rows: usize, enter: EnterBehavior,
                 window: &mut Window, cx: &mut App) -> Entity<TextareaState>;
pub fn input(id: impl Into<ElementId>, state: &Entity<InputState>,
             window: &Window, cx: &App) -> InputBase;
pub fn textarea(id: impl Into<ElementId>, state: &Entity<TextareaState>,
                window: &Window, cx: &App) -> InputBase;
pub fn on_paste<M: InputModeKind>(frame: InputBase, state: &Entity<InputBaseState<M>>,
                handler: impl Fn(&ClipboardItem, &mut Window, &mut App) -> bool + 'static)
                -> InputBase;
pub fn is_submit(event: &InputEvent, behavior: EnterBehavior) -> bool;
pub fn init(cx: &mut App);
```

Construct each entity once when opening/rebinding its owner, store it on the view,
and retain its subscriptions. Rendering only builds `input`/`textarea` frames.
Never call `set_value` or recreate entities on each render. Frames are unstyled
Base controls with compact RiWork colors/type/padding; callers can use `Styled`,
`ParentElement`, and `InteractiveElement` to adjust their surrounding layout.

Subscribe via `cx.subscribe_in(&state, window, |owner, state, event, window, cx| ...)`.
`InputEvent::Change` reads `state.read(cx).value()`; events also include Focus,
Blur, and `PressEnter { secondary, shift }`. Submit suppresses normal Enter
newlines. Both **Shift+Enter and Alt+Enter insert a newline**, including
Alt+Shift+Enter. Startup binds Alt variants to Base's existing shift-newline
action in the `Input` key context, retaining Base selection/history behavior.
Consequently Alt+Enter events report `shift: true`; this flag is a newline
gesture here, not an exact physical modifier report. Newline preserves normal
textarea Enter behavior. Handle submission only when `is_submit` is true;
never submit on Change. The `secondary` modifier retains Base's secondary
Enter semantics and is eligible for submission when `shift` is false.

### Required child-owner Enter handling

Use the shared `input`/`textarea` frames for every retained state. Raw Base
0.7.1 emits `PressEnter` even during marked composition and propagates the
single-line/submit Enter action. `is_submit` alone cannot distinguish
composition: `InputEvent` carries no composition flag. Our frames capture
Enter while the native input handler reports marked text (also for disabled
or read-only states), preventing edits, submission events, and propagation.
GPUI 0.3.8's macOS input context handles composition confirmation first;
native commit/unmark remains the responsibility of that input handler.
Frames also consume Base's propagated Enter action after its event so it
cannot submit an enclosing dialog/workspace a second time.

Chat owners must remove composer Return handling from the old raw
`on_key_down`/workspace engine and choose send/approval/empty-content behavior
in **one retained `InputEvent` subscription** using `is_submit`. Forms owners
must likewise confirm/save from one subscription, not also a parent Enter
action or key handler. Parent capture handlers run before a child frame:
they must skip focused shared inputs and must not independently submit them.
Do not redispatch Enter or synthesize `PressEnter` after native composition
commit. Changing a textarea between Submit and Newline requires updating
both `state.set_submit_on_enter(...)` and the owner's `EnterBehavior` passed
to `is_submit`. Attachment interception remains separate via `on_paste`.

### Composition Escape and form navigation ownership

Both shared frames capture **Base's bound Escape action**, before Base's state
handler. For an editable focused child with marked text, the frame calls Base's
native `unmark_text` and consumes the action. This ends the existing composition
transaction while preserving the draft, caret and persistent state; it does not
emit a submit or run Base's clean-on-Escape behavior on that composing gesture.
The next plain Escape follows the existing Base/owner path. Masking and disabled/
read-only behavior are unchanged; the new guard only acts on editable marked
states. It does not replace keybindings or override user configuration.

This pre-Base interception is deliberate: GPUI 0.3.8 dispatches matched actions
before raw key capture/bubble (`window.rs:6013–6042,6079–6095,6130–6149`). Base
0.7.1's Escape handler unmarks and then propagates (`input/base/state.rs:2079–2116`).
A parent raw capture check alone therefore observes no marked range and can
cancel the form on that same Escape. Consuming before Base prevents both its
parent action propagation and GPUI's later raw-key fallback; no post-handler
latch or second editor is needed. Base's `unmark_text` clears only the marked
range and commits its existing undo transaction (`state.rs:3967–3970`).

Owners must retain their normal close/cancel policy for plain Escape and choose
one domain owner (action or raw callback). An enclosing **action capture** runs
before a child capture: it must defer to the focused shared input. If user
configuration unbinds/replaces Base's Escape action, raw owners must still check
the **focused child's** marked range before cancel/navigation; the shared frame
does not forcibly reinstate the binding. An unfocused sibling's marked range
must not suppress a focused child's action or a parent-focused command.

Single-line Tab/Shift-Tab use Base IndentInline/OutdentInline, which propagate
without editing (`input/editor/indent.rs:244–257`). Their marked range is not
cleared first. Forms retain their raw composition guard: while the focused
field is marked, do not change focus or navigate; after native commit/unmark,
perform one owner navigation. Textarea Tab editing remains Base-owned. Do not
add a second Escape, Return or indentation engine in chat/forms.

The real Base states expose `value() -> SharedString`,
`set_value(value, window, cx)` (silent model replacement, clears history),
`replace_all(value, window, cx)` (undoable, emits Change), and
`replace(value, window, cx)` (undoable selection replacement).
Update them through `state.update(cx, |state, cx| ...)`.
Import `gpui::Focusable`; `state.read(cx).focus_handle(cx)` then
`window.focus(&handle, cx)` controls focus.
`cursor()` and `selected_range()` use **UTF-8 byte offsets**;
`set_selected_range(range, cx)` clamps to character boundaries and supports a
collapsed caret. IME uses GPUI's native UTF-16 input handler, including marked text.
Base owns actual glyph hit testing, dragging, clipboard text, undo/redo, and
keyboard editing; owners must not route these through old workspace text handlers.

The whole shared frame, including padding, focuses its retained state on a left
press unless disabled. Its bubble handler prevents the enclosing focusable view's
default focus transfer after handling that press; it does not stop propagation
or run before Base's glyph/drag handlers. Read-only fields remain focusable and
selectable, while disabled padding leaves the owner's focus policy unchanged.

`on_paste` captures Base's Paste action before insertion. Return true only after
handling an app-owned image/file attachment; this consumes paste. Return false
for ordinary text so Base inserts it normally. Disabled/read-only controls do not
invoke the callback. This desktop hook uses the synchronous native clipboard.

Startup calls `init` once after Appearance/Settings and `ui_text::init`; it installs
Kit Base behavior and projects RiWork theme tokens, following Appearance/Settings
changes. No default visual theme replaces RiWork's native/terminal palette.

## Verification status

Foundation recovery verified on macOS on 2026-10-06 at immutable foundation
`ea05f94d566cdd952b778a2edafcb13f42a1a65f` (before the Escape correction):

- `cargo check --locked`: **passed** (6.17 s).
- `cargo build --locked`: **passed** (11.74 s). Native GPUI and unchanged
  Ghostty 0.3.1 are linked.
- `cargo test --locked --bin riwork text_input::tests:: -- --test-threads=1`:
  **8 passed, 0 failed**. Covers persistent Unicode selection/history/focus,
  native clipboard text and attachment interception, editability, UTF-16 IME,
  glyph hit testing/dragging, compact wrapped layout, live palettes, and the
  Ghostty binding's headless native-window precondition. Enter tests prove
  Alt/Shift newline history, composition suppression, one submission after
  commit, single-line action isolation, and raw Base emission/propagation.
- Locked dependency tree: one `gpui-pre` 0.3.8, Kit/Base 0.7.1, and Ghostty
  0.3.1. Component/assets entries in Cargo.lock are optional-dependency lock
  entries, not enabled production features; normal/build tree excludes
  component/assets/speech/decimal/tree-sitter and test-support.
- Changed Rust files are formatted; `git diff --check` is clean.

Logs are `/tmp/riwork-gpui-kit-foundation-recovery-{check,build,input-tests}.log`.
Production feature audit command:
`cargo tree --locked -e normal,build --prefix none --format '{p} features=[{f}]'`;
output is `/tmp/riwork-gpui-kit-foundation-recovery-dependencies.log`.
All runtime checks use a private temporary child `RIWORK_HOME` and
`RIWORK_RUNTIME_DIR`, leaving the inherited environment/state untouched;
target is `/tmp/riwork-target-gpui-kit-foundation`, with
`ZIG=/Users/dominik/.local/share/riwork/toolchains/zig-0.16.0/zig`.

Limits: this is compiled foundation plus headless smoke, not a native GUI/IME
or live Ghostty terminal session. Consumer migrations and their send/save
integration checks belong to chat/forms owners. No production tmux/app/host
restart, installation, settings/schedule/relay changes, real prompts, main
merge, or push was performed. The prior worker exit cause remains unconfirmed.

### Source-only Escape correction — execution still held

Only `src/text_input.rs`, its tests and this contract changed in the correction.
Polling commit `330d25413b99c03b9272f8e935c481d4c018d205` is preserved. Reviewed
the independent Forms P2 finding at
`/Users/dominik/orca/projects/riWork-review-manu-20261005/GPUI_KIT_REVIEW_REPORT.md:243`
and the exact cached Base/GPUI sources cited above. `git diff --check` passed;
no Cargo/build/test/listing/formatter, native GUI, process/service check or
cleanup was run. **The eight-test receipt above does not validate this change.**
Forms remains the sole build owner pending parent release.

Seven new headless owner-counter tests plus the strengthened existing Return test
are proposed. They use the foundation's existing mount, real persistent Base
states and TestPlatform input/clipboard. Each gesture ends its outer window/App
update, drains `TestAppContext::run_until_parked`, and asserts in a separate turn,
including native commit before a fresh Return. Native OS input is not dispatched.
The current harness uses GPUI's built-in `NoopTextSystem`, which supplies fixed
glyph metrics/layout; there is no `TestTextSystem` symbol in this worktree or the
cached crates. The requested existing TestTextSystem location was asked for;
these tests currently reuse the established foundation harness rather than
introducing a text engine or native platform initialization.

Exact planned names (each must select **one** test; all are **unexecuted**):

```text
text_input::tests::composing_escape_cancels_only_composition_in_forms_and_chat
text_input::tests::plain_escape_reaches_one_parent_cancel_through_action_or_raw_policy
text_input::tests::composing_escape_defers_base_clean_on_escape_until_plain_escape
text_input::tests::raw_escape_fallback_retains_owner_composition_guard_and_key_configuration
text_input::tests::marked_tab_and_shift_tab_keep_child_focus_until_owner_navigation_is_safe
text_input::tests::focused_child_and_parent_ignore_an_unfocused_siblings_marked_range
text_input::tests::composing_escape_preserves_masking_and_disabled_escape_behavior
text_input::tests::composition_return_cannot_submit_edit_or_bubble
```

The first test exercises physical bound Escape and deferred direct Escape for
both single-line and chat states, checks zero parent action/raw/cancel/submission
callbacks, preserved draft and undo of the completed composition transaction.
Plain Escape tests action and raw owner policies independently, with one cancel.
The clean-on-Escape test preserves that Base option: composing Escape retains
the draft, and only the subsequent plain Escape clears it via Base.
Raw fallback explicitly unbinds only its fixture's Escape, checks marked text
still prevents owner cancellation, then checks one plain owner cancellation.
Tab tests check no marked navigation/focus loss and one navigation after unmark.
Sibling/parent-focus tests confirm events follow the focused dispatch path.
Mask/disabled tests retain presentation and original disabled Escape routing.
Return now asserts delivered zero submission/parent action/raw events after each
marked Return variant, zero after native commit, and one after a fresh Return.

After parent release, the forms build owner should use its audited private child
home/runtime/TMPDIR/environment and exclusive target with the specified Zig.
For each exact name above, the proposed command is:

```sh
cargo test --offline --locked --bin riwork text_input::tests::composing_escape_cancels_only_composition_in_forms_and_chat -- --exact --test-threads=1
```

Substitute each remaining exact name, record exit status/count/log independently,
and stop on a failed/zero/extra selection. Do not broaden to a suite or launch
Workspace/services/providers/native GUI. No executable acceptance is claimed.

### Source-only fixture compiler correction — reviewer owns execution

The reviewer reports candidate
`30051200adee219de76953a2c514c1ce8b104005` passed check and binary build, but
`cargo test --no-run` failed with E0282 at `src/text_input/tests.rs:80`.
The existing diagnostic in
`/private/tmp/rwv-icgy9z1n/logs/compile.log` confirms the shared fixture's
`capture_key_down(cx.listener(...))` event type was ambiguous.

The listener now explicitly accepts `event: &gpui::KeyDownEvent`. Source
inspection found no analogous untyped key listener in the shared input module
or its test files; the two action listeners already name their action types.
Only the test fixture type annotation and this receipt note changed; production
code and event behavior are unchanged. No Cargo/build/test/listing/formatter,
runtime, GUI, process probe, install, reload, tmux command or cleanup was run.
The correction and proposed Escape/Return tests remain **unexecuted here**.
The reviewer remains the sole build/test owner and must compile the test target
and record fresh results for the integrated correction. Earlier passing receipts
do not validate this correction. The untracked rollout audit is preserved.

### Source-only frame-padding focus correction — execution still held

The reviewer reports combined candidate
`23d01018f59f16932a840698bda3e9e23108020e` builds and compiles tests, with
76/78 selected cases passing and a separate 16-case polling receipt. The exact
`glyph_hit_testing_drag_selection_and_compact_wrapped_layout` log at
`/private/tmp/rwv-k_qkvg6s/logs/test-text_input-tests-glyph_hit_testing_drag_selection_and_compact_wrapped_layout.log`
records **0 passed, 1 failed** at the `padding focuses` assertion. These are
reviewer receipts, not new executions here; the second failing case is outside
this focused correction.

Source inspection identifies a shared-frame focus defect exposed by the
fixture's enclosing `track_focus(&parent_focus)`. InputBase 0.7.1 forwards
interaction to its frame but `focused(bool)` only sets presentation
(`gpui-base/src/input/base/mod.rs:137–157,219–240`). Base's text child tracks the
state's focus (`input/base/state.rs:4461–4464`). GPUI 0.3.8 registers an
automatic mouse-down focus handler for every tracked focusable div; it transfers
focus unless the event's default was prevented (`elements/div.rs:2770–2785`).
Mouse bubble listeners run in reverse paint order (`window.rs:5792–5829`).
When padding is hit, the adapter focuses the state, then the parent's automatic
handler takes focus back. The child glyph path already prevents that ancestor
default through its own focus handler. Kit's click helper renders, moves the
pointer, dispatches mouse-down, renders, dispatches mouse-up and renders
(`gpui-kit/src/test.rs:105–168`); the failing helper was not omitting mouse-up
or using a stale frame. This diagnosis is source-based, not a fresh reproduction.

The shared frame now calls `window.prevent_default()` immediately after its
existing non-disabled left-press focus operation. That operation is in the
bubble phase after Base's text handler; Base still owns caret placement and
dragging. No duplicate focus handle is attached to the frame, no event is
redispatched, and disabled behavior is unchanged. The original failed assertion
is retained and strengthened by switching focus to the composer before clicking
single-line padding. Its glyph hit test now also checks a click places the caret
at byte 4; drag selection, replacement and compact wrapping assertions remain.

A narrow new headless case checks all four padding corners of both single-line
and textarea frames, with editable, read-only and disabled states. Each case
starts with parent focus and selection `1..4`; mouse-down and mouse-up each get
their own action/drain/assert turns. It asserts the appropriate child/parent
focus and unchanged text/selection after both phases: 24 configurations and 48
phase assertions. The fixture uses native GPUI event dispatch and actual Base
states with the existing **NoopTextSystem**. Real-font cursor geometry, native
macOS pointer/IME behavior and OS GUI proof remain pending parent review.

No Cargo/build/test/listing/formatter/runtime/GUI/process/tmux/host execution or
cleanup occurred. Proposed verification by the sole reviewer after integration:

```sh
cargo test --offline --locked --bin riwork text_input::tests::glyph_hit_testing_drag_selection_and_compact_wrapped_layout -- --exact --test-threads=1
cargo test --offline --locked --bin riwork text_input::tests::frame_padding_keeps_focus_and_selection_after_pointer_down_and_up -- --exact --test-threads=1
```

Each must select exactly one test. Use the reviewer's approved private child
home/runtime/target/Zig boundaries and preserve separate logs. Both corrected
cases are **unexecuted here**; no broader suite rerun is requested by this fix.

### 2026-10-06 — Source-only marked Tab correction

Reviewer receipts at combined `5e73c1c`, `/private/tmp/rw2-2jhb4co_/logs/`,
reported 50 exact cases executed, 41 passing and 9 failing, including 10/15
foundation behavior cases. Two behavior fixtures lost focus immediately on
marked Tab; those receipts do not establish deferred-focus or native IME proof.

Pinned GPUI predicates evaluate positive `&&` identifiers in the same context.
Root and Input are separate ancestors/descendants, so the shared Root traversal
exclusions now use `Root > (Terminal || Input)`. Plain single-line Tab reaches
the existing owner's raw navigation; unmarked textarea Tab still reaches Base
indentation. The frame captures `IndentInline`/`OutdentInline` only while an
editable state has marked text, before Base can edit a composing textarea or
fall through to an enclosing owner. Plain, read-only/disabled, Enter/Escape and
user-configured action policies are otherwise unchanged.

Existing behavior fixtures keep strict immediate and drained focus checks,
zero marked-navigation counts, exact once-only ordinary Tab/ShiftTab navigation,
and now check marked bytes/range survive both directions in single-line and
textarea states. Corrected sources have not been compiled or executed here.
Native IME and terminal delivery remain parent-owned acceptance limits.
