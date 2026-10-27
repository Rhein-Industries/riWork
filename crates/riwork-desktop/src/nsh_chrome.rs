//! Native chrome construction, shared with the non-visible AppKit acceptance harness.
use nsh_platform_macos::{
    objc2::{MainThreadMarker, MainThreadOnly, rc::Retained},
    objc2_app_kit::{
        NSAccessibility, NSBackingStoreType, NSWindow, NSWindowButton, NSWindowStyleMask,
        NSWindowTitleVisibility,
    },
    objc2_foundation::{NSPoint, NSRect, NSSize, NSString},
};

pub(crate) fn create_window(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSWindow> {
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::Miniaturizable
                | NSWindowStyleMask::Resizable
                | NSWindowStyleMask::FullSizeContentView,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setContentMinSize(NSSize::new(640., 400.));
    window.setTitlebarAppearsTransparent(true);
    window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
    window
}

pub(crate) fn position_controls(window: &NSWindow) {
    for (index, kind) in [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ]
    .into_iter()
    .enumerate()
    {
        if let Some(button) = window.standardWindowButton(kind)
            && let Some(parent) = (unsafe { button.superview() })
        {
            // The titlebar container itself is inset from the NSWindow frame.
            // Position in window coordinates first, then convert to the actual
            // (possibly flipped) container; a parent-relative inset drifts.
            let size = button.frame().size;
            let desired = NSRect::new(
                NSPoint::new(
                    10. + index as f64 * 20.,
                    window.frame().size.height - size.height - 8.,
                ),
                size,
            );
            let origin = parent.convertRect_fromView(desired, None).origin;
            if button.frame().origin != origin {
                button.setFrameOrigin(origin);
            }
            let label = NSString::from_str(["Close", "Minimize", "Zoom"][index]);
            if button.accessibilityLabel().as_deref() != Some(&*label) {
                button.setAccessibilityLabel(Some(&label));
            }
        }
    }
}
