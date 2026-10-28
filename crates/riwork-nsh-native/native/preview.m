// Native AppKit frame/input adapter. Bytes stay in Rust/CGImage; only small
// input records cross this C boundary. No NSEvent is posted to any window.
#import <AppKit/AppKit.h>
#import <Carbon/Carbon.h>
#import <QuartzCore/QuartzCore.h>
#import <math.h>
#import <objc/runtime.h>
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>

typedef bool (*RWPreviewEvent)(void *, const char *);
typedef void (*RWPreviewRelease)(void *);
typedef void (*RWPreviewPixelsRelease)(void *, const void *, size_t);

// Match GPUI's macOS key names and modifier ordering. Text itself is supplied
// exclusively by NSTextInputClient, including alternate layouts and dead keys.
static NSString *translatedKey(unsigned short code, UInt32 modifiers) {
    TISInputSourceRef source = TISCopyCurrentKeyboardLayoutInputSource();
    if (!source) return @"";
    CFDataRef data = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData);
    NSString *result = @"";
    if (data) {
        UniChar chars[8]; UniCharCount length = 0; UInt32 dead = 0;
        OSStatus status = UCKeyTranslate((const UCKeyboardLayout *)CFDataGetBytePtr(data), code,
            kUCKeyActionDown, modifiers, LMGetKbdType(), 0, &dead, 8, &length, chars);
        if (status == noErr) result = [NSString stringWithCharacters:chars length:length];
    }
    CFRelease(source);
    return result;
}
static BOOL asciiLowercase(NSString *text) {
    if (!text.length) return NO;
    for (NSUInteger i = 0; i < text.length; i++) {
        unichar c = [text characterAtIndex:i]; if (c < 'a' || c > 'z') return NO;
    }
    return YES;
}
static BOOL asciiText(NSString *text) {
    for (NSUInteger i = 0; i < text.length; i++) if ([text characterAtIndex:i] > 127) return NO;
    return YES;
}
static NSString *keyFromFields(NSString *characters, NSString *typed, NSEventModifierFlags flags,
    NSString *plain, NSString *shifted, NSString *cmd, NSString *both, BOOL commandLayout, BOOL *textKey) {
    unichar first = characters.length ? [characters characterAtIndex:0] : 0;
    BOOL shift = (flags & NSEventModifierFlagShift) != 0;
    BOOL command = (flags & NSEventModifierFlagCommand) != 0;
    BOOL control = (flags & NSEventModifierFlagControl) != 0;
    BOOL alt = (flags & NSEventModifierFlagOption) != 0;
    BOOL function = (flags & NSEventModifierFlagFunction) != 0 &&
        !(first >= NSUpArrowFunctionKey && first <= NSModeSwitchFunctionKey);
    NSString *key = nil;
    switch (first) {
        case ' ': key = @"space"; break;
        case '\t': case NSBackTabCharacter: key = @"tab"; break;
        case '\r': case NSEnterCharacter: key = @"enter"; break;
        case NSDeleteCharacter: key = @"backspace"; break;
        case 0x1b: key = @"escape"; break;
        case NSUpArrowFunctionKey: key = @"up"; break;
        case NSDownArrowFunctionKey: key = @"down"; break;
        case NSLeftArrowFunctionKey: key = @"left"; break;
        case NSRightArrowFunctionKey: key = @"right"; break;
        case NSPageUpFunctionKey: key = @"pageup"; break;
        case NSPageDownFunctionKey: key = @"pagedown"; break;
        case NSHomeFunctionKey: key = @"home"; break;
        case NSEndFunctionKey: key = @"end"; break;
        case NSDeleteFunctionKey: key = @"delete"; break;
        case NSHelpFunctionKey: case NSInsertFunctionKey: key = @"insert"; break;
        default: if (first >= NSF1FunctionKey && first <= NSF35FunctionKey)
            key = [NSString stringWithFormat:@"f%u", first - NSF1FunctionKey + 1];
    }
    BOOL special = key != nil && ![key isEqualToString:@"space"];
    if (!key) {
        if (command || commandLayout) {
            if (![both isEqualToString:cmd]) shifted = both;
            else if (asciiLowercase(cmd)) shifted = cmd.uppercaseString;
            plain = cmd;
        }
        if (!plain.length) plain = characters;
        if (!shifted.length) shifted = characters;
        if (shift && !asciiLowercase(plain)) { key = shifted; shift = NO; }
        else key = plain;
    }
    // Empty characters are possible for a dead key: it still belongs to IME.
    *textKey = !special && !command && !control && !function &&
        [typed rangeOfCharacterFromSet:NSCharacterSet.controlCharacterSet].location == NSNotFound;
    NSMutableString *result = [NSMutableString string];
    if (function) [result appendString:@"fn-"];
    if (control) [result appendString:@"ctrl-"];
    if (alt) [result appendString:@"alt-"];
    if (command) [result appendString:@"cmd-"];
    if (shift) [result appendString:@"shift-"];
    [result appendString:key ?: @""];
    return result;
}
static NSString *previewKey(NSEvent *event, BOOL *textKey) {
    return keyFromFields(event.charactersIgnoringModifiers ?: @"", event.characters ?: @"", event.modifierFlags,
        translatedKey(event.keyCode, 0), translatedKey(event.keyCode, shiftKey >> 8),
        translatedKey(event.keyCode, cmdKey >> 8), translatedKey(event.keyCode, (cmdKey | shiftKey) >> 8),
        !asciiText(translatedKey(0, 0)) && asciiText(translatedKey(0, cmdKey >> 8)), textKey);
}
static NSDictionary *buttonRecord(NSInteger button, BOOL down, double x, double y) {
    if (button < 0 || button > 2) return nil;
    return @{@"kind":down ? @"pointer_down" : @"pointer_up", @"x":@(x), @"y":@(y),
        @"button":button == 1 ? @"right" : button == 2 ? @"middle" : @"left"};
}
static NSDictionary *scrollRecord(double x, double y, double dx, double dy, BOOL precise) {
    return @{@"kind":@"scroll", @"x":@(x), @"y":@(y), @"delta_x":@(dx), @"delta_y":@(dy), @"precise":@(precise)};
}
static NSDictionary *keyUpRecord(NSMutableDictionary *held, NSNumber *code, NSString *key, BOOL textKey) {
    NSDictionary *pressed = held[code];
    if (pressed) { key = pressed[@"key"]; textKey = [pressed[@"text_key"] boolValue]; }
    // `pressed` and its key are borrowed. Under manual reference counting the
    // held entry may be their last owner, so the record must retain the key
    // before removal releases it.
    NSDictionary *record = @{@"kind":@"key_up", @"key":key, @"text_key":@(textKey)};
    [held removeObjectForKey:code];
    return record;
}
static NSDictionary *changedHeldKey(NSMutableDictionary *held, NSNumber *code, NSString *key, BOOL textKey) {
    NSDictionary *previous = held[code];
    // A repeat can arrive after Command/Shift was released. End its old
    // logical key before changing spelling or handing printable repeats to IME.
    if (previous && (textKey || ![previous[@"key"] isEqualToString:key]))
        return keyUpRecord(held, code, key, textKey);
    return nil;
}
static NSRange markText(NSMutableAttributedString *marked, id value, NSRange selected) {
    NSAttributedString *text = [value isKindOfClass:NSAttributedString.class] ? value :
        [[[NSAttributedString alloc] initWithString:value] autorelease];
    [marked setAttributedString:text];
    NSUInteger start = MIN(selected.location, marked.length);
    return NSMakeRange(start, MIN(selected.length, marked.length - start));
}
static NSDictionary *commitRecord(id value) {
    NSString *text = [value isKindOfClass:NSAttributedString.class] ? [value string] : value;
    return [text isKindOfClass:NSString.class] && text.length ? @{@"kind":@"text", @"text":text} : nil;
}
static NSDictionary *interpretedCommandRecord(NSDictionary *key, BOOL *sent) {
    if (!key || *sent || [key[@"text_key"] boolValue]) return nil;
    *sent = YES;
    return @{@"kind":@"key", @"key":key[@"key"], @"text_key":@NO};
}
static BOOL ownsKeyUp(BOOL stopped, BOOL sameWindow, BOOL firstResponder, BOOL held) {
    return !stopped && sameWindow && firstResponder && held;
}
static NSDictionary *focusRecord(BOOL canInput, BOOL firstResponder) {
    return canInput && firstResponder ? @{@"kind":@"focus", @"focused":@YES} : @{@"kind":@"release"};
}

@interface RWPreviewView : NSView <NSTextInputClient> {
@public
    void *context;
    RWPreviewEvent eventCallback;
    RWPreviewRelease releaseCallback;
    BOOL enabled, logicalVisible, running, capture, stopped;
    CALayer *frameLayer;
    CATextLayer *placeholder;
    NSMutableAttributedString *marked;
    NSMutableDictionary *heldKeys;
    // Owned only for the synchronous interpretKeyEvents call. An IME consumes
    // composition itself; an unhandled selector forwards this key once.
    NSDictionary *interpretedKey;
    BOOL interpretedCommandSent;
    NSRange markedSelection;
    NSTrackingArea *tracking;
    id keyUpMonitor;
}
- (BOOL)emit:(NSDictionary *)event;
- (void)syncWindow;
- (void)syncAppearance;
- (void)syncGeometry;
- (void)releaseInput;
- (void)syncFocus;
- (BOOL)canInput;
@end

@implementation RWPreviewView
- (instancetype)initWithFrame:(NSRect)frame {
    if (!(self = [super initWithFrame:frame])) return nil;
    logicalVisible = NO;
    marked = [[NSMutableAttributedString alloc] initWithString:@""];
    heldKeys = [[NSMutableDictionary alloc] init];
    self.wantsLayer = YES;
    frameLayer = [[CALayer alloc] init];
    frameLayer.contentsGravity = kCAGravityResizeAspect;
    frameLayer.masksToBounds = YES;
    [self.layer addSublayer:frameLayer];
    placeholder = [[CATextLayer alloc] init];
    placeholder.fontSize = NSFont.smallSystemFontSize;
    placeholder.alignmentMode = kCAAlignmentCenter;
    placeholder.wrapped = YES;
    placeholder.string = @"Configure your app, then choose Run.";
    [self.layer addSublayer:placeholder];
    [[NSNotificationCenter defaultCenter] addObserver:self selector:@selector(windowChanged:)
        name:NSApplicationDidResignActiveNotification object:nil];
    [[NSNotificationCenter defaultCenter] addObserver:self selector:@selector(windowChanged:)
        name:NSApplicationDidBecomeActiveNotification object:nil];
    // AppKit may consume a command key-down as a key equivalent and omit its
    // normal responder-chain key-up. This process-local monitor sees that up
    // first, but consumes only keys this exact focused view actually owns.
    // Under manual reference counting __block does not retain the view; the
    // monitor is removed on the same main thread before the view is destroyed.
    __block RWPreviewView *view = self;
    keyUpMonitor = [[NSEvent addLocalMonitorForEventsMatchingMask:NSEventMaskKeyUp handler:^NSEvent *(NSEvent *event) {
        BOOL sameWindow = view.window && event.window == view.window;
        if (!ownsKeyUp(view->stopped, sameWindow, view.window.firstResponder == view,
            view->heldKeys[@(event.keyCode)] != nil)) return event;
        [view keyUp:event];
        return nil;
    }] retain];
    [self syncAppearance]; [self syncGeometry];
    return self;
}
- (void)dealloc {
    [self releaseInput];
    if (keyUpMonitor) { [NSEvent removeMonitor:keyUpMonitor]; [keyUpMonitor release]; }
    [[NSNotificationCenter defaultCenter] removeObserver:self];
    if (tracking) [self removeTrackingArea:tracking];
    [tracking release]; [marked release]; [heldKeys release]; [frameLayer release]; [placeholder release];
    if (releaseCallback) releaseCallback(context);
    [super dealloc];
}
- (BOOL)isFlipped { return YES; }
- (BOOL)isAccessibilityElement { return YES; }
- (NSString *)accessibilityRole { return NSAccessibilityGroupRole; }
- (NSString *)accessibilityLabel { return @"Application preview"; }
- (BOOL)emit:(NSDictionary *)event {
    if (stopped || !eventCallback) return NO;
    NSData *bytes = [NSJSONSerialization dataWithJSONObject:event options:0 error:nil];
    if (!bytes) return NO;
    NSString *json = [[[NSString alloc] initWithData:bytes encoding:NSUTF8StringEncoding] autorelease];
    return eventCallback(context, json.UTF8String);
}
- (BOOL)canInput {
    return !stopped && enabled && logicalVisible && self.window &&
        self.window.isVisible && self.window.isKeyWindow && NSApp.isActive &&
        !self.isHiddenOrHasHiddenAncestor &&
        (self.window.occlusionState & NSWindowOcclusionStateVisible) != 0;
}
- (BOOL)acceptsFirstResponder { return [self canInput]; }
- (BOOL)acceptsFirstMouse:(NSEvent *)event { (void)event; return YES; }
- (BOOL)becomeFirstResponder {
    if (![self canInput]) return NO;
    [self emit:@{@"kind":@"focus", @"focused":@YES}];
    [self syncAppearance]; return YES;
}
- (BOOL)resignFirstResponder { [self releaseInput]; [self syncAppearance]; return YES; }
- (void)releaseInput {
    [self unmarkText];
    [heldKeys removeAllObjects];
    [self emit:@{@"kind":@"release"}];
}
- (void)syncFocus {
    NSDictionary *record = focusRecord([self canInput], self.window.firstResponder == self);
    if ([record[@"kind"] isEqualToString:@"release"]) [self releaseInput];
    else [self emit:record];
}
- (void)windowChanged:(NSNotification *)notification { (void)notification; [self syncWindow]; }
- (void)viewWillMoveToWindow:(NSWindow *)window {
    if (self.window) [[NSNotificationCenter defaultCenter] removeObserver:self name:nil object:self.window];
    if (!window) { [self releaseInput]; [self emit:@{@"kind":@"window", @"visible":@NO, @"active":@NO}]; }
    [super viewWillMoveToWindow:window];
}
- (void)viewDidMoveToWindow {
    [super viewDidMoveToWindow];
    if (self.window) for (NSString *name in @[NSWindowDidChangeOcclusionStateNotification,
        NSWindowDidBecomeKeyNotification, NSWindowDidResignKeyNotification,
        NSWindowDidMiniaturizeNotification, NSWindowDidDeminiaturizeNotification])
        [[NSNotificationCenter defaultCenter] addObserver:self selector:@selector(windowChanged:) name:name object:self.window];
    [self syncWindow]; [self syncGeometry];
}
- (void)viewDidHide { [super viewDidHide]; [self releaseInput]; [self syncWindow]; }
- (void)viewDidUnhide { [super viewDidUnhide]; [self syncWindow]; }
- (void)syncWindow {
    BOOL visible = self.window && self.window.isVisible && !self.isHiddenOrHasHiddenAncestor &&
        (self.window.occlusionState & NSWindowOcclusionStateVisible) != 0;
    BOOL active = self.window && self.window.isKeyWindow && NSApp.isActive;
    [self emit:@{@"kind":@"window", @"visible":@(visible), @"active":@(active)}];
    // A key window can retain its first responder while inactive/hidden.
    // Re-admit that actual responder after recovery; no new click is needed.
    [self syncFocus];
    [self syncAppearance];
}
- (void)syncAppearance {
    [CATransaction begin]; [CATransaction setDisableActions:YES];
    self.layer.backgroundColor = NSColor.windowBackgroundColor.CGColor;
    self.layer.borderWidth = 1;
    self.layer.borderColor = ([self canInput] && self.window.firstResponder == self ?
        NSColor.keyboardFocusIndicatorColor : NSColor.separatorColor).CGColor;
    placeholder.foregroundColor = NSColor.secondaryLabelColor.CGColor;
    [CATransaction commit];
}
- (void)viewDidChangeEffectiveAppearance { [super viewDidChangeEffectiveAppearance]; [self syncAppearance]; }
- (void)viewDidChangeBackingProperties { [super viewDidChangeBackingProperties]; [self syncGeometry]; }
- (void)setFrameSize:(NSSize)size { [super setFrameSize:size]; [self syncGeometry]; }
- (void)setFrame:(NSRect)frame { [super setFrame:frame]; [self syncGeometry]; }
- (void)syncGeometry {
    if (!frameLayer) return;
    CGFloat scale = self.window.backingScaleFactor ?: 1;
    [CATransaction begin]; [CATransaction setDisableActions:YES];
    frameLayer.frame = self.bounds; frameLayer.contentsScale = scale;
    placeholder.contentsScale = scale;
    placeholder.frame = CGRectMake(10, MAX(0, (self.bounds.size.height - 34) / 2),
        MAX(0, self.bounds.size.width - 20), 34);
    [CATransaction commit];
    [self emit:@{@"kind":@"layout", @"width":@(self.bounds.size.width), @"height":@(self.bounds.size.height)}];
}
- (void)updateTrackingAreas {
    [super updateTrackingAreas];
    if (tracking) { [self removeTrackingArea:tracking]; [tracking release]; }
    tracking = [[NSTrackingArea alloc] initWithRect:NSZeroRect options:
        NSTrackingInVisibleRect | NSTrackingActiveInKeyWindow | NSTrackingMouseMoved |
        NSTrackingMouseEnteredAndExited owner:self userInfo:nil];
    [self addTrackingArea:tracking];
}
- (NSPoint)position:(NSEvent *)event { return [self convertPoint:event.locationInWindow fromView:nil]; }
- (void)mouseButton:(NSEvent *)event down:(BOOL)down {
    if (event.buttonNumber > 2 || (down && ![self canInput])) return;
    NSPoint point = [self position:event];
    BOOL focus = [self emit:buttonRecord(event.buttonNumber, down, point.x, point.y)];
    if (down && focus) [self.window makeFirstResponder:self];
}
- (void)mouseDown:(NSEvent *)event { [self mouseButton:event down:YES]; }
- (void)rightMouseDown:(NSEvent *)event { [self mouseButton:event down:YES]; }
- (void)otherMouseDown:(NSEvent *)event { [self mouseButton:event down:YES]; }
// AppKit sends drags and the eventual up to the original down recipient even
// outside its bounds. Rust clamps those coordinates and releases exactly once.
- (void)mouseUp:(NSEvent *)event { [self mouseButton:event down:NO]; }
- (void)rightMouseUp:(NSEvent *)event { [self mouseButton:event down:NO]; }
- (void)otherMouseUp:(NSEvent *)event { [self mouseButton:event down:NO]; }
- (void)mouseMoved:(NSEvent *)event {
    if (![self canInput]) return;
    NSPoint point = [self position:event];
    [self emit:@{@"kind":@"pointer_move", @"x":@(point.x), @"y":@(point.y)}];
}
- (void)mouseDragged:(NSEvent *)event { [self mouseMoved:event]; }
- (void)rightMouseDragged:(NSEvent *)event { [self mouseMoved:event]; }
- (void)otherMouseDragged:(NSEvent *)event { [self mouseMoved:event]; }
- (void)scrollWheel:(NSEvent *)event {
    if (![self canInput]) return;
    NSPoint point = [self position:event];
    [self emit:scrollRecord(point.x, point.y, event.scrollingDeltaX, event.scrollingDeltaY, event.hasPreciseScrollingDeltas)];
}
- (void)keyDown:(NSEvent *)event {
    if (![self canInput] || self.window.firstResponder != self) return;
    BOOL textKey = NO; NSString *key = previewKey(event, &textKey);
    NSDictionary *changed = changedHeldKey(heldKeys, @(event.keyCode), key, textKey);
    if (changed) [self emit:changed];
    if (!textKey) heldKeys[@(event.keyCode)] = @{@"key":key, @"text_key":@NO};
    if ([key isEqualToString:@"cmd-v"] && !capture) {
        NSString *text = [NSPasteboard.generalPasteboard stringForType:NSPasteboardTypeString];
        if (text) { [self emit:@{@"kind":@"key", @"key":key, @"text_key":@NO, @"paste":text}]; return; }
    }
    // An IME may consume a navigation key while composing. Its marked text is
    // local; only insertText commits to the preview.
    if (textKey || (marked.length && !(event.modifierFlags & (NSEventModifierFlagCommand | NSEventModifierFlagControl)))) {
        NSDictionary *previousKey = interpretedKey;
        BOOL previousSent = interpretedCommandSent;
        interpretedKey = [@{@"key":key, @"text_key":@(textKey)} retain];
        interpretedCommandSent = NO;
        @try { [self interpretKeyEvents:@[event]]; }
        @finally {
            [interpretedKey release]; interpretedKey = previousKey;
            interpretedCommandSent = previousSent;
        }
        return;
    }
    [self emit:@{@"kind":@"key", @"key":key, @"text_key":@(textKey)}];
}
- (void)keyUp:(NSEvent *)event {
    BOOL textKey = NO; NSString *key = previewKey(event, &textKey);
    [self emit:keyUpRecord(heldKeys, @(event.keyCode), key, textKey)];
}
- (BOOL)performKeyEquivalent:(NSEvent *)event {
    if (![self canInput] || self.window.firstResponder != self) return NO;
    BOOL textKey = NO; NSString *key = previewKey(event, &textKey);
    if ([@[@"cmd-a", @"cmd-c", @"cmd-x", @"cmd-v", @"cmd-z", @"cmd-shift-z"] containsObject:key]) {
        [self keyDown:event]; return YES;
    }
    return NO;
}
- (void)menuKey:(NSString *)key {
    if (![self canInput] || self.window.firstResponder != self) return;
    [self emit:@{@"kind":@"key", @"key":key, @"text_key":@NO}];
    [self emit:@{@"kind":@"key_up", @"key":key, @"text_key":@NO}];
}
- (void)copy:(id)sender { (void)sender; [self menuKey:@"cmd-c"]; }
- (void)cut:(id)sender { (void)sender; [self menuKey:@"cmd-x"]; }
- (void)selectAll:(id)sender { (void)sender; [self menuKey:@"cmd-a"]; }
- (void)undo:(id)sender { (void)sender; [self menuKey:@"cmd-z"]; }
- (void)redo:(id)sender { (void)sender; [self menuKey:@"cmd-shift-z"]; }
- (void)paste:(id)sender {
    (void)sender;
    if (![self canInput] || self.window.firstResponder != self) return;
    NSString *text = capture ? nil : [NSPasteboard.generalPasteboard stringForType:NSPasteboardTypeString];
    if (text) [self emit:@{@"kind":@"key", @"key":@"cmd-v", @"text_key":@NO, @"paste":text}];
    else [self menuKey:@"cmd-v"];
}
- (void)insertText:(id)value replacementRange:(NSRange)range {
    (void)range;
    NSDictionary *record = commitRecord(value);
    if ([self canInput] && self.window.firstResponder == self && record) [self emit:record];
    [self unmarkText];
}
- (void)setMarkedText:(id)value selectedRange:(NSRange)selected replacementRange:(NSRange)replacement {
    (void)replacement;
    if (![self canInput] || self.window.firstResponder != self) return;
    markedSelection = markText(marked, value, selected);
}
- (void)unmarkText { [marked deleteCharactersInRange:NSMakeRange(0, marked.length)]; markedSelection = NSMakeRange(0, 0); }
- (BOOL)hasMarkedText { return marked.length > 0; }
- (NSRange)markedRange { return marked.length ? NSMakeRange(0, marked.length) : NSMakeRange(NSNotFound, 0); }
- (NSRange)selectedRange { return markedSelection; }
- (NSArray *)validAttributesForMarkedText { return @[]; }
- (NSAttributedString *)attributedSubstringForProposedRange:(NSRange)range actualRange:(NSRangePointer)actual {
    if (range.location == NSNotFound || range.location > marked.length) return nil;
    NSRange available = NSIntersectionRange(range, NSMakeRange(0, marked.length));
    if (actual) *actual = available;
    return [marked attributedSubstringFromRange:available];
}
- (NSUInteger)characterIndexForPoint:(NSPoint)point { (void)point; return 0; }
- (NSRect)firstRectForCharacterRange:(NSRange)range actualRange:(NSRangePointer)actual {
    (void)range; if (actual) *actual = self.markedRange;
    NSRect bounds = [self convertRect:self.bounds toView:nil];
    return self.window ? [self.window convertRectToScreen:bounds] : bounds;
}
- (void)doCommandBySelector:(SEL)selector {
    (void)selector;
    NSDictionary *record = interpretedCommandRecord(interpretedKey, &interpretedCommandSent);
    if (record && [self canInput] && self.window.firstResponder == self) [self emit:record];
}
@end

void *rw_preview_new(void *context, RWPreviewEvent event, RWPreviewRelease release) {
    NSCAssert(NSThread.isMainThread, @"preview construction must be on the main thread");
    RWPreviewView *view = [[RWPreviewView alloc] initWithFrame:NSZeroRect];
    view->context = context; view->eventCallback = event; view->releaseCallback = release;
    return view;
}
void rw_preview_visible(void *raw, bool visible) {
    RWPreviewView *view = raw; view->logicalVisible = visible;
    if (!visible) [view releaseInput];
    [view syncWindow]; [view syncGeometry];
}
void rw_preview_state(void *raw, bool running, bool enabled, bool capture) {
    RWPreviewView *view = raw;
    view->running = running; view->enabled = enabled; view->capture = capture;
    [view syncFocus];
    view->placeholder.string = running ? @"Waiting for the app’s first frame…" : @"Configure your app, then choose Run.";
    [view syncAppearance];
}
static CGImageRef previewImage(const unsigned char *bytes, size_t length,
    unsigned int width, unsigned int height, void *owner, RWPreviewPixelsRelease release) {
    CGImageRef image = NULL;
    if (bytes && width && height && width <= 2048 && height <= 2048 && length == (size_t)width * height * 4) {
        CGDataProviderRef provider = CGDataProviderCreateWithData(owner, bytes, length, release);
        if (provider) {
            CGColorSpaceRef colors = CGColorSpaceCreateDeviceRGB();
            image = CGImageCreate(width, height, 8, 32, (size_t)width * 4, colors,
                kCGImageAlphaFirst | kCGBitmapByteOrder32Little, provider, NULL, false, kCGRenderingIntentDefault);
            CGColorSpaceRelease(colors); CGDataProviderRelease(provider);
        } else if (release) release(owner, bytes, length);
    } else if (release) release(owner, bytes, length);
    return image;
}
void rw_preview_frame(void *raw, const unsigned char *bytes, size_t length,
    unsigned int width, unsigned int height, void *owner, RWPreviewPixelsRelease release) {
    RWPreviewView *view = raw;
    CGImageRef image = previewImage(bytes, length, width, height, owner, release);
    [CATransaction begin]; [CATransaction setDisableActions:YES];
    view->frameLayer.contents = (id)image;
    view->placeholder.hidden = image != NULL;
    [CATransaction commit];
    if (image) CGImageRelease(image);
}
void rw_preview_shutdown(void *raw) {
    RWPreviewView *view = raw;
    [view releaseInput]; view->stopped = YES; view->enabled = NO;
    view->frameLayer.contents = nil;
    if (view->keyUpMonitor) {
        [NSEvent removeMonitor:view->keyUpMonitor]; [view->keyUpMonitor release]; view->keyUpMonitor = nil;
    }
    [[NSNotificationCenter defaultCenter] removeObserver:view];
    if (view.window.firstResponder == view) [view.window makeFirstResponder:nil];
}

// Called only by Rust unit fixtures. These functions use Foundation values and
// CGImage/CGDataProvider; they never instantiate an NSView, NSWindow, NSEvent,
// NSApplication, CALayer or access the current keyboard/clipboard/window state.
static void fixturePixelsRelease(void *owner, const void *bytes, size_t length) {
    (void)bytes; (void)length; (*(unsigned int *)owner)++;
}
// A per-event key like previewKey's. Its deallocation is recorded but its
// memory is kept until the fixture disposes it, so a record that outlives the
// key's last owner is detected deterministically instead of reading reused memory.
@interface RWFixtureKey : NSString {
@public
    NSString *text;
    BOOL deallocated;
}
@end
@implementation RWFixtureKey
- (NSUInteger)length { return text.length; }
- (unichar)characterAtIndex:(NSUInteger)index { return [text characterAtIndex:index]; }
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wobjc-missing-super-calls"
- (void)dealloc { deallocated = YES; }
#pragma clang diagnostic pop
@end
char *rw_preview_fixture(const char *json) {
    @autoreleasepool {
        NSData *data = [NSData dataWithBytes:json length:strlen(json)];
        NSDictionary *input = [NSJSONSerialization JSONObjectWithData:data options:0 error:nil];
        id result = nil;
        NSString *operation = input[@"operation"];
        if ([operation isEqualToString:@"key"]) {
            NSEventModifierFlags flags = 0;
            for (NSString *modifier in input[@"modifiers"]) {
                if ([modifier isEqualToString:@"shift"]) flags |= NSEventModifierFlagShift;
                if ([modifier isEqualToString:@"cmd"]) flags |= NSEventModifierFlagCommand;
                if ([modifier isEqualToString:@"ctrl"]) flags |= NSEventModifierFlagControl;
                if ([modifier isEqualToString:@"alt"]) flags |= NSEventModifierFlagOption;
                if ([modifier isEqualToString:@"fn"]) flags |= NSEventModifierFlagFunction;
            }
            NSString *plain = input[@"plain"] ?: input[@"characters"] ?: @"";
            NSString *shifted = input[@"shifted"] ?: plain.uppercaseString;
            NSString *cmd = input[@"cmd"] ?: plain;
            NSString *both = input[@"both"] ?: cmd.uppercaseString;
            BOOL textKey = NO;
            NSString *key = keyFromFields(input[@"characters"] ?: @"", input[@"typed"] ?: @"", flags,
                plain, shifted, cmd, both, [input[@"commandLayout"] boolValue], &textKey);
            result = @{@"key":key, @"text_key":@(textKey)};
        } else if ([operation isEqualToString:@"pointer"]) {
            result = buttonRecord([input[@"button"] integerValue], [input[@"down"] boolValue],
                [input[@"x"] doubleValue], [input[@"y"] doubleValue]) ?: NSNull.null;
        } else if ([operation isEqualToString:@"scroll"]) {
            result = scrollRecord([input[@"x"] doubleValue], [input[@"y"] doubleValue],
                [input[@"dx"] doubleValue], [input[@"dy"] doubleValue], [input[@"precise"] boolValue]);
        } else if ([operation isEqualToString:@"keyup"]) {
            NSMutableDictionary *held = [NSMutableDictionary dictionaryWithDictionary:
                @{@(42):@{@"key":@"cmd-shift-a", @"text_key":@NO}}];
            NSDictionary *record = keyUpRecord(held, @(42), @"a", YES);
            result = @{@"record":record, @"held":@(held.count)};
        } else if ([operation isEqualToString:@"repeat"]) {
            NSMutableDictionary *held = [NSMutableDictionary dictionaryWithDictionary:
                @{@(42):@{@"key":@"cmd-shift-a", @"text_key":@NO}}];
            NSDictionary *unchanged = changedHeldKey(held, @(42), @"cmd-shift-a", NO);
            NSDictionary *changed = changedHeldKey(held, @(42), @"a", YES);
            result = @{@"unchanged":unchanged ?: NSNull.null, @"changed":changed, @"held":@(held.count)};
        } else if ([operation isEqualToString:@"keyup_dynamic"]) {
            // Once the key-down autorelease pool drains, the held entry is the
            // key's only owner. Key-up and a changed repeat must both emit a
            // record that owns the key it read from that entry.
            RWFixtureKey *up = [[RWFixtureKey alloc] init], *repeat = [[RWFixtureKey alloc] init];
            up->text = @"cmd-shift-b"; repeat->text = @"cmd-shift-c";
            NSMutableDictionary *held = [NSMutableDictionary dictionary];
            @autoreleasepool {
                held[@(11)] = @{@"key":up, @"text_key":@NO};
                held[@(8)] = @{@"key":repeat, @"text_key":@NO};
            }
            [up release]; [repeat release];
            BOOL upOwned, repeatOwned;
            @autoreleasepool {
                NSDictionary *upRecord = keyUpRecord(held, @(11), @"b", YES);
                NSDictionary *repeatRecord = changedHeldKey(held, @(8), @"c", YES);
                upOwned = !up->deallocated && [upRecord[@"key"] isEqualToString:@"cmd-shift-b"] &&
                    ![upRecord[@"text_key"] boolValue];
                repeatOwned = !repeat->deallocated && [repeatRecord[@"key"] isEqualToString:@"cmd-shift-c"] &&
                    ![repeatRecord[@"text_key"] boolValue];
            }
            // Both records are gone, so the keys must be too; only now free them.
            BOOL released = up->deallocated && repeat->deallocated;
            object_dispose(up); object_dispose(repeat);
            result = @{@"up_owned":@(upOwned), @"repeat_owned":@(repeatOwned), @"released":@(released),
                @"held":@(held.count)};
        } else if ([operation isEqualToString:@"ime"]) {
            NSMutableAttributedString *marked = [[[NSMutableAttributedString alloc] initWithString:@""] autorelease];
            NSRange selection = markText(marked, @"にほん😀", NSMakeRange(99, 99));
            NSUInteger markedLength = marked.length;
            NSDictionary *commit = commitRecord([[[NSAttributedString alloc] initWithString:@"日本😀"] autorelease]);
            [marked deleteCharactersInRange:NSMakeRange(0, marked.length)];
            result = @{@"marked_utf16":@(markedLength), @"selection_start":@(selection.location),
                @"selection_length":@(selection.length), @"commit":commit, @"marked_after":@(marked.length)};
        } else if ([operation isEqualToString:@"ime_command"]) {
            NSDictionary *key = @{@"key":input[@"key"] ?: @"left", @"text_key":input[@"text_key"] ?: @NO};
            NSMutableArray *events = [NSMutableArray array];
            BOOL sent = NO;
            if (![input[@"consumed"] boolValue]) for (NSUInteger i = 0; i < 3; i++) {
                NSDictionary *event = interpretedCommandRecord(key, &sent);
                if (event) [events addObject:event];
            }
            // The synchronous call returned: its remembered key no longer exists.
            NSDictionary *after = interpretedCommandRecord(nil, &sent);
            result = @{@"events":events, @"after_return":after ?: NSNull.null};
        } else if ([operation isEqualToString:@"key_up_monitor"]) {
            result = @{@"consume":@(ownsKeyUp([input[@"stopped"] boolValue], [input[@"same_window"] boolValue],
                [input[@"first_responder"] boolValue], [input[@"held"] boolValue]))};
        } else if ([operation isEqualToString:@"focus"]) {
            result = focusRecord([input[@"can_input"] boolValue], [input[@"first_responder"] boolValue]);
        } else if ([operation isEqualToString:@"image"]) {
            const unsigned char pixels[] = {0x10, 0x20, 0x30, 0xff, 0x40, 0x50, 0x60, 0x80};
            unsigned int released = 0;
            CGImageRef image = previewImage(pixels, sizeof(pixels), 2, 1, &released, fixturePixelsRelease);
            BOOL valid = image && CGImageGetWidth(image) == 2 && CGImageGetHeight(image) == 1 &&
                CGImageGetBytesPerRow(image) == 8 && CGImageGetAlphaInfo(image) == kCGImageAlphaFirst;
            BOOL identical = NO;
            if (image) {
                CFDataRef bytes = CGDataProviderCopyData(CGImageGetDataProvider(image));
                identical = bytes && CFDataGetLength(bytes) == sizeof(pixels) &&
                    memcmp(CFDataGetBytePtr(bytes), pixels, sizeof(pixels)) == 0;
                if (bytes) CFRelease(bytes);
            }
            unsigned int before = released;
            if (image) CGImageRelease(image);
            unsigned int rejected = 0;
            CGImageRef invalid = previewImage(pixels, 3, 2, 1, &rejected, fixturePixelsRelease);
            if (invalid) CGImageRelease(invalid);
            result = @{@"valid":@(valid), @"identical_bgra":@(identical), @"release_before":@(before),
                @"release_after":@(released), @"invalid_releases":@(rejected)};
        }
        if (!result) result = @{@"error":@"unknown fixture operation"};
        NSData *encoded = [NSJSONSerialization dataWithJSONObject:result options:NSJSONWritingFragmentsAllowed error:nil];
        NSString *output = [[[NSString alloc] initWithData:encoded encoding:NSUTF8StringEncoding] autorelease];
        return strdup(output.UTF8String);
    }
}
void rw_preview_fixture_free(char *value) { free(value); }
