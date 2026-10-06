//! Form-owned presentation, paste policies, and explicit draft replacements.
use crate::text_input::{self, InputBase, InputState};
use gpui::{App, Context, Entity, EntityInputHandler, Window, prelude::*};

/// Replace a domain draft intentionally from a callback without a Window.
/// Refresh/render never call this unless the domain value actually changed.
pub fn set_value<V: 'static>(state: &Entity<InputState>, value: String, cx: &mut Context<V>) {
    let state = state.clone();
    let owner = cx.entity_id();
    cx.defer(move |app| {
        app.with_window(owner, |window, app| {
            state.update(app, |input, cx| input.set_value(value, window, cx));
        });
    });
}

pub fn is_composing(state: &Entity<InputState>, window: &mut Window, cx: &mut App) -> bool {
    state.update(cx, |state, cx| {
        state.marked_text_range(window, cx).is_some()
    })
}

#[derive(Clone, Copy)]
enum PastePolicy {
    Fold,
    Spaces,
}

fn normalize_paste(text: &str, policy: PastePolicy) -> String {
    match policy {
        PastePolicy::Fold => crate::project_settings::single_line(text),
        PastePolicy::Spaces => text.replace(['\r', '\n'], " "),
    }
}

fn paste_policy(frame: InputBase, state: &Entity<InputState>, policy: PastePolicy) -> InputBase {
    let input = state.clone();
    text_input::on_paste(frame, state, move |clipboard, window, cx| {
        let Some(text) = clipboard.text() else {
            return false;
        };
        let normalized = normalize_paste(&text, policy);
        if text == normalized {
            return false;
        }
        input.update(cx, |input, cx| input.replace(normalized, window, cx));
        true
    })
}

/// Creator/Files retain Base's CR/LF removal policy. Other form owners use frame.
pub fn plain_frame(
    id: impl Into<gpui::ElementId>,
    state: &Entity<InputState>,
    disabled: bool,
    window: &Window,
    cx: &mut App,
) -> InputBase {
    // Only presentation changes on pending/result transitions; values/history never do.
    if state.read(cx).presentation().is_disabled() != disabled {
        state.update(cx, |state, cx| state.set_disabled(disabled, cx));
    }
    text_input::input(id, state, window, cx).when(crate::ui_text::is_native(), |frame| {
        frame.rounded(crate::controls::radius(crate::controls::FIELD_RADIUS))
    })
}

pub fn frame(
    id: impl Into<gpui::ElementId>,
    state: &Entity<InputState>,
    disabled: bool,
    window: &Window,
    cx: &mut App,
) -> InputBase {
    let frame = plain_frame(id, state, disabled, window, cx);
    paste_policy(frame, state, PastePolicy::Fold)
}

pub fn search_frame(
    id: impl Into<gpui::ElementId>,
    state: &Entity<InputState>,
    window: &Window,
    cx: &mut App,
) -> InputBase {
    let colors = crate::theme::palette(cx);
    let frame = plain_frame(id, state, false, window, cx)
        .when(crate::ui_text::is_native(), |frame| {
            frame.rounded_full().bg(gpui::rgb(colors.panel_active))
        });
    paste_policy(frame, state, PastePolicy::Spaces)
}

#[cfg(test)]
pub(crate) fn test_window<V: gpui::Render + 'static>(
    cx: &mut gpui::TestAppContext,
    build: impl FnOnce(&mut Window, &mut Context<V>) -> V + 'static,
) -> (gpui::WindowHandle<V>, Entity<V>) {
    let (handle, view) = cx.update(|app| {
        app.set_global(crate::settings::Settings::default());
        app.set_global(crate::theme::Appearance {
            selected: crate::theme::ThemeChoice::RiWork,
            palette: crate::theme::Palette::RIWORK,
            terminal: None,
            ghostty: None,
            error: None,
        });
        text_input::init(app);
        let handle = app
            .open_window(gpui::WindowOptions::default(), |window, app| {
                app.new(|cx| build(window, cx))
            })
            .unwrap();
        let view = handle.update(app, |_, _, cx| cx.entity()).unwrap();
        (handle, view)
    });
    cx.update_window(handle.into(), |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();
    (handle, view)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_form_and_workspace_paste_policies_remain_distinct() {
        let pasted = "\r\nA🦀\r\n中\u{2028}e\u{301}\n";
        assert_eq!(
            normalize_paste(pasted, PastePolicy::Fold),
            "A🦀 中 e\u{301}"
        );
        assert_eq!(
            normalize_paste(pasted, PastePolicy::Spaces),
            "  A🦀  中\u{2028}e\u{301} "
        );
    }
}

/// An external metadata refresh must not overwrite a newly edited or marked draft.
pub fn refresh_value<V: 'static>(
    state: &Entity<InputState>,
    previous: String,
    value: String,
    cx: &mut Context<V>,
) {
    let state = state.clone();
    let owner = cx.entity_id();
    cx.defer(move |app| {
        app.with_window(owner, |window, app| {
            state.update(app, |input, cx| {
                if input.value().trim() == previous && input.marked_text_range(window, cx).is_none()
                {
                    input.set_value(value, window, cx);
                }
            });
        });
    });
}

/// Presentation-only changes do not replace the draft or its editing history.
pub fn placeholder<V: 'static>(
    state: &Entity<InputState>,
    value: &'static str,
    cx: &mut Context<V>,
) {
    if state.read(cx).placeholder().as_ref() == value {
        return;
    }
    let state = state.clone();
    let owner = cx.entity_id();
    cx.defer(move |app| {
        app.with_window(owner, |window, app| {
            state.update(app, |input, cx| input.set_placeholder(value, window, cx));
        });
    });
}
