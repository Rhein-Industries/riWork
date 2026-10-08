//! Small Kit modal. Base owns the focus trap and Escape dismissal.
use crate::{
    behavior_controls as behavior,
    controls::{self, Button},
    settings::TabCloseBehavior,
    theme::{diff_colors, palette},
    ui_text,
};
use gpui::{Context, EventEmitter, FocusHandle, Render, Window, div, prelude::*, rgb};
pub struct TabCloseDialog {
    focus: FocusHandle,
    buttons: [FocusHandle; 3],
    handle: gpui_kit::base::DialogHandle,
    title: String,
}
impl EventEmitter<Option<TabCloseBehavior>> for TabCloseDialog {}
impl TabCloseDialog {
    pub fn new(title: String, cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            buttons: [cx.focus_handle(), cx.focus_handle(), cx.focus_handle()],
            handle: gpui_kit::base::DialogHandle::new(true),
            title,
        }
    }
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.buttons[1].focus(window, cx);
    }
}
impl Render for TabCloseDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = palette(cx);
        let error = diff_colors(cx).removed;
        let panel = div()
            .id("tab-close-dialog")
            // Pinned Base Dialog's focus-trap host omits its AX role.
            .role(gpui::Role::Dialog)
            .aria_label(format!("Close {}", self.title))
            .occlude()
            .w(ui_text::space(380.0))
            .max_w_full()
            .p(ui_text::space(18.0))
            .flex()
            .flex_col()
            .gap(ui_text::space(10.0))
            .font_family(ui_text::ui_family())
            .text_size(ui_text::text(10.0))
            .text_color(rgb(colors.text))
            .bg(rgb(colors.panel))
            .border_1()
            .border_color(rgb(colors.magenta))
            // Native: a sheet with rounded corners, a hairline and a shadow, as Hand off is.
            .map(|dialog| {
                controls::native(dialog, |dialog| {
                    dialog
                        .rounded(controls::radius(12.0))
                        .border_color(rgb(colors.divider))
                        .shadow_lg()
                })
            })
            .child(
                div()
                    .text_size(ui_text::text(13.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(format!("Close {}?", self.title)),
            )
            .child(
                div()
                    .text_color(rgb(colors.muted))
                    .child("Detach hides the tab and keeps the session running; reopen it from ＋ › Open a worker or shell. Exit stops it; chat history is kept."),
            )
            .child(
                div()
                    .pt(ui_text::space(6.0))
                    .flex()
                    .justify_end()
                    .gap(ui_text::space(8.0))
                    .children(
                        [
                            ("Cancel", None, Button::Secondary),
                            ("Exit", Some(TabCloseBehavior::Exit), Button::Secondary),
                            ("Detach", Some(TabCloseBehavior::Detach), Button::Primary),
                        ]
                        .into_iter()
                        .map(|(label, choice, kind)| {
                            let index = match choice {
                                None => 0,
                                Some(TabCloseBehavior::Detach) => 1,
                                Some(_) => 2,
                            };
                            behavior::button(("tab-close-action", index), label, kind, colors)
                                .track_focus(&self.buttons[index])
                                .when(choice == Some(TabCloseBehavior::Exit), |button| {
                                    button.text_color(rgb(error))
                                })
                                .on_click(cx.listener(move |_, _, _, cx| cx.emit(choice)))
                        }),
                    ),
            );
        let cancel = cx.listener(|_, _: &gpui::ClickEvent, _, cx| cx.emit(None));
        gpui_kit::base::Dialog::new(cx)
            .handle(self.handle.clone())
            .focus_handle(self.focus.clone())
            .close_on_backdrop_press(false)
            .on_ok(|_, _, _| false)
            .on_cancel(move |event, window, cx| {
                cancel(event, window, cx);
                true
            })
            .popup(panel)
    }
}
