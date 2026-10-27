// THROWAWAY SPIKE: direct libghostty/AppKit host. No GPUI runtime or Entity.
// ABI pinned to gpui-libghostty 0.3.1's vendored Ghostty header/archive.
// See evaluation.md for upstream API and license provenance.
#import <AppKit/AppKit.h>
#import <QuartzCore/QuartzCore.h>
#import <ghostty.h>
#import <stdatomic.h>
#import <stdio.h>
#import <unistd.h>
#import <signal.h>
#import <time.h>

static NSMutableArray *liveTerminals;
static NSUInteger nextSurfaceID;
static BOOL quitting;
static void stopAndWakeHost(void) {
    [NSApp stop:nil];
    // stop: from a timer does not wake nextEventMatchingMask. Use the same
    // application-defined wake event as NSH's AppView::stop implementation.
    NSEvent *event = [NSEvent otherEventWithType:NSEventTypeApplicationDefined
        location:NSZeroPoint modifierFlags:0 timestamp:0 windowNumber:0
        context:nil subtype:0 data1:0 data2:0];
    [NSApp postEvent:event atStart:YES];
}
static double monotonicMS(void) {
    struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec * 1000.0 + t.tv_nsec / 1000000.0;
}
#import "profile.h"

@interface SpikeTerminalInput : NSView <NSTextInputClient> {
@public
    ghostty_app_t app;
    ghostty_config_t config;
    ghostty_surface_t surface;
    _Atomic bool dirty;
    NSTimer *timer;
    NSMutableAttributedString *marked;
    NSMutableString *committed;
    BOOL interpreting;
    BOOL detached;
    BOOL stopped;
    BOOL closing, detachConfirmed;
    NSUInteger surfaceID;
    double shutdownStart, lastTick;
    _Atomic int detachResult;
    NSString *surfaceTTY;
    double forcedScale;
    NSUInteger keyCount, mouseCount, themeCount, detachCount, attachCount;
    NSTrackingArea *tracking;
}
- (BOOL)start:(const char *)command config:(const char *)path;
- (void)syncGeometry;
- (void)shutdown;
- (void)record:(NSString *)event;
- (void)teardownRecord:(NSString *)event fields:(NSDictionary *)fields;
- (void)finishShutdown;
@end

@interface SpikeTerminalContainer : NSView {
@public
    SpikeTerminalInput *terminal;
    NSMutableArray *terminals;
    NSString *launchCommand, *configPath;
}
- (void)createBatch;
- (void)closeBatch;
- (void)layoutTerminals;
@end

static ghostty_input_mods_e mods(NSEventModifierFlags flags) {
    unsigned result = 0;
    if (flags & NSEventModifierFlagShift) result |= GHOSTTY_MODS_SHIFT;
    if (flags & NSEventModifierFlagControl) result |= GHOSTTY_MODS_CTRL;
    if (flags & NSEventModifierFlagOption) result |= GHOSTTY_MODS_ALT;
    if (flags & NSEventModifierFlagCommand) result |= GHOSTTY_MODS_SUPER;
    if (flags & NSEventModifierFlagCapsLock) result |= GHOSTTY_MODS_CAPS;
    return (ghostty_input_mods_e)result;
}

static void wakeup(void *userdata) {
    SpikeTerminalInput *view = (SpikeTerminalInput *)userdata;
    atomic_store(&view->dirty, true);
}
static bool action(ghostty_app_t app, ghostty_target_s target, ghostty_action_s action) {
    if (action.tag == GHOSTTY_ACTION_RENDER && target.tag == GHOSTTY_TARGET_SURFACE) {
        ghostty_surface_draw(target.target.surface);
        return true;
    }
    return false;
}
// Clipboard is deliberately disabled for the isolated fixture.
static bool readClipboard(void *u, ghostty_clipboard_e c, void *request) { return false; }
static void confirmClipboard(void *u, const char *text, void *request, ghostty_clipboard_request_e kind) {
    SpikeTerminalInput *view = (SpikeTerminalInput *)u;
    if (view->surface) ghostty_surface_complete_clipboard_request(view->surface, "", request, true);
}
static void writeClipboard(void *u, ghostty_clipboard_e c, const ghostty_clipboard_content_s *content, size_t count, bool confirm) {}
static void closeSurface(void *u, bool alive) { [(SpikeTerminalInput *)u record:@"process-exit"]; }

@implementation SpikeTerminalInput
- (BOOL)start:(const char *)command config:(const char *)path {
    NSAssert([NSThread isMainThread], @"AppKit must run on main thread");
    profileStart();
    profile("create-surface", 'b', nextSurfaceID + 1);
    static dispatch_once_t once;
    static int initialized = -1;
    dispatch_once(&once, ^{ initialized = ghostty_init(0, NULL); });
    if (initialized != GHOSTTY_SUCCESS) return NO;
    surfaceID = ++nextSurfaceID;
    atomic_init(&dirty, true);
    atomic_init(&detachResult, 0);
    marked = [[NSMutableAttributedString alloc] initWithString:@""];
    committed = [[NSMutableString alloc] init];
    config = ghostty_config_new();
    ghostty_config_load_file(config, path); // Never load the user's Ghostty config.
    ghostty_config_finalize(config);
    if (ghostty_config_diagnostics_count(config)) {
        fprintf(stderr, "spike: config diagnostics=%u\n", ghostty_config_diagnostics_count(config));
        return NO;
    }
    ghostty_runtime_config_s runtime = {
        .userdata = self, .supports_selection_clipboard = false,
        .wakeup_cb = wakeup, .action_cb = action,
        .read_clipboard_cb = readClipboard, .confirm_read_clipboard_cb = confirmClipboard,
        .write_clipboard_cb = writeClipboard, .close_surface_cb = closeSurface,
    };
    app = ghostty_app_new(&runtime, config);
    if (!app) return NO;
    ghostty_surface_config_s opts = ghostty_surface_config_new();
    opts.platform_tag = GHOSTTY_PLATFORM_MACOS;
    opts.platform.macos.nsview = self;
    opts.userdata = self;
    opts.scale_factor = self.window.backingScaleFactor ?: NSScreen.mainScreen.backingScaleFactor;
    opts.command = command;
    opts.working_directory = getenv("RIWORK_HOME");
    ghostty_env_var_s env[] = {
        {"TERM", "xterm-256color"}, {"COLORTERM", "truecolor"},
        {"TERM_PROGRAM", "riwork-ghostty-nsh-spike"}, {"GHOSTTY_QUIET_LOGIN", "1"},
    };
    opts.env_vars = env;
    opts.env_var_count = sizeof(env) / sizeof(env[0]);
    surface = ghostty_surface_new(app, &opts);
    if (!surface) return NO;
    if (!liveTerminals) liveTerminals = [[NSMutableArray alloc] init];
    [liveTerminals addObject:self];
    // Spike-only coalescing pump. A production host should schedule a main-loop
    // wakeup instead of keeping a 60 Hz timer alive. No render polling occurs.
    timer = [[NSTimer scheduledTimerWithTimeInterval:1.0/60.0 target:self
        selector:@selector(tick:) userInfo:nil repeats:YES] retain];
    [[NSRunLoop mainRunLoop] addTimer:timer forMode:NSRunLoopCommonModes];
    [self syncGeometry];
    [self record:@"created"];
    [self teardownRecord:@"created" fields:@{}];
    profile("create-surface", 'e', surfaceID);
    return YES;
}
- (BOOL)isFlipped { return YES; }
- (BOOL)acceptsFirstResponder { return YES; }
- (BOOL)acceptsFirstMouse:(NSEvent *)event { return YES; }
- (BOOL)becomeFirstResponder {
    if (surface) { ghostty_app_set_focus(app, true); ghostty_surface_set_focus(surface, true); }
    return YES;
}
- (BOOL)resignFirstResponder {
    if (surface) ghostty_surface_set_focus(surface, false);
    return YES;
}
- (BOOL)isAccessibilityElement { return YES; }
- (NSString *)accessibilityRole { return NSAccessibilityGroupRole; }
- (NSString *)accessibilityLabel { return @"Direct libghostty terminal"; }
- (void)tick:(NSTimer *)unused {
    BOOL closingAtEntry = closing;
    NSUInteger tickID = surfaceID;
    const char *tickName = closingAtEntry ? "closing-tick" : "attached-tick";
    profile(tickName, 'b', tickID);
    double now = monotonicMS();
    if (closing && !stopped && lastTick > 0)
        [self teardownRecord:@"main-loop-gap" fields:@{@"gap_ms":@(now-lastTick),
            @"gap_start_ms":@(lastTick), @"gap_end_ms":@(now)}];
    lastTick = now;
    if (app && atomic_exchange(&dirty, false)) {
        profile(closing ? "closing-app-tick" : "attached-app-tick", 'b', surfaceID);
        ghostty_app_tick(app);
        profile(closing ? "closing-app-tick" : "attached-app-tick", 'e', surfaceID);
    }
    if (!closing || stopped) { profile(tickName, 'e', tickID); return; }
    int result = atomic_load(&detachResult);
    if (result == 1 && !detachConfirmed) {
        detachConfirmed = YES;
        [self teardownRecord:@"detach-confirmed" fields:@{}];
    }
    profile("child-exit-check", 'b', surfaceID);
    BOOL childExited = detachConfirmed && ghostty_surface_process_exited(surface);
    profile("child-exit-check", 'e', surfaceID);
    if (childExited) {
        [self teardownRecord:@"child-exit-confirmed" fields:@{@"process_exited":@YES}];
        [self finishShutdown];
    } else if (result < 0 || monotonicMS() - shutdownStart >= 2000.0) {
        // Fail the fixture, never claim a parked/leaked surface is a pass, and
        // never enter native free with a possibly-live PTY child.
        [self teardownRecord:@"teardown-failed" fields:@{@"detach_result":@(result)}];
        fflush(NULL); _exit(70);
    }
    profile(tickName, 'e', tickID);
}
- (void)setFrameSize:(NSSize)size { [super setFrameSize:size]; [self syncGeometry]; }
- (void)viewDidMoveToWindow { [super viewDidMoveToWindow]; [self syncGeometry]; }
- (void)viewDidChangeBackingProperties {
    [super viewDidChangeBackingProperties];
    [self syncGeometry];
    [self record:@"backing-properties-changed"];
}
- (void)syncGeometry {
    if (!surface) return;
    double scale = forcedScale ?: (self.window.backingScaleFactor ?: NSScreen.mainScreen.backingScaleFactor);
    NSSize size = self.bounds.size;
    [CATransaction begin];
    [CATransaction setDisableActions:YES];
    self.layer.contentsScale = scale;
    [CATransaction commit];
    ghostty_surface_set_content_scale(surface, scale, scale);
    ghostty_surface_set_size(surface, MAX(1, (uint32_t)(size.width * scale)), MAX(1, (uint32_t)(size.height * scale)));
    NSNumber *display = [self.window.screen.deviceDescription objectForKey:@"NSScreenNumber"];
    if (display) ghostty_surface_set_display_id(surface, display.unsignedIntValue);
    ghostty_surface_set_occlusion(surface, !detached);
    ghostty_surface_refresh(surface);
    [self record:@"geometry"];
}
- (void)updateTrackingAreas {
    if (tracking) { [self removeTrackingArea:tracking]; [tracking release]; }
    tracking = [[NSTrackingArea alloc] initWithRect:NSZeroRect
        options:NSTrackingMouseMoved | NSTrackingActiveAlways | NSTrackingInVisibleRect
        owner:self userInfo:nil];
    [self addTrackingArea:tracking];
    [super updateTrackingAreas];
}
- (void)sendKey:(NSEvent *)event action:(ghostty_input_action_e)keyAction text:(NSString *)text composing:(BOOL)composing {
    if (!surface) return;
    NSString *plain = event.charactersIgnoringModifiers;
    uint32_t codepoint = plain.length ? [plain characterAtIndex:0] : 0;
    ghostty_input_key_s key = {
        .action = keyAction, .mods = mods(event.modifierFlags),
        .consumed_mods = mods(event.modifierFlags & (NSEventModifierFlagShift | NSEventModifierFlagOption)),
        .keycode = event.keyCode, .text = text.length ? text.UTF8String : NULL,
        .unshifted_codepoint = codepoint, .composing = composing,
    };
    ghostty_surface_key(surface, key);
}
- (void)keyDown:(NSEvent *)event {
    keyCount++;
    [committed setString:@""];
    BOOL wasComposing = marked.length != 0;
    interpreting = YES;
    [self interpretKeyEvents:@[event]];
    interpreting = NO;
    // NSTextInputClient collects commits during interpretation; one key event
    // carries that text. Sending surface_text here as well would duplicate it.
    [self sendKey:event action:event.isARepeat ? GHOSTTY_ACTION_REPEAT : GHOSTTY_ACTION_PRESS
        text:committed composing:(marked.length != 0 || (wasComposing && committed.length == 0))];
    [self record:@"key-down"];
}
- (void)keyUp:(NSEvent *)event { [self sendKey:event action:GHOSTTY_ACTION_RELEASE text:nil composing:NO]; }
- (void)flagsChanged:(NSEvent *)event {
    if (marked.length) return;
    NSEventModifierFlags flag = 0;
    switch (event.keyCode) {
        case 0x39: flag = NSEventModifierFlagCapsLock; break;
        case 0x38: case 0x3c: flag = NSEventModifierFlagShift; break;
        case 0x3b: case 0x3e: flag = NSEventModifierFlagControl; break;
        case 0x3a: case 0x3d: flag = NSEventModifierFlagOption; break;
        case 0x37: case 0x36: flag = NSEventModifierFlagCommand; break;
        default: return;
    }
    [self sendKey:event action:(event.modifierFlags & flag) ? GHOSTTY_ACTION_PRESS : GHOSTTY_ACTION_RELEASE
        text:nil composing:NO];
}
- (void)insertText:(id)value replacementRange:(NSRange)range {
    NSString *text = [value isKindOfClass:[NSAttributedString class]] ? [value string] : value;
    [self unmarkText];
    if (interpreting) [committed appendString:text];
    else if (surface) {
        ghostty_input_key_s key = {.action = GHOSTTY_ACTION_PRESS, .text = text.UTF8String};
        ghostty_surface_key(surface, key);
    }
    [self record:@"insert-text"];
}
- (void)setMarkedText:(id)value selectedRange:(NSRange)selected replacementRange:(NSRange)replacement {
    NSAttributedString *text = [value isKindOfClass:[NSAttributedString class]] ? value :
        [[[NSAttributedString alloc] initWithString:value] autorelease];
    [marked setAttributedString:text];
    NSString *string = marked.string;
    if (surface) ghostty_surface_preedit(surface, string.UTF8String, [string lengthOfBytesUsingEncoding:NSUTF8StringEncoding]);
    [self record:@"set-marked-text"];
}
- (void)unmarkText {
    [marked deleteCharactersInRange:NSMakeRange(0, marked.length)];
    if (surface) ghostty_surface_preedit(surface, NULL, 0);
}
- (BOOL)hasMarkedText { return marked.length > 0; }
- (NSRange)markedRange { return marked.length ? NSMakeRange(0, marked.length) : NSMakeRange(NSNotFound, 0); }
- (NSRange)selectedRange { return NSMakeRange(marked.length, 0); }
- (NSArray *)validAttributesForMarkedText { return @[]; }
- (NSAttributedString *)attributedSubstringForProposedRange:(NSRange)range actualRange:(NSRangePointer)actual {
    if (range.location == NSNotFound || NSMaxRange(range) > marked.length) return nil;
    if (actual) *actual = range;
    return [marked attributedSubstringFromRange:range];
}
- (NSUInteger)characterIndexForPoint:(NSPoint)point { return 0; }
- (NSRect)firstRectForCharacterRange:(NSRange)range actualRange:(NSRangePointer)actual {
    if (actual) *actual = self.markedRange;
    double x=0, y=0, width=1, height=1;
    if (surface) ghostty_surface_ime_point(surface, &x, &y, &width, &height);
    NSRect rect = [self convertRect:NSMakeRect(x, y, width, height) toView:nil];
    return [self.window convertRectToScreen:rect];
}
- (void)doCommandBySelector:(SEL)selector { /* keyDown sends non-text keys to Ghostty */ }
- (void)mousePosition:(NSEvent *)event {
    NSPoint point = [self convertPoint:event.locationInWindow fromView:nil];
    if (surface) ghostty_surface_mouse_pos(surface, point.x, point.y, mods(event.modifierFlags));
}
- (void)mouseMoved:(NSEvent *)event { [self mousePosition:event]; }
- (void)mouseDragged:(NSEvent *)event { [self mousePosition:event]; }
- (void)rightMouseDragged:(NSEvent *)event { [self mousePosition:event]; }
- (void)otherMouseDragged:(NSEvent *)event { [self mousePosition:event]; }
- (void)mouseButton:(NSEvent *)event down:(BOOL)down {
    [self.window makeFirstResponder:self]; // Does not activate or raise the window.
    [self mousePosition:event];
    ghostty_input_mouse_button_e button = event.buttonNumber == 0 ? GHOSTTY_MOUSE_LEFT :
        event.buttonNumber == 1 ? GHOSTTY_MOUSE_RIGHT : GHOSTTY_MOUSE_MIDDLE;
    if (surface) ghostty_surface_mouse_button(surface, down ? GHOSTTY_MOUSE_PRESS : GHOSTTY_MOUSE_RELEASE,
        button, mods(event.modifierFlags));
    mouseCount++;
    [self record:down ? @"mouse-down" : @"mouse-up"];
}
- (void)mouseDown:(NSEvent *)event { [self mouseButton:event down:YES]; }
- (void)mouseUp:(NSEvent *)event { [self mouseButton:event down:NO]; }
- (void)rightMouseDown:(NSEvent *)event { [self mouseButton:event down:YES]; }
- (void)rightMouseUp:(NSEvent *)event { [self mouseButton:event down:NO]; }
- (void)otherMouseDown:(NSEvent *)event { [self mouseButton:event down:YES]; }
- (void)otherMouseUp:(NSEvent *)event { [self mouseButton:event down:NO]; }
- (void)scrollWheel:(NSEvent *)event {
    [self mousePosition:event];
    if (surface) ghostty_surface_mouse_scroll(surface, event.scrollingDeltaX, event.scrollingDeltaY,
        event.hasPreciseScrollingDeltas ? 1 : 0);
    mouseCount++;
    [self record:@"scroll"];
}
- (void)record:(NSString *)event {
    const char *home = getenv("RIWORK_HOME");
    if (!home) return;
    ghostty_surface_size_s size = surface ? ghostty_surface_size(surface) : (ghostty_surface_size_s){0};
    NSMutableArray *screens = [NSMutableArray array];
    for (NSScreen *screen in NSScreen.screens) {
        [screens addObject:@{@"scale":@(screen.backingScaleFactor), @"frame":NSStringFromRect(screen.frame),
            @"display_id":screen.deviceDescription[@"NSScreenNumber"] ?: @0}];
    }
    NSDictionary *record = @{
        @"event":event, @"pid":@(getpid()), @"surface":[NSString stringWithFormat:@"%p", surface],
        @"cols":@(size.columns), @"rows":@(size.rows), @"width_px":@(size.width_px), @"height_px":@(size.height_px),
        @"view_width":@(self.bounds.size.width), @"view_height":@(self.bounds.size.height),
        @"backing_scale":@(self.window.backingScaleFactor), @"forced_scale":@(forcedScale),
        @"key_count":@(keyCount), @"mouse_count":@(mouseCount), @"theme_count":@(themeCount),
        @"detach_count":@(detachCount), @"attach_count":@(attachCount), @"marked_length":@(marked.length),
        @"detached":@(detached), @"time":@([[NSDate date] timeIntervalSince1970]), @"screens":screens
    };
    NSData *data = [NSJSONSerialization dataWithJSONObject:record options:0 error:nil];
    NSString *path = [[NSString stringWithUTF8String:home] stringByAppendingPathComponent:@"native-events.jsonl"];
    FILE *file = fopen(path.fileSystemRepresentation, "a");
    if (file) { fwrite(data.bytes, 1, data.length, file); fputc('\n', file); fclose(file); }
}
- (void)shutdown {
    NSAssert([NSThread isMainThread], @"surface lifecycle is main-thread-only");
    if (stopped || closing) return;
    if (!surface) { [self finishShutdown]; return; }
    profile("shutdown-dispatch", 'b', surfaceID);
    closing = YES;
    shutdownStart = monotonicMS();
    // PTY spawn is asynchronous: its tty is not populated at surface_new.
    ghostty_string_s tty = ghostty_surface_tty_name(surface);
    surfaceTTY = [[NSString alloc] initWithBytes:tty.ptr length:tty.len encoding:NSUTF8StringEncoding];
    ghostty_string_free(tty);
    [self retain]; // Explicit pending-disposal ownership, released after free.
    [self record:@"shutdown"];
    [self teardownRecord:@"shutdown-requested" fields:@{@"tty":surfaceTTY ?: @""}];
    ghostty_surface_set_focus(surface, false);
    ghostty_surface_set_occlusion(surface, false);
    [self removeFromSuperview];
    // Only this worker contacts tmux. The main loop keeps draining Ghostty's
    // app mailbox until BOTH exact-client detach and PTY-child exit are proved.
    NSString *ttyForWorker = [surfaceTTY copy];
    NSUInteger idForWorker = surfaceID;
    _Atomic int *resultForWorker = &detachResult;
    // Do not capture self: final NSView release must stay on the main thread.
    // The explicit pending-disposal retain protects resultForWorker until the
    // main timer consumes this worker's final (and last) memory access.
    dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{
        profile("detach-confirm-wait", 'b', idForWorker);
        int workerResult = -1;
        @autoreleasepool {
            NSTask *task = [[[NSTask alloc] init] autorelease];
            task.launchPath = [NSString stringWithUTF8String:getenv("SPIKE_PYTHON")];
            task.arguments = @[[NSString stringWithUTF8String:getenv("SPIKE_DETACH_HELPER")],
                [NSString stringWithUTF8String:getenv("RIWORK_HOME")], ttyForWorker,
                [NSString stringWithFormat:@"%lu", (unsigned long)idForWorker],
                [NSString stringWithFormat:@"%d", getpid()]];
            task.standardOutput = [NSFileHandle fileHandleWithNullDevice];
            task.standardError = [NSFileHandle fileHandleWithNullDevice];
            NSError *error = nil;
            BOOL launched = [task launchAndReturnError:&error];
            double deadline = monotonicMS() + 1800.0;
            while (launched && task.running && monotonicMS() < deadline) usleep(2000);
            if (launched && task.running) kill(task.processIdentifier, SIGKILL);
            if (launched) [task waitUntilExit]; // Worker only, never AppKit.
            workerResult = launched && task.terminationStatus == 0 ? 1 : -1;
        }
        profile("detach-confirm-wait", 'e', idForWorker);
        atomic_store(resultForWorker, workerResult);
    });
    [ttyForWorker release];
    [self teardownRecord:@"shutdown-returned" fields:@{@"main_block_ms":@(monotonicMS()-shutdownStart)}];
    profile("shutdown-dispatch", 'e', surfaceID);
}
- (void)teardownRecord:(NSString *)event fields:(NSDictionary *)fields {
    NSMutableDictionary *entry = [NSMutableDictionary dictionaryWithDictionary:fields];
    [entry addEntriesFromDictionary:@{@"event":event, @"surface_id":@(surfaceID),
        @"fixture_pid":@(getpid()), @"main_thread":@([NSThread isMainThread]),
        @"monotonic_ms":@(monotonicMS())}];
    NSData *data = [NSJSONSerialization dataWithJSONObject:entry options:0 error:nil];
    NSString *path = [[NSString stringWithUTF8String:getenv("RIWORK_HOME")]
        stringByAppendingPathComponent:@"teardown-events.jsonl"];
    FILE *file = fopen(path.fileSystemRepresentation, "a");
    if (file) { fwrite(data.bytes, 1, data.length, file); fputc('\n', file); fclose(file); }
}
- (void)finishShutdown {
    NSAssert([NSThread isMainThread], @"native free stays on main thread");
    stopped = YES;
    [timer invalidate]; [timer release]; timer = nil;
    double freeStart = monotonicMS();
    if (surface) {
        NSAssert(detachConfirmed && ghostty_surface_process_exited(surface), @"detach and child exit before free");
        [self teardownRecord:@"free-begin" fields:@{}];
        profile("surface-free", 'b', surfaceID);
        ghostty_surface_free(surface); surface = NULL;
        profile("surface-free", 'e', surfaceID);
        [self teardownRecord:@"free-end" fields:@{@"free_ms":@(monotonicMS()-freeStart)}];
    }
    profile("app-config-free", 'b', surfaceID);
    if (app) { ghostty_app_free(app); app = NULL; }
    if (config) { ghostty_config_free(config); config = NULL; }
    profile("app-config-free", 'e', surfaceID);
    [self teardownRecord:@"disposed" fields:@{@"disposal_ms":@(monotonicMS()-shutdownStart),
        @"main_free_block_ms":@(monotonicMS()-freeStart)}];
    [liveTerminals removeObjectIdenticalTo:self];
    if (quitting && liveTerminals.count == 0) stopAndWakeHost();
    if (closing) [self release];
}
- (void)dealloc {
    [self shutdown];
    [marked release]; [committed release];
    [surfaceTTY release];
    if (tracking) { [self removeTrackingArea:tracking]; [tracking release]; }
    [super dealloc];
}
@end

@implementation SpikeTerminalContainer
- (BOOL)isFlipped { return YES; }
- (void)layoutTerminals {
    NSUInteger count = terminals.count;
    NSUInteger columns = count > 1 ? 2 : 1;
    NSUInteger rows = (count + columns - 1) / columns;
    for (NSUInteger i = 0; i < count; i++) {
        NSSize size = NSMakeSize(self.bounds.size.width / columns, self.bounds.size.height / rows);
        [terminals[i] setFrame:NSMakeRect((i % columns)*size.width, (i / columns)*size.height, size.width, size.height)];
    }
}
- (void)setFrameSize:(NSSize)size { [super setFrameSize:size]; [self layoutTerminals]; }
- (void)createBatch {
    for (SpikeTerminalInput *v in terminals) if (!v->stopped) return;
    for (SpikeTerminalInput *v in terminals) [v removeFromSuperview];
    [terminals removeAllObjects];
    NSUInteger count = getenv("SPIKE_SURFACES") ? atoi(getenv("SPIKE_SURFACES")) : 1;
    NSAssert(count == 1 || count == 4, @"fixture supports one or four surfaces");
    for (NSUInteger i = 0; i < count; i++) {
        SpikeTerminalInput *v = [[SpikeTerminalInput alloc] initWithFrame:self.bounds];
        [terminals addObject:v]; [self addSubview:v];
        if (![v start:launchCommand.UTF8String config:configPath.UTF8String]) abort();
        [v release];
    }
    terminal = terminals.firstObject;
    [self layoutTerminals];
}
- (void)closeBatch { for (SpikeTerminalInput *v in terminals) [v shutdown]; }
- (void)dealloc {
    [self closeBatch]; [terminals release]; [launchCommand release]; [configPath release];
    [super dealloc];
}
@end

void *spike_terminal_view_new(const char *command, const char *config) {
    SpikeTerminalContainer *container = [[SpikeTerminalContainer alloc] initWithFrame:NSMakeRect(0,0,900,500)];
    container->terminals = [[NSMutableArray alloc] init];
    container->launchCommand = [[NSString alloc] initWithUTF8String:command];
    container->configPath = [[NSString alloc] initWithUTF8String:config];
    [container createBatch];
    return container;
}
void spike_terminal_view_theme(void *view, const char *path) {
    SpikeTerminalInput *v = ((SpikeTerminalContainer *)view)->terminal;
    if (!v->surface) return;
    ghostty_config_t next = ghostty_config_new();
    ghostty_config_load_file(next, path);
    ghostty_config_finalize(next);
    if (!ghostty_config_diagnostics_count(next)) {
        ghostty_surface_update_config(v->surface, next);
        ghostty_surface_refresh(v->surface);
        v->themeCount++;
        [v record:@"theme-updated"];
    }
    ghostty_config_free(next);
}
void spike_terminal_view_detach(void *view) {
    SpikeTerminalInput *v = ((SpikeTerminalContainer *)view)->terminal;
    if (v->detached || !v->surface) return;
    v->detached = YES; v->detachCount++;
    ghostty_surface_set_focus(v->surface, false);
    ghostty_surface_set_occlusion(v->surface, false);
    [v removeFromSuperview];
    [v record:@"detached"];
}
void spike_terminal_view_reattach(void *view) {
    SpikeTerminalContainer *container = view;
    SpikeTerminalInput *v = container->terminal;
    if (!v->detached || !v->surface) return;
    v->detached = NO; v->attachCount++;
    [container addSubview:v];
    [v setFrame:container.bounds];
    [v syncGeometry];
    [container.window makeFirstResponder:v];
    [v record:@"reattached"];
}
void spike_terminal_view_ime_probe(void *view) {
    // Protocol-level composition proof, NOT a claim of Japanese input-source UI coverage.
    SpikeTerminalInput *v = ((SpikeTerminalContainer *)view)->terminal;
    [v setMarkedText:@"にほん" selectedRange:NSMakeRange(3,0) replacementRange:NSMakeRange(NSNotFound,0)];
    NSRect rect = [v firstRectForCharacterRange:v.markedRange actualRange:NULL];
    fprintf(stderr, "spike: IME candidate rect %.1f %.1f %.1f %.1f\n", rect.origin.x,rect.origin.y,rect.size.width,rect.size.height);
    [v insertText:@"日本語" replacementRange:NSMakeRange(NSNotFound,0)];
    [v record:@"ime-probe-committed"];
}
void spike_terminal_view_scale_probe(void *view, double scale) {
    SpikeTerminalInput *v = ((SpikeTerminalContainer *)view)->terminal;
    v->forcedScale = scale;
    [v syncGeometry];
    [v record:@"synthetic-scale-probe"];
}
void spike_terminal_view_shutdown(void *view) { [(SpikeTerminalContainer *)view closeBatch]; }
void spike_terminal_view_create(void *view) { [(SpikeTerminalContainer *)view createBatch]; }
void spike_terminal_prepare_quit(void) {
    quitting = YES;
    NSArray *all = [[liveTerminals copy] autorelease];
    for (SpikeTerminalInput *v in all) [v shutdown];
    if (liveTerminals.count == 0) stopAndWakeHost();
}
