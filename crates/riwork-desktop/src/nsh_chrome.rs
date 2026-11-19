//! Native chrome construction, shared with the non-visible AppKit acceptance harness.
use block2::RcBlock;
use nsh_platform_macos::{
    objc2::{MainThreadMarker, MainThreadOnly, rc::Retained},
    objc2_app_kit::{
        NSAccessibility, NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua,
        NSBackingStoreType, NSColor, NSWindow, NSWindowButton, NSWindowStyleMask,
        NSWindowTitleVisibility,
    },
    objc2_foundation::{NSArray, NSPoint, NSRect, NSSize, NSString},
};
use std::ptr::NonNull;

/// Whether `appearance` is a dark one.
pub(crate) fn is_dark(appearance: &NSAppearance) -> bool {
    // SAFETY: immutable AppKit constants.
    let (light, dark) = unsafe { (NSAppearanceNameAqua, NSAppearanceNameDarkAqua) };
    appearance
        .bestMatchFromAppearancesWithNames(&NSArray::from_slice(&[light, dark]))
        .is_some_and(|name| &*name == dark)
}

/// The sRGB bytes of [`backdrop`] for a light or a dark appearance: white and neutral-950.
pub(crate) fn backdrop_rgb(dark: bool) -> [u8; 3] {
    if dark { [10, 10, 10] } else { [255, 255, 255] }
}

/// What a window shows behind the app: before its first frame, and where a growing window is
/// not laid out yet. NSH 0.3 painted these colors itself under every `AppView`; an embedded
/// view of NSH 0.6.0 paints nothing there and shows its host's window through (its
/// `system.rs`), so the window carries them. They follow the window's appearance.
pub(crate) fn backdrop() -> Retained<NSColor> {
    let color = |dark| {
        let [r, g, b] = backdrop_rgb(dark).map(|byte| f64::from(byte) / 255.);
        NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.)
    };
    let (light, dark) = (color(false), color(true));
    let provider = RcBlock::new(move |appearance: NonNull<NSAppearance>| {
        // SAFETY: AppKit passes the appearance it resolves the color for.
        let dark_appearance = is_dark(unsafe { appearance.as_ref() });
        NonNull::from(if dark_appearance { &*dark } else { &*light })
    });
    // SAFETY: the provider returns one of the two colors it keeps alive.
    unsafe { NSColor::colorWithName_dynamicProvider(None, &provider) }
}

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
    window.setBackgroundColor(Some(&backdrop()));
    window
}

pub(crate) fn position_controls(window: &NSWindow) {
    // AppKit owns the revealed titlebar and controls in full screen. Moving
    // them while its titlebar slides in/out also fights its transition layout.
    if window.styleMask().contains(NSWindowStyleMask::FullScreen) {
        return;
    }
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
            // (possibly flipped) container; a parent-relative inset drifts. This
            // runs in AppKit's synchronous resize notification, before the frame
            // is presented, rather than correcting a frame from windowDidUpdate.
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
