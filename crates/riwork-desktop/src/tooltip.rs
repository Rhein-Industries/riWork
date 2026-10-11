//! Hover hints in their own small window, so they stay in front of the terminals.
//!
//! Every terminal is a native child view that AppKit draws above GPUI's own
//! rendering, so a hint drawn by GPUI (`.tooltip(..)`) vanishes wherever it
//! overlaps one. A hint here is a `WindowKind::PopUp` window instead: a
//! non-activating panel one level above the window, so it is never covered.
//! (`WindowKind::AnchoredPopup` would fit better, but GPUI's macOS platform
//! rejects it with `PopupNotSupportedError`.)
//!
//! An element gets a hint by adding `tooltip::anchor(text, look)` as a child. The
//! anchor fills its parent, watches the pointer and, after `SHOW_DELAY`, opens the
//! window beside the parent. At most one hint exists at a time.

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    time::Duration,
};

use gpui::{
    AnyWindowHandle, App, Bounds, Context, DispatchPhase, Global, Hitbox, HitboxBehavior,
    IntoElement, MouseDownEvent, MouseExitEvent, MouseMoveEvent, Pixels, Point, Render,
    ScrollWheelEvent, SharedString, Size, Task, TextStyle, Window, WindowBackgroundAppearance,
    WindowBounds, WindowKind, WindowOptions, canvas, div, point, prelude::*, px, rgb, size,
};

use crate::{theme, ui_text};

/// GPUI's own delay before a tooltip shows.
const SHOW_DELAY: Duration = Duration::from_millis(500);
/// How long a hint stays up at most. macOS sends no exit event when the pointer
/// leaves a window, so a hint on an anchor at the window's edge could otherwise
/// stay over the desktop or another app.
const VISIBLE_FOR: Duration = Duration::from_secs(8);
/// Transparent border around the hint. The window has the system's rounded corners
/// and a shadow; the margin keeps both off the hint itself.
const MARGIN: f32 = 8.0;

/// How a hint looks. Each keeps the styling of the GPUI tooltip it replaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Look {
    /// Pane chrome: tabs and the pane toolbar.
    Pane,
    /// Panel and settings controls, in the interface face of their labels.
    Control,
    /// Status bar items and their settings.
    Status,
}

impl Look {
    /// Grows with the interface text size, like the text inside.
    fn padding(self) -> (f32, f32) {
        let (x, y) = match self {
            Self::Pane => (9.0, 6.0),
            Self::Control => (8.0, 5.0),
            Self::Status => (8.0, 6.0),
        };
        (ui_text::space_f32(x), ui_text::space_f32(y))
    }

    fn text_style(self) -> TextStyle {
        let mut style = TextStyle::default();
        let (family, font_size) = match self {
            Self::Pane => (None, 11.0),
            Self::Control => (Some(ui_text::ui_family()), 10.0),
            Self::Status => (None, 10.0),
        };
        if let Some(family) = family {
            style.font_family = family;
        }
        style.font_size = ui_text::text(font_size).into();
        style
    }
}

/// The hint's box around one line of text of the given size.
fn box_size(look: Look, text: Size<Pixels>) -> Size<Pixels> {
    let (x, y) = look.padding();
    // The 1 px border, and a pixel of slack so rounding never wraps the text.
    size(
        (text.width + px(2.0 * x + 3.0)).ceil(),
        (text.height + px(2.0 * y + 2.0)).ceil(),
    )
}

fn measure(text: &SharedString, look: Look, window: &Window) -> Size<Pixels> {
    let style = look.text_style();
    let rem = window.rem_size();
    let font_size = style.font_size.to_pixels(rem);
    let line =
        window
            .text_system()
            .shape_line(text.clone(), font_size, &[style.to_run(text.len())], None);
    size(line.width, style.line_height_in_pixels(rem))
}

/// Which side of the anchor a hint took.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    Below,
    Above,
    Right,
    Left,
    /// Nothing fit; the hint is pushed inside the area and may cover the anchor.
    Squeezed,
}

/// Where a hint of `size` goes around `anchor` without leaving `area`. Below is
/// preferred, then above, then the right and the left; a hint above or below is
/// centred on `focus_x`, one beside the anchor on its middle. It never overlaps the
/// anchor unless nothing fits.
pub(crate) fn place(
    anchor: Bounds<Pixels>,
    focus_x: Pixels,
    size: Size<Pixels>,
    area: Bounds<Pixels>,
) -> (Point<Pixels>, Side) {
    let clamp_x = |x: Pixels| x.min(area.right() - size.width).max(area.left());
    let clamp_y = |y: Pixels| y.min(area.bottom() - size.height).max(area.top());
    let across = clamp_x(focus_x - size.width / 2.0);
    let along = clamp_y(anchor.center().y - size.height / 2.0);
    let below = anchor.bottom();
    let above = anchor.top() - size.height;
    if below + size.height <= area.bottom() {
        (point(across, below), Side::Below)
    } else if above >= area.top() {
        (point(across, above), Side::Above)
    } else if anchor.right() + size.width <= area.right() {
        (point(anchor.right(), along), Side::Right)
    } else if anchor.left() - size.width >= area.left() {
        (point(anchor.left() - size.width, along), Side::Left)
    } else {
        (point(across, clamp_y(below)), Side::Squeezed)
    }
}

/// The screen position of a point in the window's content. `frame` is the window's
/// frame and `content` the size GPUI lays out in; a window with a title bar of its
/// own has a frame taller than its content, with the content at the bottom.
fn screen_point(
    frame: Bounds<Pixels>,
    content: Size<Pixels>,
    local: Point<Pixels>,
) -> Point<Pixels> {
    frame.origin + point(px(0.0), frame.size.height - content.height) + local
}

/// One element that has a hint, as the state machine knows it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Anchor {
    window: u64,
    /// Identifies the element from frame to frame: its window, text and place.
    key: u64,
    bounds: Bounds<Pixels>,
}

impl Anchor {
    pub(crate) fn new(window: u64, text: &str, bounds: Bounds<Pixels>) -> Self {
        let mut hasher = DefaultHasher::new();
        window.hash(&mut hasher);
        text.hash(&mut hasher);
        for value in [
            bounds.origin.x,
            bounds.origin.y,
            bounds.size.width,
            bounds.size.height,
        ] {
            value.as_f32().to_bits().hash(&mut hasher);
        }
        Self {
            window,
            key: hasher.finish(),
            bounds,
        }
    }

    fn area(&self) -> f32 {
        self.bounds.size.width.as_f32() * self.bounds.size.height.as_f32()
    }
}

#[derive(Debug)]
enum Phase<H> {
    Idle,
    /// The pointer rests on the anchor; the hint shows when the timer of this
    /// generation fires.
    Waiting(Anchor),
    Shown(Anchor, H),
}

impl<H> Phase<H> {
    fn anchor(&self) -> Option<&Anchor> {
        match self {
            Self::Idle => None,
            Self::Waiting(anchor) | Self::Shown(anchor, _) => Some(anchor),
        }
    }
}

/// What to do with the show timer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Timer {
    #[default]
    Keep,
    Stop,
    /// Wait, then ask `Machine::due` with this generation.
    Start(u64),
}

/// What the caller must do after an input: close a hint window, and what to do with
/// the timer.
#[derive(Debug, PartialEq)]
pub(crate) struct Step<H> {
    pub close: Option<H>,
    pub timer: Timer,
}

impl<H> Default for Step<H> {
    fn default() -> Self {
        Self {
            close: None,
            timer: Timer::Keep,
        }
    }
}

/// Which anchor has the pointer and whether its hint is up. It knows nothing of
/// windows: `H` is the handle of the hint window, and every handle it holds is
/// handed back through `Step::close` or `shown` before it is forgotten, so the
/// caller cannot leak one.
#[derive(Debug)]
pub(crate) struct Machine<H> {
    phase: Phase<H>,
    /// Bumped on every change, so a timer that lost its turn does nothing.
    generation: u64,
}

impl<H> Default for Machine<H> {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            generation: 0,
        }
    }
}

impl<H> Machine<H> {
    /// An anchor learned whether the pointer is on it.
    pub(crate) fn pointer(&mut self, anchor: Anchor, on: bool) -> Step<H> {
        let current = self.phase.anchor().copied();
        if !on {
            return match current {
                Some(current) if current.key == anchor.key => self.hide(),
                _ => Step::default(),
            };
        }
        match current {
            Some(current) if current.key == anchor.key => return Step::default(),
            // Anchors nest (a folder row holds its buttons); the smaller one wins.
            Some(current) if current.window == anchor.window && current.area() < anchor.area() => {
                return Step::default();
            }
            _ => {}
        }
        let close = self.take_popup();
        self.generation += 1;
        self.phase = Phase::Waiting(anchor);
        Step {
            close,
            timer: Timer::Start(self.generation),
        }
    }

    /// The pointer moved to `at` in `window`, which may be nowhere near the current
    /// anchor even when that anchor is gone and can no longer say so itself.
    pub(crate) fn moved(&mut self, window: u64, at: Point<Pixels>) -> Step<H> {
        match self.phase.anchor() {
            Some(anchor) if anchor.window != window || !anchor.bounds.contains(&at) => self.hide(),
            _ => Step::default(),
        }
    }

    pub(crate) fn hide(&mut self) -> Step<H> {
        if matches!(self.phase, Phase::Idle) {
            return Step::default();
        }
        let close = self.take_popup();
        self.generation += 1;
        self.phase = Phase::Idle;
        Step {
            close,
            timer: Timer::Stop,
        }
    }

    /// A window closed; its anchors are gone with it.
    pub(crate) fn window_closed(&mut self, window: u64) -> Step<H> {
        match self.phase.anchor() {
            Some(anchor) if anchor.window == window => self.hide(),
            _ => Step::default(),
        }
    }

    /// The hint of `generation` has been up long enough.
    pub(crate) fn expire(&mut self, generation: u64) -> Step<H> {
        match self.phase {
            Phase::Shown(..) if self.generation == generation => self.hide(),
            _ => Step::default(),
        }
    }

    /// The timer of `generation` fired: the anchor to show a hint for, if it is
    /// still the one waited on.
    pub(crate) fn due(&self, generation: u64) -> Option<Anchor> {
        match self.phase {
            Phase::Waiting(anchor) if self.generation == generation => Some(anchor),
            _ => None,
        }
    }

    /// The hint window for `anchor` is open. Hands it back when nobody wants it any
    /// more, and the caller must close it.
    pub(crate) fn shown(&mut self, anchor: &Anchor, popup: H) -> Option<H> {
        match self.phase {
            Phase::Waiting(waiting) if waiting.key == anchor.key => {
                self.phase = Phase::Shown(waiting, popup);
                None
            }
            _ => Some(popup),
        }
    }

    fn take_popup(&mut self) -> Option<H> {
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Shown(_, popup) => Some(popup),
            _ => None,
        }
    }
}

#[derive(Default)]
struct Tooltips {
    machine: Machine<AnyWindowHandle>,
    /// The pending show; dropping it cancels the wait.
    timer: Option<Task<()>>,
}

impl Global for Tooltips {}

pub(crate) fn init(cx: &mut App) {
    // A key press means the user is typing, not reading.
    cx.intercept_keystrokes(|_, _, cx| hide(cx)).detach();
    // Closed before the caller's own handler counts the windows that are left.
    cx.on_window_closed(|cx, window| {
        let step = cx
            .default_global::<Tooltips>()
            .machine
            .window_closed(window.as_u64());
        finish(step, cx);
    })
    .detach();
}

/// Whether `handle` is a hint window, which no count of the app's windows should
/// include.
pub(crate) fn is_popup(handle: &AnyWindowHandle) -> bool {
    handle.downcast::<Hint>().is_some()
}

/// Takes the hint down, and forgets the one that is being waited for.
pub(crate) fn hide(cx: &mut App) {
    let step = cx.default_global::<Tooltips>().machine.hide();
    finish(step, cx);
}

fn expire(generation: u64, cx: &mut App) {
    let step = cx.default_global::<Tooltips>().machine.expire(generation);
    finish(step, cx);
}

fn finish(step: Step<AnyWindowHandle>, cx: &mut App) {
    if let Some(popup) = step.close {
        // An error means the window is already gone, or is the one running this.
        popup.update(cx, |_, window, _| window.remove_window()).ok();
    }
    if step.timer == Timer::Stop {
        cx.default_global::<Tooltips>().timer = None;
    }
}

/// A hint for the element this is a child of. Add it last: it covers its parent
/// with an invisible hitbox and paints nothing.
pub(crate) fn anchor(text: impl Into<SharedString>, look: Look) -> impl IntoElement {
    let text: SharedString = text.into();
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |bounds, hitbox, window, _| {
            let anchor = Anchor::new(window.window_handle().window_id().as_u64(), &text, bounds);
            listen(anchor, hitbox, text, look, window);
        },
    )
    .absolute()
    .inset_0()
}

fn listen(anchor: Anchor, hitbox: Hitbox, text: SharedString, look: Look, window: &mut Window) {
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
        if phase != DispatchPhase::Capture {
            return;
        }
        // A pressed button is a drag or a selection, not a reader.
        let on =
            event.pressed_button.is_none() && !cx.has_active_drag() && hitbox.is_hovered(window);
        let step = cx.default_global::<Tooltips>().machine.pointer(anchor, on);
        let timer = step.timer;
        finish(step, cx);
        if let Timer::Start(generation) = timer {
            let text = text.clone();
            let task = window.spawn(cx, async move |cx| {
                cx.background_executor().timer(SHOW_DELAY).await;
                cx.update(|window, cx| show(generation, &text, look, window, cx))
                    .ok();
                // Still this task if the hint went up; anything that hides it ends it.
                cx.background_executor().timer(VISIBLE_FOR).await;
                cx.update(|_, cx| expire(generation, cx)).ok();
            });
            cx.default_global::<Tooltips>().timer = Some(task);
        }
        let step = cx
            .default_global::<Tooltips>()
            .machine
            .moved(anchor.window, event.position);
        finish(step, cx);
    });
    // Anything else the pointer does means the reader moved on. Capture, so an
    // element that stops the event still dismisses the hint.
    window.on_mouse_event(|_: &MouseDownEvent, phase, _, cx| {
        if phase == DispatchPhase::Capture {
            hide(cx);
        }
    });
    window.on_mouse_event(|_: &ScrollWheelEvent, phase, _, cx| {
        if phase == DispatchPhase::Capture {
            hide(cx);
        }
    });
    // The pointer left the window without a last move.
    window.on_mouse_event(|_: &MouseExitEvent, phase, _, cx| {
        if phase == DispatchPhase::Capture {
            hide(cx);
        }
    });
}

fn show(generation: u64, text: &SharedString, look: Look, window: &mut Window, cx: &mut App) {
    let Some(anchor) = cx.default_global::<Tooltips>().machine.due(generation) else {
        return;
    };
    let mouse = window.mouse_position();
    let content = window.viewport_size();
    let outer = {
        let inner = box_size(look, measure(text, look, window));
        size(
            inner.width + px(2.0 * MARGIN),
            inner.height + px(2.0 * MARGIN),
        )
    };
    // The hint may cover the window's edge up to its own margin.
    let area = Bounds::new(
        point(px(-MARGIN), px(-MARGIN)),
        size(
            content.width + px(2.0 * MARGIN),
            content.height + px(2.0 * MARGIN),
        ),
    );
    let focus_x = mouse.x.max(anchor.bounds.left()).min(anchor.bounds.right());
    let (origin, _) = place(anchor.bounds, focus_x, outer, area);
    // The panel is not the key window, but a click on it would make it one, so it
    // is never put where the pointer is.
    let covered = Bounds::new(origin, outer).contains(&mouse);
    if !window.is_window_active() || !anchor.bounds.contains(&mouse) || covered {
        hide(cx);
        return;
    }
    let origin = screen_point(window.bounds(), content, origin);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(origin.x.round(), origin.y.round()),
            outer,
        ))),
        titlebar: None,
        // Shown but not focused: the panel is ordered front without becoming key,
        // so the terminal keeps the keyboard.
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        display_id: window.display(cx).map(|display| display.id()),
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    };
    let text = text.clone();
    let opened = cx.open_window(options, |window, cx| {
        // Native draws the hint as one rounded panel; the window around it must show
        // nothing of its own.
        if ui_text::is_native() {
            bare_window(window);
        }
        cx.new(|cx| {
            // A click on the panel makes it the key window, and the terminal would
            // stop getting keys. Give the key back by going away.
            cx.observe_window_activation(window, |_, window, _| {
                if window.is_window_active() {
                    window.remove_window();
                }
            })
            .detach();
            Hint { text, look }
        })
    });
    match opened {
        Ok(handle) => {
            let stale = cx
                .default_global::<Tooltips>()
                .machine
                .shown(&anchor, handle.into());
            if let Some(stale) = stale {
                finish(
                    Step {
                        close: Some(stale),
                        timer: Timer::Keep,
                    },
                    cx,
                );
            }
        }
        Err(_) => hide(cx),
    }
}

/// Strip a hint window down to its content. GPUI opens a pop-up as a titled panel with a
/// full-size content view, so AppKit gives it a window's frame: the large rounded corners,
/// the window shadow and, on macOS 26, the glass material of a titled window, all around
/// the hint's own box, which then reads as a rectangle inside a second surface. A
/// borderless panel without the system shadow leaves only what the hint paints. The
/// non-activating flag is kept, so the hint still never takes the keyboard.
fn bare_window(window: &Window) {
    use objc2::{msg_send, runtime::AnyObject};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };
    /// `NSWindowStyleMaskBorderless | NSWindowStyleMaskNonactivatingPanel`.
    const BORDERLESS_NONACTIVATING: usize = 1 << 7;
    // SAFETY: the handle is the window's NSView, alive as long as the window, and windows
    // are made and used on the main thread, where this runs. Every selector is one NSWindow
    // has; the style mask is an NSUInteger and the shadow flag a BOOL.
    unsafe {
        let view = handle.ns_view.as_ptr().cast::<AnyObject>();
        let ns_window: *mut AnyObject = msg_send![view, window];
        if ns_window.is_null() {
            return;
        }
        let _: () = msg_send![ns_window, setStyleMask: BORDERLESS_NONACTIVATING];
        let _: () = msg_send![ns_window, setHasShadow: false];
    }
}

/// A hint's corner radius under Native, as macOS rounds a help tag.
const NATIVE_RADIUS: f32 = 6.0;

/// The content of a hint window.
struct Hint {
    text: SharedString,
    look: Look,
}

impl Render for Hint {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        let style = self.look.text_style();
        let (x, y) = self.look.padding();
        div()
            .size_full()
            .p(px(MARGIN))
            // A pointer that reaches the hint has outrun it; get out of the way
            // before it can be clicked.
            .on_mouse_move(|_, window, cx| {
                cx.default_global::<Tooltips>().machine.hide();
                window.remove_window();
            })
            .child(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px(px(x))
                    .py(px(y))
                    .bg(rgb(colors.panel_active))
                    .border_1()
                    .border_color(rgb(colors.divider))
                    // Native: the only surface, a small raised panel with a hairline and a
                    // soft shadow drawn in the window's transparent margin.
                    .when(ui_text::is_native(), |hint| {
                        hint.rounded(px(NATIVE_RADIUS))
                            .bg(rgb(crate::controls::raised(colors)))
                            .shadow_md()
                    })
                    .text_color(rgb(colors.text))
                    .font_family(style.font_family)
                    .text_size(style.font_size)
                    .whitespace_nowrap()
                    .child(self.text.clone()),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
    }

    fn anchor(window: u64, name: &str, bounds: Bounds<Pixels>) -> Anchor {
        Anchor::new(window, name, bounds)
    }

    #[test]
    fn a_hint_never_overlaps_the_anchor_when_a_side_fits() {
        let area = rect(0.0, 0.0, 400.0, 300.0);
        let hint = size(px(120.0), px(24.0));
        for x in (0..380).step_by(19) {
            for y in (0..280).step_by(13) {
                let anchor = rect(x as f32, y as f32, 20.0, 20.0);
                let (origin, side) = place(anchor, px(x as f32 + 10.0), hint, area);
                assert_ne!(side, Side::Squeezed, "{anchor:?}");
                let placed = Bounds::new(origin, hint);
                assert!(!placed.intersects(&anchor), "{anchor:?} {placed:?}");
                assert!(
                    placed.left() >= area.left()
                        && placed.right() <= area.right()
                        && placed.top() >= area.top()
                        && placed.bottom() <= area.bottom(),
                    "{anchor:?} {placed:?}"
                );
            }
        }
    }

    fn button(x: f32) -> Anchor {
        anchor(1, "button", rect(x, 0.0, 20.0, 20.0))
    }

    #[test]
    fn a_window_that_opens_too_late_is_handed_back() {
        let mut machine = Machine::<u32>::default();
        machine.pointer(button(0.0), true);
        machine.hide();
        assert_eq!(machine.shown(&button(0.0), 9), Some(9));
        // Also when another anchor has taken over the wait.
        machine.pointer(button(0.0), true);
        machine.pointer(button(40.0), true);
        assert_eq!(machine.shown(&button(0.0), 10), Some(10));
        assert_eq!(machine.shown(&button(40.0), 11), None);
    }

    /// Replays inputs the way the app applies them and counts open windows: a
    /// window is open from `shown` until it comes back through `close`.
    #[derive(Default)]
    struct Replay {
        machine: Machine<u32>,
        open: Vec<u32>,
        next: u32,
        pending: Option<u64>,
    }

    impl Replay {
        fn apply(&mut self, step: Step<u32>) {
            if let Some(popup) = step.close {
                assert!(self.open.contains(&popup), "closed a window twice");
                self.open.retain(|open| *open != popup);
            }
            match step.timer {
                Timer::Start(generation) => self.pending = Some(generation),
                Timer::Stop => self.pending = None,
                Timer::Keep => {}
            }
            assert!(
                self.open.len() <= 1,
                "more than one hint window: {:?}",
                self.open
            );
        }

        fn fire(&mut self) {
            let Some(generation) = self.pending else {
                return;
            };
            let Some(anchor) = self.machine.due(generation) else {
                return;
            };
            self.next += 1;
            self.open.push(self.next);
            if let Some(stale) = self.machine.shown(&anchor, self.next) {
                self.open.retain(|open| *open != stale);
            }
            assert!(
                self.open.len() <= 1,
                "more than one hint window: {:?}",
                self.open
            );
        }
    }

    #[test]
    fn no_sequence_of_inputs_leaves_a_window_open_or_opens_two() {
        // A small deterministic generator keeps the sequences reproducible.
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move |bound: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % bound
        };
        for _ in 0..200 {
            let mut replay = Replay::default();
            for _ in 0..60 {
                let step = match next(7) {
                    0 | 1 => replay.machine.pointer(button(next(3) as f32 * 40.0), true),
                    2 => replay.machine.pointer(button(next(3) as f32 * 40.0), false),
                    3 => replay
                        .machine
                        .moved(1 + next(2), point(px(next(150) as f32), px(10.0))),
                    4 => replay.machine.hide(),
                    5 => match replay.pending {
                        Some(generation) => replay.machine.expire(generation),
                        None => replay.machine.window_closed(1),
                    },
                    _ => {
                        replay.fire();
                        continue;
                    }
                };
                replay.apply(step);
            }
            let step = replay.machine.hide();
            replay.apply(step);
            assert!(replay.open.is_empty(), "leaked {:?}", replay.open);
        }
    }
}
