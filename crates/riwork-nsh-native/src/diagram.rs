//! Offline Mermaid rendering in the bundled, network-denied WebKit worker (`riwork-diagram`,
//! macos/diagram). One disposable process per diagram, at most two at a time, each within
//! 15 s, its source at most 32 KiB and its PNG at most 2048 px on a side.
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};

const MAX_SOURCE: usize = 32 * 1024;
const MAX_RESPONSE: usize = 6 * 1024 * 1024 + 4096;
const TIMEOUT: Duration = Duration::from_secs(15);
const BUNDLE: &str = include_str!("../../../macos/diagram/mermaid-11.17.2.min.js");
const RENDERER: &str = include_str!("../../../macos/diagram/render.js");
static ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// A rendered diagram: a PNG data URL and its size in pixels.
pub struct Png {
    pub data_url: String,
    pub width: u32,
    pub height: u32,
}

fn helper() -> Option<PathBuf> {
    let beside = std::env::current_exe().ok()?.parent()?.to_path_buf();
    [Some(beside.join("riwork-diagram")), beside.parent().map(|parent| parent.join("riwork-diagram"))]
        .into_iter()
        .flatten()
        .find(|path| path.is_file())
}

struct Permit;
impl Drop for Permit {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::AcqRel);
    }
}

fn permit(started: Instant) -> Result<Permit, String> {
    loop {
        if started.elapsed() >= TIMEOUT {
            return Err("The diagram took too long to draw.".into());
        }
        if ACTIVE.try_update(Ordering::AcqRel, Ordering::Acquire, |active| (active < 2).then_some(active + 1)).is_ok() {
            return Ok(Permit);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The worker's WebKit rules directory: private, and gone afterwards.
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Result<Self, String> {
        use std::os::unix::fs::DirBuilderExt;
        let path = std::env::temp_dir().join(format!("riwork-diagram-rules-{}", uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new().mode(0o700).create(&path).map_err(|_| "Could not prepare the diagram worker.")?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Draws Mermaid `source`. Blocks for up to 15 s: call it from the blocking pool.
pub fn render(source: &str, dark: bool) -> Result<Png, String> {
    if source.len() > MAX_SOURCE {
        return Err("This diagram is larger than the 32 KiB that is drawn.".into());
    }
    let path = helper().ok_or("This build has no diagram worker (riwork-diagram).")?;
    render_with(&path, source, dark, TIMEOUT)
}

fn render_with(path: &Path, source: &str, dark: bool, timeout: Duration) -> Result<Png, String> {
    use std::os::unix::process::CommandExt;
    let started = Instant::now();
    let _permit = permit(started)?;
    let scratch = Scratch::new()?;
    let request = serde_json::to_vec(&serde_json::json!({
        "source": source, "dark": dark, "bundle": BUNDLE, "renderer": RENDERER, "rules": scratch.0,
    }))
    .map_err(|error| error.to_string())?;
    let mut child = Command::new(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|_| "Could not start the diagram worker.")?;
    let mut input = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || input.write_all(&request));
    let output = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        output.take((MAX_RESPONSE + 1) as u64).read_to_end(&mut bytes).map(|_| bytes)
    });
    let status = loop {
        if started.elapsed() >= timeout {
            unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
            let _ = child.wait();
            let _ = writer.join();
            let _ = reader.join();
            return Err("The diagram took too long to draw.".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("The diagram worker stopped unexpectedly.".into());
            }
        }
    };
    let _ = writer.join();
    let bytes = reader.join().map_err(|_| "Could not read the diagram.")?.map_err(|_| "Could not read the diagram.")?;
    if !status.success() || bytes.len() > MAX_RESPONSE {
        return Err("The diagram worker stopped unexpectedly or answered too much.".into());
    }
    decode(&bytes)
}

fn decode(bytes: &[u8]) -> Result<Png, String> {
    #[derive(serde::Deserialize)]
    struct Response {
        png: Option<String>,
        error: Option<String>,
    }
    let response: Response = serde_json::from_slice(bytes).map_err(|_| "The diagram worker answered something unreadable.")?;
    if let Some(error) = response.error {
        return Err(error.chars().filter(|c| !c.is_control() || *c == '\n').take(1000).collect());
    }
    let png = response.png.ok_or("The diagram worker returned no image.")?;
    let raw = STANDARD.decode(&png).map_err(|_| "The diagram worker returned an unreadable image.")?;
    let (width, height) = image::ImageReader::with_format(std::io::Cursor::new(&raw), image::ImageFormat::Png)
        .into_dimensions()
        .map_err(|_| "The diagram worker returned an unreadable image.")?;
    if width == 0 || height == 0 || width > 2048 || height > 2048 {
        return Err("The diagram is larger than the image limit.".into());
    }
    Ok(Png { data_url: format!("data:image/png;base64,{png}"), width, height })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_decoded_and_bounded() {
        let mut raw = Vec::new();
        image::DynamicImage::new_rgba8(3, 2).write_to(&mut std::io::Cursor::new(&mut raw), image::ImageFormat::Png).unwrap();
        let ok = decode(serde_json::json!({"png": STANDARD.encode(&raw)}).to_string().as_bytes()).unwrap();
        assert_eq!((ok.width, ok.height), (3, 2));
        assert!(ok.data_url.starts_with("data:image/png;base64,"));
        let error = decode(br#"{"error":"Parse error on line 2\u0007"}"#).err().unwrap();
        assert_eq!(error, "Parse error on line 2");
        assert!(decode(b"not json").is_err());
        assert!(render(&"x".repeat(MAX_SOURCE + 1), false).err().unwrap().contains("32 KiB"));
    }

    #[test]
    fn a_hung_worker_is_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("hang");
        std::fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let started = Instant::now();
        let error = render_with(&script, "stateDiagram-v2\n[*] --> a", false, Duration::from_millis(300)).err().unwrap();
        assert!(error.contains("too long"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
