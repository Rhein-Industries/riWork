# RiWork iOS Simulator device bridge

This helper connects RiWork's authenticated App Preview socket directly to one
booted CoreSimulator device. Simulator.app stays closed: frames come from the
device's IOSurface framebuffer, and touches go to its persistent HID connection.
It does not capture the Mac desktop, synthesize Mac input, or require Accessibility
or Screen Recording permission.

RiWork's engine creates and owns the device, builds and installs the requested
app, then starts this helper with its exact UUID. The helper never chooses a
device implicitly, launches a Simulator window, or changes device lifecycle.

```
xcrun clang -fobjc-arc -O2 \
  -framework Foundation -framework CoreGraphics \
  -framework ImageIO -framework IOSurface main.m -o bridge
./bridge --check-support
RIWORK_PREVIEW_SOCKET=/private/preview.sock \
RIWORK_PREVIEW_TOKEN=... ./bridge --udid EXACT-SIMULATOR-UUID
```

The engine embeds and compiles this one source file into its private cache. Full
Xcode must be selected with `xcode-select` or `DEVELOPER_DIR`. Private frameworks
are loaded from the selected installation at runtime. Unsupported framework
versions, missing selectors, and unexpected touch message sizes fail explicitly;
there is no alternative desktop capture or input provider.

## Transport and input

The socket uses the version 1 JSON-lines protocol in
`riwork-core/src/app_preview.rs`. The helper authenticates with its actual process
ID and the launch token. Only after the host's `ready` reply does it accept input
or send frames. Frames are PNGs up to 2048 pixels on their longest edge, capped at
20 frames per second. Unchanged frames are suppressed; visibility resume and
`refresh` explicitly force a frame. This is bounded CPU encoding and transport,
not a claim of hardware video encoding or zero-copy presentation.

The output worker holds at most one frame. Slow socket delivery cannot block the
input reader or accumulate a frame queue; an unresponsive peer fails after the
socket timeout. The native framebuffer is unlocked before PNG compression.
Pausing skips capture and releases any held finger. Shutdown unregisters callbacks
and closes only this helper's connections; the engine owns Simulator cleanup.

Input coordinates refer to received frame pixels with a top-left origin. They are
normalized against that frame before native touch delivery, so downscaling and
RiWork's aspect-fit display do not change the intended target. A left pointer
press, move, and release form one continuous finger contact with a stable ID.
Touches starting at an outer edge retain an edge tag for iOS system gestures.
Scroll input produces a short continuous finger drag. Home is `key: "home"`.
Resize is intentionally ignored because device geometry is fixed.

The initial adapter supports one finger, Home, common navigation/editing keys,
and text from the US keyboard layout. Multi-touch and general Unicode text
insertion are not yet implemented. Unsupported text is reported in the helper
log rather than treated as successful input. Touch coordinates are tested in the
default portrait orientation; automatic orientation control is not provided.

## Verified compatibility and reproduction

The native adapter has been exercised against Xcode 26.0.1 (17A400),
CoreSimulator 1048, on both iOS 26.0 and iOS 18.1. It restricts its private API
layout to CoreSimulator 1000–1099 and additionally validates symbols, selectors,
pixel formats, and the exact 384-byte digitizer message allocation before use.
Other toolchain/runtime combinations need verification before expanding support.

```
/usr/bin/python3 tools/ios-preview-bridge/tests/smoke.py \
  --artifacts /tmp/riwork-ios-proof

/usr/bin/python3 tools/ios-preview-bridge/tests/smoke.py \
  --runtime com.apple.CoreSimulator.SimRuntime.iOS-18-1 \
  --device-type com.apple.CoreSimulator.SimDeviceType.iPhone-16-Pro
```

The test creates a uniquely named device and deletes that exact device in a
`finally` block. It builds a real UIKit app from the included Xcode project and
checks both streamed PNG changes and app-written state: tap coordinates, a
continuous twelve-move drag, release on pause, capture pause/resume, refresh
without releasing a held touch, scroll, and Home backgrounding the app. It never
targets existing simulators or opens Simulator.app.

The private API call shapes and digitizer routing were investigated using the
installed frameworks and these primary open-source implementations:

- [Meta idb's native Simulator architecture](https://github.com/facebook/idb/tree/main/FBSimulatorControl).
- [SimKit's framebuffer interface](https://github.com/subdiox/SimKit/blob/main/Sources/Display/SimulatorFramebuffer.swift).
- [SimKit's digitizer dispatcher](https://github.com/subdiox/SimKit/blob/main/Sources/Input/IOHIDDigitizerDispatch.swift).

See [NOTICE](NOTICE) for attribution to those interface investigations.
