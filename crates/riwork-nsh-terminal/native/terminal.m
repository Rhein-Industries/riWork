// AppKit host for the pinned Ghostty C ABI. No GPUI event or rendering layer.
#import "terminal.h"
#import <AppKit/AppKit.h>
#import <Carbon/Carbon.h>
#import <QuartzCore/QuartzCore.h>
#import <ghostty.h>
#import <stdatomic.h>
#import <time.h>
#import <math.h>
#import <string.h>
#ifdef RIWORK_TERMINAL_FIXTURE
#import <IOSurface/IOSurface.h>
#endif

static size_t pendingDisposals;
static double monotonicSeconds(void) {
    struct timespec now; clock_gettime(CLOCK_MONOTONIC, &now);
    return now.tv_sec + now.tv_nsec / 1e9;
}
static void assertMainThread(void) { NSCAssert(NSThread.isMainThread, @"terminal lifecycle is main-thread-only"); }
#ifdef RIWORK_TERMINAL_FIXTURE
// Inspect the renderer-owned buffer, not another app or the desktop. This
// distinguishes text rendering from an AppKit layer-composition failure.
static NSDictionary *fixtureRendererContents(CALayer *layer) {
    id contents = layer.contents;
    if (!contents || CFGetTypeID((CFTypeRef)contents) != IOSurfaceGetTypeID())
        return @{@"iosurface":@NO};
    IOSurfaceRef pixels = (IOSurfaceRef)contents;
    size_t width = IOSurfaceGetWidth(pixels), height = IOSurfaceGetHeight(pixels);
    size_t stride = IOSurfaceGetBytesPerRow(pixels), element = IOSurfaceGetBytesPerElement(pixels);
    if (element != 4 || IOSurfaceLock(pixels, kIOSurfaceLockReadOnly, NULL) != kIOReturnSuccess)
        return @{@"iosurface":@YES, @"width":@(width), @"height":@(height), @"readable":@NO};
    const uint8_t *base = IOSurfaceGetBaseAddress(pixels);
    uint32_t first = 0; size_t samples = 0, different = 0;
    if (base && width && height) {
        memcpy(&first, base, 4);
        for (size_t y = 0; y < height; y += 3) for (size_t x = 0; x < width; x += 3) {
            uint32_t pixel; memcpy(&pixel, base+y*stride+x*4, 4);
            samples++; if (pixel != first) different++;
        }
    }
    IOSurfaceUnlock(pixels, kIOSurfaceLockReadOnly, NULL);
    return @{@"iosurface":@YES, @"width":@(width), @"height":@(height), @"readable":@(base != NULL),
        @"firstPixel":@(first), @"samples":@(samples), @"differentPixels":@(different)};
}
#endif

@interface RWTerminalView : NSView <NSTextInputClient> {
@public
    ghostty_app_t app;
    ghostty_config_t config;
    ghostty_surface_t surface;
    void *context;
    rw_terminal_event_cb eventCallback;
    rw_terminal_detach_cb detachCallback;
    rw_terminal_context_release_cb contextRelease;
    dispatch_source_t wakeSource;
    _Atomic bool dirty;
    _Atomic uint64_t wakeCount;
    uint64_t renderRequests, drawCalls, appTicks;
    uint64_t visibilityUpdates;
    BOOL lastShown, visibilityKnown, focusKnown, lastAppFocus, lastSurfaceFocus;
    NSDictionary *gridOriginDetails;
    NSTimer *closingTimer;
    NSMutableAttributedString *marked;
    NSMutableString *committed;
    NSRange markedSelection;
    NSTrackingArea *tracking;
    CAShapeLayer *linkLayer;
    NSArray *linkRows;
    NSCursor *terminalCursor;
    uint64_t hoverRequest;
    NSInteger hoverRow, hoverColumn;
    NSEventModifierFlags lastModifiers;
    double linkAnswerTime, swallowedClickTime;
    double gridOriginX, gridOriginY;
    BOOL gridOriginValid;
    BOOL interpreting, visible, frozen, focusOnAttach, abandoned, settingFrame;
    BOOL closing, detachInFlight, detachConfirmed, parked, endedReported;
    BOOL swallowedClick;
    double shutdownStart, lastDetachAttempt;
}
- (BOOL)attachCommand:(const char *)command cwd:(const char *)cwd config:(const char *)path;
- (void)emit:(NSString *)name fields:(NSDictionary *)fields;
- (void)diagnostic:(NSString *)phase fields:(NSDictionary *)fields;
- (void)tick;
- (void)syncGeometry;
- (void)syncVisibility;
- (void)syncAppearance;
- (BOOL)allowsInput;
- (void)shutdown;
- (void)finishShutdown;
- (void)requestDetach;
- (void)clearLink;
- (void)drawLink;
- (void)keyboardChanged:(NSNotification *)notification;
@end

static ghostty_input_mods_e mods(NSEventModifierFlags flags) {
    unsigned result = 0;
    if (flags & NSEventModifierFlagShift) result |= GHOSTTY_MODS_SHIFT;
    if (flags & NSEventModifierFlagControl) result |= GHOSTTY_MODS_CTRL;
    if (flags & NSEventModifierFlagOption) result |= GHOSTTY_MODS_ALT;
    if (flags & NSEventModifierFlagCommand) result |= GHOSTTY_MODS_SUPER;
    if (flags & NSEventModifierFlagCapsLock) result |= GHOSTTY_MODS_CAPS;
    // Device-dependent flag bits are defined by IOKit's ev_keymap ABI.
    if (flags & 0x00000004) result |= GHOSTTY_MODS_SHIFT_RIGHT;
    if (flags & 0x00002000) result |= GHOSTTY_MODS_CTRL_RIGHT;
    if (flags & 0x00000040) result |= GHOSTTY_MODS_ALT_RIGHT;
    if (flags & 0x00000010) result |= GHOSTTY_MODS_SUPER_RIGHT;
    return (ghostty_input_mods_e)result;
}
static BOOL linkGesture(NSEventModifierFlags flags) {
    NSEventModifierFlags mask = NSEventModifierFlagCommand | NSEventModifierFlagShift |
        NSEventModifierFlagOption | NSEventModifierFlagControl | NSEventModifierFlagFunction;
    NSEventModifierFlags value = flags & mask;
    return value == NSEventModifierFlagCommand || value == (NSEventModifierFlagCommand | NSEventModifierFlagShift);
}
static void wakeup(void *userdata) {
    RWTerminalView *view = userdata;
    atomic_store(&view->dirty, true); atomic_fetch_add(&view->wakeCount, 1);
    // DATA_ADD coalesces output bursts and sleeps completely when idle. The
    // context remains alive until app_free has joined the callback producers.
    dispatch_source_merge_data(view->wakeSource, 1);
}
static void drainWake(void *userdata) { [(RWTerminalView *)userdata tick]; }
static bool action(ghostty_app_t app, ghostty_target_s target, ghostty_action_s action) {
    RWTerminalView *view = ghostty_app_userdata(app);
    if (action.tag == GHOSTTY_ACTION_RELOAD_CONFIG && action.action.reload_config.soft && view->config) {
        // Color-scheme changes update conditional state, then ask the embedder
        // to replay the parsed config. Includes/themes remain process-cached.
        if (target.tag == GHOSTTY_TARGET_APP) ghostty_app_update_config(app, view->config);
        else if (target.tag == GHOSTTY_TARGET_SURFACE) ghostty_surface_update_config(target.target.surface, view->config);
        return true;
    }
    if (action.tag == GHOSTTY_ACTION_RENDER && target.tag == GHOSTTY_TARGET_SURFACE) {
        view->renderRequests++;
        if (!view->frozen && view->visible && !view->closing) { ghostty_surface_draw(target.target.surface); view->drawCalls++; }
        return true;
    }
    if (action.tag == GHOSTTY_ACTION_CELL_SIZE) { view->gridOriginValid = NO; [view clearLink]; return true; }
    if (action.tag == GHOSTTY_ACTION_SHOW_CHILD_EXITED) {
        if (!view->closing && !view->endedReported) {
            view->endedReported = YES;
            [view emit:@"ended" fields:@{}];
        }
        return true;
    }
    if (action.tag == GHOSTTY_ACTION_MOUSE_SHAPE) {
        NSCursor *cursor = NSCursor.IBeamCursor;
        switch (action.action.mouse_shape) {
            case GHOSTTY_MOUSE_SHAPE_POINTER: cursor = NSCursor.pointingHandCursor; break;
            case GHOSTTY_MOUSE_SHAPE_CROSSHAIR: cursor = NSCursor.crosshairCursor; break;
            case GHOSTTY_MOUSE_SHAPE_DEFAULT: cursor = NSCursor.arrowCursor; break;
            case GHOSTTY_MOUSE_SHAPE_NOT_ALLOWED: cursor = NSCursor.operationNotAllowedCursor; break;
            default: break;
        }
        [view->terminalCursor release]; view->terminalCursor = [cursor retain];
        return true;
    }
    // RiWork's tmux-capture link resolver owns URL validation and routing.
    if (action.tag == GHOSTTY_ACTION_OPEN_URL) return true;
    return false;
}
// Clipboard reads (including OSC52) and confirm-required writes are refused.
// Paste/drop routing is deliberately a seam for the following slice.
static bool readClipboard(void *userdata, ghostty_clipboard_e clipboard, void *request) {
    (void)userdata; (void)clipboard; (void)request; return false;
}
static void confirmClipboard(void *userdata, const char *text, void *request, ghostty_clipboard_request_e kind) {
    (void)text; (void)kind;
    RWTerminalView *view = userdata;
    if (view->surface) ghostty_surface_complete_clipboard_request(view->surface, "", request, true);
}
static void writeClipboard(void *userdata, ghostty_clipboard_e clipboard,
    const ghostty_clipboard_content_s *content, size_t count, bool confirm) {
    (void)userdata;
    if (confirm || clipboard != GHOSTTY_CLIPBOARD_STANDARD) return;
    for (size_t i = 0; i < count; i++) {
        if (content[i].mime && strcmp(content[i].mime, "text/plain") == 0 && content[i].data) {
            NSString *text = [NSString stringWithUTF8String:content[i].data];
            if (text) { [NSPasteboard.generalPasteboard clearContents];
                [NSPasteboard.generalPasteboard setString:text forType:NSPasteboardTypeString]; }
            break;
        }
    }
}
static void closeSurface(void *userdata, bool alive) {
    RWTerminalView *view = userdata;
    if (!alive && !view->closing && !view->endedReported) {
        view->endedReported = YES; [view emit:@"ended" fields:@{}];
    }
}
static ghostty_config_t loadConfig(const char *path) {
    if (!path || !path[0]) return NULL;
    NSString *base = [NSString stringWithUTF8String:path];
    NSString *override = [[base stringByDeletingPathExtension] stringByAppendingPathExtension:@"override.conf"];
    NSString *contents = [NSString stringWithContentsOfFile:base encoding:NSUTF8StringEncoding error:NULL];
    NSString *overrides = [NSString stringWithContentsOfFile:override encoding:NSUTF8StringEncoding error:NULL] ?: @"";
    if (!contents) return NULL;
    // Rust includes a process config revision in the generated text. Identical
    // surfaces share one parser result; each surface receives an owned clone.
    // The directory participates because relative includes resolve against it.
    NSString *key = [NSString stringWithFormat:@"%@\n%@\n%@", base.stringByDeletingLastPathComponent, contents, overrides];
    static NSMutableDictionary *cache;
    static NSMutableArray *cacheOrder;
    if (!cache) { cache = [[NSMutableDictionary alloc] init]; cacheOrder = [[NSMutableArray alloc] init]; }
    NSValue *existing = cache[key];
    if (existing) return ghostty_config_clone(existing.pointerValue);
    ghostty_config_t value = ghostty_config_new();
    if (!value) return NULL;
    ghostty_config_load_file(value, path);
    ghostty_config_load_recursive_files(value);
    if (overrides.length) ghostty_config_load_file(value, override.fileSystemRepresentation);
    ghostty_config_finalize(value);
    if (ghostty_config_diagnostics_count(value)) { ghostty_config_free(value); return NULL; }
    if (cacheOrder.count == 8) {
        NSString *oldest = cacheOrder.firstObject;
        ghostty_config_free([cache[oldest] pointerValue]);
        [cache removeObjectForKey:oldest]; [cacheOrder removeObjectAtIndex:0];
    }
    cache[key] = [NSValue valueWithPointer:value]; [cacheOrder addObject:key];
    return ghostty_config_clone(value);
}

@implementation RWTerminalView
- (instancetype)initWithFrame:(NSRect)frame {
    self = [super initWithFrame:frame];
    if (!self) return nil;
    visible = YES;
    marked = [[NSMutableAttributedString alloc] initWithString:@""];
    committed = [[NSMutableString alloc] init];
    markedSelection = NSMakeRange(0, 0);
    hoverRow = hoverColumn = -1;
    terminalCursor = [NSCursor.IBeamCursor retain];
    atomic_init(&dirty, false); atomic_init(&wakeCount, 0);
    // Ghostty installs its IOSurfaceLayer before enabling wantsLayer so this
    // becomes a layer-hosting view. An eager backing layer here lets AppKit own
    // its contents and can erase the renderer's frames.
    wakeSource = dispatch_source_create(DISPATCH_SOURCE_TYPE_DATA_ADD, 0, 0, dispatch_get_main_queue());
    dispatch_set_context(wakeSource, self);
    dispatch_source_set_event_handler_f(wakeSource, drainWake);
    dispatch_resume(wakeSource);
    [[NSDistributedNotificationCenter defaultCenter] addObserver:self selector:@selector(keyboardChanged:)
        name:(NSString *)kTISNotifySelectedKeyboardInputSourceChanged object:nil];
    return self;
}
- (void)emit:(NSString *)name fields:(NSDictionary *)fields {
    if (!eventCallback || (abandoned && ![name isEqualToString:@"disposed"] && ![name isEqualToString:@"teardown"])) return;
    NSData *json = [NSJSONSerialization dataWithJSONObject:fields options:0 error:NULL];
    if (!json) return;
    NSString *text = [[[NSString alloc] initWithData:json encoding:NSUTF8StringEncoding] autorelease];
    eventCallback(context, name.UTF8String, text.UTF8String);
}
- (void)diagnostic:(NSString *)phase fields:(NSDictionary *)fields {
    NSMutableDictionary *entry = [NSMutableDictionary dictionaryWithDictionary:fields];
    entry[@"phase"] = phase; entry[@"mainThread"] = @(NSThread.isMainThread);
    entry[@"monotonicMs"] = @(monotonicSeconds() * 1000);
    [self emit:@"teardown" fields:entry];
}
- (BOOL)attachCommand:(const char *)command cwd:(const char *)cwd config:(const char *)path {
    assertMainThread();
    if (abandoned || closing || surface || !command || !cwd) return NO;
    static dispatch_once_t initializedOnce;
    static int initialized = -1;
    dispatch_once(&initializedOnce, ^{ initialized = ghostty_init(0, NULL); });
    if (initialized != GHOSTTY_SUCCESS) return NO;
    config = loadConfig(path);
    if (!config) return NO;
    ghostty_runtime_config_s runtime = {
        .userdata = self, .supports_selection_clipboard = false,
        .wakeup_cb = wakeup, .action_cb = action,
        .read_clipboard_cb = readClipboard, .confirm_read_clipboard_cb = confirmClipboard,
        .write_clipboard_cb = writeClipboard, .close_surface_cb = closeSurface,
    };
    app = ghostty_app_new(&runtime, config);
    if (!app) { ghostty_config_free(config); config = NULL; return NO; }
    ghostty_surface_config_s options = ghostty_surface_config_new();
    options.platform_tag = GHOSTTY_PLATFORM_MACOS;
    options.platform.macos.nsview = self; options.userdata = self;
    options.scale_factor = self.window.backingScaleFactor ?: NSScreen.mainScreen.backingScaleFactor ?: 1;
    options.command = command; options.working_directory = cwd;
    ghostty_env_var_s environment[] = {
        {"TERM", "xterm-256color"}, {"COLORTERM", "truecolor"},
        {"TERM_PROGRAM", "riwork"}, {"GHOSTTY_QUIET_LOGIN", "1"},
    };
    options.env_vars = environment; options.env_var_count = sizeof(environment) / sizeof(environment[0]);
    // NSH may have inserted this idle view below a layer-backed ancestor, or a
    // previous surface may have left its layer behind. Reset only while there
    // is no renderer, then let Ghostty establish its layer-hosting contract.
    self.wantsLayer = NO;
    self.layer = nil;
    surface = ghostty_surface_new(app, &options);
    if (!surface) { ghostty_app_free(app); app = NULL; ghostty_config_free(config); config = NULL; return NO; }
    visibilityKnown = focusKnown = NO;
    endedReported = NO;
    [self syncAppearance]; [self syncGeometry]; [self syncVisibility];
    [self diagnostic:@"created" fields:@{}];
    [self emit:@"ready" fields:@{}];
    if (focusOnAttach && visible && !frozen) [self.window makeFirstResponder:self];
    atomic_store(&dirty, true); dispatch_source_merge_data(wakeSource, 1);
    return YES;
}
- (BOOL)isFlipped { return YES; }
- (BOOL)acceptsFirstResponder { return !abandoned && visible && !frozen && !closing; }
- (BOOL)acceptsFirstMouse:(NSEvent *)event { (void)event; return YES; }
- (BOOL)allowsInput { return surface && !closing && !abandoned && visible && !frozen; }
- (NSView *)hitTest:(NSPoint)point { return frozen || !visible || closing ? nil : [super hitTest:point]; }
- (BOOL)becomeFirstResponder {
    if (![self acceptsFirstResponder]) return NO;
    if (surface) {
        lastAppFocus = self.window.isKeyWindow; lastSurfaceFocus = YES; focusKnown = YES;
        ghostty_app_set_focus(app, lastAppFocus); ghostty_surface_set_focus(surface, true);
    }
    [self emit:@"focusChange" fields:@{@"focused":@YES}]; return YES;
}
- (BOOL)resignFirstResponder {
    if (surface) { lastSurfaceFocus = NO; ghostty_surface_set_focus(surface, false); }
    [self emit:@"focusChange" fields:@{@"focused":@NO}]; return YES;
}
- (BOOL)isAccessibilityElement { return YES; }
- (NSString *)accessibilityRole { return NSAccessibilityGroupRole; }
- (NSString *)accessibilityLabel { return @"RiWork terminal"; }
- (void)keyboardChanged:(NSNotification *)notification { (void)notification; if (app) ghostty_app_keyboard_changed(app); }
- (void)windowChanged:(NSNotification *)notification {
    if ([notification.name isEqualToString:NSWindowDidChangeScreenNotification]) [self syncGeometry];
    else [self syncVisibility];
}
- (void)viewWillMoveToWindow:(NSWindow *)window {
    if (self.window) [[NSNotificationCenter defaultCenter] removeObserver:self name:nil object:self.window];
    [super viewWillMoveToWindow:window];
}
- (void)viewDidMoveToWindow {
    [super viewDidMoveToWindow];
    if (self.window) {
        for (NSString *name in @[NSWindowDidChangeOcclusionStateNotification, NSWindowDidBecomeKeyNotification,
            NSWindowDidResignKeyNotification, NSWindowDidChangeScreenNotification]) {
            [[NSNotificationCenter defaultCenter] addObserver:self selector:@selector(windowChanged:) name:name object:self.window];
        }
    }
    [self syncGeometry]; [self syncVisibility];
    if (focusOnAttach && [self allowsInput] && self.window) [self.window makeFirstResponder:self];
}
- (void)viewDidChangeBackingProperties { [super viewDidChangeBackingProperties]; [self syncGeometry]; }
- (void)setFrameSize:(NSSize)size {
    NSSize previous = self.frame.size;
    [super setFrameSize:size];
    if (!settingFrame && !NSEqualSizes(previous, self.frame.size)) [self syncGeometry];
}
- (void)setFrame:(NSRect)frame {
    NSSize previous = self.frame.size;
    BOOL wasSettingFrame = settingFrame;
    settingFrame = YES;
    [super setFrame:frame];
    settingFrame = wasSettingFrame;
    if (!settingFrame && !NSEqualSizes(previous, self.frame.size)) [self syncGeometry];
}
- (void)syncAppearance {
    if (!app || !surface || closing) return;
    NSString *appearance = [self.effectiveAppearance bestMatchFromAppearancesWithNames:@[NSAppearanceNameAqua, NSAppearanceNameDarkAqua]];
    ghostty_color_scheme_e scheme = [appearance isEqualToString:NSAppearanceNameDarkAqua] ? GHOSTTY_COLOR_SCHEME_DARK : GHOSTTY_COLOR_SCHEME_LIGHT;
    ghostty_app_set_color_scheme(app, scheme);
    ghostty_surface_set_color_scheme(surface, scheme);
}
- (void)viewDidChangeEffectiveAppearance { [super viewDidChangeEffectiveAppearance]; [self syncAppearance]; }
- (void)syncVisibility {
    if (!surface) return;
    BOOL shown = visible && !frozen && !closing && self.window &&
        (self.window.occlusionState & NSWindowOcclusionStateVisible) != 0;
    if (!visibilityKnown || shown != lastShown) {
        lastShown = shown; visibilityKnown = YES; visibilityUpdates++;
        ghostty_surface_set_occlusion(surface, shown);
        if (shown) ghostty_surface_refresh(surface);
    }
    BOOL appFocus = self.window.isKeyWindow;
    BOOL surfaceFocus = shown && self.window.firstResponder == self;
    if (!focusKnown || appFocus != lastAppFocus) ghostty_app_set_focus(app, appFocus);
    if (!focusKnown || surfaceFocus != lastSurfaceFocus) ghostty_surface_set_focus(surface, surfaceFocus);
    lastAppFocus = appFocus; lastSurfaceFocus = surfaceFocus; focusKnown = YES;
}
- (void)syncGeometry {
    gridOriginValid = NO;
    if (!surface || closing) return;
    double scale = self.window.backingScaleFactor ?: NSScreen.mainScreen.backingScaleFactor ?: 1;
    [CATransaction begin]; [CATransaction setDisableActions:YES]; self.layer.contentsScale = scale; [CATransaction commit];
    ghostty_surface_set_content_scale(surface, scale, scale);
    ghostty_surface_set_size(surface, (uint32_t)MAX(1, llround(self.bounds.size.width * scale)),
        (uint32_t)MAX(1, llround(self.bounds.size.height * scale)));
    NSNumber *display = self.window.screen.deviceDescription[@"NSScreenNumber"];
    if (display) ghostty_surface_set_display_id(surface, display.unsignedIntValue);
    [self clearLink]; [self syncVisibility];
    ghostty_surface_refresh(surface);
}
- (void)tick {
    assertMainThread();
    if (app && atomic_exchange(&dirty, false)) { ghostty_app_tick(app); appTicks++; }
    if (!closing || !surface) return;
    if (detachConfirmed && ghostty_surface_process_exited(surface)) {
        [self diagnostic:@"child-exit-confirmed" fields:@{}]; [self finishShutdown]; return;
    }
    double now = monotonicSeconds();
    if (!parked && now - shutdownStart >= 2) {
        parked = YES; [self diagnostic:@"parked" fields:@{@"detachConfirmed":@(detachConfirmed)}];
    }
    // tty allocation precedes tmux attach registration. An early successful
    // absence readback is provisional while the PTY child is still alive: a
    // late-registering attach client must be detached on the next bounded retry.
    if (detachConfirmed && !detachInFlight && now - lastDetachAttempt >= (parked ? 2 : 0.05)) {
        detachConfirmed = NO;
        [self diagnostic:@"detach-recheck" fields:@{}];
    }
    if (!detachConfirmed && !detachInFlight && now - lastDetachAttempt >= (parked ? 2 : 0.05)) [self requestDetach];
}
- (void)closingTick:(NSTimer *)timer { (void)timer; [self tick]; }
- (void)requestDetach {
    if (!surface || detachConfirmed || detachInFlight) return;
    ghostty_string_s raw = ghostty_surface_tty_name(surface);
    NSString *tty = [[[NSString alloc] initWithBytes:raw.ptr length:raw.len encoding:NSUTF8StringEncoding] autorelease];
    ghostty_string_free(raw);
    // surface_new starts the child asynchronously. Empty tty is never an
    // invitation to detach every client, and never proof of successful detach.
    if (!tty.length) {
        if (ghostty_surface_process_exited(surface)) {
            detachConfirmed = YES;
            [self diagnostic:@"detach-confirmed" fields:@{@"noPtyChild":@YES}];
        }
        return;
    }
    lastDetachAttempt = monotonicSeconds();
    if (!detachCallback) return;
    detachInFlight = YES;
    [self diagnostic:@"detach-requested" fields:@{@"tty":tty}];
    detachCallback(context, self, tty.UTF8String);
}
- (void)shutdown {
    assertMainThread();
    if (closing || !surface) return;
    closing = YES; detachConfirmed = NO; detachInFlight = NO; parked = NO;
    shutdownStart = monotonicSeconds(); lastDetachAttempt = 0;
    pendingDisposals++; [self retain]; // Pending-disposal ownership, main-thread release after native free.
    [self clearLink]; [self unmarkText];
    ghostty_surface_set_focus(surface, false); ghostty_surface_set_occlusion(surface, false);
    if (self.window.firstResponder == self) [self.window makeFirstResponder:nil];
    [self diagnostic:@"shutdown-requested" fields:@{}];
    closingTimer = [[NSTimer timerWithTimeInterval:0.01 target:self selector:@selector(closingTick:)
        userInfo:nil repeats:YES] retain];
    [[NSRunLoop mainRunLoop] addTimer:closingTimer forMode:NSRunLoopCommonModes];
    [self requestDetach];
    [self diagnostic:@"shutdown-returned" fields:@{@"elapsedMs":@((monotonicSeconds()-shutdownStart)*1000)}];
}
- (void)finishShutdown {
    assertMainThread();
    NSAssert(closing && detachConfirmed && !detachInFlight && ghostty_surface_process_exited(surface), @"detach and child-exit barriers before free");
    [closingTimer invalidate]; [closingTimer release]; closingTimer = nil;
    double freeStart = monotonicSeconds();
    [self diagnostic:@"free-begin" fields:@{}];
    ghostty_surface_free(surface); surface = NULL;
    [self diagnostic:@"free-end" fields:@{@"elapsedMs":@((monotonicSeconds()-freeStart)*1000)}];
    if (app) { ghostty_app_free(app); app = NULL; }
    if (config) { ghostty_config_free(config); config = NULL; }
    closing = NO; pendingDisposals--;
    [self diagnostic:@"disposed" fields:@{@"elapsedMs":@((monotonicSeconds()-shutdownStart)*1000)}];
    [self emit:@"disposed" fields:@{}];
    [self release];
}
- (ghostty_input_key_s)key:(NSEvent *)event action:(ghostty_input_action_e)keyAction text:(NSString *)text composing:(BOOL)composing {
    NSString *plain = event.charactersIgnoringModifiers;
    NSData *unicode = [plain dataUsingEncoding:NSUTF32LittleEndianStringEncoding];
    uint32_t codepoint = 0;
    if (unicode.length >= 4) memcpy(&codepoint, unicode.bytes, 4);
    ghostty_input_mods_e translation = surface ? ghostty_surface_key_translation_mods(surface, mods(event.modifierFlags)) : mods(event.modifierFlags);
    ghostty_input_key_s key = {
        .action = keyAction, .mods = mods(event.modifierFlags),
        .consumed_mods = translation & (GHOSTTY_MODS_SHIFT | GHOSTTY_MODS_ALT),
        .keycode = event.keyCode, .text = text.length ? text.UTF8String : NULL,
        .unshifted_codepoint = codepoint, .composing = composing,
    };
    return key;
}
- (void)keyDown:(NSEvent *)event {
    if (![self allowsInput]) return;
    [committed setString:@""];
    BOOL wasComposing = marked.length != 0;
    ghostty_input_mods_e translated = ghostty_surface_key_translation_mods(surface, mods(event.modifierFlags));
    NSEvent *interpretedEvent = event;
    if ((event.modifierFlags & NSEventModifierFlagOption) && !(translated & GHOSTTY_MODS_ALT)) {
        // Ghostty's macos-option-as-alt removes Option from Cocoa text
        // interpretation, while the original physical modifier reaches Ghostty.
        interpretedEvent = [NSEvent keyEventWithType:event.type location:event.locationInWindow
            modifierFlags:event.modifierFlags & ~NSEventModifierFlagOption timestamp:event.timestamp
            windowNumber:event.windowNumber context:nil characters:event.charactersIgnoringModifiers
            charactersIgnoringModifiers:event.charactersIgnoringModifiers isARepeat:event.isARepeat keyCode:event.keyCode];
    }
    interpreting = YES; [self interpretKeyEvents:@[interpretedEvent]]; interpreting = NO;
    ghostty_input_key_s key = [self key:event action:event.isARepeat ? GHOSTTY_ACTION_REPEAT : GHOSTTY_ACTION_PRESS
        text:committed composing:(marked.length != 0 || (wasComposing && committed.length == 0))];
    ghostty_surface_key(surface, key);
    [self clearLink];
}
- (void)keyUp:(NSEvent *)event {
    if ([self allowsInput]) ghostty_surface_key(surface, [self key:event action:GHOSTTY_ACTION_RELEASE text:nil composing:NO]);
}
- (BOOL)performKeyEquivalent:(NSEvent *)event {
    if (![self allowsInput] || self.window.firstResponder != self) return NO;
    NSString *characters = event.charactersIgnoringModifiers.lowercaseString;
    if ((event.modifierFlags & NSEventModifierFlagCommand) && [characters isEqualToString:@"f"]) return NO;
    if ((event.modifierFlags & NSEventModifierFlagCommand) && [characters isEqualToString:@"v"]) { [self paste:nil]; return YES; }
    // Only Ghostty-declared bindings are consumed here. App/window commands and
    // standard Edit menu selectors retain the normal responder-chain priority.
    ghostty_input_key_s key = [self key:event action:GHOSTTY_ACTION_PRESS text:event.characters composing:NO];
    ghostty_binding_flags_e flags = 0;
    if (!ghostty_surface_key_is_binding(surface, key, &flags)) return NO;
    return ghostty_surface_key(surface, key);
}
- (void)flagsChanged:(NSEvent *)event {
    lastModifiers = event.modifierFlags;
    if (linkRows.count) [(linkGesture(lastModifiers) ? NSCursor.pointingHandCursor : terminalCursor) set];
    if (![self allowsInput] || marked.length) return;
    NSEventModifierFlags flag = 0;
    // Per-side bits prevent releasing one Shift/Option while its opposite side
    // is held from being mistaken for another press.
    switch (event.keyCode) {
        case 0x39: flag = NSEventModifierFlagCapsLock; break;
        case 0x38: flag = 0x00000002; break;
        case 0x3c: flag = 0x00000004; break;
        case 0x3b: flag = 0x00000001; break;
        case 0x3e: flag = 0x00002000; break;
        case 0x3a: flag = 0x00000020; break;
        case 0x3d: flag = 0x00000040; break;
        case 0x37: flag = 0x00000008; break;
        case 0x36: flag = 0x00000010; break;
        default: return;
    }
    ghostty_surface_key(surface, [self key:event action:(event.modifierFlags & flag) ? GHOSTTY_ACTION_PRESS : GHOSTTY_ACTION_RELEASE text:nil composing:NO]);
}
- (void)insertText:(id)value replacementRange:(NSRange)range {
    (void)range;
    if (![self allowsInput]) return;
    NSString *text = [value isKindOfClass:NSAttributedString.class] ? [value string] : value;
    if (![text isKindOfClass:NSString.class]) return;
    [self unmarkText];
    if (interpreting) [committed appendString:text];
    else {
        // Input-method commits are key text, not paste: surface_text applies
        // bracketed-paste semantics and must not be used for IME commits.
        ghostty_input_key_s key = {.action = GHOSTTY_ACTION_PRESS, .text = text.UTF8String};
        ghostty_surface_key(surface, key);
    }
}
- (void)setMarkedText:(id)value selectedRange:(NSRange)selected replacementRange:(NSRange)replacement {
    (void)replacement;
    if (![self allowsInput]) return;
    NSAttributedString *text = [value isKindOfClass:NSAttributedString.class] ? value :
        [[[NSAttributedString alloc] initWithString:value] autorelease];
    [marked setAttributedString:text];
    NSUInteger start = MIN(selected.location, marked.length);
    markedSelection = NSMakeRange(start, MIN(selected.length, marked.length-start));
    NSString *string = marked.string;
    ghostty_surface_preedit(surface, string.UTF8String, [string lengthOfBytesUsingEncoding:NSUTF8StringEncoding]);
}
- (void)unmarkText {
    [marked deleteCharactersInRange:NSMakeRange(0, marked.length)]; markedSelection = NSMakeRange(0, 0);
    if (surface) ghostty_surface_preedit(surface, NULL, 0);
}
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
- (NSUInteger)characterIndexForPoint:(NSPoint)point { (void)point; return NSNotFound; }
- (NSRect)firstRectForCharacterRange:(NSRange)range actualRange:(NSRangePointer)actual {
    (void)range;
    if (actual) *actual = self.markedRange;
    double x=0, y=0, width=1, height=1;
    if (surface) ghostty_surface_ime_point(surface, &x, &y, &width, &height);
    NSRect rect = [self convertRect:NSMakeRect(x, y, width, height) toView:nil];
    return self.window ? [self.window convertRectToScreen:rect] : rect;
}
- (void)doCommandBySelector:(SEL)selector { (void)selector; /* keyDown forwards non-text keys once */ }
- (void)copy:(id)sender {
    (void)sender;
    if ([self allowsInput]) ghostty_surface_binding_action(surface, "copy_to_clipboard", strlen("copy_to_clipboard"));
}
- (void)paste:(id)sender {
    (void)sender;
    [self emit:@"inputRefused" fields:@{@"reason":@"Paste and drop routing will be available in the next terminal slice."}];
}
- (void)selectAll:(id)sender {
    (void)sender;
    if ([self allowsInput]) ghostty_surface_binding_action(surface, "select_all", strlen("select_all"));
}
- (void)updateTrackingAreas {
    if (tracking) { [self removeTrackingArea:tracking]; [tracking release]; }
    tracking = [[NSTrackingArea alloc] initWithRect:NSZeroRect options:NSTrackingMouseMoved |
        NSTrackingMouseEnteredAndExited | NSTrackingActiveInKeyWindow | NSTrackingInVisibleRect
        owner:self userInfo:nil];
    [self addTrackingArea:tracking]; [super updateTrackingAreas];
}
- (NSPoint)mousePosition:(NSEvent *)event {
    NSPoint point = [self convertPoint:event.locationInWindow fromView:nil];
    lastModifiers = event.modifierFlags;
    if ([self allowsInput]) ghostty_surface_mouse_pos(surface, point.x, point.y, mods(event.modifierFlags));
    return point;
}
- (BOOL)resolveGridOrigin:(double)cellHeight {
    if (gridOriginValid) return YES;
    if (!surface || cellHeight <= 0) return NO;
    // The pinned API does not expose WindowPadding through config_get. Read a
    // single viewport cell for its actual left edge and baseline, then combine
    // that baseline with the IME cursor-bottom coordinate to recover top
    // padding. This uses Ghostty's effective geometry, including asymmetric or
    // balanced padding, theme includes and noninteger display scales.
    ghostty_point_s corner = {.tag = GHOSTTY_POINT_VIEWPORT, .coord = GHOSTTY_POINT_COORD_EXACT, .x = 0, .y = 0};
    ghostty_selection_s selection = {.top_left = corner, .bottom_right = corner, .rectangle = true};
    ghostty_text_s text = {0};
    if (!ghostty_surface_read_text(surface, selection, &text)) {
        [gridOriginDetails release]; gridOriginDetails = [@{@"reason":@"read-text-failed"} retain]; return NO;
    }
    double left = text.tl_px_x, baseline = text.tl_px_y;
    ghostty_surface_free_text(surface, &text);
    if (left < 0 || baseline < 0) {
        [gridOriginDetails release]; gridOriginDetails = [@{@"reason":@"viewport-unavailable", @"left":@(left), @"baseline":@(baseline)} retain]; return NO;
    }
    double imeX, imeY, imeWidth, imeHeight;
    ghostty_surface_ime_point(surface, &imeX, &imeY, &imeWidth, &imeHeight);
    double remainder = fmod(imeY, cellHeight);
    if (remainder < 0) remainder += cellHeight;
    double top = remainder + floor((baseline-remainder+1e-6)/cellHeight)*cellHeight;
    ghostty_surface_size_s grid = ghostty_surface_size(surface);
    double scale = self.window.backingScaleFactor ?: NSScreen.mainScreen.backingScaleFactor ?: 1;
    double available = MAX(0, (double)grid.height_px-grid.rows*grid.cell_height_px)/scale;
    [gridOriginDetails release]; gridOriginDetails = [@{@"reason":@"measured", @"left":@(left), @"baseline":@(baseline),
        @"imeY":@(imeY), @"cellHeight":@(cellHeight), @"top":@(top), @"remainder":@(remainder), @"available":@(available)} retain];
    if (top < -1e-6 || top > available+1e-6) return NO;
    gridOriginX = left; gridOriginY = MAX(0,top); gridOriginValid = YES;
    return YES;
}
- (NSDictionary *)linkPoint:(NSPoint)point action:(NSString *)action {
    if (!surface) return nil;
    ghostty_surface_size_s grid = ghostty_surface_size(surface);
    if (!grid.columns || !grid.rows || !grid.cell_width_px || !grid.cell_height_px) return nil;
    double scale = self.window.backingScaleFactor ?: NSScreen.mainScreen.backingScaleFactor ?: 1;
    double cellWidth = grid.cell_width_px / scale, cellHeight = grid.cell_height_px / scale;
    if (![self resolveGridOrigin:cellHeight]) return nil;
    double originX = gridOriginX, originY = gridOriginY;
    NSInteger column = floor((point.x-originX)/cellWidth), row = floor((point.y-originY)/cellHeight);
    if (column < 0 || row < 0 || column >= grid.columns || row >= grid.rows) return nil;
    return @{@"requestId":@(hoverRequest), @"x":@(point.x), @"y":@(point.y),
        @"width":@(self.bounds.size.width), @"height":@(self.bounds.size.height),
        @"col":@(column), @"row":@(row), @"cols":@(grid.columns), @"rows":@(grid.rows),
        @"cellWidth":@(cellWidth), @"cellHeight":@(cellHeight), @"originX":@(originX), @"originY":@(originY),
        @"scale":@(scale), @"action":action};
}
- (void)hoverAt:(NSPoint)point {
    NSDictionary *hit = [self linkPoint:point action:@"hover"];
    if (!hit) { [self clearLink]; return; }
    NSInteger row = [hit[@"row"] integerValue], column = [hit[@"col"] integerValue];
    if (row == hoverRow && column == hoverColumn && monotonicSeconds()-linkAnswerTime < 0.5) return;
    [self clearLink]; hoverRow = row; hoverColumn = column; hoverRequest++;
    linkAnswerTime = monotonicSeconds();
    NSMutableDictionary *request = [NSMutableDictionary dictionaryWithDictionary:hit];
    request[@"requestId"] = @(hoverRequest);
    [self emit:@"linkHit" fields:request];
}
- (void)mouseMoved:(NSEvent *)event {
    if (![self allowsInput]) return;
    NSPoint point = [self mousePosition:event]; [self hoverAt:point];
    [(linkRows.count && linkGesture(event.modifierFlags) ? NSCursor.pointingHandCursor : terminalCursor) set];
}
- (void)mouseEntered:(NSEvent *)event { [self mouseMoved:event]; }
- (void)mouseExited:(NSEvent *)event {
    (void)event; [self clearLink];
    if ([self allowsInput]) ghostty_surface_mouse_pos(surface, -1, -1, GHOSTTY_MODS_NONE);
    [NSCursor.arrowCursor set];
}
- (void)mouseDragged:(NSEvent *)event { if ([self allowsInput]) [self mousePosition:event]; }
- (void)rightMouseDragged:(NSEvent *)event { [self mouseDragged:event]; }
- (void)otherMouseDragged:(NSEvent *)event { [self mouseDragged:event]; }
- (BOOL)containsLink:(NSDictionary *)point {
    if (!point || monotonicSeconds()-linkAnswerTime > 0.6) return NO;
    NSInteger row = [point[@"row"] integerValue], col = [point[@"col"] integerValue];
    for (NSDictionary *span in linkRows) if ([span[@"row"] integerValue] == row && col >= [span[@"start"] integerValue] && col < [span[@"end"] integerValue]) return YES;
    return NO;
}
- (void)mouseButton:(NSEvent *)event down:(BOOL)down {
    if (![self allowsInput]) return;
    if (down) [self.window makeFirstResponder:self];
    NSPoint point = [self mousePosition:event];
    if (event.buttonNumber == 0) {
        if (!down && swallowedClick) {
            swallowedClick = NO;
            if (monotonicSeconds()-swallowedClickTime < 2) return;
        }
        if (down && linkGesture(event.modifierFlags)) {
            NSDictionary *hit = [self linkPoint:point action:(event.modifierFlags & NSEventModifierFlagShift) ? @"edit" : @"open"];
            if ([self containsLink:hit]) {
                swallowedClick = YES; swallowedClickTime = monotonicSeconds();
                [self diagnostic:@"link-consumed" fields:@{@"requestId":@(hoverRequest)}];
                [self emit:@"linkHit" fields:hit]; return;
            }
            [self hoverAt:point]; // Missing/slow links stay terminal-owned.
        }
    }
    ghostty_input_mouse_button_e button = event.buttonNumber <= 10 ? (ghostty_input_mouse_button_e)(event.buttonNumber+1) : GHOSTTY_MOUSE_UNKNOWN;
    ghostty_surface_mouse_button(surface, down ? GHOSTTY_MOUSE_PRESS : GHOSTTY_MOUSE_RELEASE, button, mods(event.modifierFlags));
}
- (void)mouseDown:(NSEvent *)event { [self mouseButton:event down:YES]; }
- (void)mouseUp:(NSEvent *)event { [self mouseButton:event down:NO]; }
- (void)rightMouseDown:(NSEvent *)event { [self mouseButton:event down:YES]; }
- (void)rightMouseUp:(NSEvent *)event { [self mouseButton:event down:NO]; }
- (void)otherMouseDown:(NSEvent *)event { [self mouseButton:event down:YES]; }
- (void)otherMouseUp:(NSEvent *)event { [self mouseButton:event down:NO]; }
- (void)scrollWheel:(NSEvent *)event {
    if (![self allowsInput]) return;
    [self mousePosition:event]; [self clearLink];
    ghostty_input_mouse_momentum_e momentum = GHOSTTY_MOUSE_MOMENTUM_NONE;
    switch (event.momentumPhase) {
        case NSEventPhaseBegan: momentum = GHOSTTY_MOUSE_MOMENTUM_BEGAN; break;
        case NSEventPhaseStationary: momentum = GHOSTTY_MOUSE_MOMENTUM_STATIONARY; break;
        case NSEventPhaseChanged: momentum = GHOSTTY_MOUSE_MOMENTUM_CHANGED; break;
        case NSEventPhaseEnded: momentum = GHOSTTY_MOUSE_MOMENTUM_ENDED; break;
        case NSEventPhaseCancelled: momentum = GHOSTTY_MOUSE_MOMENTUM_CANCELLED; break;
        case NSEventPhaseMayBegin: momentum = GHOSTTY_MOUSE_MOMENTUM_MAY_BEGIN; break;
        default: break;
    }
    ghostty_surface_mouse_scroll(surface, event.scrollingDeltaX, event.scrollingDeltaY,
        (event.hasPreciseScrollingDeltas ? 1 : 0) | (momentum << 1));
}
- (void)pressureChangeWithEvent:(NSEvent *)event {
    if ([self allowsInput]) ghostty_surface_mouse_pressure(surface, (uint32_t)event.stage, event.pressure);
}
- (void)clearLink {
    [linkRows release]; linkRows = nil;
    [linkLayer removeFromSuperlayer]; [linkLayer release]; linkLayer = nil;
    hoverRow = hoverColumn = -1; hoverRequest++;
}
- (void)drawLink {
    [linkLayer removeFromSuperlayer]; [linkLayer release]; linkLayer = nil;
    if (!surface || !linkRows.count || frozen || !visible) return;
    ghostty_surface_size_s grid = ghostty_surface_size(surface);
    double scale = self.window.backingScaleFactor ?: NSScreen.mainScreen.backingScaleFactor ?: 1;
    double cw = grid.cell_width_px / scale, ch = grid.cell_height_px / scale;
    if (![self resolveGridOrigin:ch]) return;
    double ox = gridOriginX, oy = gridOriginY;
    CGMutablePathRef path = CGPathCreateMutable();
    for (NSDictionary *span in linkRows) {
        double y = oy + ([span[@"row"] integerValue]+1)*ch-1/scale;
        CGPathMoveToPoint(path, NULL, ox+[span[@"start"] integerValue]*cw, y);
        CGPathAddLineToPoint(path, NULL, ox+[span[@"end"] integerValue]*cw, y);
    }
    linkLayer = [[CAShapeLayer alloc] init]; linkLayer.path = path; CGPathRelease(path);
    linkLayer.strokeColor = NSColor.linkColor.CGColor; linkLayer.fillColor = nil;
    linkLayer.lineWidth = 1/scale; linkLayer.contentsScale = scale;
    linkLayer.frame = self.bounds; linkLayer.zPosition = 100;
    [self.layer addSublayer:linkLayer];
}
- (void)dealloc {
    assertMainThread();
    // A component must call abandon before dropping its owned view. Pending
    // teardown retains the view, so dealloc can never begin with a live child.
    NSAssert(!surface && !app && !closing, @"abandon terminal before releasing its final owner");
    [[NSNotificationCenter defaultCenter] removeObserver:self];
    [[NSDistributedNotificationCenter defaultCenter] removeObserver:self];
    dispatch_source_cancel(wakeSource); dispatch_release(wakeSource);
    [marked release]; [committed release]; [terminalCursor release]; [gridOriginDetails release];
    [self clearLink];
    if (tracking) { [self removeTrackingArea:tracking]; [tracking release]; }
    if (contextRelease) contextRelease(context);
    [super dealloc];
}
@end

void *rw_terminal_new(void *context, rw_terminal_event_cb event,
    rw_terminal_detach_cb detach, rw_terminal_context_release_cb release_context) {
    assertMainThread();
    RWTerminalView *view = [[RWTerminalView alloc] initWithFrame:NSMakeRect(0, 0, 640, 400)];
    view->context = context; view->eventCallback = event; view->detachCallback = detach; view->contextRelease = release_context;
    return view;
}
bool rw_terminal_attach(void *view, const char *command, const char *cwd, const char *path) {
    return [(RWTerminalView *)view attachCommand:command cwd:cwd config:path];
}
void rw_terminal_set_state(void *raw, bool visible, bool frozen, bool focus_on_attach) {
    assertMainThread(); RWTerminalView *view = raw;
    view->visible = visible; view->frozen = frozen; view->focusOnAttach = focus_on_attach;
    if ((!visible || frozen) && view.window.firstResponder == view) [view.window makeFirstResponder:nil];
    [view syncVisibility]; [view drawLink];
}
bool rw_terminal_update_config(void *raw, const char *path) {
    assertMainThread(); RWTerminalView *view = raw;
    if (!view->surface || view->closing) return false;
    ghostty_config_t next = loadConfig(path);
    if (!next) return false;
    ghostty_surface_update_config(view->surface, next);
    ghostty_config_free(view->config); view->config = next;
    [view syncGeometry];
    ghostty_config_color_s background = {0};
    ghostty_config_get(next, &background, "background", strlen("background"));
    [view diagnostic:@"theme-updated" fields:@{@"background":[NSString stringWithFormat:@"#%02x%02x%02x", background.r, background.g, background.b]}];
    return true;
}
void rw_terminal_command(void *raw, const char *name) {
    assertMainThread(); RWTerminalView *view = raw;
    if (!name) return;
    if (strcmp(name, "focus") == 0) { if ([view allowsInput]) [view.window makeFirstResponder:view]; }
    else if (strcmp(name, "blur") == 0) { if (view.window.firstResponder == view) [view.window makeFirstResponder:nil]; }
    else if (strcmp(name, "copy") == 0) [view copy:nil];
    else if (strcmp(name, "paste") == 0) [view paste:nil];
    else if (strcmp(name, "selectAll") == 0) [view selectAll:nil];
}
void rw_terminal_set_link(void *raw, const char *json) {
    assertMainThread(); RWTerminalView *view = raw;
    if (!json || ![view allowsInput]) return;
    NSData *data = [[NSString stringWithUTF8String:json] dataUsingEncoding:NSUTF8StringEncoding];
    NSDictionary *answer = [NSJSONSerialization JSONObjectWithData:data options:0 error:NULL];
    if (![answer isKindOfClass:NSDictionary.class] || [answer[@"requestId"] unsignedLongLongValue] != view->hoverRequest) return;
    NSArray *rows = answer[@"rows"];
    if (![rows isKindOfClass:NSArray.class]) return;
    ghostty_surface_size_s grid = ghostty_surface_size(view->surface);
    if ([answer[@"cols"] unsignedIntValue] != grid.columns || [answer[@"rowsCount"] unsignedIntValue] != grid.rows) return;
    for (NSDictionary *span in rows) {
        if (![span isKindOfClass:NSDictionary.class] || [span[@"row"] integerValue] < 0 || [span[@"row"] unsignedIntValue] >= grid.rows ||
            [span[@"start"] integerValue] < 0 || [span[@"start"] unsignedIntValue] >= [span[@"end"] unsignedIntValue] || [span[@"end"] unsignedIntValue] > grid.columns) return;
    }
    [view->linkRows release]; view->linkRows = [rows copy]; view->linkAnswerTime = monotonicSeconds();
    [view drawLink];
    [view diagnostic:@"link-resolved" fields:@{@"requestId":@(view->hoverRequest), @"spans":@(rows.count)}];
    [(rows.count && linkGesture(view->lastModifiers) ? NSCursor.pointingHandCursor : view->terminalCursor) set];
}
void rw_terminal_detach_result(void *raw, bool confirmed) {
    assertMainThread(); RWTerminalView *view = raw;
    if (!view->closing || !view->detachInFlight) return;
    view->detachInFlight = NO; view->detachConfirmed = confirmed;
    if (confirmed) [view diagnostic:@"detach-confirmed" fields:@{}];
    else { view->lastDetachAttempt = monotonicSeconds(); [view diagnostic:@"detach-retry" fields:@{}]; }
    // Let the next run-loop turn observe child exit; the callback's caller may
    // still be using its context until it returns.
    atomic_store(&view->dirty, true); dispatch_source_merge_data(view->wakeSource, 1);
}
void rw_terminal_shutdown(void *view) { [(RWTerminalView *)view shutdown]; }
void rw_terminal_abandon(void *raw) {
    assertMainThread(); RWTerminalView *view = raw; view->abandoned = YES; [view shutdown];
}
size_t rw_terminal_pending_count(void) { assertMainThread(); return pendingDisposals; }

double rw_terminal_monotonic_ms(void) { return monotonicSeconds() * 1000; }

#ifdef RIWORK_TERMINAL_FIXTURE
// Protocol-level fixture delivery: these events are never posted to AppKit's
// global event stream. The production NSResponder/NSTextInputClient methods are
// called directly on the already-identified isolated fixture surface.
bool rw_terminal_fixture_probe(void *raw, const char *kind) {
    assertMainThread(); RWTerminalView *view = raw;
    if (![view allowsInput] || !kind) return false;
    if (strcmp(kind, "keys") == 0) {
        unichar arrow = NSLeftArrowFunctionKey;
        NSArray *characters = @[[NSString stringWithCharacters:&arrow length:1], @"\r", @"\003"];
        NSArray *plain = @[characters[0], @"\r", @"c"];
        unsigned short codes[] = {123, 36, 8};
        for (NSUInteger i = 0; i < 3; i++) {
            NSEventModifierFlags flags = i == 2 ? NSEventModifierFlagControl : 0;
            NSEvent *down = [NSEvent keyEventWithType:NSEventTypeKeyDown location:NSZeroPoint modifierFlags:flags
                timestamp:monotonicSeconds() windowNumber:view.window.windowNumber context:nil characters:characters[i]
                charactersIgnoringModifiers:plain[i] isARepeat:NO keyCode:codes[i]];
            [view keyDown:down];
            NSEvent *up = [NSEvent keyEventWithType:NSEventTypeKeyUp location:NSZeroPoint modifierFlags:flags
                timestamp:monotonicSeconds() windowNumber:view.window.windowNumber context:nil characters:characters[i]
                charactersIgnoringModifiers:plain[i] isARepeat:NO keyCode:codes[i]];
            [view keyUp:up];
        }
    } else if (strcmp(kind, "ime") == 0) {
        [view setMarkedText:@"にほん" selectedRange:NSMakeRange(1, 1) replacementRange:NSMakeRange(NSNotFound, 0)];
        NSRect anchor = [view firstRectForCharacterRange:view.markedRange actualRange:NULL];
        NSRange selection = view.selectedRange;
        [view diagnostic:@"ime-probe" fields:@{@"candidateRect":NSStringFromRect(anchor),
            @"selectionLocation":@(selection.location), @"selectionLength":@(selection.length)}];
        [view insertText:@"日本語" replacementRange:NSMakeRange(NSNotFound, 0)];
    } else if (strcmp(kind, "link-hover") == 0 || strcmp(kind, "link-click") == 0) {
        ghostty_surface_size_s grid = ghostty_surface_size(view->surface);
        double scale = view.window.backingScaleFactor ?: NSScreen.mainScreen.backingScaleFactor ?: 1;
        double cw = grid.cell_width_px/scale, ch = grid.cell_height_px/scale;
        if (![view resolveGridOrigin:ch]) return false;
        NSPoint point = [view convertPoint:NSMakePoint(view->gridOriginX+4.5*cw, view->gridOriginY+2.5*ch) toView:nil];
        BOOL click = strcmp(kind, "link-click") == 0;
        NSEvent *event = [NSEvent mouseEventWithType:click ? NSEventTypeLeftMouseDown : NSEventTypeMouseMoved
            location:point modifierFlags:NSEventModifierFlagCommand timestamp:monotonicSeconds()
            windowNumber:view.window.windowNumber context:nil eventNumber:1 clickCount:click ? 1 : 0 pressure:click ? 1 : 0];
        if (click) {
            [view mouseDown:event];
            NSEvent *up = [NSEvent mouseEventWithType:NSEventTypeLeftMouseUp location:point
                modifierFlags:NSEventModifierFlagCommand timestamp:monotonicSeconds() windowNumber:view.window.windowNumber
                context:nil eventNumber:2 clickCount:1 pressure:0];
            [view mouseUp:up];
        } else [view mouseMoved:event];
    } else if (strcmp(kind, "geometry") == 0) {
        ghostty_surface_size_s grid = ghostty_surface_size(view->surface);
        double scale = view.window.backingScaleFactor ?: NSScreen.mainScreen.backingScaleFactor ?: 1;
        BOOL resolved = [view resolveGridOrigin:grid.cell_height_px/scale];
        ghostty_selection_s selected = {
            .top_left = {.tag = GHOSTTY_POINT_VIEWPORT, .coord = GHOSTTY_POINT_COORD_EXACT, .x = 0, .y = 0},
            .bottom_right = {.tag = GHOSTTY_POINT_VIEWPORT, .coord = GHOSTTY_POINT_COORD_EXACT,
                .x = MIN(grid.columns, 100)-1, .y = MIN(grid.rows, 3)-1}, .rectangle = true,
        };
        ghostty_text_s text = {0}; NSString *sample = @"";
        ghostty_config_color_s foreground = {0}, background = {0};
        ghostty_config_get(view->config, &foreground, "foreground", strlen("foreground"));
        ghostty_config_get(view->config, &background, "background", strlen("background"));
        if (grid.columns && grid.rows && ghostty_surface_read_text(view->surface, selected, &text)) {
            sample = [[[NSString alloc] initWithBytes:text.text length:text.text_len encoding:NSUTF8StringEncoding] autorelease] ?: @"";
            ghostty_surface_free_text(view->surface, &text);
        }
        [view diagnostic:@"geometry-probe" fields:@{@"resolved":@(resolved), @"originX":@(view->gridOriginX),
            @"originY":@(view->gridOriginY), @"scale":@(scale), @"cols":@(grid.columns), @"rows":@(grid.rows),
            @"widthPx":@(grid.width_px), @"heightPx":@(grid.height_px),
            @"viewWidth":@(view.bounds.size.width), @"viewHeight":@(view.bounds.size.height),
            @"frame":NSStringFromRect(view.frame), @"visibleRect":NSStringFromRect(view.visibleRect),
            @"windowOcclusion":@(view.window.occlusionState), @"hasWindow":@(view.window != nil),
            @"layerClass":NSStringFromClass(view.layer.class), @"layerHidden":@(view.layer.hidden),
            @"layerBounds":NSStringFromRect(view.layer.bounds), @"layerScale":@(view.layer.contentsScale),
            @"layerHasContents":@(view.layer.contents != nil), @"wantsLayer":@(view.wantsLayer),
            @"renderer":fixtureRendererContents(view.layer), @"viewportText":sample,
            @"foreground":[NSString stringWithFormat:@"#%02x%02x%02x", foreground.r, foreground.g, foreground.b],
            @"background":[NSString stringWithFormat:@"#%02x%02x%02x", background.r, background.g, background.b],
            @"superviewClass":NSStringFromClass(view.superview.class) ?: @"none",
            @"frozen":@(view->frozen), @"visible":@(view->visible), @"closing":@(view->closing),
            @"lastShown":@(view->lastShown), @"visibilityUpdates":@(view->visibilityUpdates),
            @"wakeCount":@(atomic_load(&view->wakeCount)), @"appTicks":@(view->appTicks),
            @"renderRequests":@(view->renderRequests), @"drawCalls":@(view->drawCalls),
            @"originDetails":view->gridOriginDetails ?: @{}}];
        return true;
    } else if (strcmp(kind, "mouse") == 0) {
        NSPoint start = [view convertPoint:NSMakePoint(view.bounds.size.width/2, view.bounds.size.height/2) toView:nil];
        NSEventType types[] = {NSEventTypeLeftMouseDown, NSEventTypeLeftMouseDragged, NSEventTypeLeftMouseUp};
        for (NSUInteger i = 0; i < 3; i++) {
            NSEvent *event = [NSEvent mouseEventWithType:types[i] location:NSMakePoint(start.x+i*10,start.y)
                modifierFlags:0 timestamp:monotonicSeconds() windowNumber:view.window.windowNumber context:nil
                eventNumber:i clickCount:1 pressure:i == 2 ? 0 : 1];
            if (i == 0) [view mouseDown:event];
            else if (i == 1) [view mouseDragged:event];
            else [view mouseUp:event];
        }
    } else return false;
    [view diagnostic:@"fixture-probe" fields:@{@"kind":[NSString stringWithUTF8String:kind]}];
    return true;
}
#endif
