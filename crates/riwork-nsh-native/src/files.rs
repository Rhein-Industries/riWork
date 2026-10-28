//! Descriptor-relative, no-follow Files bridge. Watches are shared by root and
//! exist only while a rendering owner holds a subscription; no view polls disk.
use std::{
    cmp::Ordering,
    collections::BTreeMap,
    ffi::{CStr, OsString},
    fs::File,
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStringExt, fs::PermissionsExt, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex, OnceLock, PoisonError, Weak, mpsc},
    thread,
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use notify::{RecursiveMode, Watcher};
use nsh_core::prelude::*;
use riwork_engine::file_preview::{self, FileIdentity, PreviewContent};
use serde::{Deserialize, Serialize};

use crate::{
    hub::{Hub, Snapshot, TopicSubscription},
    service::blocking,
};

const DISPLAY_LIMIT: usize = 5_000;
const SCAN_LIMIT: usize = 50_000;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct FileEntryDto {
    pub name: String,
    pub path: String,
    pub relative_path: String,
    pub kind: String,
    #[prop(skip_none)]
    pub identity: Option<String>,
    #[prop(skip_none)]
    pub git_status: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryListingDto {
    pub schema: u32,
    pub root: String,
    pub path: String,
    pub entries: Vec<FileEntryDto>,
    pub omitted: usize,
    pub scan_limited: bool,
    #[prop(skip_none)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct PreviewDto {
    pub schema: u32,
    pub kind: String,
    #[prop(skip_none)]
    pub text: Option<String>,
    #[prop(skip_none)]
    pub lines: Option<Vec<String>>,
    pub truncated: bool,
    pub markdown: bool,
    #[prop(skip_none)]
    pub data_url: Option<String>,
    #[prop(skip_none)]
    pub mime: Option<String>,
    #[prop(skip_none)]
    pub width: Option<u32>,
    #[prop(skip_none)]
    pub height: Option<u32>,
    #[prop(skip_none)]
    pub page: Option<usize>,
    #[prop(skip_none)]
    pub pages: Option<usize>,
    #[prop(skip_none)]
    pub message: Option<String>,
}

impl PreviewDto {
    fn new(kind: &str) -> Self {
        Self {
            schema: 3,
            kind: kind.into(),
            text: None,
            lines: None,
            truncated: false,
            markdown: false,
            data_url: None,
            mime: None,
            width: None,
            height: None,
            page: None,
            pages: None,
            message: None,
        }
    }
    fn message(message: impl Into<String>) -> Self {
        Self {
            message: Some(message.into()),
            ..Self::new("message")
        }
    }
}

fn bridge_error(message: String) -> String {
    if [
        "refused:",
        "broken:",
        "not_found:",
        "invalid:",
        "conflict:",
        "unavailable:",
        "io:",
    ]
    .iter()
    .any(|prefix| message.starts_with(prefix))
    {
        message
    } else {
        format!("refused: {message}")
    }
}

pub(crate) fn decode_identity(identity: &str) -> Result<FileIdentity, String> {
    if identity.len() > 512 {
        return Err("invalid: file identity is malformed".into());
    }
    serde_json::from_str(identity).map_err(|_| "invalid: file identity is malformed".into())
}
fn encode_identity(identity: FileIdentity) -> String {
    serde_json::to_string(&identity).expect("file identity is serializable")
}
fn path_string(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| "invalid: file name is not UTF-8".into())
}

#[module]
pub struct Files;
#[module]
impl Files {
    /// Isolated debug/test fixtures only; release builds always refuse.
    pub async fn fixture_directory(name: String) -> Result<String, String> {
        blocking(move || fixture_directory(&name))
            .await
            .map_err(bridge_error)
    }
    pub async fn list_directory(
        root: String,
        path: String,
        show_hidden: bool,
    ) -> Result<DirectoryListingDto, String> {
        blocking(move || list_directory(Path::new(&root), Path::new(&path), show_hidden))
            .await
            .map_err(bridge_error)
    }
    pub fn subscribe(
        root: String,
        path: String,
        show_hidden: bool,
        listener: Callback<Snapshot<DirectoryListingDto>>,
        cx: CallCx<'_>,
    ) -> Result<crate::Subscription, String> {
        if !Path::new(&root).is_absolute() || !Path::new(&path).is_absolute() {
            return Err("invalid: Files paths must be absolute".into());
        }
        Ok(crate::Subscription::start(cx, async move {
            // Registration and the initial read are off the engine thread.
            let error_root = root.clone();
            let error_path = path.clone();
            let subscription =
                blocking(move || watch_directory(Path::new(&root), Path::new(&path), show_hidden))
                    .await;
            match subscription {
                Ok(subscription) => {
                    crate::deliver(subscription.topic, listener).await;
                    drop(subscription.watch);
                }
                Err(error) => listener.post(Snapshot {
                    kind: "snapshot".into(),
                    rev: 1,
                    resync: false,
                    data: DirectoryListingDto {
                        schema: 3,
                        root: error_root,
                        path: error_path,
                        entries: Vec::new(),
                        omitted: 0,
                        scan_limited: false,
                        error: Some(bridge_error(error)),
                    },
                }),
            }
        }))
    }
    pub async fn preview(
        root: String,
        path: String,
        identity: Option<String>,
        pdf_requested: Option<bool>,
        page: Option<usize>,
    ) -> Result<PreviewDto, String> {
        blocking(move || {
            preview_file(
                Path::new(&root),
                Path::new(&path),
                identity.as_deref(),
                pdf_requested.unwrap_or(false),
                page.unwrap_or(1),
            )
        })
        .await
        .map_err(bridge_error)
    }
    pub async fn read_text(root: String, path: String, identity: String) -> Result<String, String> {
        blocking(move || {
            let expected = decode_identity(&identity)?;
            if let Some(message) = file_preview::copy_block(Path::new(&path), expected) {
                return Err(message.into());
            }
            file_preview::read_text(Path::new(&root), Path::new(&path), expected)
        })
        .await
        .map_err(bridge_error)
    }

    pub async fn validate_edit(
        root: String,
        path: String,
        identity: String,
    ) -> Result<String, String> {
        blocking(move || {
            file_preview::validated_editor_path(
                Path::new(&root),
                Path::new(&path),
                decode_identity(&identity)?,
            )?;
            // Preserve the caller's trusted root spelling; Sessions resolves its
            // canonical editor identity after validating against the store root.
            Ok(path)
        })
        .await
        .map_err(bridge_error)
    }

    pub async fn validate_open(
        root: String,
        path: String,
        identity: Option<String>,
    ) -> Result<String, String> {
        blocking(move || validate_open(Path::new(&root), Path::new(&path), identity.as_deref()))
            .await
            .map_err(bridge_error)
    }
}

pub fn preview_file(
    root: &Path,
    path: &Path,
    identity: Option<&str>,
    pdf_requested: bool,
    page: usize,
) -> Result<PreviewDto, String> {
    // PDF parsers are never invoked by arrow selection or implicit refresh.
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
        && !pdf_requested
    {
        // Validate even a placeholder so escaped paths never look selectable.
        file_preview::validated_editor_path(
            root,
            path,
            identity
                .map(decode_identity)
                .transpose()?
                .ok_or("Cannot verify this file. Refresh and try again.")?,
        )?;
        return Ok(PreviewDto::message(
            "PDFs are only rendered on request, because the PDF parser runs inside RiWork.",
        ));
    }
    let expected = identity
        .map(decode_identity)
        .transpose()?
        .ok_or("Cannot verify this file. Refresh and try again.")?;
    match file_preview::load(root, path, Some(expected))? {
        PreviewContent::Text {
            text,
            lines,
            truncated,
            markdown,
        } => Ok(PreviewDto {
            text: Some(text),
            lines: Some(lines),
            truncated,
            markdown,
            ..PreviewDto::new("text")
        }),
        PreviewContent::Image {
            bytes,
            mime,
            width,
            height,
        } => Ok(PreviewDto {
            data_url: Some(format!("data:{mime};base64,{}", STANDARD.encode(bytes))),
            mime: Some(mime),
            width: Some(width),
            height: Some(height),
            ..PreviewDto::new("image")
        }),
        PreviewContent::Pdf { bytes } => {
            let rendered = file_preview::render_pdf(&bytes, page)?;
            Ok(PreviewDto {
                data_url: Some(format!(
                    "data:image/png;base64,{}",
                    STANDARD.encode(rendered.bytes)
                )),
                mime: Some("image/png".into()),
                width: Some(rendered.width),
                height: Some(rendered.height),
                page: Some(rendered.page),
                pages: Some(rendered.pages),
                ..PreviewDto::new("pdf")
            })
        }
        PreviewContent::Message(message) => Ok(PreviewDto::message(message)),
    }
}

/// Safe external-open target. Launchable bundles, executable files and every
/// symlink component are refused before the host can hand anything to macOS.
pub fn validate_open(root: &Path, path: &Path, identity: Option<&str>) -> Result<String, String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "This item is outside the selected worktree. Use Reveal instead.")?;
    let launchers = [
        "app", "command", "tool", "terminal", "workflow", "action", "scpt", "scptd", "pkg", "mpkg",
        "dmg", "jar", "prefpane", "saver", "webloc", "inetloc",
    ];
    if relative.components().any(|part| {
        Path::new(part.as_os_str())
            .extension()
            .and_then(|s| s.to_str())
            .is_some_and(|ext| launchers.contains(&ext.to_ascii_lowercase().as_str()))
    }) {
        return Err(
            "Launchable files and bundles cannot be opened here. Use Reveal instead.".into(),
        );
    }
    if let Ok((_, canonical)) = file_preview::open_directory_in(root, path) {
        return path_string(&canonical);
    }
    let (file, canonical) = file_preview::open_regular_in(root, path)
        .map_err(|error| format!("{error} Use Reveal instead."))?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if metadata.permissions().mode() & 0o111 != 0 {
        return Err("Executable files cannot be opened here. Use Reveal instead.".into());
    }
    let expected = identity
        .map(decode_identity)
        .transpose()?
        .ok_or("Cannot verify this file. Refresh and try again.")?;
    if FileIdentity::of(&metadata) != expected {
        return Err("This file changed or was replaced. Refresh Files and try again.".into());
    }
    path_string(&canonical)
}

/// All entry operations stay relative to this opened directory descriptor;
/// pathname re-resolution after validation would permit ancestor swaps.
pub fn list_directory(
    root: &Path,
    path: &Path,
    show_hidden: bool,
) -> Result<DirectoryListingDto, String> {
    let (directory, _) = file_preview::open_directory_in(root, path)?;
    let fd = unsafe { libc::dup(directory.as_raw_fd()) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let stream = unsafe { libc::fdopendir(fd) };
    if stream.is_null() {
        unsafe {
            libc::close(fd);
        }
        return Err(std::io::Error::last_os_error().to_string());
    }
    struct Directory(*mut libc::DIR);
    impl Drop for Directory {
        fn drop(&mut self) {
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let stream = Directory(stream);
    let mut entries = Vec::new();
    let mut scanned = 0;
    let mut scan_limited = false;
    let mut unrepresentable = 0;
    loop {
        let raw = unsafe { libc::readdir(stream.0) };
        if raw.is_null() {
            break;
        }
        let name = unsafe { CStr::from_ptr((*raw).d_name.as_ptr()) };
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        if scanned == SCAN_LIMIT {
            scan_limited = true;
            break;
        }
        scanned += 1;
        if !show_hidden && name.to_bytes().starts_with(b".") {
            continue;
        }
        let os_name = OsString::from_vec(name.to_bytes().to_vec());
        let Some(display_name) = os_name.to_str() else {
            unrepresentable += 1;
            continue;
        };
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe {
            libc::fstatat(
                directory.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            continue;
        }
        let mode = unsafe { stat.assume_init() }.st_mode & libc::S_IFMT;
        let kind = if mode == libc::S_IFDIR {
            "directory"
        } else if mode == libc::S_IFREG {
            "file"
        } else if mode == libc::S_IFLNK {
            "symlink"
        } else {
            "other"
        };
        let child = path.join(&os_name);
        let identity = if kind == "file" {
            let child_fd = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
            };
            if child_fd < 0 {
                None
            } else {
                let file = unsafe { File::from_raw_fd(child_fd) };
                file.metadata()
                    .ok()
                    .filter(|meta| meta.is_file())
                    .map(|meta| encode_identity(FileIdentity::of(&meta)))
            }
        } else {
            None
        };
        entries.push(FileEntryDto {
            name: display_name.into(),
            path: path_string(&child)?,
            relative_path: path_string(
                child
                    .strip_prefix(root)
                    .map_err(|_| "Folder outside worktree")?,
            )?,
            kind: kind.into(),
            identity,
            git_status: None,
        });
    }
    entries.sort_by(|a, b| {
        (a.kind != "directory")
            .cmp(&(b.kind != "directory"))
            .then_with(|| natural_cmp(&a.name, &b.name))
            .then_with(|| a.path.cmp(&b.path))
    });
    let omitted = entries.len().saturating_sub(DISPLAY_LIMIT) + unrepresentable;
    entries.truncate(DISPLAY_LIMIT);
    let statuses = git_status(root);
    for entry in &mut entries {
        entry.git_status = statuses.get(&entry.relative_path).cloned().or_else(|| {
            if entry.kind != "directory" {
                return None;
            }
            let prefix = format!("{}/", entry.relative_path);
            statuses
                .iter()
                .find(|(path, _)| path.starts_with(&prefix))
                .map(|(_, status)| status.clone())
        });
    }
    Ok(DirectoryListingDto {
        schema: 3,
        root: path_string(root)?,
        path: path_string(path)?,
        entries,
        omitted,
        scan_limited,
        error: None,
    })
}

fn natural_cmp(a: &str, b: &str) -> Ordering {
    let a = a.to_lowercase();
    let b = b.to_lowercase();
    let mut left = a.as_bytes();
    let mut right = b.as_bytes();
    while !left.is_empty() && !right.is_empty() {
        if left[0].is_ascii_digit() && right[0].is_ascii_digit() {
            let ll = left.iter().take_while(|ch| ch.is_ascii_digit()).count();
            let rl = right.iter().take_while(|ch| ch.is_ascii_digit()).count();
            let l = &left[..ll];
            let r = &right[..rl];
            let ln = l
                .iter()
                .position(|ch| *ch != b'0')
                .map(|i| &l[i..])
                .unwrap_or(b"0");
            let rn = r
                .iter()
                .position(|ch| *ch != b'0')
                .map(|i| &r[i..])
                .unwrap_or(b"0");
            let order = ln
                .len()
                .cmp(&rn.len())
                .then_with(|| ln.cmp(rn))
                .then_with(|| ll.cmp(&rl));
            if order != Ordering::Equal {
                return order;
            }
            left = &left[ll..];
            right = &right[rl..];
        } else {
            let order = left[0].cmp(&right[0]);
            if order != Ordering::Equal {
                return order;
            }
            left = &left[1..];
            right = &right[1..];
        }
    }
    left.len().cmp(&right.len())
}

fn git_command(root: &Path) -> Result<Command, String> {
    let (directory, _) = file_preview::open_directory_in(root, root)?;
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0");
    command
        .args([
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
            "-C",
        ])
        .arg(".")
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    // Resolve the cwd from the already-open no-follow descriptor in the child.
    // A replaced root path cannot redirect Git outside the selected directory.
    unsafe {
        command.pre_exec(move || {
            if libc::fchdir(directory.as_raw_fd()) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    Ok(command)
}
/// Decorations are best effort and bounded; Git cannot stall the watcher or
/// allocate an unbounded porcelain response for a very large repository.
fn git_output(root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
    let mut child = git_command(root)
        .ok()?
        .args(args)
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    let mut output = child.stdout.take()?;
    let flags = unsafe { libc::fcntl(output.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(output.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut bytes = Vec::new();
    let mut buffer = [0; 16 * 1024];
    loop {
        match output.read(&mut buffer) {
            Ok(0) => match child.try_wait() {
                Ok(Some(status)) => return status.success().then_some(bytes),
                Ok(None) => thread::sleep(Duration::from_millis(10)),
                Err(_) => break,
            },
            Ok(count) => {
                if bytes.len() + count > OUTPUT_LIMIT {
                    break;
                }
                bytes.extend_from_slice(&buffer[..count]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
        if Instant::now() >= deadline {
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}
fn git_status(root: &Path) -> BTreeMap<String, String> {
    let Some(output) = git_output(
        root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--no-renames",
        ],
    ) else {
        return BTreeMap::new();
    };
    output
        .split(|byte| *byte == 0)
        .filter_map(|record| {
            if record.len() < 4 {
                return None;
            }
            Some((
                std::str::from_utf8(&record[3..]).ok()?.to_owned(),
                std::str::from_utf8(&record[..2]).ok()?.to_owned(),
            ))
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ListingKey {
    path: PathBuf,
    show_hidden: bool,
}
struct RootWatch {
    root: PathBuf,
    listings: Mutex<BTreeMap<ListingKey, Arc<Hub<DirectoryListingDto>>>>,
    wake: mpsc::SyncSender<bool>,
}
impl Drop for RootWatch {
    fn drop(&mut self) {
        let _ = self.wake.try_send(false);
    }
}
struct FilesSubscription {
    topic: TopicSubscription<DirectoryListingDto>,
    watch: Arc<RootWatch>,
}
static WATCHES: OnceLock<Mutex<BTreeMap<PathBuf, Weak<RootWatch>>>> = OnceLock::new();

fn watch_directory(
    root: &Path,
    path: &Path,
    show_hidden: bool,
) -> Result<FilesSubscription, String> {
    // Reject links/escapes before installing even a filesystem observer.
    file_preview::open_directory_in(root, path)?;
    let watches = WATCHES.get_or_init(Default::default);
    let mut roots = watches.lock().unwrap_or_else(PoisonError::into_inner);
    roots.retain(|_, value| value.strong_count() > 0);
    let watch = if let Some(watch) = roots.get(root).and_then(Weak::upgrade) {
        watch
    } else {
        let (tx, rx) = mpsc::sync_channel(1);
        let changed = tx.clone();
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            // Access notifications are generated by our own reads on some platforms.
            if !matches!(event, Ok(ref event) if matches!(event.kind, notify::EventKind::Access(_))) {
                let _ = changed.try_send(true);
            }
        }).map_err(|e| format!("Cannot watch Files: {e}"))?;
        watcher
            .watch(root, RecursiveMode::Recursive)
            .map_err(|e| format!("Cannot watch Files: {e}"))?;
        // A linked worktree's gitdir lives outside the selected root.
        if let Some(output) = git_output(root, &["rev-parse", "--absolute-git-dir"]) {
            if let Ok(git_dir) = std::str::from_utf8(&output) {
                let git_dir = Path::new(git_dir.trim());
                if !git_dir.starts_with(root) {
                    let _ = watcher.watch(git_dir, RecursiveMode::Recursive);
                }
            }
        }
        let watch = Arc::new(RootWatch {
            root: root.to_owned(),
            listings: Mutex::new(BTreeMap::new()),
            wake: tx,
        });
        let weak = Arc::downgrade(&watch);
        thread::Builder::new()
            .name("riwork-files-watch".into())
            .spawn(move || {
                let _watcher = watcher;
                while let Ok(changed) = rx.recv() {
                    if !changed {
                        break;
                    }
                    // Coalesce a burst once, entirely on the watcher worker.
                    thread::sleep(Duration::from_millis(60));
                    while let Ok(changed) = rx.try_recv() {
                        if !changed {
                            return;
                        }
                    }
                    let Some(watch) = weak.upgrade() else {
                        break;
                    };
                    let listings = watch
                        .listings
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone();
                    for (key, hub) in listings {
                        if !hub.has_subscribers() {
                            continue;
                        }
                        let listing = list_directory(&watch.root, &key.path, key.show_hidden)
                            .unwrap_or_else(|error| DirectoryListingDto {
                                schema: 3,
                                root: watch.root.to_string_lossy().into(),
                                path: key.path.to_string_lossy().into(),
                                entries: Vec::new(),
                                omitted: 0,
                                scan_limited: false,
                                error: Some(error),
                            });
                        hub.publish(listing);
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        roots.insert(root.to_owned(), Arc::downgrade(&watch));
        watch
    };
    drop(roots);
    let key = ListingKey {
        path: path.to_owned(),
        show_hidden,
    };
    let mut listings = watch
        .listings
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let initial = list_directory(root, path, show_hidden)?;
    let hub = listings
        .entry(key)
        .or_insert_with(|| Arc::new(Hub::new(initial.clone())));
    hub.publish(initial);
    let topic = hub.subscribe();
    drop(listings);
    Ok(FilesSubscription { topic, watch })
}

use nsh_core::{
    JsonValue,
    element::{JsHost, JsIn, JsOwned, PropError, TsTypes},
};
impl JsValue for Snapshot<DirectoryListingDto> {
    fn ts_type(types: &mut TsTypes) -> String {
        types.declare("FilesEvent", |types| format!(
            "export interface FilesEvent {{ kind: 'snapshot'; rev: number; data: {}; resync?: boolean; }}",
            DirectoryListingDto::ts_type(types)))
    }
    fn from_js(value: JsIn<'_>) -> Result<Self, PropError> {
        serde_json::from_value(JsonValue::from_js(value)?)
            .map_err(|error| PropError(error.to_string()))
    }
    fn to_js<'a>(&self, host: &'a dyn JsHost) -> Result<JsOwned<'a>, PropError> {
        serde_json::to_value(self)
            .map_err(|error| PropError(error.to_string()))?
            .to_js(host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };
    use tempfile::TempDir;

    struct Fixture {
        _temp: TempDir,
        root: PathBuf,
        outside: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let base = temp.path().canonicalize().unwrap();
            let root = base.join("fixture repo 🦀");
            let outside = base.join("outside");
            fs::create_dir(&root).unwrap();
            fs::create_dir(&outside).unwrap();
            fs::write(outside.join("secret"), "do not read").unwrap();
            Self {
                _temp: temp,
                root,
                outside,
            }
        }
        fn listing(&self) -> DirectoryListingDto {
            list_directory(&self.root, &self.root, false).unwrap()
        }
        fn git(&self, args: &[&str]) {
            let output = git_command(&self.root)
                .unwrap()
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "{args:?}");
        }
    }
    fn entry<'a>(listing: &'a DirectoryListingDto, name: &str) -> &'a FileEntryDto {
        listing
            .entries
            .iter()
            .find(|entry| entry.name == name)
            .unwrap()
    }

    #[test]
    fn fixture_lists_one_level_naturally_with_opaque_identities_and_git_decoration() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.root.join("nested spaced 日本語")).unwrap();
        for name in ["f10", "f02", "F2", "f1", ".hidden", "café 🦀.txt"] {
            fs::write(fixture.root.join(name), name).unwrap();
        }
        fs::write(
            fixture.root.join("nested spaced 日本語/child.md"),
            "# Child",
        )
        .unwrap();
        symlink(&fixture.outside, fixture.root.join("link")).unwrap();
        fixture.git(&["init", "-q"]);
        fixture.git(&["add", "."]);
        fixture.git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@localhost",
            "commit",
            "-qm",
            "fixture",
        ]);
        fs::write(fixture.root.join("f1"), "changed").unwrap();
        fs::write(fixture.root.join("untracked new.txt"), "new").unwrap();
        let listing = fixture.listing();
        assert_eq!(listing.entries[0].name, "nested spaced 日本語");
        assert_eq!(
            listing
                .entries
                .iter()
                .filter(|e| e.name.starts_with('f') || e.name == "F2")
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            ["f1", "F2", "f02", "f10"]
        );
        assert!(!listing.entries.iter().any(|e| e.name == ".hidden"));
        assert!(!listing.entries.iter().any(|e| e.name == "child.md"));
        assert_eq!(entry(&listing, "f1").git_status.as_deref(), Some(" M"));
        assert_eq!(
            entry(&listing, "untracked new.txt").git_status.as_deref(),
            Some("??")
        );
        assert_eq!(entry(&listing, "link").kind, "symlink");
        assert!(entry(&listing, "link").identity.is_none());
        let unicode = entry(&listing, "café 🦀.txt");
        let expected = decode_identity(unicode.identity.as_deref().unwrap()).unwrap();
        assert_eq!(
            file_preview::read_text(&fixture.root, Path::new(&unicode.path), expected).unwrap(),
            "café 🦀.txt"
        );
        assert!(
            list_directory(&fixture.root, &fixture.root, true)
                .unwrap()
                .entries
                .iter()
                .any(|e| e.name == ".hidden")
        );
        let child = list_directory(
            &fixture.root,
            &fixture.root.join("nested spaced 日本語"),
            false,
        )
        .unwrap();
        assert_eq!(
            child.entries[0].relative_path,
            "nested spaced 日本語/child.md"
        );
    }

    #[test]
    fn listings_and_actions_fail_closed_for_escapes_and_replaced_ancestors() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.root.join("folder")).unwrap();
        fs::write(fixture.root.join("folder/selected.txt"), "selected").unwrap();
        let listing = list_directory(&fixture.root, &fixture.root.join("folder"), false).unwrap();
        let selected = &listing.entries[0];
        for attack in [
            fixture.outside.clone(),
            fixture.root.join("../outside"),
            fixture.root.join("folder/../../outside"),
        ] {
            assert!(list_directory(&fixture.root, &attack, true).is_err());
            assert!(
                preview_file(
                    &fixture.root,
                    &attack.join("secret"),
                    selected.identity.as_deref(),
                    false,
                    1
                )
                .is_err()
            );
        }
        symlink(&fixture.outside, fixture.root.join("linked")).unwrap();
        assert!(list_directory(&fixture.root, &fixture.root.join("linked"), true).is_err());
        fs::rename(fixture.root.join("folder"), fixture.root.join("old-folder")).unwrap();
        symlink(&fixture.outside, fixture.root.join("folder")).unwrap();
        assert!(list_directory(&fixture.root, &fixture.root.join("folder"), false).is_err());
        assert!(
            preview_file(
                &fixture.root,
                Path::new(&selected.path),
                selected.identity.as_deref(),
                false,
                1
            )
            .is_err()
        );
        assert!(
            validate_open(
                &fixture.root,
                &fixture.root.join("folder/secret"),
                selected.identity.as_deref()
            )
            .is_err()
        );
        let linked_root = fixture.outside.join("root-link");
        symlink(&fixture.root, &linked_root).unwrap();
        assert!(list_directory(&linked_root, &linked_root, false).is_err());
        assert!(list_directory(Path::new("relative"), Path::new("relative"), false).is_err());
    }

    #[test]
    fn preview_transport_checks_bytes_types_bounds_and_stale_identity() {
        let fixture = Fixture::new();
        fs::write(fixture.root.join("hello.md"), "# Héllo\r\nworld\n").unwrap();
        fs::write(fixture.root.join("binary.bin"), [0, 1, 2, 255]).unwrap();
        fs::write(fixture.root.join("large.txt"), vec![b'x'; 1024 * 1024 + 64]).unwrap();
        let png = STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=").unwrap();
        fs::write(fixture.root.join("pixel.png"), &png).unwrap();
        fs::write(fixture.root.join("fake.pdf"), "not PDF").unwrap();
        let listing = fixture.listing();
        let preview = |name: &str, explicit| {
            let e = entry(&listing, name);
            preview_file(
                &fixture.root,
                Path::new(&e.path),
                e.identity.as_deref(),
                explicit,
                1,
            )
            .unwrap()
        };
        let text = preview("hello.md", false);
        assert_eq!(text.kind, "text");
        assert!(text.markdown);
        assert_eq!(
            file_preview::read_text(
                &fixture.root,
                &fixture.root.join("hello.md"),
                decode_identity(entry(&listing, "hello.md").identity.as_deref().unwrap()).unwrap()
            )
            .unwrap(),
            "# Héllo\r\nworld\n"
        );
        assert_eq!(preview("binary.bin", false).kind, "message");
        let large = preview("large.txt", false);
        assert!(large.truncated);
        assert!(large.text.unwrap().len() < 1024 * 1024 + 64);
        let image = preview("pixel.png", false);
        assert_eq!(image.kind, "image");
        assert_eq!(image.mime.as_deref(), Some("image/png"));
        let bytes = STANDARD
            .decode(image.data_url.unwrap().split_once(',').unwrap().1)
            .unwrap();
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!((image.width, image.height), (Some(1), Some(1)));
        assert!(
            preview("fake.pdf", false)
                .message
                .unwrap()
                .contains("only rendered on request")
        );
        assert!(
            preview("fake.pdf", true)
                .message
                .unwrap()
                .contains("not a valid PDF")
        );
        fs::write(fixture.root.join("hello.md"), "replaced").unwrap();
        let selected = entry(&listing, "hello.md");
        assert!(
            preview_file(
                &fixture.root,
                Path::new(&selected.path),
                selected.identity.as_deref(),
                false,
                1
            )
            .is_err()
        );
        assert!(
            validate_open(
                &fixture.root,
                Path::new(&selected.path),
                selected.identity.as_deref()
            )
            .is_err()
        );
        assert!(
            preview_file(
                &fixture.root,
                &fixture.root.join("binary.bin"),
                None,
                false,
                1
            )
            .is_err()
        );
        assert!(decode_identity("{}").is_err());
    }

    #[test]
    fn external_open_refuses_executables_launchers_links_and_changed_files() {
        let fixture = Fixture::new();
        for name in ["safe.txt", "run.txt", "launch.command"] {
            fs::write(fixture.root.join(name), "text").unwrap();
        }
        fs::set_permissions(
            fixture.root.join("run.txt"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        fs::create_dir(fixture.root.join("Danger.app")).unwrap();
        fs::write(fixture.root.join("Danger.app/innocent.txt"), "innocent").unwrap();
        symlink(fixture.root.join("safe.txt"), fixture.root.join("link.txt")).unwrap();
        let listing = fixture.listing();
        for name in ["run.txt", "launch.command", "link.txt", "Danger.app"] {
            let e = entry(&listing, name);
            assert!(
                validate_open(&fixture.root, Path::new(&e.path), e.identity.as_deref()).is_err(),
                "{name}"
            );
        }
        let e = entry(&listing, "safe.txt");
        assert_eq!(
            validate_open(&fixture.root, Path::new(&e.path), e.identity.as_deref()).unwrap(),
            e.path
        );
        assert!(
            validate_open(
                &fixture.root,
                &fixture.root.join("Danger.app/innocent.txt"),
                e.identity.as_deref()
            )
            .is_err()
        );
        assert_eq!(
            Path::new(&validate_open(&fixture.root, &fixture.root, None).unwrap()),
            fixture.root
        );
    }

    #[test]
    fn huge_directory_is_capped_with_directories_first() {
        let fixture = Fixture::new();
        for i in 0..DISPLAY_LIMIT + 4 {
            fs::write(fixture.root.join(format!("f{i}")), "").unwrap();
        }
        fs::create_dir(fixture.root.join("last directory")).unwrap();
        let listing = fixture.listing();
        assert_eq!(listing.entries.len(), DISPLAY_LIMIT);
        assert_eq!(listing.entries[0].name, "last directory");
        assert_eq!(listing.omitted, 5);
        assert!(!listing.scan_limited);
    }

    #[tokio::test]
    async fn watcher_is_shared_publishes_files_and_git_changes_and_cancels() {
        let fixture = Fixture::new();
        fs::write(fixture.root.join("tracked.txt"), "old").unwrap();
        fixture.git(&["init", "-q"]);
        fixture.git(&["add", "."]);
        fixture.git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@localhost",
            "commit",
            "-qm",
            "fixture",
        ]);
        let first = watch_directory(&fixture.root, &fixture.root, false).unwrap();
        let second = watch_directory(&fixture.root, &fixture.root, false).unwrap();
        assert!(Arc::ptr_eq(&first.watch, &second.watch));
        first.topic.recv().await.unwrap();
        second.topic.recv().await.unwrap();
        fs::write(fixture.root.join("tracked.txt"), "new contents").unwrap();
        fs::write(fixture.root.join("new 日本語.txt"), "new").unwrap();
        let updated = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let updated = first.topic.recv().await.unwrap();
                if updated
                    .data
                    .entries
                    .iter()
                    .any(|e| e.name == "new 日本語.txt")
                {
                    break updated;
                }
            }
        })
        .await
        .unwrap();
        assert!(updated.rev > 1);
        assert_eq!(
            entry(&updated.data, "tracked.txt").git_status.as_deref(),
            Some(" M")
        );
        fixture.git(&["add", "tracked.txt"]);
        let staged = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let updated = first.topic.recv().await.unwrap();
                if entry(&updated.data, "tracked.txt").git_status.as_deref() == Some("M ") {
                    // Identity unchanged: a .git/index-only event still publishes
                    // staged-vs-unstaged decoration once the XY status changes.
                    break updated;
                }
            }
        })
        .await
        .unwrap();
        assert!(staged.rev >= updated.rev);
        let weak = Arc::downgrade(&first.watch);
        first.topic.close();
        second.topic.close();
        assert!(first.topic.recv().await.is_err());
        drop(first);
        drop(second);
        for _ in 0..100 {
            if weak.strong_count() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            weak.strong_count(),
            0,
            "closed owners release shared watcher"
        );
    }
}

fn fixture_directory(name: &str) -> Result<String, String> {
    #[cfg(any(debug_assertions, feature = "test-support"))]
    {
        use std::fs;
        if std::env::var_os("RIWORK_NSH_FILES_FIXTURE").as_deref()
            != Some(std::ffi::OsStr::new("1"))
        {
            return Err("unavailable: Files fixtures are off".into());
        }
        let home = crate::service::process_home()?
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if !(home.starts_with("/tmp") || home.starts_with("/private/tmp")) {
            return Err("refused: Files fixtures require an isolated /tmp RIWORK_HOME".into());
        }
        if name.is_empty()
            || name.len() > 80
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(
                "invalid: Files fixture name must contain only ASCII letters, digits, - or _"
                    .into(),
            );
        }
        static FIXTURES: OnceLock<Mutex<BTreeMap<(PathBuf, String), String>>> = OnceLock::new();
        let mut fixtures = FIXTURES
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let key = (home.clone(), name.to_owned());
        if let Some(root) = fixtures.get(&key) {
            return Ok(root.clone());
        }
        // The unpredictable direct child is created atomically, so existing
        // fixture symlinks cannot turn this helper into a write outside /tmp.
        let root = home.join(format!("files-{name}-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).map_err(|e| e.to_string())?;
        fs::create_dir(root.join("nested dir")).map_err(|e| e.to_string())?;
        fs::write(
            root.join("nested dir/日本語.md"),
            "# Fixture heading\n\n日本語 Markdown.\n",
        )
        .map_err(|e| e.to_string())?;
        fs::write(root.join("note.txt"), "fixture exact text\r\nsecond line\n")
            .map_err(|e| e.to_string())?;
        fs::write(root.join(".hidden"), "hidden\n").map_err(|e| e.to_string())?;
        fs::write(root.join("binary.bin"), [0, 255, 1, 2]).map_err(|e| e.to_string())?;
        fs::write(root.join("large.txt"), vec![b'x'; 1024 * 1024 + 100])
            .map_err(|e| e.to_string())?;
        fs::write(root.join("tiny.png"), STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=").unwrap()).map_err(|e| e.to_string())?;
        // Two small colored pages let native and NSH tests check real paging.
        let red = "1 0 0 rg 0 0 300 200 re f\n";
        let blue = "0 0 1 rg 0 0 200 300 re f\n";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 5 0 R >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] /Contents 6 0 R >>".to_owned(),
            format!("<< /Length {} >>\nstream\n{red}endstream", red.len()),
            format!("<< /Length {} >>\nstream\n{blue}endstream", blue.len()),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        fs::write(root.join("doc.pdf"), pdf).map_err(|e| e.to_string())?;
        let root = path_string(&root)?;
        fixtures.insert(key, root.clone());
        return Ok(root);
    }
    #[cfg(not(any(debug_assertions, feature = "test-support")))]
    {
        let _ = name;
        Err("unavailable: Files fixtures are off".into())
    }
}
