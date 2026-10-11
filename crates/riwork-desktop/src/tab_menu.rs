//! Shared strip operations. The menu owns keyboard focus while it is open.
use crate::{
    behavior_controls as behavior, controls, form_input, project_tabs, text_input, theme, ui_text,
};
use gpui::{
    Context, Entity, EventEmitter, FocusHandle, Focusable, Render, Subscription, Window, div,
    prelude::*, rgb,
};

#[derive(Clone, Debug)]
pub enum Event {
    Update(project_tabs::Update),
    Close,
    Cancel,
}
pub struct TabMenu {
    key: String,
    left: Option<project_tabs::Update>,
    right: Option<project_tabs::Update>,
    focus: FocusHandle,
    buttons: Vec<FocusHandle>,
    rename: bool,
    input: Entity<text_input::InputState>,
    _subscription: Subscription,
}
impl EventEmitter<Event> for TabMenu {}
impl TabMenu {
    pub fn new(
        entry: &project_tabs::Entry,
        left: Option<project_tabs::Update>,
        right: Option<project_tabs::Update>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = text_input::single_line(entry.title.clone(), "Tab title", window, cx);
        let subscription = cx.subscribe_in(&input, window, |menu, _, event, _, cx| {
            if menu.rename && text_input::is_submit(event, text_input::EnterBehavior::Submit) {
                menu.submit(cx);
            }
        });
        Self {
            key: entry.key.clone(),
            left,
            right,
            focus: cx.focus_handle(),
            buttons: (0..5).map(|_| cx.focus_handle()).collect(),
            rename: false,
            input,
            _subscription: subscription,
        }
    }
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.buttons[0].focus(window, cx);
    }
    fn submit(&self, cx: &mut Context<Self>) {
        cx.emit(Event::Update(project_tabs::Update::Rename {
            key: self.key.clone(),
            title: self.input.read(cx).value().to_string(),
        }));
    }
}
impl Render for TabMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        let panel = div()
            .id("shared-tab-menu")
            .role(gpui::Role::Menu)
            .aria_label("Tab actions")
            .track_focus(&self.focus)
            .w(ui_text::space(220.0))
            .p(ui_text::space(3.0))
            .bg(rgb(colors.panel_active))
            .border_1()
            .border_color(rgb(colors.magenta))
            .flex()
            .flex_col()
            .font_family(ui_text::ui_family())
            .text_size(ui_text::text(10.0))
            .text_color(rgb(colors.text))
            .map(|menu| controls::native(menu, |menu| controls::menu(menu, colors)))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(Event::Cancel)))
            .on_key_down(cx.listener(|menu, event: &gpui::KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "escape" => {
                        cx.emit(Event::Cancel);
                        cx.stop_propagation();
                    }
                    "up" | "down" if !menu.rename => {
                        let current = menu
                            .buttons
                            .iter()
                            .position(|f| f.is_focused(window))
                            .unwrap_or(0);
                        let step = if event.keystroke.key == "up" { 4 } else { 1 };
                        let mut next = (current + step) % 5;
                        while (next == 1 && menu.left.is_none())
                            || (next == 2 && menu.right.is_none())
                        {
                            next = (next + step) % 5;
                        }
                        menu.buttons[next].focus(window, cx);
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }));
        let panel = behavior::focus_scope(panel, "shared-tab-menu-scope", &self.focus);
        if self.rename {
            return panel
                .child(div().p(ui_text::space(5.0)).child(form_input::frame(
                    "shared-tab-rename-input",
                    &self.input,
                    false,
                    window,
                    cx,
                )))
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(ui_text::space(6.0))
                        .p(ui_text::space(5.0))
                        .child(
                            behavior::button(
                                "shared-tab-rename-cancel",
                                "Cancel",
                                controls::Button::Secondary,
                                colors,
                            )
                            .track_focus(&self.buttons[4])
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::Cancel))),
                        )
                        .child(
                            behavior::button(
                                "shared-tab-rename-save",
                                "Save title",
                                controls::Button::Primary,
                                colors,
                            )
                            .track_focus(&self.buttons[0])
                            .on_click(cx.listener(|menu, _, _, cx| menu.submit(cx))),
                        ),
                )
                .into_any_element();
        }
        let row = move |row: behavior::Button| {
            row.flex()
                .items_center()
                .px(ui_text::space(8.0))
                .py(ui_text::space(5.0))
                .border_1()
                .border_color(gpui::transparent_black())
                .focus_visible(move |style| style.border_color(rgb(colors.focus)))
                .hover(move |style| {
                    controls::hovered(style, controls::menu_row_hover(colors), |style| {
                        style.bg(rgb(colors.divider)).text_color(rgb(colors.cyan))
                    })
                })
                .map(|row| controls::native(row, |row| controls::menu_row(row, colors)))
        };
        panel
            .children(
                ["Rename…", "Move left", "Move right", "Close", "Cancel"]
                    .into_iter()
                    .enumerate()
                    .flat_map(|(index, label)| {
                        let disabled = (index == 1 && self.left.is_none())
                            || (index == 2 && self.right.is_none());
                        let item = row(behavior::button_content(
                            ("shared-tab-menu-action", index),
                            label,
                            label,
                        ))
                        .role(gpui::Role::MenuItem)
                        .track_focus(&self.buttons[index])
                        .disabled(disabled)
                        .when(disabled, |item| item.text_color(rgb(colors.muted)))
                        .on_click(cx.listener(move |menu, _, window, cx| match index {
                            0 => {
                                menu.rename = true;
                                menu.input.read(cx).focus_handle(cx).focus(window, cx);
                                cx.notify();
                            }
                            3 => cx.emit(Event::Close),
                            1 | 2 => {
                                if let Some(update) =
                                    if index == 1 { &menu.left } else { &menu.right }
                                {
                                    cx.emit(Event::Update(update.clone()));
                                }
                            }
                            _ => cx.emit(Event::Cancel),
                        }))
                        .into_any_element();
                        let separator =
                            matches!(index, 1 | 3 | 4).then(|| controls::menu_separator(colors));
                        separator.into_iter().chain([item])
                    }),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::test::TestWindowExt;
    #[gpui::test]
    fn keyboard_menu_moves_focus_and_rename_uses_persistent_input(cx: &mut gpui::TestAppContext) {
        let entry: project_tabs::Entry = serde_json::from_value(serde_json::json!({"key":format!("chat:{}",uuid::Uuid::new_v4()),"kind":"chat","title":"Original","status":"waiting","hidden":false,"worker":false,"order":0,"parent":null,"children":[],"child_count":0})).unwrap();
        let (window, menu) = crate::form_input::test_window(cx, move |window, cx| {
            TabMenu::new(&entry, None, None, window, cx)
        });
        crate::form_input::test_turn(cx, window, |window, cx| {
            menu.update(cx, |menu, cx| menu.focus(window, cx));
        });
        // Down skips the disabled moves to Close; up comes back to Rename….
        crate::form_input::test_turn(cx, window, |window, cx| window.press("down", cx));
        cx.update_window(window, |_, window, cx| {
            assert!(menu.read(cx).buttons[3].is_focused(window));
        })
        .unwrap();
        crate::form_input::test_turn(cx, window, |window, cx| window.press("up", cx));
        cx.update_window(window, |_, window, cx| {
            assert!(menu.read(cx).buttons[0].is_focused(window));
        })
        .unwrap();
        crate::form_input::test_turn(cx, window, |window, cx| {
            window.press("enter", cx);
        });
        cx.update_window(window, |_, window, cx| {
            assert!(menu.read(cx).rename);
            assert_eq!(menu.read(cx).input.read(cx).value(), "Original");
            assert!(
                menu.read(cx)
                    .input
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window)
            );
            window.remove_window();
        })
        .unwrap();
    }
}
