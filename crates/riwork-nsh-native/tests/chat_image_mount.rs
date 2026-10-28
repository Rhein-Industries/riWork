//! Actual ChatTab bundle -> QuickJS -> native ChatHandle -> persisted fixture
//! -> NSH Image mount bytes. No platform backend, native window or GUI input.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use nsh_core::{ApproxTextEngine, MountBatch, MountItem, NodeId, Points, Size};
use nsh_engine::{Bundle, Engine, EngineConfig, EngineControl, MountSink};
use riwork_engine::chat::{
    client::socket_path,
    host::Host,
    log::{ChatLog, chat_dir},
    model::{ChatEvent, ChatInfo, Item, ItemBody, ItemStatus},
    testing::{fake_providers, quick_options},
    wire::Envelope,
};
use riwork_nsh_native::chat::{ChatBackend, service::install_backend};
use serde_json::json;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, OnceLock, mpsc},
    time::{Duration, Instant},
};

struct Backend(PathBuf);
impl ChatBackend for Backend {
    fn home(&self) -> Result<PathBuf, String> {
        Ok(self.0.clone())
    }
    fn ensure(&self) -> Result<PathBuf, String> {
        Ok(socket_path(&self.0))
    }
}
struct Sink {
    control: Arc<OnceLock<EngineControl>>,
    batches: mpsc::Sender<Result<MountBatch, String>>,
}
impl MountSink for Sink {
    fn submit(&self, batch: MountBatch) {
        let _ = self.batches.send(Ok(batch));
    }
    fn report_error(&self, error: &str) {
        let _ = self.batches.send(Err(error.into()));
    }
    fn request_frame(&self) {
        self.control.get().unwrap().frame();
    }
}

#[test]
#[ignore = "requires private HOME/RIWORK_HOME and RIWORK_CHAT_IMAGE_BUNDLE; no GUI"]
fn persisted_screenshot_pixels_reach_the_actual_chat_image_mount() {
    let path = PathBuf::from(std::env::var_os("RIWORK_CHAT_IMAGE_BUNDLE").expect("fixture bundle"));
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    let info: ChatInfo =
        serde_json::from_value(json!({"id":"11111111-2222-4333-8444-555555555555",
        "provider":"codex", "cwd":home, "title":"Synthetic screenshot", "created_at_unix":1}))
        .unwrap();
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(12, 8, image::Rgba([200, 40, 10, 255]))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let item = Item {
        id: "cua-shot".into(),
        turn_id: Some("turn-1".into()),
        status: ItemStatus::Completed,
        presentation: riwork_engine::chat::media::presentation(&json!({"type":"mcpToolCall",
        "result":{"content":[
            {"type":"image", "mimeType":"image/png", "data":STANDARD.encode(png.into_inner())},
            {"type":"image", "mimeType":"image/png", "data":"not-base64"}
        ]}})),
        started_at: None,
        ended_at: None,
        body: ItemBody::ToolCall {
            server: Some("cua-driver".into()),
            tool: "get_window_state".into(),
            input: json!({}),
            output: None,
        },
    };
    let log = ChatLog::create(&chat_dir(home, &info.id).unwrap(), &info).unwrap();
    for (index, event) in [
        ChatEvent::Info { info: info.clone() },
        ChatEvent::ItemCompleted { item },
    ]
    .into_iter()
    .enumerate()
    {
        let envelope = Envelope {
            chat_id: info.id.clone(),
            seq: index as u64 + 1,
            event,
        };
        let mut line = serde_json::to_vec(&envelope).unwrap();
        line.push(b'\n');
        log.append(&line, true).unwrap();
    }
    drop(log);
    let _host = Host::start(home, fake_providers(), quick_options()).unwrap();
    install_backend(Arc::new(Backend(home.to_owned()))).unwrap();
    let mut config = EngineConfig::new(Bundle::read(&path).unwrap());
    config.viewport = Size::new(Points(900.0), Points(1800.0));
    config.ui_sets = vec![nsh_base::registry(), riwork_nsh_native::registry()];
    let control = Arc::new(OnceLock::new());
    let setup = control.clone();
    let (send, receive) = mpsc::channel();
    let _engine = Engine::start_with(
        config,
        || ApproxTextEngine,
        Sink {
            control,
            batches: send,
        },
        move |value| {
            assert!(setup.set(value.clone()).is_ok());
        },
    )
    .unwrap();
    let mut names = HashMap::<NodeId, String>::new();
    let mut images = HashMap::<NodeId, nsh_core::ImageSource>::new();
    let mut frames = HashMap::new();
    let mut fallback = false;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let batch = receive
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("image mount before deadline")
            .expect("no JS error");
        for item in batch.items {
            match item {
                MountItem::ViewProps { id, props } => {
                    if let Some(name) = props.test_id {
                        names.insert(id, name.to_string());
                    }
                }
                MountItem::Image {
                    id,
                    source: Some(source),
                    resize_mode,
                    ..
                } => {
                    assert_eq!(resize_mode, nsh_core::ResizeMode::Contain);
                    images.insert(id, source);
                }
                MountItem::Frame { id, frame } => {
                    frames.insert(id, frame);
                }
                MountItem::Text { text, .. } => {
                    fallback |= text.text.contains("Invalid image encoding");
                }
                MountItem::Delete { id } => {
                    names.remove(&id);
                    images.remove(&id);
                    frames.remove(&id);
                }
                _ => {}
            }
        }
        if let Some((&id, _)) = names
            .iter()
            .find(|(_, name)| name.as_str() == "chat-image:cua-shot:0")
        {
            if let (Some(source), Some(frame)) = (images.get(&id), frames.get(&id)) {
                if !fallback {
                    continue;
                }
                assert!(frame.size.width.0 > 0.0 && frame.size.height.0 > 0.0);
                let bytes = source
                    .data
                    .as_ref()
                    .expect("Image mount contains real image bytes")
                    .bytes();
                assert_eq!(image::guess_format(bytes).unwrap(), image::ImageFormat::Png);
                let pixels = image::load_from_memory(bytes).unwrap().into_rgba8();
                assert_eq!(pixels.dimensions(), (12, 8));
                assert!(pixels.pixels().all(|pixel| pixel.0 == [200, 40, 10, 255]));
                assert!(!names.values().any(|name| name == "chat-image:cua-shot:1"));
                eprintln!(
                    "Verified {} PNG bytes in ChatTab's visible Image mount, 12x8 pixels [200,40,10,255]; malformed-image fallback visible",
                    bytes.len()
                );
                break;
            }
        }
    }
}
