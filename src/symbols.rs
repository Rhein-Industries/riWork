//! SF Symbols, the icons the Native theme draws instead of RiWork's own vector glyphs.
//!
//! AppKit draws a symbol (`NSImage imageWithSystemSymbolName:`) at the requested point size and
//! weight into a bitmap at the window's backing scale. Only its coverage is kept, and the pixels
//! are filled with the requested color, so a symbol is tinted exactly like the text beside it.
//! Each rasterized symbol is cached per name, size, weight, color and scale: a GPUI image is
//! uploaded to the sprite atlas once per instance, so reusing the instance keeps that to once.
//! A symbol this macOS does not have is remembered as missing, and the caller draws its own.

use std::{
    cell::RefCell,
    collections::HashMap,
    ffi::{CString, c_void},
    ptr,
    sync::Arc,
};

use gpui::RenderImage;
use image::{Frame, RgbaImage};
use objc2::{
    class,
    encode::{Encode, Encoding, RefEncode},
    msg_send,
    rc::autoreleasepool,
    runtime::AnyObject,
};

/// The stroke weight of a symbol, matched to the text it sits beside.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Weight {
    Regular,
    Medium,
}

impl Weight {
    /// `NSFontWeightRegular` and `NSFontWeightMedium`.
    fn ns_font_weight(self) -> f64 {
        match self {
            Self::Regular => 0.0,
            Self::Medium => 0.23,
        }
    }
}

/// One rasterized symbol. Lengths are kept in hundredths of a point, so the key is exact
/// and hashable while the interface scale is any fraction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub name: &'static str,
    /// The symbol's point size.
    points: u32,
    /// The square box it is centered in, in points.
    side: u32,
    pub weight: Weight,
    pub color: u32,
    /// The window's backing scale factor.
    scale: u32,
}

fn hundredths(value: f32) -> u32 {
    (value.max(0.0) * 100.0).round() as u32
}

impl Key {
    pub fn new(
        name: &'static str,
        points: f32,
        side: f32,
        weight: Weight,
        color: u32,
        scale: f32,
    ) -> Self {
        Self {
            name,
            points: hundredths(points),
            side: hundredths(side),
            weight,
            color: color & 0xff_ffff,
            scale: hundredths(scale),
        }
    }

    /// The bitmap's side in pixels: the box at the backing scale.
    pub fn pixels(&self) -> usize {
        ((self.side as f32 / 100.0) * (self.scale as f32 / 100.0))
            .ceil()
            .clamp(1.0, 512.0) as usize
    }
}

thread_local! {
    static CACHE: RefCell<HashMap<Key, Option<Arc<RenderImage>>>> = RefCell::new(HashMap::new());
}

/// The symbol for `key`, tinted and ready to paint over its box, or None when this macOS
/// has no symbol by that name.
pub fn image(key: Key) -> Option<Arc<RenderImage>> {
    if let Some(cached) = CACHE.with(|cache| cache.borrow().get(&key).cloned()) {
        return cached;
    }
    let image = coverage(&key).map(|alpha| tinted(&alpha, key.pixels(), key.color));
    CACHE.with(|cache| cache.borrow_mut().insert(key, image.clone()));
    image
}

/// GPUI images are BGRA with straight alpha: every pixel is the color, its alpha the coverage.
fn tinted(alpha: &[u8], side: usize, color: u32) -> Arc<RenderImage> {
    let [_, r, g, b] = color.to_be_bytes();
    let mut pixels = Vec::with_capacity(alpha.len() * 4);
    for &a in alpha {
        pixels.extend_from_slice(&[b, g, r, a]);
    }
    let buffer = RgbaImage::from_raw(side as u32, side as u32, pixels)
        .expect("one pixel per coverage value");
    Arc::new(RenderImage::new([Frame::new(buffer)]))
}

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

/// A `CGContextRef` is only ever passed along.
#[repr(C)]
struct CGContext {
    _opaque: [u8; 0],
}

// SAFETY: `CGContextRef` is `struct CGContext *`.
unsafe impl RefEncode for CGContext {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("CGContext", &[]));
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGColorSpaceCreateDeviceRGB() -> *mut c_void;
    fn CGColorSpaceRelease(space: *mut c_void);
    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: *mut c_void,
        bitmap_info: u32,
    ) -> *mut c_void;
    fn CGContextRelease(context: *mut c_void);
    fn CGContextScaleCTM(context: *mut c_void, sx: f64, sy: f64);
}

/// `kCGImageAlphaPremultipliedLast`: RGBA, which is all a bitmap context draws into.
const PREMULTIPLIED_LAST: u32 = 1;
/// `NSCompositingOperationSourceOver`.
const SOURCE_OVER: usize = 2;

/// The symbol's coverage over a `key.pixels()` square, row by row from the top: drawn at its
/// own size, centered in the box, and scaled down only if it would not fit.
fn coverage(key: &Key) -> Option<Vec<u8>> {
    let name = CString::new(key.name).ok()?;
    let side = key.pixels();
    let scale = f64::from(key.scale) / 100.0;
    let box_points = f64::from(key.side) / 100.0;
    let stride = side * 4;
    let mut pixels = vec![0u8; stride * side];
    autoreleasepool(|_| unsafe {
        let name: *mut AnyObject = msg_send![class!(NSString), stringWithUTF8String: name.as_ptr()];
        if name.is_null() {
            return None;
        }
        let none: *mut AnyObject = ptr::null_mut();
        let symbol: *mut AnyObject = msg_send![
            class!(NSImage),
            imageWithSystemSymbolName: name,
            accessibilityDescription: none
        ];
        if symbol.is_null() {
            return None;
        }
        let configuration: *mut AnyObject = msg_send![
            class!(NSImageSymbolConfiguration),
            configurationWithPointSize: f64::from(key.points) / 100.0,
            weight: key.weight.ns_font_weight()
        ];
        let symbol: *mut AnyObject = msg_send![symbol, imageWithSymbolConfiguration: configuration];
        if symbol.is_null() {
            return None;
        }
        let size: CGSize = msg_send![symbol, size];
        if !(size.width > 0.0 && size.height > 0.0) {
            return None;
        }
        let fit = (box_points / size.width.max(size.height)).min(1.0);
        let (width, height) = (size.width * fit, size.height * fit);
        let rect = CGRect {
            origin: CGPoint {
                x: (box_points - width) / 2.0,
                y: (box_points - height) / 2.0,
            },
            size: CGSize { width, height },
        };

        let space = CGColorSpaceCreateDeviceRGB();
        if space.is_null() {
            return None;
        }
        let context = CGBitmapContextCreate(
            pixels.as_mut_ptr().cast(),
            side,
            side,
            8,
            stride,
            space,
            PREMULTIPLIED_LAST,
        );
        CGColorSpaceRelease(space);
        if context.is_null() {
            return None;
        }
        CGContextScaleCTM(context, scale, scale);
        let graphics: *mut AnyObject = msg_send![
            class!(NSGraphicsContext),
            graphicsContextWithCGContext: context.cast::<CGContext>(),
            flipped: false
        ];
        let drawn = !graphics.is_null();
        if drawn {
            let _: () = msg_send![class!(NSGraphicsContext), saveGraphicsState];
            let _: () = msg_send![class!(NSGraphicsContext), setCurrentContext: graphics];
            let whole = CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize {
                    width: 0.0,
                    height: 0.0,
                },
            };
            let _: () = msg_send![
                symbol,
                drawInRect: rect,
                fromRect: whole,
                operation: SOURCE_OVER,
                fraction: 1.0f64
            ];
            let _: () = msg_send![class!(NSGraphicsContext), restoreGraphicsState];
        }
        CGContextRelease(context);
        drawn.then_some(())
    })?;
    // A bitmap context's first row is the image's top.
    Some(pixels.chunks_exact(4).map(|pixel| pixel[3]).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_tell_apart_everything_that_changes_the_pixels() {
        let key = Key::new("lock", 13.0, 16.5, Weight::Regular, 0x1d1d1f, 2.0);
        assert_eq!(
            key,
            Key::new("lock", 13.0, 16.5, Weight::Regular, 0xff1d1d1f, 2.0)
        );
        for other in [
            Key::new("lock.open", 13.0, 16.5, Weight::Regular, 0x1d1d1f, 2.0),
            Key::new("lock", 14.0, 16.5, Weight::Regular, 0x1d1d1f, 2.0),
            Key::new("lock", 13.0, 17.0, Weight::Regular, 0x1d1d1f, 2.0),
            Key::new("lock", 13.0, 16.5, Weight::Medium, 0x1d1d1f, 2.0),
            Key::new("lock", 13.0, 16.5, Weight::Regular, 0xffffff, 2.0),
            Key::new("lock", 13.0, 16.5, Weight::Regular, 0x1d1d1f, 1.0),
        ] {
            assert_ne!(key, other);
        }
        assert_eq!(key.pixels(), 33);
        assert_eq!(
            Key::new("lock", 13.0, 14.0, Weight::Regular, 0, 1.0).pixels(),
            14
        );
    }

    #[test]
    fn tinting_keeps_the_coverage_as_alpha_under_one_color() {
        let image = tinted(&[0, 128, 255, 7], 2, 0x102030);
        let bytes = image.as_bytes(0).unwrap();
        assert_eq!(
            bytes,
            [
                0x30, 0x20, 0x10, 0, 0x30, 0x20, 0x10, 128, 0x30, 0x20, 0x10, 255, 0x30, 0x20,
                0x10, 7
            ]
        );
    }

    #[test]
    fn a_symbol_rasterizes_inside_its_box_and_a_missing_one_is_none() {
        let key = Key::new("lock", 13.0, 16.0, Weight::Regular, 0x000000, 2.0);
        let alpha = coverage(&key).expect("macOS 11+ has the lock symbol");
        assert_eq!(alpha.len(), 32 * 32);
        let covered = alpha.iter().filter(|&&a| a > 128).count();
        assert!(covered > 40, "the lock covers {covered} pixels");
        // Centered: nothing touches the outermost ring of pixels.
        for i in 0..32 {
            for (x, y) in [(i, 0), (i, 31), (0, i), (31, i)] {
                assert_eq!(alpha[y * 32 + x], 0, "edge pixel {x},{y}");
            }
        }
        assert!(image(key).is_some());
        let missing = Key::new("riwork.not-a-symbol", 13.0, 16.0, Weight::Regular, 0, 2.0);
        assert!(coverage(&missing).is_none());
        assert!(image(missing).is_none());
    }
}
