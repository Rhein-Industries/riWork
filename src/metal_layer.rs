//! The Metal layer behind a window.
//!
//! GPUI lets each window's `CAMetalLayer` hold three drawables, each as large as
//! the window in device pixels. A window that has been resized, or that ever
//! had a frame in flight behind another, ends up with all three allocated and
//! keeps them for good: on a 6534 x 2364 px window that is 180 MB for what two
//! hold just as well. Two is the double buffering Apple documents for an
//! interface that is drawn only when something changes, and the window still
//! never waits for a frame it has not started.
//!
//! GPUI sets the count once, when it makes the renderer, and offers no option
//! for it, so this asks the layer directly.

use gpui::Window;
use objc2::{msg_send, runtime::AnyObject, sel};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// The most drawables a window's layer may hold.
pub(crate) const DRAWABLES: usize = 2;

/// Allow `window`'s layer at most `DRAWABLES` drawables. Returns whether the
/// layer took it; a window without a native view or a layer that lacks the
/// setting is left as GPUI made it.
pub(crate) fn limit_drawables(window: &Window) -> bool {
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return false;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return false;
    };
    // SAFETY: the handle is the window's NSView, alive as long as the window,
    // and windows are made and used on the main thread, where this runs. The
    // view's layer is the renderer's CAMetalLayer (GPUI answers
    // `makeBackingLayer` with it); the selector is checked before it is sent,
    // and the count is an NSUInteger.
    unsafe {
        let view = handle.ns_view.as_ptr().cast::<AnyObject>();
        let layer: *mut AnyObject = msg_send![view, layer];
        if layer.is_null() {
            return false;
        }
        let accepts: bool = msg_send![layer, respondsToSelector: sel!(setMaximumDrawableCount:)];
        if !accepts {
            return false;
        }
        let _: () = msg_send![layer, setMaximumDrawableCount: DRAWABLES];
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_drawables_is_the_lowest_count_a_layer_accepts() {
        // CAMetalLayer accepts 2 or 3 and raises for anything else.
        assert!((2..=3).contains(&DRAWABLES));
    }
}
