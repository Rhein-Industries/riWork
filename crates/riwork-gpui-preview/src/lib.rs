//! Opt-in, separate-process GPUI development preview. No native window is created.
//!
//! This uses GPUI's `test-support` headless platform and its real GPU renderer.
//! It is a development adapter: native dialogs, native window handles, clipboard,
//! accessibility, and platform IME composition are not implemented by that platform.

use anyhow::{Context as _, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use gpui::{
    App, AssetSource, Context, Entity, FocusHandle, HeadlessAppContext, KeyUpEvent, Keystroke,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PlatformInput, Render, ScrollDelta,
    ScrollWheelEvent, Window, WindowHandle, point, px, size,
};
use image::{ImageEncoder as _, RgbaImage};
use riwork_core::app_preview::{
    MAX_PREVIEW_DIMENSION, MAX_PREVIEW_INPUT_MESSAGE_BYTES, MAX_PREVIEW_MESSAGE_BYTES,
    PREVIEW_PROTOCOL_VERSION, PreviewClientMessage, PreviewHostMessage,
};
pub use riwork_core::app_preview::{PreviewInput, PreviewMouseButton};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

const FRAME_INTERVAL: Duration = Duration::from_millis(50);

pub fn requested() -> bool {
    std::env::var_os("RIWORK_PREVIEW_SOCKET").is_some()
}

/// A real GPUI root rendered into an offscreen Metal texture on macOS.
/// Construct and drive this on the application's main thread.
pub struct HeadlessPreview<V: Render + 'static> {
    pub context: HeadlessAppContext,
    pub window: WindowHandle<V>,
    logical_size: (u32, u32),
    pressed_button: Option<MouseButton>,
    saved_focus: Option<FocusHandle>,
}

impl<V: Render + 'static> HeadlessPreview<V> {
    pub fn new(
        width: u32,
        height: u32,
        build_root: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
    ) -> Result<Self> {
        Self::with_assets(width, height, Arc::new(()), build_root)
    }

    pub fn with_assets(
        width: u32,
        height: u32,
        assets: Arc<dyn AssetSource>,
        build_root: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
    ) -> Result<Self> {
        validate_size(width, height)?;
        // The native platform supplies text shaping only. All windows belong to
        // GPUI's in-memory TestPlatform, which has no raw/native window handles.
        let text_system = gpui_platform::current_platform(true).text_system();
        let mut context = HeadlessAppContext::with_platform(
            text_system,
            assets,
            gpui_platform::current_headless_renderer,
        );
        let window = context.open_window(size(px(width as f32), px(height as f32)), build_root)?;
        Ok(Self {
            context,
            window,
            logical_size: (width, height),
            pressed_button: None,
            saved_focus: None,
        })
    }

    pub fn update<R>(
        &mut self,
        update: impl FnOnce(&mut V, &mut Window, &mut Context<V>) -> R,
    ) -> Result<R> {
        self.window.update(&mut self.context, update)
    }

    pub fn pump(&mut self, elapsed: Duration) -> Result<()> {
        self.advance(elapsed);
        self.context
            .update_window(self.window.into(), |_, window, cx| {
                window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
            })?;
        Ok(())
    }

    fn advance(&mut self, elapsed: Duration) {
        self.context.advance_clock(elapsed);
        self.context.run_until_parked();
    }

    /// Produces an actual rasterized frame; the wire format uses logical pixels.
    pub fn frame(&mut self) -> Result<RgbaImage> {
        self.pump(Duration::ZERO)?;
        let image = self.context.capture_screenshot(self.window.into())?;
        let (width, height) = self.logical_size;
        // GPUI TestWindow uses a 2x backing scale. Downsample to a stable 1x wire
        // image so input, resize and frame dimensions use the same coordinate space.
        Ok(if image.dimensions() == (width, height) {
            image
        } else {
            image::imageops::resize(&image, width, height, image::imageops::FilterType::Triangle)
        })
    }

    pub fn dispatch(&mut self, input: PreviewInput) -> Result<()> {
        if let PreviewInput::Resize { width, height } = input {
            validate_size(width, height)?;
            self.logical_size = (width, height);
            self.context
                .update_window(self.window.into(), |_, window, cx| {
                    window.resize(size(px(width as f32), px(height as f32)));
                    window.bounds_changed(cx);
                })?;
            return Ok(());
        }
        let pressed_button = &mut self.pressed_button;
        let saved_focus = &mut self.saved_focus;
        self.context
            .update_window(self.window.into(), |_, window, cx| -> Result<()> {
                match input {
                    PreviewInput::Click { x, y, button } => {
                        let button = mouse_button(button);
                        pointer(window, cx, x, y, button, true);
                        // Drawing between down/up makes focus and new listeners visible.
                        window.draw(cx).clear(cx);
                        pointer(window, cx, x, y, button, false);
                    }
                    PreviewInput::PointerDown { x, y, button } => {
                        let button = mouse_button(button);
                        *pressed_button = Some(button);
                        pointer(window, cx, x, y, button, true);
                    }
                    PreviewInput::PointerUp { x, y, button } => {
                        *pressed_button = None;
                        pointer(window, cx, x, y, mouse_button(button), false);
                    }
                    PreviewInput::PointerMove { x, y } => {
                        window.dispatch_event(
                            PlatformInput::MouseMove(MouseMoveEvent {
                                position: point(px(x), px(y)),
                                pressed_button: *pressed_button,
                                ..Default::default()
                            }),
                            cx,
                        );
                    }
                    PreviewInput::Scroll {
                        x,
                        y,
                        delta_x,
                        delta_y,
                    } => {
                        window.dispatch_event(
                            PlatformInput::ScrollWheel(ScrollWheelEvent {
                                position: point(px(x), px(y)),
                                delta: ScrollDelta::Pixels(point(px(delta_x), px(delta_y))),
                                ..Default::default()
                            }),
                            cx,
                        );
                    }
                    PreviewInput::Text { text } => {
                        // One committed string reaches the focused EntityInputHandler.
                        // Empty key avoids interpreting committed text as a shortcut.
                        window.dispatch_keystroke(
                            Keystroke {
                                key: String::new(),
                                key_char: Some(text),
                                ..Default::default()
                            },
                            cx,
                        );
                    }
                    PreviewInput::Key { key } => {
                        window.dispatch_keystroke(Keystroke::parse(&key)?, cx);
                    }
                    PreviewInput::KeyUp { key } => {
                        window.dispatch_event(
                            PlatformInput::KeyUp(KeyUpEvent {
                                keystroke: Keystroke::parse(&key)?,
                            }),
                            cx,
                        );
                    }
                    PreviewInput::Focus { focused } => {
                        if focused {
                            if let Some(focus) = saved_focus.take() {
                                window.focus(&focus, cx);
                            }
                        } else {
                            *pressed_button = None;
                            if saved_focus.is_none() {
                                *saved_focus = window.focused(cx);
                            }
                            window.blur(cx);
                        }
                    }
                    PreviewInput::ShowWindow => {
                        bail!("A windowless preview has no desktop window to show")
                    }
                    PreviewInput::Resize { .. } => unreachable!(),
                    PreviewInput::Refresh => {}
                }
                Ok(())
            })??;
        Ok(())
    }
}

fn pointer(window: &mut Window, cx: &mut App, x: f32, y: f32, button: MouseButton, down: bool) {
    let position = point(px(x), px(y));
    window.dispatch_event(
        PlatformInput::MouseMove(MouseMoveEvent {
            position,
            ..Default::default()
        }),
        cx,
    );
    let event = if down {
        PlatformInput::MouseDown(MouseDownEvent {
            position,
            button,
            click_count: 1,
            ..Default::default()
        })
    } else {
        PlatformInput::MouseUp(MouseUpEvent {
            position,
            button,
            click_count: 1,
            ..Default::default()
        })
    };
    window.dispatch_event(event, cx);
}

fn mouse_button(button: PreviewMouseButton) -> MouseButton {
    match button {
        PreviewMouseButton::Left => MouseButton::Left,
        PreviewMouseButton::Right => MouseButton::Right,
        PreviewMouseButton::Middle => MouseButton::Middle,
    }
}

fn validate_size(width: u32, height: u32) -> Result<()> {
    if width == 0 || height == 0 || width > MAX_PREVIEW_DIMENSION || height > MAX_PREVIEW_DIMENSION
    {
        bail!("Preview size must be between 1 and {MAX_PREVIEW_DIMENSION} logical pixels per side");
    }
    Ok(())
}

/// Runs until RiWork closes the preview or disconnects. Frames are capped at
/// 20fps, unchanged frames are suppressed, and the send queue holds one frame.
pub fn run<V: Render + 'static>(
    build_root: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> Result<()> {
    run_with_assets(Arc::new(()), build_root)
}

pub fn run_with_assets<V: Render + 'static>(
    assets: Arc<dyn AssetSource>,
    build_root: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> Result<()> {
    let socket = std::env::var_os("RIWORK_PREVIEW_SOCKET").context(
        "RIWORK_PREVIEW_SOCKET is missing; launch this command from a RiWork App Preview tab",
    )?;
    let token = std::env::var("RIWORK_PREVIEW_TOKEN").context("RIWORK_PREVIEW_TOKEN is missing")?;
    let mut stream = UnixStream::connect(socket).context("Connecting to RiWork preview")?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    write_message(
        &mut stream,
        &PreviewClientMessage::Hello {
            token,
            pid: std::process::id(),
            protocol: PREVIEW_PROTOCOL_VERSION,
        },
    )?;
    let mut reader = BufReader::new(stream.try_clone()?);
    match read_message(&mut reader)? {
        Some(PreviewHostMessage::Ready {
            protocol: PREVIEW_PROTOCOL_VERSION,
        }) => {}
        _ => bail!("RiWork did not acknowledge preview protocol {PREVIEW_PROTOCOL_VERSION}"),
    }
    stream.set_read_timeout(None)?;

    let mut preview = HeadlessPreview::with_assets(960, 600, assets, build_root)?;

    let (incoming_tx, incoming_rx) = mpsc::sync_channel(128);
    std::thread::spawn(move || {
        loop {
            let message = read_message(&mut reader);
            let done = !matches!(message, Ok(Some(_)));
            if incoming_tx.send(message).is_err() || done {
                break;
            }
        }
    });
    let (frames_tx, frames_rx) = mpsc::sync_channel(1);
    let (writer_error_tx, writer_error_rx) = mpsc::channel();
    let mut writer = stream.try_clone()?;
    std::thread::spawn(move || {
        for message in frames_rx {
            if let Err(error) = write_message(&mut writer, &message) {
                let _ = writer_error_tx.send(error.to_string());
                break;
            }
        }
    });

    let mut previous_frame = Vec::new();
    let mut previous_tick = Instant::now();
    let mut next_frame = Instant::now();
    let mut visible = true;
    let result = (|| -> Result<()> {
        loop {
            if let Ok(error) = writer_error_rx.try_recv() {
                bail!("Cannot stream preview: {error}");
            }
            while let Ok(message) = incoming_rx.try_recv() {
                match message? {
                    Some(PreviewHostMessage::Input { input }) => {
                        if matches!(input, PreviewInput::Refresh) {
                            previous_frame.clear();
                        }
                        preview.dispatch(input)?;
                    }
                    Some(PreviewHostMessage::Visibility { visible: value }) => {
                        visible = value;
                        previous_frame.clear();
                    }
                    Some(PreviewHostMessage::Shutdown) | None => return Ok(()),
                    Some(PreviewHostMessage::Ready { .. }) => {}
                }
            }
            let now = Instant::now();
            preview.advance(now.duration_since(previous_tick));
            previous_tick = now;
            if visible && now >= next_frame {
                let image = preview.frame()?;
                if image.as_raw() != &previous_frame {
                    let mut png = Vec::new();
                    image::codecs::png::PngEncoder::new(&mut png).write_image(
                        image.as_raw(),
                        image.width(),
                        image.height(),
                        image::ExtendedColorType::Rgba8,
                    )?;
                    let message = PreviewClientMessage::Frame {
                        png: STANDARD.encode(png),
                        width: image.width(),
                        height: image.height(),
                    };
                    match frames_tx.try_send(message) {
                        Ok(()) => previous_frame = image.into_raw(),
                        Err(mpsc::TrySendError::Full(_)) => {}
                        Err(mpsc::TrySendError::Disconnected(_)) => return Ok(()),
                    }
                }
                next_frame = now + FRAME_INTERVAL;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    let _ = stream.shutdown(std::net::Shutdown::Both);
    result
}

fn write_message(stream: &mut impl Write, message: &PreviewClientMessage) -> Result<()> {
    let mut line = serde_json::to_vec(message)?;
    if line.len() + 1 > MAX_PREVIEW_MESSAGE_BYTES {
        bail!("Preview message exceeds size limit");
    }
    line.push(b'\n');
    stream.write_all(&line)?;
    Ok(())
}

fn read_message(reader: &mut impl BufRead) -> Result<Option<PreviewHostMessage>> {
    let mut line = Vec::new();
    let count = reader
        .take((MAX_PREVIEW_INPUT_MESSAGE_BYTES + 1) as u64)
        .read_until(b'\n', &mut line)?;
    if count == 0 {
        return Ok(None);
    }
    if line.len() > MAX_PREVIEW_INPUT_MESSAGE_BYTES {
        bail!("Preview input message exceeds size limit");
    }
    if !line.ends_with(b"\n") {
        bail!("Truncated preview message");
    }
    Ok(Some(serde_json::from_slice(&line)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_handles_split_reads_eof_and_truncation() {
        let mut reader = BufReader::with_capacity(
            2,
            &b"{\"type\":\"visibility\",\"visible\":false}\n{\"type\":\"shutdown\"}\n"[..],
        );
        assert!(matches!(
            read_message(&mut reader).unwrap(),
            Some(PreviewHostMessage::Visibility { visible: false })
        ));
        assert!(matches!(
            read_message(&mut reader).unwrap(),
            Some(PreviewHostMessage::Shutdown)
        ));
        assert!(read_message(&mut reader).unwrap().is_none());
        assert!(read_message(&mut &b"{\"type\":\"shutdown\"}"[..]).is_err());
    }

    #[test]
    fn decoder_and_size_validation_bound_allocations() {
        assert!(
            read_message(&mut vec![b'x'; MAX_PREVIEW_INPUT_MESSAGE_BYTES + 2].as_slice()).is_err()
        );
        assert!(validate_size(0, 600).is_err());
        assert!(validate_size(2049, 1).is_err());
        assert!(validate_size(2048, 2048).is_ok());
    }
}
