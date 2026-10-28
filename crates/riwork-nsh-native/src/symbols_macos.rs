//! The original Native theme's SF Symbols, displayed by AppKit itself.
//! Images are immutable and shared by name, point size and weight. Color belongs
//! to the image view, so tinting one icon cannot recolor another. There is no
//! raster data, file IO, timer or work on the JS engine thread here.
use std::{cell::RefCell, collections::HashMap, rc::Rc};

use nsh_core::{JsonValue, color::Rgba};
use nsh_platform_macos::{
    ComponentContext, Components, NativeComponent,
    objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained},
    objc2_app_kit::{
        NSAccessibility, NSColor, NSImage, NSImageAlignment, NSImageFrameStyle,
        NSImageScaling, NSImageSymbolConfiguration, NSImageView, NSView,
        NSViewLayerContentsRedrawPolicy,
    },
    objc2_foundation::{NSObjectProtocol, NSPoint, NSRect, NSString},
};
use serde_json::json;

define_class!(
    #[unsafe(super(NSImageView))]
    #[name = "RiWorkSymbolImageView"]
    #[thread_kind = MainThreadOnly]
    struct SymbolImageView;

    unsafe impl NSObjectProtocol for SymbolImageView {}

    impl SymbolImageView {
        // The NSH node also has pointerEvents="none". That keeps its hit.native
        // flag out of NSH's picker; this is the AppKit-level backstop.
        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, _point: NSPoint) -> Option<Retained<NSView>> { None }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool { false }

        #[unsafe(method(setFrame:))]
        fn set_frame(&self, frame: NSRect) {
            let previous = self.frame();
            // NSH places hosted views again when their wrapper moves. The
            // icon's local frame usually has not changed at all; keep AppKit's
            // existing symbol backing instead of invalidating it on each move.
            if previous == frame { return; }
            // SAFETY: delegate real geometry changes to NSImageView.
            unsafe { msg_send![super(self), setFrame: frame] }
            if previous.size != frame.size {
                NSView::setNeedsDisplay(self, true);
            }
        }
    }
);

impl SymbolImageView {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        // SAFETY: the subclass adds no ivars and uses NSImageView's initializer.
        let view: Retained<Self> = unsafe { msg_send![Self::alloc(mtm), init] };
        // Retain the symbol's pixels while its ancestors are resized. The
        // view redraws only for its own size, image, tint or AppKit invalidation.
        view.setWantsLayer(true);
        view.setLayerContentsRedrawPolicy(NSViewLayerContentsRedrawPolicy::OnSetNeedsDisplay);
        view.setEditable(false);
        view.setAnimates(false);
        view.setImageAlignment(NSImageAlignment::AlignCenter);
        view.setImageScaling(NSImageScaling::ScaleProportionallyDown);
        view.setImageFrameStyle(NSImageFrameStyle::None);
        view.setAccessibilityElement(false);
        view
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct Key {
    name: String,
    /// Quantize only the cache key, matching the original's hundredths of a point.
    points: u32,
    medium: bool,
}

thread_local! {
    static IMAGES: RefCell<HashMap<Key, Option<Retained<NSImage>>>> = RefCell::new(HashMap::new());
}

fn image(key: &Key) -> Option<Retained<NSImage>> {
    if let Some(cached) = IMAGES.with(|images| images.borrow().get(key).cloned()) {
        return cached;
    }
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(&key.name), None,
    ).and_then(|symbol| {
        // The same NSFontWeightRegular / NSFontWeightMedium used by symbols.rs
        // in the original desktop. AppKit supplies backing-scale rasterization.
        let configuration = NSImageSymbolConfiguration::configurationWithPointSize_weight(
            f64::from(key.points) / 100.0, if key.medium { 0.23 } else { 0.0 },
        );
        symbol.imageWithSymbolConfiguration(&configuration)
    });
    IMAGES.with(|images| {
        let mut images = images.borrow_mut();
        // UI zoom may introduce new sizes over time. Views keep their own retain
        // when this bounded cache is emptied, so no live image is invalidated.
        if images.len() >= 512 { images.clear(); }
        images.insert(key.clone(), image.clone());
    });
    image
}

struct Symbol {
    view: Retained<SymbolImageView>,
    context: ComponentContext,
    key: Option<Key>,
    color: Option<String>,
    reported: Option<(String, bool)>,
}

impl NativeComponent for Symbol {
    fn view(&self) -> Retained<NSView> {
        // SymbolImageView -> NSImageView -> NSControl -> NSView.
        Retained::into_super(Retained::into_super(Retained::into_super(self.view.clone())))
    }

    fn set_prop(&mut self, prop: &str, value: Option<&JsonValue>) {
        if prop != "symbol" { return; }
        let name = value.and_then(|v| v.get("name")).and_then(JsonValue::as_str).unwrap_or("");
        if name.is_empty() || name.len() > 128 {
            self.view.setImage(None);
            NSView::setNeedsDisplay(&self.view, true);
            self.key = None;
            return;
        }
        let points = value.and_then(|v| v.get("points")).and_then(JsonValue::as_f64)
            .filter(|points| points.is_finite()).unwrap_or(11.2).clamp(1.0, 256.0);
        let medium = value.and_then(|v| v.get("weight")).and_then(JsonValue::as_str) == Some("medium");
        let key = Key { name: name.into(), points: (points * 100.0).round() as u32, medium };
        if self.key.as_ref() != Some(&key) {
            let image = image(&key);
            self.view.setImage(image.as_deref());
            NSView::setNeedsDisplay(&self.view, true);
            let reported = (name.to_owned(), image.is_some());
            if self.reported.as_ref() != Some(&reported) {
                self.context.emit("availability", json!({"name": name, "available": reported.1}));
                self.reported = Some(reported);
            }
            self.key = Some(key);
        }
        let color = value.and_then(|v| v.get("color")).and_then(JsonValue::as_str).unwrap_or("#98989d");
        if self.color.as_deref() != Some(color) {
            let rgba = Rgba::try_from(color).unwrap_or_else(|_| nsh_core::color::rgb(0x98989d));
            let tint = NSColor::colorWithSRGBRed_green_blue_alpha(
                f64::from(rgba.r), f64::from(rgba.g), f64::from(rgba.b), f64::from(rgba.a),
            );
            self.view.setContentTintColor(Some(&tint));
            NSView::setNeedsDisplay(&self.view, true);
            self.color = Some(color.into());
        }
    }
}

/// Register beside the terminal and preview before constructing the first AppView.
pub fn register(components: &Components) {
    components.register("riwork.symbol", Rc::new(|context| {
        let mtm = MainThreadMarker::new().expect("native symbol created on AppKit's main thread");
        Box::new(Symbol {
            view: SymbolImageView::new(mtm), context, key: None, color: None, reported: None,
        })
    }));
}
