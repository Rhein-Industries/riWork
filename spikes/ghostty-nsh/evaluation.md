# Native libghostty embedding evaluation

Throwaway spike research, 2026-10-27. This document records source inspection,
not GUI verification; `WORKER_REPORT.md` owns the observed results and GO/NO-GO.

## Decision being tested

Use a small Rust-owned AppKit `NSView` bridge calling the full Ghostty C surface
API directly. A GPUI runtime or Entity is not required by that API. Reusing the
MIT `gpui-libghostty` native shim is feasible but does not supply AppKit input:
its child view returns `nil` from `hitTest:` and expects the GPUI adapter to
forward events. Its key wrapper also hardcodes `composing = false`. A native
host therefore needs its own responder and `NSTextInputClient` implementation.

| Route | What it supplies | Remaining work / risk |
| --- | --- | --- |
| Fork the `gpui-libghostty` 0.3.1 Objective-C shim | Full Ghostty Metal surface, config loading, mouse/key FFI wrappers, snapshot/frame instrumentation | Replace pass-through view with native input handling; add IME, display notifications, reparenting, host lifecycle; remove unnecessary layer-method interposition |
| Direct full Ghostty C surface API | Same native Metal renderer and PTY; accepts a macOS `NSView` pointer | Small new ownership/input bridge; pin internal ABI and Ghostty build; maintain AppKit integration |
| Ghostling / `libghostty-vt` | VT parser/state and render-state API, demonstrated with Raylib | Own renderer, glyph shaping/font handling, PTY, native AppKit input and lifecycle; substantially more work for this spike |

The full surface API and `libghostty-vt` must not be conflated. Ghostling uses
the latter, which intentionally has no drawing or windowing code. Its README
also notes input limitations inherited from Raylib. It is useful reference code
for VT consumers, not a ready-made AppKit/Metal surface replacement.

## Exact local provenance

Package source:

```text
/Users/dominik/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui-libghostty-0.3.1
```

Relevant package-relative files:

- `shim/ghostty_surface.m`: Objective-C native bridge (no GPUI imports).
- `src/native.rs`: Rust native ownership and narrow FFI wrappers.
- `src/adapter.rs`: the separate GPUI adapter macro.
- `vendor/ghostty/include/ghostty.h`: full app/surface C API used by this spike.
- `build.rs`: compile/link setup and native cache fingerprint.
- `vendor/ghostty/LICENSE`: Ghostty MIT notice.

`.cargo_vcs_info.json` reports package revision
`2bbde4ae53a8e163cdbe05294bd09934b30c1af3`, path `crates/gpui-ghostty`.
Normalized `Cargo.toml` identifies version `0.3.1`, MIT, and
`https://github.com/behzade/gpui-libghostty`.
Vendored Ghostty `build.zig.zon` identifies `1.3.2-dev` and minimum Zig `0.16.0`.

Existing local native cache (read, not installed or rebuilt by this research):

```text
/Users/dominik/Library/Caches/gpui-libghostty/native/b1b8e8c04734504b2bf3bf7553ee247f
```

The cache's `lib/libghostty-internal.a` is the full native library. Its
`lib/libghostty-vt.a` is a different artifact and is insufficient for the full
`ghostty_surface_*` API. The cache includes `include/ghostty.h`; its SHA-256
matches the package's vendored header exactly.

```text
75443f9cd0b74a0b41f51b214bbc1cc00c10b94d10710302f5eca9971f157b66  cached libghostty-internal.a
f2c12e1bb02540d79feed6bd29ee37312c4e0d24391442a63e66f06e1f7f2e4a  cached and vendored ghostty.h
a8746d10331bcc24ae8d5360188d24b96cca4fbb52564811f4f5a00cdd47f1a1  package shim/ghostty_surface.m
```

The package's native fingerprint includes target triple, Zig version, Xcode
developer/SDK paths, SDK/Clang versions, Ghostty build options, and source-tree
contents. A cache directory name is not a portable dependency lock or a
substitute for a reproducible production build. These hashes record what this
spike used, not an independent audit of how the cached binary was produced.

## Build/link reference

The package compiles its Objective-C shim using Clang with `-fblocks`,
`-fno-objc-arc`, the macOS SDK path, and the vendored include directory, then
archives it with `ar rcs`. Link the native archive, `c++`, `objc`, and frameworks
AppKit, Carbon, CoreFoundation, CoreGraphics, CoreText, CoreVideo, Foundation,
IOSurface, Metal, and QuartzCore.

The package's Ghostty build options are:

```text
-Dversion-string=1.3.2-dev -Dapp-runtime=none
-Demit-xcframework=false -Demit-macos-app=false -Demit-docs=false
-Demit-terminfo=false -Demit-bench=false -Demit-webdata=false
-Di18n=false -Dsentry=false -Doptimize=ReleaseFast
```

Compile against the matching full header/archive pair. Production must provide
the Ghostty resources/font/shell-integration/terminfo policy and all relevant
third-party notices, rather than rely on this workstation's cache.

## AppKit integration reference

Upstream Ghostty was inspected at commit
`919dca5c848d43a489a4cb2e1feade016f0fa2db`; this is a reference for host behavior,
not a claim that its evolving sources are ABI-identical to the cached build.

- `keyDown:` keeps a text accumulator while calling `interpretKeyEvents:`.
  `insertText:replacementRange:` clears preedit and appends to that accumulator
  during key handling. The completed key path sends text exactly once.
  Do not both send text from `insertText:` and then resend `event.characters`.
- Preserve the original `NSEvent` when modifier translation does not change it;
  upstream notes that reconstructing it can break Korean IME behavior.
- `setMarkedText:...` retains marked text; sync UTF-8 bytes through
  `ghostty_surface_preedit`. Clear with `(NULL, 0)`. A raw key remains composing
  when marked text exists **or existed before the event**, so cancellation does
  not leak a backspace into the terminal. Suppress bare C0 controls while
  composing. Committed text should be sent as key input, not paste input.
- For `insertText:` outside a key event, upstream emits a synthetic press with
  keycode/modifiers/unshifted codepoint zero and the committed text. Full
  Korean/Japanese behavior also needs selective replay of navigation after
  commitment; the minimal spike is not coverage of every input method.
- `ghostty_surface_ime_point` gives the candidate position. Convert top-left
  surface coordinates to AppKit bottom-left (`height - y`), then view to window
  and window to screen in `firstRectForCharacterRange:actualRange:`.
- Forward mouse positions in local **logical points**, top-left origin, without
  multiplying by backing scale. Track mouse movement/drag; use `(-1, -1)` for
  pointer exit when no drag is active. Set position before button events if the
  host cannot guarantee an earlier move event.
- Scroll uses `scrollingDeltaX/Y`. The final argument is packed scroll metadata,
  **not keyboard modifiers**: precision in bit 0, Ghostty momentum enum in bits
  1–3. Upstream subjectively multiplies precise deltas by 2.
- In `viewDidChangeBackingProperties`, update `layer.contentsScale` with implicit
  animations disabled, then `ghostty_surface_set_content_scale` and the physical
  size from `convertToBacking:`. Update `ghostty_surface_set_display_id` on
  display changes. A forced scale API test does not prove a real display move.
- Runtime wakeups must schedule `ghostty_app_tick` on the main run loop.
  Surface/app/config lifetime must outlive callbacks and the native view.
- Live theme update uses a finalized replacement config,
  `ghostty_surface_update_config`, and `ghostty_surface_refresh`; it should not
  recreate the surface or tmux client.
- Detach/reattach can preserve the same retained NSView and `ghostty_surface_t`
  while removing/re-adding it. Reapply size, scale, display, occlusion and focus.
  Separately, tmux protects the shell/session when a rendering client is freed.
  Verify surface identity, tmux session identity, pane ID and pane PID rather
  than infer continuity from identical text.

## License/attribution

The crate archive does not include a root `LICENSE`, but its pinned upstream
revision includes the MIT license with `Copyright (c) 2026 Behzad Ehsani`.
Forking substantial shim code requires retaining that notice and MIT text.
Ghostty's vendored MIT notice is `Copyright (c) 2024 Mitchell Hashimoto, Ghostty
contributors`. Ghostling's separate MIT notice is `Copyright (c) 2026 Mitchell
Hashimoto`. Preserve applicable notices when copying their source. Using the
native archive also requires auditing its bundled dependency notices before
production distribution.

## Production gates beyond the spike

Pin/rebuild the full internal ABI, add ownership/threading invariants, complete
keyboard layout and IME behavior, clipboard policy, accessibility, selection,
search and cursor/URL actions, secure-input decisions, resource packaging,
display-change and teardown race tests, native-component lifecycle integration,
and session error/recovery behavior. The upstream full C surface API remains an
internal/evolving interface; a successful spike proves feasibility, not a stable
upstream support contract.

## Observed teardown blocker: NO-GO for the pinned runtime

The fixture completed input, theme, detach/reattach and geometry exercises, but
normal window close then blocked in `ghostty_surface_free` for more than 50
seconds. The captured sample of fixture PID 26521 shows the main thread in
`NativeComponent::drop` → `SpikeTerminalInput shutdown` →
`ghostty_surface_free` → `pthread_join`. The renderer thread is absent. The IO
thread repeatedly visits `killpg`, `wait4` and `nanosleep`; the reader is in
`__ulock_wait2` and the gather thread in `poll`.

The matching vendored source path is:

- `src/Surface.zig:799`: surface teardown joins the renderer, then the IO thread.
- `src/termio/Exec.zig:200`: IO exit calls `subprocess.stop()` before writing the
  read-thread quit pipe.
- `src/termio/Exec.zig:1186`: `killPid` repeatedly sends SIGHUP and calls
  `waitpid(WNOHANG)`, sleeping 10 ms, without a timeout while the child remains.

The still-attached fixture tmux client was PID 26816 under login PID 26815.
Detaching exactly that isolated client via `detach-client -t /dev/ttys019`
released teardown: the app logged the closed PTY, closed surface and stopped
host, then exited. The tmux session, pane and recorder identities survived.
This recovery is diagnostic evidence, not a fix implemented by the fixture.

The stack shape corresponds closely to upstream
[Ghostty issue #14245](https://github.com/ghostty-org/ghostty/issues/14245), which
describes surface-free joins blocked by reader-to-app mailbox backpressure and
an alternative subprocess kill-loop hang. Our binary has stripped internal Zig
symbols, so the reader's exact wait site is not established; mailbox involvement
is an inference, not a confirmed root cause. The issue remained open when
checked on 2026-10-27. Its proposed fix,
[PR #14444, “surface: release mailbox producers before joining them”](https://github.com/ghostty-org/ghostty/pull/14444),
was also open and unmerged. It was not applied, built or verified here.

This blocks accepting the pinned library's synchronous teardown in a live host.
A GPUI-host fallback using the same native archive must independently prove
safe teardown; changing the view framework alone does not establish that the
underlying join/PTY lifecycle problem disappears. The current spike stops at
NO-GO; further work would require a separately scoped runtime/lifecycle fix and
revalidation, including closing while terminal output continues.

## Primary sources

- [Pinned gpui-libghostty MIT license](https://github.com/behzade/gpui-libghostty/blob/2bbde4ae53a8e163cdbe05294bd09934b30c1af3/LICENSE).
- [Ghostty architecture](https://ghostty.org/docs/about).
- [Ghostling README](https://github.com/ghostty-org/ghostling), including VT-only scope and Raylib limitations.
- [Ghostling license](https://github.com/ghostty-org/ghostling/blob/main/LICENSE).
- [Pinned Ghostty AppKit surface implementation](https://github.com/ghostty-org/ghostty/blob/919dca5c848d43a489a4cb2e1feade016f0fa2db/macos/Sources/Ghostty/Surface%20View/SurfaceView_AppKit.swift).
- [Pinned Ghostty input types](https://github.com/ghostty-org/ghostty/blob/919dca5c848d43a489a4cb2e1feade016f0fa2db/macos/Sources/Ghostty/Ghostty.Input.swift).
- [Ghostty MIT license](https://github.com/ghostty-org/ghostty/blob/main/LICENSE).
- [Ghostty teardown issue #14245](https://github.com/ghostty-org/ghostty/issues/14245).
- [Unmerged proposed teardown fix #14444](https://github.com/ghostty-org/ghostty/pull/14444).

## Phase 2 runtime evaluation (2026-10-27)

Phase 2 compares the cached archive above with a fresh build of the exact
package-vendored source. The package's
[pinned VENDOR.md](https://github.com/behzade/gpui-libghostty/blob/2bbde4ae53a8e163cdbe05294bd09934b30c1af3/crates/gpui-ghostty/vendor/ghostty/VENDOR.md)
identifies upstream Ghostty commit `9f0e1719dc918368367d368bfe300f59bb68b5a4`,
plus local changes for internal archive installation, the caller-owned OpenGL
context API, GLAD linkage and SDKROOT handling. Its `1.3.2-dev` label alone is
therefore insufficient to identify the runtime. The inspected vendored source
does not include the proposed teardown fix discussed below.

The rebuild succeeded in 134.562 s using the existing Zig 0.16.0 toolchain,
Apple Clang 17.0.0 and Xcode macOS SDK 26.0, with the package's exact
`app-runtime=none` / `ReleaseFast` build options. Source, copied dependency
cache, fresh object caches and output all live under ignored
`.runtime/pinned/`; no shared source/cache was changed and no installation was
performed. The full command, source manifest, toolchain hashes and output paths
are in [build provenance](evidence/phase2/runtime/pinned-rebuild-provenance.json)
and the [build log](evidence/phase2/runtime/pinned-rebuild.log).

| Artifact | SHA-256 |
| --- | --- |
| Cached `libghostty-internal.a` | `75443f9cd0b74a0b41f51b214bbc1cc00c10b94d10710302f5eca9971f157b66` |
| Fresh pinned `libghostty-internal.a` | `b51367aa565527eddfa180d5ae6304899d903af6fc094b4531671ffc20fb064f` |
| Cached, vendored and rebuilt `ghostty.h` | `f2c12e1bb02540d79feed6bd29ee37312c4e0d24391442a63e66f06e1f7f2e4a` |

The two archives are not byte-identical. Independently recomputing the package's
native fingerprint from the current vendor tree, target, Zig 0.16.0, Xcode SDK,
Clang and build options reproduces cache key
`b1b8e8c04734504b2bf3bf7553ee247f`. This supports source/toolchain consistency,
but does not independently attest which source produced the cached binary.
The [cache audit](evidence/phase2/runtime/native-cache-audit.json) records these
limits. Rebuilding improves provenance; it does not itself establish safe
teardown or a behavioral difference between the archives.

As checked through the official GitHub API on 2026-10-27,
[issue #14245](https://github.com/ghostty-org/ghostty/issues/14245) remains open,
and [PR #14444](https://github.com/ghostty-org/ghostty/pull/14444) remains open
and unmerged, with a changes-requested review. Its current head is
[`8eb96765685e581963889a85b4340fd6e289bcd6`](https://github.com/ghostty-org/ghostty/commit/8eb96765685e581963889a85b4340fd6e289bcd6),
whose revised approach keeps the app mailbox draining while joining threads.
That change was not applied, built or tested in this spike. Raw
[issue](evidence/phase2/runtime/upstream-issues-14245.json),
[PR](evidence/phase2/runtime/upstream-pulls-14444.json) and
[review](evidence/phase2/runtime/upstream-pulls-14444-reviews.json) snapshots
preserve the checked status.

The earlier NO-GO describes the original close path. The Phase 2 disposal
results, exact runtime exercised and current GO/NO-GO are recorded separately in
[WORKER_REPORT.md](WORKER_REPORT.md); a successful rebuild is not a disposal
proof.
