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

`on_paste` captures Base's Paste action before insertion. Return true only after
handling an app-owned image/file attachment; this consumes paste. Return false
for ordinary text so Base inserts it normally. Disabled/read-only controls do not
invoke the callback. This desktop hook uses the synchronous native clipboard.

Startup calls `init` once after Appearance/Settings and `ui_text::init`; it installs
Kit Base behavior and projects RiWork theme tokens, following Appearance/Settings
changes. No default visual theme replaces RiWork's native/terminal palette.

## Verification status

Foundation recovery verified on macOS on 2026-10-06:

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
