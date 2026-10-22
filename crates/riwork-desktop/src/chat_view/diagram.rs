//! Offline upstream Mermaid rendering in a disposable, network-denied WebKit worker.
//! Source text stays authoritative; the bitmap is only a bounded presentation cache.
use super::{
    ChatView,
    widgets::{Look, button, copy_button},
};
use crate::{
    file_preview::{self, PreviewContent},
    ui_text,
};
use base64::Engine as _;
use gpui::{AnyElement, Context, RenderImage, SharedString, Task, div, img, prelude::*, px, rgb};
use std::{
    collections::VecDeque,
    hash::{Hash, Hasher},
    io::{Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_SOURCE: usize = 32 * 1024;
const MAX_RESPONSE: usize = 6 * 1024 * 1024 + 4096;
const MAX_IMAGES: usize = 4;
const MAX_ENTRIES: usize = 64;
const TIMEOUT: Duration = Duration::from_secs(15);
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
const BUNDLE: &str = include_str!("../../../../macos/diagram/mermaid-11.17.2.min.js");
const RENDERER: &str = include_str!("../../../../macos/diagram/render.js");

pub(super) fn source_key(key: &str) -> String {
    format!("diagram-source:{key}")
}

#[derive(Clone)]
struct Rendered {
    image: Arc<RenderImage>,
    png: Arc<str>,
}

struct Pending {
    cancelled: Arc<AtomicBool>,
    _task: Task<()>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}
enum State {
    Pending(Pending),
    Ready(Rendered),
    Error(String),
    Evicted,
}
struct Entry {
    key: String,
    source: String,
    dark: bool,
    revision: u64,
    state: State,
}
#[derive(Default)]
pub(super) struct Diagrams {
    entries: VecDeque<Entry>,
}

impl Diagrams {
    fn remove(&mut self, key: &str) -> Option<Entry> {
        self.entries
            .iter()
            .position(|entry| entry.key == key)
            .and_then(|at| self.entries.remove(at))
    }
    fn release(entry: Entry, cx: &mut Context<ChatView>) {
        if let State::Ready(rendered) = entry.state {
            cx.defer(move |cx| cx.drop_image(rendered.image, None));
        }
    }
    fn complete(
        &mut self,
        key: &str,
        source: &str,
        dark: bool,
        result: Result<Rendered, String>,
        cx: &mut Context<ChatView>,
    ) {
        let Some(entry) = self.entries.iter_mut().find(|e| {
            e.key == key
                && e.source == source
                && e.dark == dark
                && matches!(e.state, State::Pending(_))
        }) else {
            if let Ok(rendered) = result {
                cx.defer(move |cx| cx.drop_image(rendered.image, None));
            }
            return;
        };
        entry.state = result.map_or_else(State::Error, State::Ready);
        while self
            .entries
            .iter()
            .filter(|e| matches!(e.state, State::Ready(_)))
            .count()
            > MAX_IMAGES
        {
            let at = self
                .entries
                .iter()
                .position(|e| e.key != key && matches!(e.state, State::Ready(_)))
                .unwrap();
            if let State::Ready(rendered) =
                std::mem::replace(&mut self.entries[at].state, State::Evicted)
            {
                cx.defer(move |cx| cx.drop_image(rendered.image, None));
            }
        }
    }
}

fn helper() -> Option<PathBuf> {
    let beside = std::env::current_exe().ok()?.parent()?.to_path_buf();
    [
        Some(beside.join("riwork-diagram")),
        beside.parent().map(|p| p.join("riwork-diagram")),
        option_env!("RIWORK_DIAGRAM_HELPER_BUILT").map(PathBuf::from),
    ]
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

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Result<Self, String> {
        let path =
            std::env::temp_dir().join(format!("riwork-diagram-rules-{}", uuid::Uuid::new_v4()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&path)
            .map_err(|_| "Could not create the isolated diagram workspace.")?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn permit(cancelled: &AtomicBool, started: Instant) -> Result<Permit, String> {
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err("Diagram rendering was cancelled.".into());
        }
        if started.elapsed() >= TIMEOUT {
            return Err("Diagram rendering took too long. Source is still available.".into());
        }
        if ACTIVE
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < 2).then_some(active + 1)
            })
            .is_ok()
        {
            return Ok(Permit);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn render(source: &str, dark: bool, cancelled: &AtomicBool) -> Result<Rendered, String> {
    if source.len() > MAX_SOURCE {
        return Err(
            "This diagram exceeds the 32 KiB rendering limit. Its source is available below."
                .into(),
        );
    }
    let path = helper().ok_or(
        "The offline diagram helper is unavailable in this build. Its source is available below.",
    )?;
    render_with(&path, source, dark, cancelled, TIMEOUT)
}

fn render_with(
    path: &std::path::Path,
    source: &str,
    dark: bool,
    cancelled: &AtomicBool,
    timeout: Duration,
) -> Result<Rendered, String> {
    let started = Instant::now();
    let _permit = permit(cancelled, started)?;
    let scratch = Scratch::new()?;
    let request = serde_json::to_vec(
        &serde_json::json!({"source":source,"dark":dark,"bundle":BUNDLE,"renderer":RENDERER,"rules":scratch.0}),
    )
    .map_err(|e| e.to_string())?;
    let mut command = Command::new(path);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|_| "Could not start the offline diagram renderer.")?;
    let mut input = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || input.write_all(&request));
    let output = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        output
            .take((MAX_RESPONSE + 1) as u64)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let status = loop {
        if cancelled.load(Ordering::Acquire) || started.elapsed() >= timeout {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            let _ = writer.join();
            let _ = reader.join();
            return Err(if cancelled.load(Ordering::Acquire) {
                "Diagram rendering was cancelled."
            } else {
                "Diagram rendering took too long. Source is still available."
            }
            .into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("The diagram renderer stopped unexpectedly.".into());
            }
        }
    };
    let _ = writer.join();
    let bytes = reader
        .join()
        .map_err(|_| "Could not read the diagram.")?
        .map_err(|_| "Could not read the diagram.")?;
    if !status.success() || bytes.len() > MAX_RESPONSE {
        return Err(
            "The diagram renderer exceeded its output limit or stopped unexpectedly.".into(),
        );
    }
    decode(&bytes)
}

fn decode(bytes: &[u8]) -> Result<Rendered, String> {
    #[derive(serde::Deserialize)]
    struct Response {
        png: Option<String>,
        error: Option<String>,
    }
    let response: Response = serde_json::from_slice(bytes)
        .map_err(|_| "The diagram renderer returned an invalid response.")?;
    if let Some(error) = response.error {
        return Err(error
            .chars()
            .filter(|c| !c.is_control() || *c == '\n')
            .take(1000)
            .collect());
    }
    let png = response
        .png
        .ok_or("The diagram renderer returned no image.")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&png)
        .map_err(|_| "The diagram renderer returned an invalid image.")?;
    // Inspect dimensions before decoding pixel data, including responses from a broken helper.
    let reader =
        image::ImageReader::with_format(std::io::Cursor::new(&bytes), image::ImageFormat::Png);
    let (width, height) = reader
        .into_dimensions()
        .map_err(|_| "The diagram renderer returned an invalid PNG.")?;
    if width == 0 || height == 0 || width > 2048 || height > 2048 {
        return Err("The diagram exceeds the image size limit.".into());
    }
    match file_preview::decode_image(&bytes)? {
        PreviewContent::Image { image, .. } => Ok(Rendered {
            image,
            png: Arc::from(png),
        }),
        _ => Err("The diagram renderer returned no image.".into()),
    }
}

impl ChatView {
    pub(super) fn release_diagrams(&mut self, cx: &mut gpui::App) {
        for entry in self.diagrams.get_mut().entries.drain(..) {
            if let State::Ready(rendered) = entry.state {
                cx.defer(move |cx| cx.drop_image(rendered.image, None));
            }
        }
    }
    fn diagram_completed(&self, key: &str) -> bool {
        self.completed_code(key)
    }
    fn diagram_revision(&self, key: &str) -> Option<u64> {
        use crate::chat::model::ItemBody;
        let item = self
            .model
            .transcript
            .items
            .iter()
            .filter_map(|item| {
                [
                    format!("{}/", item.id),
                    format!("user:{}/", item.id),
                    format!("plan:{}:document/", item.id),
                ]
                .into_iter()
                .filter(|prefix| key.starts_with(prefix))
                .map(|prefix| prefix.len())
                .max()
                .map(|length| (length, item))
            })
            .max_by_key(|(length, _)| *length)?
            .1;
        let source = match &item.body {
            ItemBody::AgentMessage { text } | ItemBody::UserMessage { text, .. } => text.as_str(),
            ItemBody::Plan { explanation, .. } => explanation.as_deref()?,
            _ => return None,
        };
        let mut state = std::hash::DefaultHasher::new();
        source.hash(&mut state);
        Some(state.finish())
    }
    pub(super) fn diagram_hides_source(&self, leaf_key: &str, text: &str, look: Look) -> bool {
        let Some(key) = leaf_key.strip_prefix("code:") else {
            return false;
        };
        let diagrams = self.diagrams.borrow();
        let Some(entry) = diagrams
            .entries
            .iter()
            .find(|entry| entry.key == key && matches!(entry.state, State::Ready(_)))
        else {
            return false;
        };
        if entry.dark != crate::theme::is_dark(look.colors.bg)
            || self.open.contains(&source_key(key))
        {
            return false;
        }
        let same_text = if entry.source.contains('\t') {
            entry.source.replace('\t', "    ") == text
        } else {
            entry.source == text
        };
        // Ordinary code has no diagram entry. Only the at-most-four cached
        // bitmaps need a current owner/revision check during source projection.
        same_text
            && self.diagram_completed(key)
            && self.diagram_revision(key) == Some(entry.revision)
    }
    fn choose_diagram_source(&mut self, key: &str, source: bool, cx: &mut Context<Self>) {
        let offset = self.list.logical_scroll_top();
        self.list.pause_following_tail();
        if source {
            self.open.insert(source_key(key));
        } else {
            self.open.remove(&source_key(key));
        }
        let mut diagrams = self.diagrams.borrow_mut();
        if diagrams.entries.iter().any(|e| {
            e.key == key
                && (matches!(e.state, State::Pending(_))
                    || !source && matches!(e.state, State::Error(_) | State::Evicted))
        }) {
            if let Some(entry) = diagrams.remove(key) {
                Diagrams::release(entry, cx);
            }
        }
        drop(diagrams);
        self.transcript_selection.changed();
        self.list.remeasure();
        self.list.scroll_to(offset);
        cx.notify();
    }
    pub(super) fn diagram_block(
        &self,
        source: &str,
        key: &str,
        fallback: AnyElement,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let dark = crate::theme::is_dark(look.colors.bg);
        let complete = self.diagram_completed(key);
        let revision = self.diagram_revision(key).unwrap_or_default();
        let source_mode = self.open.contains(&source_key(key));
        let mut diagrams = self.diagrams.borrow_mut();
        if diagrams.entries.iter().any(|e| {
            e.key == key
                && (e.source != source
                    || e.revision != revision
                    || e.dark != dark
                    || !complete
                    || source_mode && matches!(e.state, State::Pending(_)))
        }) {
            if let Some(entry) = diagrams.remove(key) {
                Diagrams::release(entry, cx);
                self.transcript_selection.changed();
            }
        }
        if complete
            && source.len() <= MAX_SOURCE
            && !source_mode
            && !diagrams.entries.iter().any(|e| e.key == key)
            && diagrams
                .entries
                .iter()
                .filter(|e| matches!(e.state, State::Pending(_)))
                .count()
                < 2
        {
            while diagrams.entries.len() >= MAX_ENTRIES {
                if let Some(entry) = diagrams.entries.pop_front() {
                    Diagrams::release(entry, cx);
                    self.transcript_selection.changed();
                }
            }
            let cancelled = Arc::new(AtomicBool::new(false));
            let flag = cancelled.clone();
            let owned = source.to_owned();
            let rendered_key = key.to_owned();
            let work_source = owned.clone();
            let work = cx
                .background_executor()
                .spawn(async move { render(&work_source, dark, &flag) });
            let task = cx.spawn(async move |this, cx| {
                let result = work.await;
                let _ = this.update(cx, |view, cx| {
                    view.diagrams
                        .borrow_mut()
                        .complete(&rendered_key, &owned, dark, result, cx);
                    view.transcript_selection.changed();
                    view.list.remeasure();
                    cx.notify();
                });
            });
            diagrams.entries.push_back(Entry {
                key: key.into(),
                source: source.into(),
                dark,
                revision,
                state: State::Pending(Pending {
                    cancelled,
                    _task: task,
                }),
            });
        }
        let (rendered, mut error, evicted) = match diagrams
            .entries
            .iter()
            .find(|e| e.key == key)
            .map(|e| &e.state)
        {
            Some(State::Ready(rendered)) => (Some(rendered.clone()), None, false),
            Some(State::Error(error)) => (None, Some(error.clone()), false),
            Some(State::Evicted) => (None, None, true),
            _ => (None, None, false),
        };
        if source.len() > MAX_SOURCE {
            error = Some(
                "This diagram exceeds the 32 KiB rendering limit. Its source is available below."
                    .into(),
            );
        }
        drop(diagrams);
        let show_image = complete && !source_mode && rendered.is_some();
        let diagram_key = key.to_owned();
        let code_key = key.to_owned();
        let owned = source.to_owned();
        let copy_id = format!("diagram-copy:{key}");
        let copied = self.copied.as_deref() == Some(copy_id.as_str());
        let mut card = div()
            .id(SharedString::from(format!("diagram:{key}")))
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(ui_text::space(6.))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(ui_text::space(6.))
                    .child(
                        button(format!("diagram-show:{key}"), "Diagram", None, look)
                            .disabled(!complete || show_image)
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.choose_diagram_source(&diagram_key, false, cx)
                            })),
                    )
                    .child(
                        button(source_key(key), "Source", None, look)
                            .disabled(source_mode || !complete)
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.choose_diagram_source(&code_key, true, cx)
                            })),
                    )
                    .child(
                        copy_button(
                            SharedString::from(copy_id.clone()),
                            copied,
                            "Copy source",
                            "Copy Mermaid source",
                            look,
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.copy(copy_id.clone(), owned.clone(), cx)
                        })),
                    ),
            );
        if show_image {
            let rendered = rendered.unwrap();
            let enlarge_key = format!("{key}/image/mermaid");
            let image = rendered.image.clone();
            card = card
                .child(
                    button(
                        format!("diagram-enlarge:{key}"),
                        "Enlarge diagram",
                        None,
                        look,
                    )
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.show_generated_diagram(
                            enlarge_key.clone(),
                            rendered.image.clone(),
                            rendered.png.clone(),
                            cx,
                        )
                    })),
                )
                .child(
                    img(image)
                        .w_full()
                        .max_h(px(360.))
                        .object_fit(gpui::ObjectFit::ScaleDown),
                );
        } else {
            let message = if !complete {
                Some("Diagram available when this response finishes.".to_owned())
            } else if source_mode {
                None
            } else if let Some(error) = error {
                Some(format!("Could not render diagram: {error}"))
            } else if evicted {
                Some("Preview released to save memory. Choose Diagram to render again.".to_owned())
            } else {
                Some("Rendering diagram… Source remains available below.".to_owned())
            };
            if let Some(message) = message {
                card = card.child(
                    div()
                        .text_size(ui_text::text(11.))
                        .text_color(rgb(look.colors.muted))
                        .child(message),
                );
            }
            card = card.child(fallback);
        }
        card.into_any_element()
    }
}

#[cfg(test)]
mod tests;
