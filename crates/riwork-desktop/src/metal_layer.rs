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
//!
//! The terminals' layers have the opposite problem. Ghostty gives each its
//! window's scale once, when it makes the layer, and sizes every frame by it;
//! AppKit leaves a layer a view hosts alone when the window moves to a display
//! of another scale, so a terminal would keep drawing at the old one until it
//! is made anew. `sync_terminal_scale` hands them the window's scale instead.

use gpui::Window;
use objc2::{
    class, msg_send,
    rc::autoreleasepool,
    runtime::{AnyClass, AnyObject},
    sel,
};
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

/// Give every terminal in `window` the window's scale, for one moved to a
/// display of another. Terminals already at it, and a build without Ghostty's
/// view, are left alone.
pub(crate) fn sync_terminal_scale(window: &Window) {
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };
    // SAFETY: as in `limit_drawables`, the view is the window's, alive with it,
    // and this runs on the main thread.
    unsafe {
        sync_scale_in(
            handle.ns_view.as_ptr().cast::<AnyObject>(),
            f64::from(window.scale_factor()),
        );
    }
}

/// Set the layer of each Ghostty view directly in `view` to `scale`. Returns
/// how many changed.
///
/// # Safety
///
/// `view` is a live NSView, and this runs on the main thread.
unsafe fn sync_scale_in(view: *mut AnyObject, scale: f64) -> usize {
    // The view gpui-libghostty places each terminal in, as a subview of the
    // window's own.
    let Some(terminal) = AnyClass::get(c"GpuiGhosttyView") else {
        return 0;
    };
    let mut changed = 0;
    autoreleasepool(|_| unsafe {
        let subviews: *mut AnyObject = msg_send![view, subviews];
        let count: usize = msg_send![subviews, count];
        for index in 0..count {
            let subview: *mut AnyObject = msg_send![subviews, objectAtIndex: index];
            let is_terminal: bool = msg_send![subview, isKindOfClass: terminal];
            if !is_terminal {
                continue;
            }
            let layer: *mut AnyObject = msg_send![subview, layer];
            if layer.is_null() {
                continue;
            }
            let current: f64 = msg_send![layer, contentsScale];
            if current == scale {
                continue;
            }
            if changed == 0 {
                // Swap the scale at once rather than animate it.
                let _: () = msg_send![class!(CATransaction), begin];
                let _: () = msg_send![class!(CATransaction), setDisableActions: true];
            }
            let _: () = msg_send![layer, setContentsScale: scale];
            changed += 1;
        }
        if changed > 0 {
            let _: () = msg_send![class!(CATransaction), commit];
        }
    });
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_drawables_is_the_lowest_count_a_layer_accepts() {
        // CAMetalLayer accepts 2 or 3 and raises for anything else.
        assert!((2..=3).contains(&DRAWABLES));
    }

    #[test]
    fn only_terminals_off_the_scale_take_it() {
        // SAFETY: the views are made, used and released here; AppKit lets a view
        // with no window be used off the main thread.
        unsafe {
            let terminal_class =
                AnyClass::get(c"GpuiGhosttyView").expect("gpui-libghostty's view is linked");
            let parent: *mut AnyObject = msg_send![class!(NSView), new];
            let terminal: *mut AnyObject = msg_send![terminal_class, new];
            let other: *mut AnyObject = msg_send![class!(NSView), new];
            for view in [terminal, other] {
                let _: () = msg_send![view, setWantsLayer: true];
                let layer: *mut AnyObject = msg_send![view, layer];
                let _: () = msg_send![layer, setContentsScale: 1.0f64];
                let _: () = msg_send![parent, addSubview: view];
            }
            let scale = |view: *mut AnyObject| -> f64 {
                let layer: *mut AnyObject = msg_send![view, layer];
                msg_send![layer, contentsScale]
            };

            assert_eq!(sync_scale_in(parent, 2.0), 1);
            assert_eq!(scale(terminal), 2.0);
            assert_eq!(scale(other), 1.0);
            assert_eq!(sync_scale_in(parent, 2.0), 0);

            for view in [terminal, other, parent] {
                let _: () = msg_send![view, release];
            }
        }
    }
}
