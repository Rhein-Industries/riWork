# Windowless GPUI development preview

This opt-in adapter renders a GPUI root in a separate process and streams its
actual GPU output into a RiWork App Preview tab. It does not create an NSWindow,
desktop surface, or Dock entry. It does not use screen capture or accessibility
input, so the cooperative mode needs neither permission.

The current adapter is for **development**, using the pinned `gpui-pre` 0.3.8
`test-support` headless platform with real Metal rendering and native text
shaping on macOS. The production GPUI headless application does not expose this
rendering interface yet. Rendering is PNG over a private local socket, not shared
GPU memory. Native dialogs, native window handles, native clipboard,
accessibility, and platform IME composition are unavailable in this headless
platform. Committed Unicode text, GPUI key bindings, pointer events, scrolling,
and resizing work. Background tasks use GPUI's simulated scheduler, advanced
from elapsed wall time; native platform integration needs a dedicated adapter.
RiWork forwards plain text pasted from the host clipboard. Copying text from
the preview back to the host clipboard is not yet synchronized.

## Try the included application

In RiWork, add an **App Preview** tab, select cooperative mode, use this repository
as the working directory, and enter:

```sh
cargo run -p riwork-gpui-preview --example counter
```

The tab owns the process. Restart rebuilds it; Stop closes it. Restoring the tab
does not run its saved command automatically.

The example contains a clickable counter and an editable Unicode text field.
To verify real Metal rendering, GPUI click dispatch, committed text, Backspace,
and resizing without opening a desktop window:

```sh
cargo run -p riwork-gpui-preview --example counter -- --self-test
```

To save that test's final rendered frame, set `RIWORK_PREVIEW_SMOKE_PNG` on this
command. The protocol smoke test additionally checks authentication, PNG frames,
resize, paused rendering, and shutdown:

```sh
cargo build -p riwork-gpui-preview --example counter
python3 crates/riwork-gpui-preview/tests/smoke.py target/debug/examples/counter
```

## Add preview mode to your GPUI app

Add this path dependency, keeping your GPUI dependencies pinned to the same
version:

```toml
[dependencies]
riwork-gpui-preview = { path = "../riWork/crates/riwork-gpui-preview" }
```

In your app's main function, select the preview entry point before starting its
normal application loop. Reuse the same root builder:

```rust,ignore
fn main() -> anyhow::Result<()> {
    if riwork_gpui_preview::requested() {
        return riwork_gpui_preview::run(|window, cx| {
            // Register your globals/actions/assets first if your root needs them.
            cx.new(|cx| MyRoot::new(window, cx))
        });
    }
    // Existing normal desktop startup.
    start_desktop_app()
}
```

This adapter uses an empty asset source by default. `run_with_assets` and
`HeadlessPreview::with_assets` accept your application's existing GPUI
`AssetSource`; initialize globals, fonts, themes, and key bindings in the root
builder before constructing the view. Native calls that expect a desktop window
must be replaced or skipped in preview mode.

## Wire protocol, version 1

RiWork launches the command with a private `RIWORK_PREVIEW_SOCKET` and a random
`RIWORK_PREVIEW_TOKEN`. Only the launched process and its owned descendants may
authenticate. Do not expose or persist the token. Every message is newline
delimited UTF-8 JSON, bounded at 16 MiB including its newline.

The child sends `{"type":"hello","token":"…","pid":123,"protocol":1}`;
RiWork replies `{"type":"ready","protocol":1}`. Frames are
`{"type":"frame","png":"<base64 PNG>","width":960,"height":600}`.
Frame dimensions, pointer coordinates, scroll deltas, and resize dimensions all
use logical pixels at scale one. The adapter downsamples GPUI's 2x backing image
before sending it. Each side is limited to 2048 pixels; very large noisy frames
can also exceed the message limit.

RiWork sends `{"type":"input","input":{"kind":"…",…}}` with:

| Input kind | Fields |
| --- | --- |
| `click`, `pointer_down`, `pointer_up` | `x`, `y`, `button` (`left`, `right`, `middle`) |
| `pointer_move` | `x`, `y` |
| `scroll` | `x`, `y`, `delta_x`, `delta_y` |
| `text` | `text`, a committed UTF-8 string |
| `key`, `key_up` | `key`, GPUI syntax such as `cmd-a`, `backspace`, `enter` |
| `resize` | `width`, `height` |
| `focus` | `focused` |

`{"type":"visibility","visible":false}` pauses frame rendering while the tab
is hidden; true resumes it. `{"type":"shutdown"}` requests exit. EOF also ends
the preview. RiWork can terminate the entire owned process group after shutdown.
The SDK caps visible rendering at 20 fps, suppresses unchanged frames, and holds
one queued outgoing frame. Input messages have a separate 64 KiB limit and input
buffering is bounded to 128 messages.
