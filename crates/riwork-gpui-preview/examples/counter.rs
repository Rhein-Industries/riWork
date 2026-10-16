//! Run from RiWork with: cargo run -p riwork-gpui-preview --example counter
//! Exercise real offscreen Metal rendering without a window: add -- --self-test

use anyhow::Result;
use gpui::{
    App, Bounds, Context, ElementInputHandler, Entity, EntityInputHandler, FocusHandle, Pixels,
    Point, Render, UTF16Selection, Window, canvas, div, prelude::*, rgb,
};
use riwork_gpui_preview::{HeadlessPreview, PreviewInput, PreviewMouseButton};
use std::ops::Range;

struct Counter {
    value: usize,
    text: String,
    focus: FocusHandle,
}

impl Counter {
    fn build(window: &mut Window, cx: &mut App) -> Entity<Self> {
        let entity = cx.new(|cx| Self {
            value: 0,
            text: String::new(),
            focus: cx.focus_handle(),
        });
        entity.update(cx, |view, cx| view.focus.focus(window, cx));
        entity
    }
}

impl Render for Counter {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let focus = self.focus.clone();
        div()
            .size_full()
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .bg(rgb(0x121826))
            .text_color(rgb(0xe8eef8))
            .font_family("Helvetica")
            .text_size(gpui::px(16.0))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|view, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "backspace" {
                    view.text.pop();
                    cx.notify();
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .text_size(gpui::px(24.0))
                    .child("Windowless GPUI preview"),
            )
            .child("This process has no desktop window or Dock icon.")
            .child(
                div()
                    .id("increment")
                    .w(gpui::px(200.0))
                    .h(gpui::px(44.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(rgb(0x3e72e6))
                    .rounded_md()
                    .cursor_pointer()
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.value += 1;
                        cx.notify();
                    }))
                    .child(format!("Increment · {}", self.value)),
            )
            .child(
                div()
                    .id("text")
                    .relative()
                    .w_full()
                    .h(gpui::px(48.0))
                    .p_3()
                    .border_1()
                    .border_color(rgb(0x60718d))
                    .rounded_md()
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(|view, _, window, cx| view.focus.focus(window, cx)),
                    )
                    .child(if self.text.is_empty() {
                        "Type here…".to_owned()
                    } else {
                        format!("{}│", self.text)
                    })
                    .child(
                        canvas(
                            |_, _, _| (),
                            move |bounds, _, window, cx| {
                                window.handle_input(
                                    &focus,
                                    ElementInputHandler::new(bounds, entity),
                                    cx,
                                );
                            },
                        )
                        .absolute()
                        .size_full(),
                    ),
            )
            .child("Try clicking, typing Unicode, Backspace, and resizing the tab.")
    }
}

impl EntityInputHandler for Counter {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let utf16: Vec<_> = self.text.encode_utf16().collect();
        let start = range.start.min(utf16.len());
        let end = range.end.clamp(start, utf16.len());
        *actual = Some(start..end);
        Some(String::from_utf16_lossy(&utf16[start..end]))
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self.text.encode_utf16().count();
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        None
    }
    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {}
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(range) = range {
            let mut utf16: Vec<_> = self.text.encode_utf16().collect();
            let start = range.start.min(utf16.len());
            let end = range.end.clamp(start, utf16.len());
            utf16.splice(start..end, text.encode_utf16());
            self.text = String::from_utf16_lossy(&utf16);
        } else {
            self.text.push_str(text);
        }
        cx.notify();
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_text_in_range(range, text, window, cx);
    }
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(bounds)
    }
    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.text.encode_utf16().count())
    }
}

fn self_test() -> Result<()> {
    let mut preview = HeadlessPreview::new(640, 360, Counter::build)?;
    let before = preview.frame()?;
    assert_eq!(before.dimensions(), (640, 360));
    assert!(
        before.pixels().any(|pixel| pixel.0[..3] != [18, 24, 38]),
        "GPU frame must contain rendered text and controls"
    );
    preview.dispatch(PreviewInput::Click {
        x: 80.0,
        y: 132.0,
        button: PreviewMouseButton::Left,
    })?;
    let count = preview.update(|view, _, _| view.value)?;
    assert_eq!(
        count, 1,
        "pointer input must activate the actual GPUI button"
    );
    preview.dispatch(PreviewInput::Text {
        text: "Hello 👋 世界".into(),
    })?;
    assert_eq!(
        preview.update(|view, _, _| view.text.clone())?,
        "Hello 👋 世界"
    );
    preview.dispatch(PreviewInput::Key {
        key: "backspace".into(),
    })?;
    assert_eq!(
        preview.update(|view, _, _| view.text.clone())?,
        "Hello 👋 世"
    );
    preview.dispatch(PreviewInput::Resize {
        width: 800,
        height: 450,
    })?;
    let after = preview.frame()?;
    assert_eq!(after.dimensions(), (800, 450));
    assert_ne!(before.as_raw(), after.as_raw());
    if let Some(path) = std::env::var_os("RIWORK_PREVIEW_SMOKE_PNG") {
        after.save(path)?;
    }
    println!(
        "PASS: real GPU rendering; counter click; committed Unicode text; Backspace; resize; no native window"
    );
    Ok(())
}

fn main() -> Result<()> {
    if std::env::args().any(|arg| arg == "--self-test") {
        self_test()
    } else {
        riwork_gpui_preview::run(Counter::build)
    }
}
