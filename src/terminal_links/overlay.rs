//! The line under a hovered link, drawn above the terminal.
//!
//! A Ghostty surface is a native view of its own, placed over the window's GPUI view, so
//! nothing GPUI paints can be seen over a terminal. The line is drawn instead by Core Animation
//! layers added to the window's view above every other layer, the terminals' included. A layer
//! is not a view and takes no part in hit testing, so the mouse still reaches GPUI and the
//! terminal as before.

use std::ptr;

use gpui::Window;
use objc2::{
    class,
    encode::{Encode, Encoding, RefEncode},
    msg_send,
    rc::{Retained, autoreleasepool},
    runtime::AnyObject,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use super::Strip;

/// Above the layers AppKit keeps for the window's subviews, which sit at zero.
const ABOVE_TERMINALS: f64 = 1000.0;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

// SAFETY: the layouts are CoreGraphics' own on 64-bit Macs, where CGFloat is a double.
unsafe impl Encode for CGPoint {
    const ENCODING: Encoding = Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
}
unsafe impl Encode for CGSize {
    const ENCODING: Encoding = Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
}
unsafe impl Encode for CGRect {
    const ENCODING: Encoding = Encoding::Struct("CGRect", &[CGPoint::ENCODING, CGSize::ENCODING]);
}

/// A `CGColorRef` is only ever passed along.
#[repr(C)]
struct CGColor {
    _opaque: [u8; 0],
}

// SAFETY: `CGColorRef` is `struct CGColor *`.
unsafe impl RefEncode for CGColor {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("CGColor", &[]));
}

/// A window's underline layers, and what they show.
#[derive(Default)]
pub struct Overlay {
    /// Holds one layer per strip.
    layer: Option<Retained<AnyObject>>,
    /// The strips shown, in window points, their color, and the height of the view they were
    /// placed in: the layers are only rebuilt when one of these changes.
    shown: Option<(Vec<Strip>, u32, f64)>,
}

impl Overlay {
    /// Show `strips` (in window points, from the top left) in `color`, or nothing for none.
    pub fn show(&mut self, window: &Window, strips: &[Strip], color: u32) {
        if strips.is_empty() {
            self.hide();
            return;
        }
        let Some(view) = native_view(window) else {
            return;
        };
        // SAFETY: the view is the window's, alive as long as the window, and painting runs on
        // the main thread. The selectors are those of NSView, CALayer, NSColor and
        // CATransaction, with arguments of the types they declare.
        unsafe {
            let parent: *mut AnyObject = msg_send![view, layer];
            if parent.is_null() {
                return;
            }
            let bounds: CGRect = msg_send![view, bounds];
            let height = bounds.size.height;
            if self
                .shown
                .as_ref()
                .is_some_and(|(shown, shown_color, shown_height)| {
                    shown == strips && *shown_color == color && *shown_height == height
                })
            {
                return;
            }
            let _: () = msg_send![class!(CATransaction), begin];
            // A strip moves with the pointer at once, without Core Animation's fade.
            let _: () = msg_send![class!(CATransaction), setDisableActions: true];
            let layer = self.layer_in(parent);
            let _: () = msg_send![&*layer, setFrame: bounds];
            let none: *mut AnyObject = ptr::null_mut();
            let _: () = msg_send![&*layer, setSublayers: none];
            autoreleasepool(|_| {
                let channel = |shift: u32| f64::from((color >> shift) & 0xff) / 255.0;
                let ns_color: *mut AnyObject = msg_send![
                    class!(NSColor),
                    colorWithSRGBRed: channel(16),
                    green: channel(8),
                    blue: channel(0),
                    alpha: 1.0f64
                ];
                let cg_color: *const CGColor = msg_send![ns_color, CGColor];
                for strip in strips {
                    let line: Retained<AnyObject> = msg_send![class!(CALayer), new];
                    let _: () = msg_send![&*line, setBackgroundColor: cg_color];
                    // The view is not flipped: its origin is the bottom left corner.
                    let frame = CGRect {
                        origin: CGPoint {
                            x: strip.x,
                            y: height - strip.y - strip.height,
                        },
                        size: CGSize {
                            width: strip.width,
                            height: strip.height,
                        },
                    };
                    let _: () = msg_send![&*line, setFrame: frame];
                    let _: () = msg_send![&*layer, addSublayer: &*line];
                }
            });
            let _: () = msg_send![class!(CATransaction), commit];
            self.shown = Some((strips.to_vec(), color, height));
        }
    }

    /// Take the line away.
    pub fn hide(&mut self) {
        if self.shown.take().is_none() {
            return;
        }
        if let Some(layer) = &self.layer {
            // SAFETY: the layer is alive while it is held, and this runs on the main thread.
            unsafe {
                let none: *mut AnyObject = ptr::null_mut();
                let _: () = msg_send![class!(CATransaction), begin];
                let _: () = msg_send![class!(CATransaction), setDisableActions: true];
                let _: () = msg_send![&**layer, setSublayers: none];
                let _: () = msg_send![class!(CATransaction), commit];
            }
        }
    }

    /// The layer that holds the strips, made and put on top of `parent` the first time.
    ///
    /// # Safety
    ///
    /// `parent` is a live layer, and this runs on the main thread.
    unsafe fn layer_in(&mut self, parent: *mut AnyObject) -> Retained<AnyObject> {
        if let Some(layer) = &self.layer {
            let superlayer: *mut AnyObject = unsafe { msg_send![&**layer, superlayer] };
            if superlayer == parent {
                return layer.clone();
            }
            let _: () = unsafe { msg_send![&**layer, removeFromSuperlayer] };
        }
        let layer: Retained<AnyObject> = unsafe { msg_send![class!(CALayer), new] };
        unsafe {
            let _: () = msg_send![&*layer, setZPosition: ABOVE_TERMINALS];
            let _: () = msg_send![parent, addSublayer: &*layer];
        }
        self.layer = Some(layer.clone());
        layer
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        if let Some(layer) = self.layer.take() {
            // SAFETY: the layer is alive while it is held; the window is dropped on the main
            // thread.
            let _: () = unsafe { msg_send![&*layer, removeFromSuperlayer] };
        }
    }
}

/// The window's NSView, where GPUI draws and the terminals' views are placed.
fn native_view(window: &Window) -> Option<*mut AnyObject> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return None;
    };
    Some(handle.ns_view.as_ptr().cast::<AnyObject>())
}
