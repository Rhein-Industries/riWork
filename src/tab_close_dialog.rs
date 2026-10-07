//! Small Kit modal. Base owns the focus trap and Escape dismissal.
use gpui::{Context, EventEmitter, FocusHandle, Render, Window, div, prelude::*, rgb};
use crate::{behavior_controls as behavior, settings::TabCloseBehavior, theme::palette, ui_text};
pub struct TabCloseDialog { focus: FocusHandle, buttons: [FocusHandle; 3], handle: gpui_kit::base::DialogHandle, title: String }
impl EventEmitter<Option<TabCloseBehavior>> for TabCloseDialog {}
impl TabCloseDialog {
    pub fn new(title: String, cx: &mut Context<Self>) -> Self {
        Self { focus: cx.focus_handle(), buttons: [cx.focus_handle(), cx.focus_handle(), cx.focus_handle()], handle: gpui_kit::base::DialogHandle::new(true), title }
    }
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) { self.buttons[1].focus(window, cx); }
}
impl Render for TabCloseDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = palette(cx);
        let panel = div().w(ui_text::space(360.0)).p(ui_text::space(20.0)).bg(rgb(colors.panel)).border_1().border_color(rgb(colors.divider))
            .flex().flex_col().gap(ui_text::space(12.0))
            .child(format!("Close {}?", self.title))
            .child("Detach keeps the session running. Exit stops it and keeps chat history.")
            .child(div().flex().justify_end().gap(ui_text::space(8.0)).children(
                [("Cancel", None), ("Detach", Some(TabCloseBehavior::Detach)), ("Exit", Some(TabCloseBehavior::Exit))].into_iter().enumerate().map(|(index, (label, choice))| {
                    behavior::button_content(("tab-close-action", index), label, div().child(label))
                        .track_focus(&self.buttons[index]).p(ui_text::space(6.0))
                        .on_click(cx.listener(move |_, _, _, cx| cx.emit(choice)))
                })
            ));
        let cancel = cx.listener(|_, _: &gpui::ClickEvent, _, cx| cx.emit(None));
        gpui_kit::base::Dialog::new(cx).handle(self.handle.clone()).focus_handle(self.focus.clone())
            .close_on_backdrop_press(false).on_ok(|_, _, _| false)
            .on_cancel(move |event, window, cx| { cancel(event, window, cx); true }).popup(panel)
    }
}
