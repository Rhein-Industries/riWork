//! Form-owned presentation, paste policies, and explicit draft replacements.
use crate::text_input::{self, InputBase, InputState};
use gpui::{App, Entity, EntityInputHandler, Focusable, Window, prelude::*};

/// Intentional domain replacements happen synchronously in the owning callback.
pub fn set_value(state: &Entity<InputState>, value: String, window: &mut Window, cx: &mut App) {
    state.update(cx, |input, cx| input.set_value(value, window, cx));
}

/// An untouched metadata field can follow external changes; any user edit (even
/// whitespace or edit/undo ABA) protects its draft until an explicit save.
pub fn refresh_unedited(
    state: &Entity<InputState>,
    previous: &str,
    value: String,
    touched: bool,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    if touched || state.read(cx).value().as_ref() != previous || is_composing(state, window, cx) {
        return false;
    }
    if state.read(cx).value().as_ref() != value.as_str() {
        set_value(state, value, window, cx);
    }
    true
}

/// Mirror only an acknowledged, inactive, unmarked draft. Preserve selection,
/// scrolling and undo history. The owner must ignore the one emitted Change;
/// that event is a mirror update, not a new canonical query or user edit.
pub fn sync_value(
    state: &Entity<InputState>,
    previous: &str,
    value: &str,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let input = state.read(cx);
    if input.value().as_ref() == value
        || input.value().as_ref() != previous
        || input.focus_handle(cx).is_focused(window)
        || is_composing(state, window, cx)
    {
        return false;
    }
    state.update(cx, |input, cx| {
        let selection = input.selected_range();
        let scroll = input.scroll_offset();
        input.replace_all(value.to_owned(), window, cx);
        input.set_selected_range(selection, cx);
        input.set_scroll_offset(scroll, cx);
    });
    true
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
    // The search field around it is the one box: its fill, edge and focus ring. The text box
    // only holds the text, with no edge, fill or padding of its own.
    let frame = plain_frame(id, state, false, window, cx)
        .flex_1()
        .border_0()
        .rounded(gpui::px(0.))
        .bg(gpui::transparent_black())
        .styles(|styles| {
            styles.focused(|style| {
                style
                    .border_color(gpui::transparent_black())
                    .bg(gpui::transparent_black())
            })
        })
        .px(gpui::px(0.))
        .py(gpui::px(0.));
    paste_policy(frame, state, PastePolicy::Spaces)
}

#[cfg(test)]
pub(crate) fn test_window<V: gpui::Render + 'static>(
    cx: &mut gpui::TestAppContext,
    build: impl FnOnce(&mut Window, &mut gpui::Context<V>) -> V + 'static,
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

    struct DraftFixture {
        state: Entity<InputState>,
        touched: bool,
        _subscription: gpui::Subscription,
    }
    impl gpui::Render for DraftFixture {
        fn render(
            &mut self,
            window: &mut Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            frame("draft", &self.state, false, window, cx)
        }
    }

    #[gpui::test]
    fn synchronous_replacements_and_refresh_preserve_newer_whitespace_and_aba_drafts(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui_kit::test::TestWindowExt;
        let (handle, owner) = test_window(cx, |window, cx| {
            let state = text_input::single_line("A", "Draft", window, cx);
            let subscription = cx.subscribe_in(
                &state,
                window,
                |owner: &mut DraftFixture, _, event, _, _| {
                    if matches!(event, text_input::InputEvent::Change) {
                        owner.touched = true;
                    }
                },
            );
            DraftFixture {
                state,
                touched: false,
                _subscription: subscription,
            }
        });
        let state = cx
            .update_window(handle.into(), |_, window, app| {
                let state = owner.read(app).state.clone();
                assert!(refresh_unedited(
                    &state,
                    "A",
                    "metadata B".into(),
                    false,
                    window,
                    app
                ));
                assert!(refresh_unedited(
                    &state,
                    "metadata B",
                    "A".into(),
                    false,
                    window,
                    app
                ));
                set_value(&state, "saved A".into(), window, app);
                state.update(app, |input, cx| input.replace_all("newer B", window, cx));
                state
            })
            .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, app| {
            assert_eq!(state.read(app).value(), "newer B");
            assert!(!refresh_unedited(
                &state,
                "saved A",
                "stale metadata".into(),
                owner.read(app).touched,
                window,
                app
            ));
            state.update(app, |input, cx| input.replace_all(" A ", window, cx));
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, app| {
            assert!(!refresh_unedited(
                &state,
                "A",
                "metadata".into(),
                false,
                window,
                app
            ));
            set_value(&state, "A".into(), window, app);
            window.render_frame(app);
            state.read(app).focus_handle(app).focus(window, app);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, app| {
            window.render_frame(app);
            window.input("B", app);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, app| {
            window.press("cmd-z", app);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, app| {
            assert_eq!(state.read(app).value(), "A");
            assert!(owner.read(app).touched);
            assert!(!refresh_unedited(
                &state,
                "A",
                "metadata".into(),
                owner.read(app).touched,
                window,
                app
            ));
        })
        .unwrap();
    }

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

/// Presentation-only changes do not replace the draft or its editing history.
pub fn placeholder(
    state: &Entity<InputState>,
    value: &'static str,
    window: &mut Window,
    cx: &mut App,
) {
    if state.read(cx).presentation().placeholder().as_ref() != value {
        state.update(cx, |input, cx| input.set_placeholder(value, window, cx));
    }
}
