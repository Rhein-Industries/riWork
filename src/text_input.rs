//! RiWork presentation over GPUI Kit's persistent Base text editing states.
//!
//! Create states once, retain subscriptions on their owner, and build frames in
//! render. Never synchronize model values by calling `set_value` during render.
#![allow(dead_code)] // Consumers migrate in separate chat/forms commits.

use gpui::{
    App, AppContext, ClipboardItem, ElementId, Entity, EntityInputHandler, Focusable, Hsla,
    InteractiveElement, KeyBinding, MouseButton, ParentElement, SharedString, Styled, Window, rgb,
};
pub use gpui_kit::base::input::{InputBase, InputEvent, InputState, TextareaState};
use gpui_kit::base::{
    ColorTokens, Theme, ThemeAppearance,
    input::{
        Enter, Escape, IndentInline, Input, InputBaseState, InputModeKind, OutdentInline, Paste,
        Textarea,
    },
};

use crate::{settings::Settings, theme, ui_text};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnterBehavior {
    Submit,
    Newline,
}

/// Classify events from the guarded `input`/`textarea` frames only. Base events
/// alone carry no composition flag; a raw Base control is not submit-safe.
pub fn is_submit(event: &InputEvent, behavior: EnterBehavior) -> bool {
    behavior == EnterBehavior::Submit
        && matches!(event, InputEvent::PressEnter { shift: false, .. })
}

pub fn single_line(
    value: impl Into<SharedString>,
    placeholder: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .default_value(value)
            .placeholder(placeholder)
    })
}

pub fn multiline(
    value: impl Into<SharedString>,
    placeholder: impl Into<SharedString>,
    min_rows: usize,
    max_rows: usize,
    enter: EnterBehavior,
    window: &mut Window,
    cx: &mut App,
) -> Entity<TextareaState> {
    let min_rows = min_rows.max(1);
    cx.new(|cx| {
        TextareaState::new(window, cx)
            .default_value(value)
            .placeholder(placeholder)
            .soft_wrap(true)
            .auto_grow(min_rows, max_rows.max(min_rows))
            .submit_on_enter(enter == EnterBehavior::Submit)
    })
}

fn frame<M: InputModeKind>(
    id: impl Into<ElementId>,
    state: &Entity<InputBaseState<M>>,
    window: &Window,
    cx: &App,
) -> InputBase {
    let colors = theme::palette(cx);
    let enter_state = state.clone();
    let escape_state = state.clone();
    let indent_state = state.clone();
    let outdent_state = state.clone();
    let retained = state.clone();
    let state = state.read(cx);
    InputBase::new(id)
        .focused(state.focus_handle(cx).is_focused(window))
        .disabled(state.presentation().is_disabled())
        .styles(|styles| styles.focused(|style| style.border_color(rgb(colors.focus))))
        .w_full()
        .min_w_0()
        .border_1()
        .border_color(rgb(colors.divider))
        .rounded(ui_text::space(3.))
        .bg(rgb(colors.bg))
        .text_color(rgb(colors.text))
        .font_family(ui_text::ui_family())
        .text_size(ui_text::text(11.))
        .line_height(ui_text::space(18.))
        .px(ui_text::space(6.))
        .py(ui_text::space(3.))
        .capture_action(move |_: &Escape, window, cx| {
            // Bound actions run before raw-key capture. Base would unmark then
            // propagate Escape, letting a parent cancel on that same key.
            // Use Base's native composition cancellation before its handler;
            // consuming here also prevents GPUI's later raw-key fallback.
            let cancelled = escape_state.update(cx, |state, cx| {
                if !state.is_editable() || state.marked_text_range(window, cx).is_none() {
                    return false;
                }
                state.unmark_text(window, cx);
                cx.notify();
                true
            });
            if cancelled {
                cx.stop_propagation();
            }
        })
        .capture_action(move |_: &Enter, window, cx| {
            // Base 0.7.1 does not guard Enter against marked composition.
            // Native IME owns confirmation; a leaked action must neither edit
            // marked text nor emit an application submission event.
            let blocked = enter_state.update(cx, |state, cx| {
                !state.is_editable() || state.marked_text_range(window, cx).is_some()
            });
            if blocked {
                cx.stop_propagation();
            }
        })
        .on_action(|_: &Enter, _, cx| {
            // Base propagates single-line/submit Enter after emitting its
            // event. Owners submit only from that event, once; keep the action
            // away from enclosing dialogs/workspace handlers.
            cx.stop_propagation();
        })
        .capture_action(move |_: &IndentInline, window, cx| {
            // Bound Input actions precede parent raw capture. Base indentation
            // does not check marked composition, including in a textarea.
            let composing = indent_state.update(cx, |state, cx| {
                state.is_editable() && state.marked_text_range(window, cx).is_some()
            });
            if composing {
                cx.stop_propagation();
            }
        })
        .capture_action(move |_: &OutdentInline, window, cx| {
            let composing = outdent_state.update(cx, |state, cx| {
                state.is_editable() && state.marked_text_range(window, cx).is_some()
            });
            if composing {
                cx.stop_propagation();
            }
        })
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            // Padding is part of the control's click target. Base still places
            // the caret using its glyph hit test when the text itself is hit.
            let state = retained.read(cx);
            if !state.presentation().is_disabled() {
                window.focus(&state.focus_handle(cx), cx);
                // This bubble handler runs after Base's glyph/drag handler.
                // Keep an enclosing focusable view's default mouse handler
                // from taking focus back when only our padding was hit.
                window.prevent_default();
            }
        })
}

pub fn input(
    id: impl Into<ElementId>,
    state: &Entity<InputState>,
    window: &Window,
    cx: &App,
) -> InputBase {
    frame(id, state, window, cx).child(Input::new(state))
}

pub fn textarea(
    id: impl Into<ElementId>,
    state: &Entity<TextareaState>,
    window: &Window,
    cx: &App,
) -> InputBase {
    frame(id, state, window, cx).child(Textarea::new(state))
}

/// Capture native paste before Base inserts text. Return true only if consumed.
/// The persistent state supplies live disabled/read-only checks at dispatch time.
pub fn on_paste<M: InputModeKind>(
    frame: InputBase,
    state: &Entity<InputBaseState<M>>,
    handler: impl Fn(&ClipboardItem, &mut Window, &mut App) -> bool + 'static,
) -> InputBase {
    let state = state.clone();
    frame.capture_action(move |_: &Paste, window, cx| {
        if state.read(cx).is_editable()
            && let Some(clipboard) = cx.read_from_clipboard()
            && handler(&clipboard, window, cx)
        {
            cx.stop_propagation();
        }
    })
}

/// Composer-specific reader. Forms keep `on_paste` and Kit's ordinary text path.
/// Read errors are offered to the owner so image failures cannot fall through to text.
pub fn on_paste_with_reader<M: InputModeKind>(
    frame: InputBase,
    state: &Entity<InputBaseState<M>>,
    reader: impl Fn(&mut App) -> Result<Option<ClipboardItem>, String> + 'static,
    handler: impl Fn(Result<&ClipboardItem, &str>, &mut Window, &mut App) -> bool + 'static,
) -> InputBase {
    let state = state.clone();
    frame.capture_action(move |_: &Paste, window, cx| {
        if !state.read(cx).is_editable() || !state.read(cx).focus_handle(cx).is_focused(window) {
            return;
        }
        let clipboard = reader(cx);
        let consumed = match &clipboard {
            Ok(Some(item)) => handler(Ok(item), window, cx),
            Err(error) => handler(Err(error), window, cx),
            Ok(None) => false,
        };
        if consumed {
            cx.stop_propagation();
        }
    })
}

/// Call once after RiWork's appearance, settings, and UI text initialization.
pub fn init(cx: &mut App) {
    gpui_kit::init(cx);
    // Reuse Base's newline action and history. PressEnter intentionally reports
    // shift=true for Alt+Enter too, so is_submit rejects either newline gesture.
    cx.bind_keys(["alt-enter", "alt-shift-enter"].map(|key| {
        KeyBinding::new(
            key,
            Enter {
                secondary: false,
                shift: true,
            },
            Some("Input"),
        )
    }));
    sync_theme(cx);
    cx.observe_global::<theme::Appearance>(sync_theme).detach();
    cx.observe_global::<Settings>(sync_theme).detach();
    cx.observe_global::<ui_text::TerminalFontSize>(sync_theme)
        .detach();
}

fn sync_theme(cx: &mut App) {
    let palette = theme::palette(cx);
    let color = |value| Hsla::from(rgb(value));
    let colors = ColorTokens {
        background: color(palette.bg),
        foreground: color(palette.text),
        surface: color(palette.panel),
        surface_foreground: color(palette.text),
        primary: color(palette.cyan),
        primary_foreground: color(palette.bg),
        secondary: color(palette.panel_active),
        secondary_foreground: color(palette.text),
        muted: color(palette.panel),
        muted_foreground: color(palette.muted),
        accent: color(palette.cyan),
        accent_foreground: color(palette.bg),
        destructive: color(palette.gold),
        destructive_foreground: color(palette.bg),
        border: color(palette.divider),
        input: color(palette.divider),
        ring: color(palette.focus),
        selection: color(palette.cyan).alpha(0.4),
    };
    let kit = Theme::global_mut(cx);
    kit.appearance = if theme::is_dark(palette.bg) {
        ThemeAppearance::Dark
    } else {
        ThemeAppearance::Light
    };
    kit.tokens.colors = colors;
    kit.tokens.typography.sans = ui_text::ui_family();
    kit.tokens.typography.mono = ui_text::code_family();
}

#[cfg(test)]
mod tests;
