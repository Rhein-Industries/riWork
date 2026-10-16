// RiWork's device-only Simulator transport. No AppKit window or macOS input API.
// Private API call shapes are documented in README.md. Xcode updates may require
// a new adapter; unknown CoreSimulator layouts fail before accepting input.
#import <Foundation/Foundation.h>
#import <CoreGraphics/CoreGraphics.h>
#import <ImageIO/ImageIO.h>
#import <IOSurface/IOSurface.h>
#import <objc/message.h>
#import <objc/runtime.h>
#import <dlfcn.h>
#import <mach/mach_time.h>
#import <malloc/malloc.h>
#import <sys/socket.h>
#import <sys/un.h>
#import <sys/stat.h>
#import <unistd.h>
#import <signal.h>
#import <math.h>

@interface NSObject (RiWorkSimulatorAPI)
+ (id)sharedServiceContextForDeveloperDir:(NSString *)path error:(NSError **)error;
- (id)defaultDeviceSetWithError:(NSError **)error;
- (id)initWithDevice:(id)device error:(NSError **)error;
- (void)updateIOPorts;
- (void)registerScreenCallbacksWithUUID:(NSUUID *)uuid callbackQueue:(dispatch_queue_t)queue frameCallback:(void (^)(void))frame surfacesChangedCallback:(void (^)(void))surfaces propertiesChangedCallback:(void (^)(void))properties;
- (void)unregisterScreenCallbacksWithUUID:(NSUUID *)uuid;
- (void)sendWithMessage:(void *)message freeWhenDone:(BOOL)release completionQueue:(dispatch_queue_t)queue completion:(void (^)(NSError *))completion;
@end

static volatile sig_atomic_t stopped;
static volatile sig_atomic_t transport_failed;
static void stop_signal(int signal_number) { (void)signal_number; stopped = 1; }
static void fail(NSString *message) { @throw [NSException exceptionWithName:@"RiWorkPreview" reason:message userInfo:nil]; }
static id get(id object, NSString *selector) {
    SEL sel = NSSelectorFromString(selector);
    if (!object || ![object respondsToSelector:sel]) fail([NSString stringWithFormat:@"Simulator API %@ is unavailable. This Xcode version needs an updated RiWork Simulator adapter.", selector]);
    return ((id (*)(id, SEL))objc_msgSend)(object, sel);
}
static void *symbol(void *library, const char *name) {
    void *value = dlsym(library, name);
    if (!value) fail([NSString stringWithFormat:@"Simulator symbol %s is unavailable. This Xcode version needs an updated RiWork Simulator adapter.", name]);
    return value;
}
static NSString *developer_directory(void) {
    NSString *configured = NSProcessInfo.processInfo.environment[@"DEVELOPER_DIR"];
    if (configured.length) {
        NSString *nested = [configured stringByAppendingPathComponent:@"Contents/Developer"];
        return [NSFileManager.defaultManager fileExistsAtPath:[nested stringByAppendingPathComponent:@"Library/PrivateFrameworks/SimulatorKit.framework/SimulatorKit"]] ? nested : configured;
    }
    NSTask *task = [NSTask new]; NSPipe *pipe = [NSPipe pipe];
    task.executableURL = [NSURL fileURLWithPath:@"/usr/bin/xcode-select"];
    task.arguments = @[@"-p"]; task.standardOutput = pipe;
    [task launchAndReturnError:nil];
    NSData *data = [pipe.fileHandleForReading readDataToEndOfFile]; [task waitUntilExit];
    NSString *path = [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
    path = [path stringByTrimmingCharactersInSet:NSCharacterSet.whitespaceAndNewlineCharacterSet];
    if (!path.length || task.terminationStatus) fail(@"Select a full Xcode installation with xcode-select before starting an iOS preview.");
    return path;
}

typedef struct { void *kit; void *iokit; NSInteger version; } SimulatorFrameworks;
static SimulatorFrameworks load_frameworks(NSString *developer) {
    SimulatorFrameworks frameworks = {0};
    NSString *corePath = @"/Library/Developer/PrivateFrameworks/CoreSimulator.framework";
    NSBundle *core = [NSBundle bundleWithPath:corePath];
    NSInteger version = [core.infoDictionary[@"CFBundleVersion"] integerValue];
    frameworks.version = version;
    // The Indigo wire offsets below are checked against CoreSimulator 1048
    // (Xcode 26.0.1). Xcode 27 replaces the guest HID transport entirely.
    if (version < 1000 || version >= 1100) fail([NSString stringWithFormat:@"CoreSimulator %@ is unsupported by this Simulator adapter (supported layout: Xcode 16 / 26). Update RiWork before using this Xcode version.", core.infoDictionary[@"CFBundleVersion"] ?: @"unknown"]);
    if (!dlopen([[corePath stringByAppendingPathComponent:@"CoreSimulator"] fileSystemRepresentation], RTLD_NOW | RTLD_GLOBAL)) fail([NSString stringWithFormat:@"Cannot load CoreSimulator: %s", dlerror()]);
    NSString *kitPath = [developer stringByAppendingPathComponent:@"Library/PrivateFrameworks/SimulatorKit.framework/SimulatorKit"];
    frameworks.kit = dlopen(kitPath.fileSystemRepresentation, RTLD_NOW | RTLD_GLOBAL);
    if (!frameworks.kit) fail([NSString stringWithFormat:@"Cannot load SimulatorKit from the selected Xcode: %s", dlerror()]);
    frameworks.iokit = dlopen("/System/Library/Frameworks/IOKit.framework/IOKit", RTLD_NOW | RTLD_GLOBAL);
    if (!frameworks.iokit) fail(@"Cannot load IOKit for Simulator touch events.");
    return frameworks;
}

typedef CFTypeRef (*CreateDigitizer)(CFAllocatorRef, uint64_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, double, double, double, double, double, bool, bool, uint32_t);
typedef CFTypeRef (*CreateFinger)(CFAllocatorRef, uint64_t, uint32_t, uint32_t, uint32_t, double, double, double, double, double, bool, bool, uint32_t);
typedef void (*AppendEvent)(CFTypeRef, CFTypeRef, uint32_t);
typedef void *(*WrapDigitizer)(CFTypeRef);
typedef void *(*KeyboardEvent)(uint32_t, uint32_t);
typedef void *(*ButtonEvent)(uint32_t, uint32_t, uint32_t);

@interface RiWorkBridge : NSObject
@property int socket;
@property id device;
@property id io;
@property id hid;
@property NSArray *descriptors;
@property NSUUID *callbackID;
@property dispatch_queue_t frameQueue;
@property dispatch_queue_t hidQueue;
@property NSLock *frameLock;
@property NSLock *outputLock;
@property dispatch_queue_t outputQueue;
@property BOOL sendingFrame;
@property BOOL dirty;
@property BOOL visible;
@property uint32_t width;
@property uint32_t height;
@property BOOL touching;
@property CGPoint lastPoint;
@property uint32_t touchID;
@property uint8_t touchEdge;
@property CreateDigitizer createDigitizer;
@property CreateFinger createFinger;
@property AppendEvent appendEvent;
@property WrapDigitizer wrapDigitizer;
@property KeyboardEvent keyboardEvent;
@property ButtonEvent buttonEvent;
@property NSData *previousPNG;
- (void)sendJSON:(NSDictionary *)value;
- (void)startWithUDID:(NSString *)udid developer:(NSString *)developer;
- (void)capture;
- (void)input:(NSDictionary *)input;
- (void)releaseTouch;
- (void)close;
@end

@implementation RiWorkBridge
- (instancetype)init {
    if ((self = [super init])) { _socket = -1; _visible = YES; _dirty = YES; _frameLock = [NSLock new]; _outputLock = [NSLock new]; _outputQueue = dispatch_queue_create("dev.riwork.ios-preview.output", DISPATCH_QUEUE_SERIAL); _callbackID = [NSUUID UUID]; _frameQueue = dispatch_queue_create("dev.riwork.ios-preview.frames", DISPATCH_QUEUE_SERIAL); _hidQueue = dispatch_queue_create("dev.riwork.ios-preview.hid", DISPATCH_QUEUE_SERIAL); }
    return self;
}
- (void)sendJSON:(NSDictionary *)value {
    [_outputLock lock];
    @try {
    NSError *error; NSMutableData *line = [[NSJSONSerialization dataWithJSONObject:value options:0 error:&error] mutableCopy];
    if (!line || line.length >= 16 * 1024 * 1024) fail(@"Simulator preview frame exceeds the transport limit.");
    [line appendBytes:"\n" length:1];
    const uint8_t *bytes = line.bytes; size_t remaining = line.length;
    while (remaining && !stopped) { ssize_t count = send(_socket, bytes, remaining, 0); if (count < 0 && errno == EINTR) continue; if (count <= 0) fail(@"The RiWork preview connection closed or stopped reading frames."); remaining -= count; bytes += count; }
    } @finally { [_outputLock unlock]; }
}
- (void)startWithUDID:(NSString *)udid developer:(NSString *)developer {
    SimulatorFrameworks frameworks = load_frameworks(developer);
    void *kit = frameworks.kit, *iokit = frameworks.iokit;
    NSInteger version = frameworks.version;
    _createDigitizer = symbol(iokit, "IOHIDEventCreateDigitizerEvent"); _createFinger = symbol(iokit, "IOHIDEventCreateDigitizerFingerEvent"); _appendEvent = symbol(iokit, "IOHIDEventAppendEvent");
    _wrapDigitizer = symbol(kit, "IndigoHIDMessageForTrackpadEventFromHIDEventRef"); _keyboardEvent = symbol(kit, "IndigoHIDMessageForKeyboardArbitrary"); _buttonEvent = symbol(kit, "IndigoHIDMessageForButton");
    Class contextClass = NSClassFromString(@"SimServiceContext");
    if (![contextClass respondsToSelector:@selector(sharedServiceContextForDeveloperDir:error:)]) fail(@"This Xcode has no supported Simulator service context.");
    NSError *error;
    id context = [contextClass sharedServiceContextForDeveloperDir:developer error:&error];
    id devices = [context defaultDeviceSetWithError:&error];
    for (id device in get(devices, @"availableDevices")) { if ([[get(device, @"UDID") UUIDString] caseInsensitiveCompare:udid] == NSOrderedSame) { _device = device; break; } }
    if (!_device) fail([NSString stringWithFormat:@"Simulator %@ was not found in the selected Xcode's device set. %@", udid, error.localizedDescription ?: @""]);
    NSNumber *state = [_device valueForKey:@"state"];
    if (state.integerValue != 3) fail(@"The selected Simulator must be fully booted before streaming starts.");
    _io = get(_device, @"io"); [_io updateIOPorts];
    NSMutableArray *descriptors = [NSMutableArray array];
    for (id port in get(_io, @"deviceIOPorts")) {
        if ([get(port, @"portIdentifier") isEqual:@"com.apple.framebuffer.display"]) {
            id descriptor = get(port, @"descriptor");
            if ([descriptor respondsToSelector:NSSelectorFromString(@"framebufferSurface")]) [descriptors addObject:descriptor];
        }
    }
    if (!descriptors.count) fail(@"The booted Simulator exposes no framebuffer. Wait for boot to finish, then restart the preview.");
    _descriptors = descriptors;
    __weak RiWorkBridge *weak = self;
    void (^dirty)(void) = ^{ RiWorkBridge *bridge = weak; [bridge.frameLock lock]; bridge.dirty = YES; [bridge.frameLock unlock]; };
    SEL registration = @selector(registerScreenCallbacksWithUUID:callbackQueue:frameCallback:surfacesChangedCallback:propertiesChangedCallback:);
    for (id descriptor in _descriptors) {
        if (![descriptor respondsToSelector:registration]) fail(@"This Xcode's framebuffer callback API is unsupported. Update RiWork's Simulator adapter.");
        [descriptor registerScreenCallbacksWithUUID:_callbackID callbackQueue:_frameQueue frameCallback:dirty surfacesChangedCallback:dirty propertiesChangedCallback:dirty];
    }
    Class hidClass = NSClassFromString(@"SimulatorKit.SimDeviceLegacyHIDClient") ?: NSClassFromString(@"_TtC12SimulatorKit24SimDeviceLegacyHIDClient");
    if (![hidClass instancesRespondToSelector:@selector(initWithDevice:error:)]) fail(@"This Xcode's Simulator HID client is unsupported. Update RiWork's Simulator adapter.");
    _hid = [[hidClass alloc] initWithDevice:_device error:&error];
    if (!_hid || ![_hid respondsToSelector:@selector(sendWithMessage:freeWhenDone:completionQueue:completion:)]) fail([NSString stringWithFormat:@"Cannot connect to Simulator touch input: %@", error.localizedDescription ?: @"unsupported HID API"]);
    fprintf(stderr, "RiWork iOS device bridge connected to %s (CoreSimulator %ld); native framebuffer and HID, no Simulator window.\n", udid.UTF8String, (long)version);
}
- (void)capture {
    if (!_visible) return;
    [_frameLock lock]; BOOL dirty = _dirty; if (_sendingFrame) { [_frameLock unlock]; return; } _dirty = NO; [_frameLock unlock];
    if (!dirty && _previousPNG) return;
    IOSurfaceRef surface = NULL; size_t area = 0;
    for (id descriptor in _descriptors) {
        IOSurfaceRef candidate = (__bridge IOSurfaceRef)get(descriptor, @"framebufferSurface");
        if (candidate && IOSurfaceGetWidth(candidate) * IOSurfaceGetHeight(candidate) > area) { surface = candidate; area = IOSurfaceGetWidth(candidate) * IOSurfaceGetHeight(candidate); }
    }
    if (!surface) { [_frameLock lock]; _dirty = YES; [_frameLock unlock]; return; }
    CFRetain(surface);
    size_t width = IOSurfaceGetWidth(surface), height = IOSurfaceGetHeight(surface);
    OSType format = IOSurfaceGetPixelFormat(surface);
    if (!width || !height || width > 8192 || height > 8192 || (format != 'BGRA' && format != 'RGBA')) { CFRelease(surface); fail(@"The Simulator framebuffer has an unsupported pixel format or size."); }
    double scale = fmin(1.0, 2048.0 / fmax(width, height));
    uint32_t outWidth = MAX(1, (uint32_t)llround(width * scale)), outHeight = MAX(1, (uint32_t)llround(height * scale));
    CGColorSpaceRef color = CGColorSpaceCreateDeviceRGB();
    IOSurfaceLock(surface, kIOSurfaceLockReadOnly, NULL);
    CGBitmapInfo bitmap = format == 'BGRA' ? kCGBitmapByteOrder32Little | kCGImageAlphaPremultipliedFirst : kCGBitmapByteOrder32Big | kCGImageAlphaPremultipliedLast;
    CGContextRef source = CGBitmapContextCreate(IOSurfaceGetBaseAddress(surface), width, height, 8, IOSurfaceGetBytesPerRow(surface), color, bitmap);
    CGImageRef image = source ? CGBitmapContextCreateImage(source) : NULL;
    CGContextRef destination = CGBitmapContextCreate(NULL, outWidth, outHeight, 8, outWidth * 4, color, kCGBitmapByteOrder32Big | kCGImageAlphaPremultipliedLast);
    if (!image || !destination) { if (image) CGImageRelease(image); if (source) CGContextRelease(source); if (destination) CGContextRelease(destination); IOSurfaceUnlock(surface, kIOSurfaceLockReadOnly, NULL); CGColorSpaceRelease(color); CFRelease(surface); fail(@"Cannot convert the Simulator framebuffer to a preview image."); }
    CGContextSetInterpolationQuality(destination, kCGInterpolationLow);
    CGContextDrawImage(destination, CGRectMake(0, 0, outWidth, outHeight), image);
    // Release the guest framebuffer as soon as pixels reach our bounded bitmap;
    // PNG compression and socket delivery must not hold the Simulator's surface.
    CGImageRelease(image); CGContextRelease(source);
    IOSurfaceUnlock(surface, kIOSurfaceLockReadOnly, NULL); CFRelease(surface);
    CGImageRef scaled = CGBitmapContextCreateImage(destination);
    NSMutableData *png = [NSMutableData data];
    CGImageDestinationRef encoder = CGImageDestinationCreateWithData((__bridge CFMutableDataRef)png, CFSTR("public.png"), 1, NULL);
    if (encoder) CGImageDestinationAddImage(encoder, scaled, NULL);
    BOOL encoded = encoder && CGImageDestinationFinalize(encoder);
    if (encoder) CFRelease(encoder); CGImageRelease(scaled); CGContextRelease(destination); CGColorSpaceRelease(color);
    if (!encoded) fail(@"Cannot encode the Simulator preview image.");
    _width = outWidth; _height = outHeight;
    if (![_previousPNG isEqualToData:png]) {
        _previousPNG = png; [_frameLock lock]; _sendingFrame = YES; [_frameLock unlock];
        NSDictionary *frame = @{@"type":@"frame", @"png":[png base64EncodedStringWithOptions:0], @"width":@(outWidth), @"height":@(outHeight)};
        dispatch_async(_outputQueue, ^{
            @autoreleasepool {
            @try { [self sendJSON:frame]; }
            @catch (NSException *error) { fprintf(stderr, "RiWork iOS preview: %s\n", error.reason.UTF8String); transport_failed = 1; stopped = 1; }
            [self.frameLock lock]; self.sendingFrame = NO; [self.frameLock unlock];
            }
        });
    }
}
- (void)sendHID:(void *)message {
    if (!message) fail(@"The selected Xcode could not construct a Simulator input event.");
    __weak RiWorkBridge *weak = self;
    [_hid sendWithMessage:message freeWhenDone:YES completionQueue:_hidQueue completion:^(NSError *error) { if (error) { fprintf(stderr, "Simulator HID delivery failed: %s\n", error.localizedDescription.UTF8String); transport_failed = 1; stopped = 1; } else { RiWorkBridge *bridge = weak; [bridge.frameLock lock]; bridge.dirty = YES; [bridge.frameLock unlock]; } }];
}
- (void)touch:(CGPoint)point phase:(int)phase {
    BOOL touching = phase != 2;
    uint32_t mask = touching ? 7 : 6;
    uint64_t now = mach_absolute_time();
    CFTypeRef parent = _createDigitizer(NULL, now, 2, 0, _touchID, mask, 0, point.x, point.y, 0, 0, 0, touching, touching, 0);
    CFTypeRef finger = _createFinger(NULL, now, 0, _touchID, mask, point.x, point.y, 0, 0, 0, touching, touching, 0);
    if (!parent || !finger) { if (parent) CFRelease(parent); if (finger) CFRelease(finger); fail(@"Cannot construct the Simulator digitizer event."); }
    _appendEvent(parent, finger, 0); void *message = _wrapDigitizer(parent); CFRelease(finger); CFRelease(parent);
    // The wrapper leaves routing unset. Reject unknown layouts before patching.
    if (!message || malloc_size(message) != 0x180) { if (message) free(message); fail(@"This Xcode produced an unknown touch-event layout. Update RiWork's Simulator adapter."); }
    uint8_t *bytes = message; uint32_t target = 0x32;
    memcpy(bytes + 0x6c, &target, 4); memcpy(bytes + 0x10c, &target, 4);
    bytes[0x3a] = bytes[0xda] = _touchEdge ? 4 : 0; bytes[0x3b] = bytes[0xdb] = _touchEdge;
    [self sendHID:message]; _lastPoint = point; _touching = touching;
}
- (void)releaseTouch { if (_touching) [self touch:_lastPoint phase:2]; }
- (void)keyUsage:(uint32_t)usage down:(BOOL)down { [self sendHID:_keyboardEvent(usage, down ? 1 : 2)]; }
- (void)input:(NSDictionary *)input {
    NSString *kind = input[@"kind"];
    if ([kind isEqual:@"refresh"]) { _previousPNG = nil; [_frameLock lock]; _dirty = YES; [_frameLock unlock]; return; }
    double x = [input[@"x"] doubleValue], y = [input[@"y"] doubleValue];
    if (!isfinite(x) || !isfinite(y)) return;
    CGPoint point = CGPointMake(fmax(0, fmin(1, x / MAX(1, _width))), fmax(0, fmin(1, y / MAX(1, _height))));
    if ([kind isEqual:@"pointer_down"] || [kind isEqual:@"click"]) {
        if (![input[@"button"] isEqual:@"left"]) { fprintf(stderr, "Simulator touch input accepts the left pointer button only.\n"); return; }
        if (!_width) return;
        [self releaseTouch]; _touchID = _touchID % 100000 + 1;
        _touchEdge = point.y >= .99 ? 1 : point.x <= .01 ? 2 : point.x >= .99 ? 4 : point.y <= .01 ? 8 : 0;
        [self touch:point phase:0];
        if ([kind isEqual:@"click"]) { usleep(30000); [self releaseTouch]; }
    } else if ([kind isEqual:@"pointer_move"]) { if (_touching) [self touch:point phase:1]; }
    else if ([kind isEqual:@"pointer_up"]) { if (_touching) [self touch:point phase:2]; }
    else if ([kind isEqual:@"focus"]) { if (![input[@"focused"] boolValue]) [self releaseTouch]; }
    else if ([kind isEqual:@"scroll"]) {
        double dx = [input[@"delta_x"] doubleValue], dy = [input[@"delta_y"] doubleValue];
        if (!isfinite(dx) || !isfinite(dy) || !_width) return;
        [self releaseTouch]; _touchID = _touchID % 100000 + 1; _touchEdge = 0; [self touch:point phase:0];
        CGPoint end = CGPointMake(fmax(.02, fmin(.98, point.x + dx / _width)), fmax(.02, fmin(.98, point.y + dy / _height)));
        for (int step = 1; step <= 6; ++step) { usleep(8000); [self touch:CGPointMake(point.x + (end.x - point.x) * step / 6, point.y + (end.y - point.y) * step / 6) phase:1]; }
        [self releaseTouch];
    } else if ([kind isEqual:@"key"] || [kind isEqual:@"key_up"]) {
        NSDictionary *keys = @{@"enter":@40, @"escape":@41, @"backspace":@42, @"tab":@43, @"space":@44, @"delete":@76, @"right":@79, @"left":@80, @"down":@81, @"up":@82};
        NSString *key = input[@"key"]; NSNumber *usage = keys[key];
        if (usage) [self keyUsage:usage.unsignedIntValue down:[kind isEqual:@"key"]];
        else if ([key isEqual:@"home"] && [kind isEqual:@"key"]) { [self releaseTouch]; [self sendHID:_buttonEvent(0, 1, 0x33)]; usleep(30000); [self sendHID:_buttonEvent(0, 2, 0x33)]; }
    } else if ([kind isEqual:@"text"]) {
        NSString *text = input[@"text"];
        if (![text isKindOfClass:NSString.class] || text.length > 4096) return;
        NSString *plain = @"abcdefghijklmnopqrstuvwxyz1234567890\n\t -=[]\\;\',./";
        NSString *shift = @"ABCDEFGHIJKLMNOPQRSTUVWXYZ!@#$%^&*()\n\t _+{}|:\"<>?";
        uint32_t usages[] = {4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,43,44,45,46,47,48,49,51,52,54,55,56};
        for (NSUInteger index = 0; index < text.length; ++index) { unichar character = [text characterAtIndex:index]; NSString *item = [NSString stringWithCharacters:&character length:1]; NSRange found = [plain rangeOfString:item]; BOOL shifted = NO; if (found.location == NSNotFound) { found = [shift rangeOfString:item]; shifted = YES; } if (found.location == NSNotFound || found.location >= sizeof(usages)/sizeof(*usages)) { fprintf(stderr, "Simulator preview text input currently supports the US keyboard layout; unsupported character skipped.\n"); continue; } if (shifted) [self keyUsage:225 down:YES]; [self keyUsage:usages[found.location] down:YES]; [self keyUsage:usages[found.location] down:NO]; if (shifted) [self keyUsage:225 down:NO]; }
    }
}
- (void)close {
    dispatch_sync(_outputQueue, ^{});
    @try { [self releaseTouch]; for (id descriptor in _descriptors) if ([descriptor respondsToSelector:@selector(unregisterScreenCallbacksWithUUID:)]) [descriptor unregisterScreenCallbacksWithUUID:_callbackID]; } @catch (NSException *ignored) { (void)ignored; }
    _descriptors = nil; _hid = nil; _io = nil; _device = nil;
    if (_socket >= 0) { shutdown(_socket, SHUT_RDWR); close(_socket); _socket = -1; }
}
@end

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        signal(SIGTERM, stop_signal); signal(SIGINT, stop_signal); signal(SIGPIPE, SIG_IGN);
        RiWorkBridge *bridge = [RiWorkBridge new]; int result = 0;
        @try {
            if (argc == 2 && !strcmp(argv[1], "--check-support")) {
                SimulatorFrameworks frameworks = load_frameworks(developer_directory());
                symbol(frameworks.iokit, "IOHIDEventCreateDigitizerEvent"); symbol(frameworks.iokit, "IOHIDEventCreateDigitizerFingerEvent"); symbol(frameworks.iokit, "IOHIDEventAppendEvent");
                symbol(frameworks.kit, "IndigoHIDMessageForTrackpadEventFromHIDEventRef"); symbol(frameworks.kit, "IndigoHIDMessageForKeyboardArbitrary"); symbol(frameworks.kit, "IndigoHIDMessageForButton");
                Class context = NSClassFromString(@"SimServiceContext");
                Class hid = NSClassFromString(@"SimulatorKit.SimDeviceLegacyHIDClient") ?: NSClassFromString(@"_TtC12SimulatorKit24SimDeviceLegacyHIDClient");
                if (![context respondsToSelector:@selector(sharedServiceContextForDeveloperDir:error:)] || ![hid instancesRespondToSelector:@selector(initWithDevice:error:)] || ![hid instancesRespondToSelector:@selector(sendWithMessage:freeWhenDone:completionQueue:completion:)]) fail(@"This Xcode's Simulator service or HID API is unsupported. Update RiWork's Simulator adapter.");
                printf("{\"supported\":true,\"core_simulator_version\":%ld,\"protocol\":1}\n", (long)frameworks.version);
                return 0;
            }
            NSString *udid;
            for (int index = 1; index < argc; ++index) { if (!strcmp(argv[index], "--udid") && index + 1 < argc) udid = [NSString stringWithUTF8String:argv[++index]]; else fail(@"Usage: riwork-ios-preview-bridge --udid <exact Simulator UUID>"); }
            if (!udid || ![[NSUUID alloc] initWithUUIDString:udid]) fail(@"An exact Simulator UUID is required; implicit 'booted' targeting is forbidden.");
            NSDictionary *env = NSProcessInfo.processInfo.environment; NSString *socketPath = env[@"RIWORK_PREVIEW_SOCKET"], *token = env[@"RIWORK_PREVIEW_TOKEN"];
            if (!socketPath.length || !token.length || token.length > 1024) fail(@"Start this helper through RiWork: its private preview socket and token are required.");
            struct sockaddr_un address = {0}; address.sun_family = AF_UNIX;
            if (strlen(socketPath.fileSystemRepresentation) >= sizeof(address.sun_path)) fail(@"The private preview socket path is too long.");
            strlcpy(address.sun_path, socketPath.fileSystemRepresentation, sizeof(address.sun_path));
            bridge.socket = socket(AF_UNIX, SOCK_STREAM, 0);
            struct timeval timeout = {.tv_sec = 2}; setsockopt(bridge.socket, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof(timeout));
            if (bridge.socket < 0 || connect(bridge.socket, (struct sockaddr *)&address, sizeof(address))) fail([NSString stringWithFormat:@"Cannot connect to RiWork's private preview socket: %s", strerror(errno)]);
            [bridge sendJSON:@{@"type":@"hello", @"token":token, @"pid":@(getpid()), @"protocol":@1}];
            [bridge startWithUDID:udid developer:developer_directory()];
            NSMutableData *incoming = [NSMutableData data]; BOOL ready = NO;
            uint64_t lastCapture = 0; mach_timebase_info_data_t timebase; mach_timebase_info(&timebase);
            while (!stopped) { @autoreleasepool {
                fd_set readfds; FD_ZERO(&readfds); FD_SET(bridge.socket, &readfds); struct timeval tick = {.tv_usec = 10000};
                int selected = select(bridge.socket + 1, &readfds, NULL, NULL, &tick);
                if (selected < 0 && errno != EINTR) fail(@"Cannot read the RiWork preview connection.");
                if (selected > 0) {
                    uint8_t buffer[8192]; ssize_t count = recv(bridge.socket, buffer, sizeof(buffer), 0);
                    if (count <= 0) break;
                    [incoming appendBytes:buffer length:count];
                    while (YES) {
                        const uint8_t *newline = memchr(incoming.bytes, '\n', incoming.length);
                        if (!newline) { if (incoming.length > 65536) fail(@"Preview input message exceeds the size limit."); break; }
                        NSUInteger length = newline - (const uint8_t *)incoming.bytes;
                        if (length > 65536) fail(@"Preview input message exceeds the size limit.");
                        NSData *line = [incoming subdataWithRange:NSMakeRange(0, length)]; [incoming replaceBytesInRange:NSMakeRange(0, length + 1) withBytes:NULL length:0];
                        NSDictionary *message = [NSJSONSerialization JSONObjectWithData:line options:0 error:nil];
                        if (![message isKindOfClass:NSDictionary.class]) fail(@"Invalid preview input JSON.");
                        NSString *type = message[@"type"];
                        if ([type isEqual:@"ready"]) { if ([message[@"protocol"] unsignedIntValue] != 1) fail(@"Unsupported RiWork preview protocol."); ready = YES; }
                        else if ([type isEqual:@"shutdown"]) stopped = 1;
                        else if (ready && [type isEqual:@"visibility"]) { bridge.visible = [message[@"visible"] boolValue]; if (!bridge.visible) [bridge releaseTouch]; else { bridge.previousPNG = nil; [bridge.frameLock lock]; bridge.dirty = YES; [bridge.frameLock unlock]; } }
                        else if (ready && [type isEqual:@"input"]) { if ([message[@"input"] isKindOfClass:NSDictionary.class]) [bridge input:message[@"input"]]; }
                    }
                }
                uint64_t now = mach_absolute_time(); double elapsed = (double)(now - lastCapture) * timebase.numer / timebase.denom / 1e9;
                if (ready && !stopped && elapsed >= .05) { [bridge capture]; lastCapture = now; }
            } }
        } @catch (NSException *error) {
            fprintf(stderr, "RiWork iOS preview: %s\n", error.reason.UTF8String); result = 1;
            @try { if (bridge.socket >= 0 && !stopped) [bridge sendJSON:@{@"type":@"error", @"message":error.reason ?: @"Simulator bridge failed"}]; } @catch (NSException *ignored) { (void)ignored; }
        }
        [bridge close]; return result || transport_failed;
    }
}
