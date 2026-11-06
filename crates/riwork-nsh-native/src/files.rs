//! Descriptor-relative, no-follow Files bridge. Watches are shared by root and
//! exist only while a rendering owner holds a subscription; no view polls disk.
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    ffi::{CStr, CString},
    fs::File,
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{fs::PermissionsExt, process::CommandExt},
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
    #[serde(skip_serializing_if = "Option::is_none")]
    #[prop(skip_none)]
    pub identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    #[prop(skip_none)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct PreviewDto {
    pub schema: u32,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[prop(skip_none)]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[prop(skip_none)]
    pub lines: Option<Vec<String>>,
    pub truncated: bool,
    pub markdown: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[prop(skip_none)]
    pub data_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[prop(skip_none)]
    pub mime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[prop(skip_none)]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[prop(skip_none)]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[prop(skip_none)]
    pub page: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[prop(skip_none)]
    pub pages: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
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
        listener: Callback<crate::hub::SharedSnapshot<DirectoryListingDto>>,
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
                }.into()),
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

/// The Mac's argument policy for a path another device names (`somtus native invoke`): only
/// a path relative to a project or worktree root that the Mac took from its own store. It
/// refuses an absolute path, an empty, `.` or `..` component, NUL and an overlong path before
/// anything is opened; the descriptor walk of the functions below then refuses a symbolic
/// link on the way, a symbolic-link root and anything that is not what was asked for. The
/// text is JSON, so it is UTF-8. An empty path is the root itself where `root_allowed`.
pub fn confined_path(root: &Path, relative: &str, root_allowed: bool) -> Result<PathBuf, String> {
    if !root.is_absolute() {
        return Err("refused: the project root is not an absolute path".into());
    }
    if relative.len() > 4096 {
        return Err("invalid: the path is longer than 4096 bytes".into());
    }
    if relative.contains('\0') {
        return Err("refused: the path contains NUL".into());
    }
    if relative.starts_with('/') {
        return Err("refused: send a path relative to the project, not an absolute one".into());
    }
    if relative.is_empty() {
        return if root_allowed {
            Ok(root.to_owned())
        } else {
            Err("refused: name a file inside the project".into())
        };
    }
    if relative
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == ".." || part.len() > 255)
    {
        return Err("refused: the path leaves the project or is malformed".into());
    }
    Ok(root.join(relative))
}

/// `Files.listDirectory` for another device: `relative` under `root` (see [`confined_path`]).
pub fn list_relative(
    root: &Path,
    relative: &str,
    show_hidden: bool,
) -> Result<DirectoryListingDto, String> {
    let path = confined_path(root, relative, true)?;
    list_directory(root, &path, show_hidden).map_err(bridge_error)
}

/// `Files.preview` for another device: `relative` under `root` (see [`confined_path`]).
pub fn preview_relative(
    root: &Path,
    relative: &str,
    identity: Option<&str>,
    pdf_requested: bool,
    page: usize,
) -> Result<PreviewDto, String> {
    let path = confined_path(root, relative, false)?;
    preview_file(root, &path, identity, pdf_requested, page).map_err(bridge_error)
}

/// `Files.readText` for another device: `relative` under `root` (see [`confined_path`]).
pub fn read_text_relative(root: &Path, relative: &str, identity: &str) -> Result<String, String> {
    let path = confined_path(root, relative, false)?;
    let expected = decode_identity(identity)?;
    if let Some(message) = file_preview::copy_block(&path, expected) {
        return Err(bridge_error(message.into()));
    }
    file_preview::read_text(root, &path, expected).map_err(bridge_error)
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
            "PDFs are only rendered on request, because the PDF parser runs inside Somtus.",
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
    list_directory_with_status(root, path, show_hidden, &OnceLock::new())
}

fn list_directory_with_status(
    root: &Path,
    path: &Path,
    show_hidden: bool,
    statuses: &OnceLock<BTreeMap<String, String>>,
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
    let mut child = PathBuf::new();
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
        let Some(display_name) = entry_name(name, show_hidden, &mut unrepresentable) else {
            continue;
        };
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        #[cfg(test)]
        ENTRY_STATS.with(|calls| calls.set(calls.get() + 1));
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
        // Validate every successful child before sorting, including omitted
        // entries. Keep error precedence for non-UTF-8 parent paths, but defer
        // owned DTO strings until the display cap has selected their owners.
        child.clear();
        child.push(path);
        child.push(display_name);
        checked_entry_paths(root, &child)?;
        entries.push(ScannedEntry::new(display_name, mode, entries.len()));
    }
    let omitted = entries.len().saturating_sub(DISPLAY_LIMIT) + unrepresentable;
    let mut entries = displayed_entries(entries, root, path, DISPLAY_LIMIT)?;
    // Only displayed files need an identity. Keep the validated directory open
    // through this pass: ancestor swaps cannot redirect these no-follow opens.
    // Metadata still comes from the opened file, not the earlier directory stat.
    for entry in entries.iter_mut().filter(|entry| entry.kind == "file") {
        if let Ok(name) = CString::new(entry.name.as_bytes()) {
            entry.identity = entry_identity(&directory, &name);
        }
    }
    // A watcher refresh decorates every subscribed folder with the same Git
    // snapshot. Initialize lazily, after path validation and successful reads.
    decorate_entries(&mut entries, statuses.get_or_init(|| git_status(root)));
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

#[cfg(test)]
thread_local! {
    static IDENTITY_OPENS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static ENTRY_STATS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn entry_identity(directory: &File, name: &CStr) -> Option<String> {
    #[cfg(test)]
    IDENTITY_OPENS.with(|opens| opens.set(opens.get() + 1));
    let child_fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if child_fd < 0 {
        return None;
    }
    let file = unsafe { File::from_raw_fd(child_fd) };
    file.metadata()
        .ok()
        .filter(|meta| meta.is_file())
        .map(|meta| encode_identity(FileIdentity::of(&meta)))
}

fn entry_name<'a>(
    name: &'a CStr,
    show_hidden: bool,
    unrepresentable: &mut usize,
) -> Option<&'a str> {
    if !show_hidden && name.to_bytes().starts_with(b".") {
        return None;
    }
    match name.to_str() {
        Ok(name) => Some(name),
        Err(_) => {
            *unrepresentable += 1;
            None
        }
    }
}

fn checked_entry_paths<'a>(root: &Path, child: &'a Path) -> Result<(&'a str, &'a str), String> {
    let absolute = child
        .to_str()
        .ok_or_else(|| "invalid: file name is not UTF-8".to_owned())?;
    let relative = child
        .strip_prefix(root)
        .map_err(|_| "Folder outside worktree")?
        .to_str()
        .ok_or_else(|| "invalid: file name is not UTF-8".to_owned())?;
    Ok((absolute, relative))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryKind {
    Directory,
    File,
    Symlink,
    Other,
}

impl EntryKind {
    fn from_mode(mode: libc::mode_t) -> Self {
        match mode {
            libc::S_IFDIR => Self::Directory,
            libc::S_IFREG => Self::File,
            libc::S_IFLNK => Self::Symlink,
            _ => Self::Other,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Directory => "directory",
            Self::File => "file",
            Self::Symlink => "symlink",
            Self::Other => "other",
        }
    }
}

// Only nonempty, non-dot, single-component readdir names reach this record.
// In particular, no name can replace or traverse the shared parent path.
struct ScannedEntry {
    name: String,
    lower: String,
    kind: EntryKind,
    at: usize,
}

impl ScannedEntry {
    fn new(name: &str, mode: libc::mode_t, at: usize) -> Self {
        debug_assert!(
            !name.is_empty()
                && name != "."
                && name != ".."
                && !name.contains('/')
                && !name.contains('\0')
        );
        Self {
            name: name.into(),
            lower: name.to_lowercase(),
            kind: EntryKind::from_mode(mode),
            at,
        }
    }

    fn compare(a: &Self, b: &Self) -> Ordering {
        (a.kind != EntryKind::Directory)
            .cmp(&(b.kind != EntryKind::Directory))
            .then_with(|| natural_cmp(&a.lower, &b.lower))
            // All names came from one directory and are single components.
            // Their joined paths share a prefix, so this is the same path tie.
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.at.cmp(&b.at))
    }
}

fn displayed_entries(
    mut entries: Vec<ScannedEntry>,
    root: &Path,
    path: &Path,
    limit: usize,
) -> Result<Vec<FileEntryDto>, String> {
    sort_bounded(&mut entries, limit, ScannedEntry::compare);
    let mut displayed = Vec::with_capacity(entries.len());
    // Every child already passed the original validation during enumeration.
    // Recover exact caller spelling from one checked selected child, then append
    // other single-component names without repeating joins and prefix parsing.
    // Empty/zero-limit selections must not introduce eager path validation.
    if let Some(first) = entries.first() {
        let child = path.join(&first.name);
        let (absolute, relative) = checked_entry_paths(root, &child)?;
        let absolute_prefix = absolute
            .strip_suffix(first.name.as_str())
            .expect("joined child name");
        let relative_prefix = relative
            .strip_suffix(first.name.as_str())
            .expect("relative child name");
        let append = |prefix: &str, name: &str| {
            let mut result = String::with_capacity(prefix.len() + name.len());
            result.push_str(prefix);
            result.push_str(name);
            result
        };
        for entry in entries {
            displayed.push(FileEntryDto {
                path: append(absolute_prefix, &entry.name),
                relative_path: append(relative_prefix, &entry.name),
                name: entry.name,
                kind: entry.kind.as_str().into(),
                identity: None,
                git_status: None,
            });
        }
    }
    Ok(displayed)
}

fn sort_bounded<T>(entries: &mut Vec<T>, limit: usize, compare: impl Fn(&T, &T) -> Ordering) {
    // Do not sort the omitted suffix of a large directory. Comparators include
    // the original index to preserve stable ordering across partial selection.
    // Preserve the linear fast path for already ordered directory enumeration.
    if !entries
        .windows(2)
        .all(|pair| compare(&pair[0], &pair[1]) != Ordering::Greater)
    {
        if limit > 0 && entries.len() > limit.saturating_mul(2) {
            entries.select_nth_unstable_by(limit, &compare);
            entries.truncate(limit);
        }
        entries.sort_unstable_by(compare);
    }
    entries.truncate(limit);
}

#[cfg(test)]
fn sort_entries(entries: Vec<FileEntryDto>, limit: usize) -> Vec<FileEntryDto> {
    // Unicode lowercasing is name-sized allocation/work; pay it once per
    // entry, rather than twice for each of O(n log n) comparisons.
    let mut keyed: Vec<_> = entries
        .into_iter()
        .enumerate()
        .map(|(at, entry)| (entry.name.to_lowercase(), at, entry))
        .collect();
    let compare = |(a_key, a_at, a): &(String, usize, FileEntryDto),
                   (b_key, b_at, b): &(String, usize, FileEntryDto)| {
        (a.kind != "directory")
            .cmp(&(b.kind != "directory"))
            .then_with(|| natural_cmp(a_key, b_key))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a_at.cmp(b_at))
    };
    sort_bounded(&mut keyed, limit, compare);
    keyed.into_iter().map(|(_, _, entry)| entry).collect()
}

fn decorate_entries(entries: &mut [FileEntryDto], statuses: &BTreeMap<String, String>) {
    for entry in entries {
        entry.git_status = statuses.get(&entry.relative_path).cloned().or_else(|| {
            if entry.kind != "directory" {
                return None;
            }
            let prefix = format!("{}/", entry.relative_path);
            // Descendants are contiguous in a BTreeMap. Seek straight to the
            // first possible descendant instead of scanning the whole repo for
            // every displayed directory (including clean directories).
            statuses
                .range(prefix.clone()..)
                .next()
                .filter(|(path, _)| path.starts_with(&prefix))
                .map(|(_, status)| status.clone())
        });
    }
}

// Inputs are already lowercased by sort_entries.
fn natural_cmp(a: &str, b: &str) -> Ordering {
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
    // A failed read waits for an explicit resubscribe/refresh. Unrelated
    // filesystem activity must not repeatedly ask macOS to open a denied path.
    failed: Mutex<BTreeSet<ListingKey>>,
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
            failed: Mutex::new(BTreeSet::new()),
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
                    let statuses = OnceLock::new();
                    for (key, hub) in listings {
                        if !hub.has_subscribers() {
                            continue;
                        }
                        let mut failed =
                            watch.failed.lock().unwrap_or_else(PoisonError::into_inner);
                        if failed.contains(&key) {
                            continue;
                        }
                        let listing = match list_directory_with_status(
                            &watch.root,
                            &key.path,
                            key.show_hidden,
                            &statuses,
                        ) {
                            Ok(listing) => listing,
                            Err(error) => {
                                failed.insert(key.clone());
                                DirectoryListingDto {
                                    schema: 3,
                                    root: watch.root.to_string_lossy().into(),
                                    path: key.path.to_string_lossy().into(),
                                    entries: Vec::new(),
                                    omitted: 0,
                                    scan_limited: false,
                                    error: Some(format!("{error} Refresh Files to try again.")),
                                }
                            }
                        };
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
    watch
        .failed
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&key);
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
        ffi::OsString,
        fs,
        os::unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::{PermissionsExt, symlink},
        },
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

    fn synthetic_entry(name: String, kind: &str) -> FileEntryDto {
        FileEntryDto {
            path: format!("/fixture/{name}"),
            relative_path: name.clone(),
            name,
            kind: kind.into(),
            identity: None,
            git_status: None,
        }
    }

    #[test]
    fn cached_natural_sort_preserves_unicode_numeric_and_path_ties() {
        let names = [
            "f10",
            "f02",
            "F2",
            "f1",
            "f0",
            "f00",
            "f000",
            "f999999999999999999999",
            "f1000000000000000000000",
            "İ2",
            "i\u{307}10",
            "É10",
            "é2",
            "same2",
            "Same2",
        ];
        let mut entries: Vec<_> = names
            .into_iter()
            .map(|name| synthetic_entry(name.into(), "file"))
            .collect();
        entries.push(synthetic_entry("z-directory".into(), "directory"));
        let sorted = sort_entries(entries, usize::MAX);
        assert_eq!(
            sorted
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            [
                "z-directory",
                "f0",
                "f00",
                "f000",
                "f1",
                "F2",
                "f02",
                "f10",
                "f999999999999999999999",
                "f1000000000000000000000",
                "İ2",
                "i\u{307}10",
                "Same2",
                "same2",
                "é2",
                "É10"
            ]
        );
    }

    #[test]
    fn git_badges_preserve_exact_status_and_first_descendant_without_prefix_collisions() {
        let statuses = BTreeMap::from([
            ("a/file".into(), " M".into()),
            ("aa/file".into(), "??".into()),
            ("dir".into(), "D ".into()),
            ("dir/a".into(), " M".into()),
            ("nested/日本語/a".into(), "A ".into()),
            ("nested/日本語/z".into(), " M".into()),
            ("only-child/a".into(), "??".into()),
        ]);
        let mut entries = vec![
            synthetic_entry("a".into(), "directory"),
            synthetic_entry("aa".into(), "directory"),
            synthetic_entry("aaa".into(), "directory"),
            synthetic_entry("dir".into(), "directory"),
            synthetic_entry("nested/日本語".into(), "directory"),
            synthetic_entry("only-child".into(), "file"),
            synthetic_entry("z-clean".into(), "directory"),
        ];
        decorate_entries(&mut entries, &statuses);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.git_status.as_deref())
                .collect::<Vec<_>>(),
            [
                Some(" M"),
                Some("??"),
                None,
                Some("D "),
                Some("A "),
                None,
                None
            ]
        );
    }

    #[test]
    fn refresh_batch_reuses_git_snapshot_but_next_refresh_reads_changes() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.root.join("folder")).unwrap();
        fs::write(fixture.root.join("folder/new.txt"), "new").unwrap();
        fixture.git(&["init", "-q"]);
        let statuses = OnceLock::new();
        assert!(
            list_directory_with_status(&fixture.root, &fixture.outside, false, &statuses).is_err()
        );
        assert!(
            statuses.get().is_none(),
            "invalid paths must not trigger Git"
        );
        let root =
            list_directory_with_status(&fixture.root, &fixture.root, false, &statuses).unwrap();
        assert_eq!(entry(&root, "folder").git_status.as_deref(), Some("??"));
        fixture.git(&["add", "."]);
        let nested = list_directory_with_status(
            &fixture.root,
            &fixture.root.join("folder"),
            false,
            &statuses,
        )
        .unwrap();
        assert_eq!(entry(&nested, "new.txt").git_status.as_deref(), Some("??"));
        let refreshed = list_directory(&fixture.root, &fixture.root.join("folder"), false).unwrap();
        assert_eq!(
            entry(&refreshed, "new.txt").git_status.as_deref(),
            Some("A ")
        );
    }

    #[test]
    fn large_listing_sort_and_badges_match_previous_results() {
        // Paired algorithm timings exclude filesystem/Git latency. No machine-
        // dependent deadline: exact DTO equality is the regression assertion.
        let entries: Vec<_> = (0..10_000)
            .map(|i| {
                synthetic_entry(
                    format!(
                        "component-Équipe-{}-日本語-file{}",
                        (i * 7919) % 10_000,
                        i % 31
                    ),
                    if i % 5 == 0 { "directory" } else { "file" },
                )
            })
            .collect();
        let mut previous = entries.clone();
        let start = Instant::now();
        previous.sort_by(|a, b| {
            (a.kind != "directory")
                .cmp(&(b.kind != "directory"))
                .then_with(|| natural_cmp(&a.name.to_lowercase(), &b.name.to_lowercase()))
                .then_with(|| a.path.cmp(&b.path))
        });
        let before_sort = start.elapsed();
        let start = Instant::now();
        let current = sort_entries(entries, usize::MAX);
        let after_sort = start.elapsed();
        assert_eq!(current, previous);

        let statuses: BTreeMap<_, _> = (0..4_000)
            .map(|i| (format!("dir{i:05}/file"), " M".to_owned()))
            .collect();
        let mut current: Vec<_> = (0..2_000)
            .map(|i| synthetic_entry(format!("dir{:05}", i * 2 + 1), "directory"))
            .collect();
        current.push(synthetic_entry("z-clean".into(), "directory"));
        let mut previous = current.clone();
        let start = Instant::now();
        for entry in &mut previous {
            entry.git_status = statuses.get(&entry.relative_path).cloned().or_else(|| {
                let prefix = format!("{}/", entry.relative_path);
                statuses
                    .iter()
                    .find(|(path, _)| path.starts_with(&prefix))
                    .map(|(_, status)| status.clone())
            });
        }
        let before_badges = start.elapsed();
        let start = Instant::now();
        decorate_entries(&mut current, &statuses);
        let after_badges = start.elapsed();
        assert_eq!(current, previous);
        eprintln!(
            "Files sort (10k): {before_sort:?} -> {after_sort:?}; badges (2k dirs/4k statuses): {before_badges:?} -> {after_badges:?}"
        );
    }

    #[test]
    fn bounded_sort_keeps_the_full_sort_prefix_including_equal_keys() {
        let mut entries: Vec<_> = (0..2000)
            .map(|index| {
                let mut entry = synthetic_entry(
                    format!("Équipe-{}-日本語-file{}", (index * 7919) % 117, index % 31),
                    if index % 5 == 0 { "directory" } else { "file" },
                );
                // Equal sort keys may carry distinct payloads; keep their
                // original relative order across partial selection as well.
                entry.identity = Some(index.to_string());
                entry
            })
            .collect();
        let duplicates: Vec<_> = entries[..100]
            .iter()
            .rev()
            .cloned()
            .map(|mut entry| {
                entry.identity = Some(format!("late-{}", entry.identity.as_deref().unwrap()));
                entry
            })
            .collect();
        entries.extend(duplicates);
        let mut expected = entries.clone();
        expected.sort_by(|a, b| {
            (a.kind != "directory")
                .cmp(&(b.kind != "directory"))
                .then_with(|| natural_cmp(&a.name.to_lowercase(), &b.name.to_lowercase()))
                .then_with(|| a.path.cmp(&b.path))
        });
        for input in [entries, expected.clone()] {
            for limit in [0, 1, 17, 100, 1049, 1050, 2099, 2100, usize::MAX] {
                assert_eq!(
                    sort_entries(input.clone(), limit),
                    expected[..limit.min(expected.len())]
                );
            }
        }
    }

    #[test]
    fn deferred_entries_match_eager_paths_natural_order_and_stable_cutoffs() {
        let mut names: Vec<_> = (0..2100)
            .map(|at| {
                (
                    format!("Équipe-{}-日本語-file{}", (at * 7919) % 117, at % 31),
                    [libc::S_IFDIR, libc::S_IFREG, libc::S_IFLNK, libc::S_IFIFO][at % 4],
                )
            })
            .collect();
        // Same keys with distinct kind payloads check stable cutoff ties.
        names.extend([
            ("same2".into(), libc::S_IFLNK),
            ("same2".into(), libc::S_IFREG),
            ("same2".into(), libc::S_IFIFO),
            ("İ2".into(), libc::S_IFREG),
            ("i\u{307}2".into(), libc::S_IFREG),
            ("F02".into(), libc::S_IFREG),
            ("f2".into(), libc::S_IFREG),
            ("f000".into(), libc::S_IFREG),
            ("f999999999999999999999999999999999".into(), libc::S_IFREG),
        ]);
        let root = Path::new("/fixture");
        for spelling in [
            "/fixture",
            "/fixture/",
            "/fixture/nested 日本語",
            "/fixture//nested/",
            "/fixture//nested///",
            "/fixture/./nested/",
            "/fixture/nested/.",
            "//fixture/./日本語/",
        ] {
            let path = Path::new(spelling);
            for reversed in [false, true] {
                let input: Vec<_> = if reversed {
                    names.iter().rev().collect()
                } else {
                    names.iter().collect()
                };
                let mut expected: Vec<_> = input
                    .iter()
                    .map(|(name, mode)| {
                        let child = path.join(name);
                        FileEntryDto {
                            name: name.clone(),
                            path: path_string(&child).unwrap(),
                            relative_path: path_string(child.strip_prefix(root).unwrap()).unwrap(),
                            kind: EntryKind::from_mode(*mode).as_str().into(),
                            identity: None,
                            git_status: None,
                        }
                    })
                    .collect();
                expected.sort_by(|a, b| {
                    (a.kind != "directory")
                        .cmp(&(b.kind != "directory"))
                        .then_with(|| natural_cmp(&a.name.to_lowercase(), &b.name.to_lowercase()))
                        .then_with(|| a.path.cmp(&b.path))
                });
                for limit in [0, 1, 17, 100, 1054, 1055, 2108, 2109, usize::MAX] {
                    let scanned = input
                        .iter()
                        .enumerate()
                        .map(|(at, (name, mode))| {
                            checked_entry_paths(root, &path.join(name)).unwrap();
                            ScannedEntry::new(name, *mode, at)
                        })
                        .collect();
                    assert_eq!(
                        displayed_entries(scanned, root, path, limit).unwrap(),
                        expected[..limit.min(expected.len())]
                    );
                }
            }
        }
    }

    #[test]
    fn reused_path_buffer_preserves_raw_spelling_and_empty_selection_validation() {
        let mut parents: Vec<_> = [
            "/",
            "//",
            "/fixture/",
            "/fixture//nested///",
            "/fixture/nested/.",
            "/fixture/./日本語/",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
        parents.push(PathBuf::from(OsString::from_vec(
            b"/fixture/p\xff/".to_vec(),
        )));
        for count in 0..32 {
            parents.push(PathBuf::from(format!(
                "/fixture/{}//nested/",
                "parent-".repeat(count)
            )));
        }
        let mut child = PathBuf::from("previous contents must be cleared");
        for path in parents {
            for name in [
                "a",
                "日本語🦀",
                ".hidden",
                "İΟΣ",
                "a long file name 1234567890",
                "z",
            ] {
                child.clear();
                child.push(&path);
                child.push(name);
                let previous = path.join(name);
                assert_eq!(
                    child.as_os_str().as_bytes(),
                    previous.as_os_str().as_bytes()
                );
                for root in [Path::new("/"), Path::new("/fixture"), Path::new("/outside")] {
                    assert_eq!(
                        checked_entry_paths(root, &child),
                        checked_entry_paths(root, &previous)
                    );
                    // Enumeration validates accepted children before selection.
                    // Materialization itself must stay lazy for empty selections.
                    assert!(
                        displayed_entries(Vec::new(), root, &path, 5)
                            .unwrap()
                            .is_empty()
                    );
                    assert!(
                        displayed_entries(
                            vec![ScannedEntry::new(name, libc::S_IFREG, 0)],
                            root,
                            &path,
                            0
                        )
                        .unwrap()
                        .is_empty()
                    );
                }
            }
        }
    }

    #[test]
    fn borrowed_name_and_path_validation_preserve_utf8_filter_and_error_order() {
        // APFS rejects non-UTF-8 names at creation, so exercise those bytes
        // directly as well as the filesystem fixture on supporting platforms.
        for hidden in [false, true] {
            let mut omitted = 0;
            for (bytes, visible) in [
                (b"visible\0".as_slice(), true),
                (b".hidden\0".as_slice(), hidden),
                (b"n\xff\0".as_slice(), false),
                (b".\xff\0".as_slice(), false),
            ] {
                let name = CStr::from_bytes_with_nul(bytes).unwrap();
                assert_eq!(entry_name(name, hidden, &mut omitted).is_some(), visible);
            }
            assert_eq!(omitted, if hidden { 2 } else { 1 });
        }
        let invalid_parent = PathBuf::from(OsString::from_vec(b"/fixture/p\xff".to_vec()));
        let invalid_child = invalid_parent.join("valid-child");
        for root in [
            Path::new("/fixture"),
            Path::new("/outside"),
            &invalid_parent,
        ] {
            assert_eq!(
                checked_entry_paths(root, &invalid_child).unwrap_err(),
                "invalid: file name is not UTF-8"
            );
        }
        assert_eq!(
            checked_entry_paths(Path::new("/fixture"), Path::new("/outside/valid-child")).unwrap_err(),
            "Folder outside worktree"
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_utf8_parent_errors_keep_their_lazy_status_precedence() {
        let fixture = Fixture::new();
        let invalid = OsString::from_vec(vec![b'p', 0xff]);
        let folder = fixture.root.join(&invalid);
        fs::create_dir(&folder).unwrap();
        let error = "invalid: file name is not UTF-8";
        for root in [&fixture.root, &folder] {
            // Empty/all-skipped directories reach lazy Git before final path
            // serialization, exactly as before deferred DTO construction.
            let statuses = OnceLock::new();
            assert_eq!(
                list_directory_with_status(root, &folder, false, &statuses).unwrap_err(),
                error
            );
            assert!(statuses.get().is_some());
            fs::write(folder.join(".hidden"), b"").unwrap();
            fs::write(folder.join(OsString::from_vec(vec![b'n', 0xff])), b"").unwrap();
            let statuses = OnceLock::new();
            assert_eq!(
                list_directory_with_status(root, &folder, false, &statuses).unwrap_err(),
                error
            );
            assert!(statuses.get().is_some());
            // The first successful visible child must still fail immediately,
            // before sorting, identity opening, or lazy status initialization.
            let statuses = OnceLock::new();
            IDENTITY_OPENS.with(|count| count.set(0));
            assert_eq!(
                list_directory_with_status(root, &folder, true, &statuses).unwrap_err(),
                error
            );
            assert!(statuses.get().is_none());
            assert_eq!(IDENTITY_OPENS.with(|count| count.get()), 0);
            fs::remove_file(folder.join(".hidden")).unwrap();
            fs::remove_file(folder.join(OsString::from_vec(vec![b'n', 0xff]))).unwrap();
        }
    }

    #[test]
    fn hidden_and_non_utf8_names_keep_scan_omission_and_stat_behavior() {
        let fixture = Fixture::new();
        let mut names = vec![OsString::from("visible"), OsString::from(".hidden")];
        if !cfg!(target_os = "macos") {
            names.extend([
                OsString::from_vec(vec![b'n', 0xff]),
                OsString::from_vec(vec![b'.', 0xff]),
            ]);
        }
        for name in names {
            fs::write(fixture.root.join(name), b"").unwrap();
        }
        for hidden in [false, true] {
            let statuses = OnceLock::from(BTreeMap::new());
            ENTRY_STATS.with(|calls| calls.set(0));
            IDENTITY_OPENS.with(|calls| calls.set(0));
            let listing =
                list_directory_with_status(&fixture.root, &fixture.root, hidden, &statuses).unwrap();
            let count = if hidden { 2 } else { 1 };
            assert_eq!(listing.entries.len(), count);
            assert_eq!(
                listing.omitted,
                if cfg!(target_os = "macos") { 0 } else { count }
            );
            assert!(!listing.scan_limited);
            assert_eq!(ENTRY_STATS.with(|calls| calls.get()), count);
            assert_eq!(IDENTITY_OPENS.with(|calls| calls.get()), count);
        }
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
    fn oversized_directory_opens_only_displayed_files_and_keeps_the_same_sorted_page() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.root.join("directory")).unwrap();
        for index in 0..DISPLAY_LIMIT + 3 {
            fs::write(fixture.root.join(format!("file-{index}")), b"").unwrap();
        }
        IDENTITY_OPENS.with(|opens| opens.set(0));
        ENTRY_STATS.with(|calls| calls.set(0));
        let listing = fixture.listing();
        assert_eq!(listing.entries.len(), DISPLAY_LIMIT);
        assert_eq!(listing.omitted, 4);
        assert!(!listing.scan_limited);
        assert_eq!(listing.entries[0].name, "directory");
        assert_eq!(IDENTITY_OPENS.with(|opens| opens.get()), DISPLAY_LIMIT - 1);
        assert_eq!(ENTRY_STATS.with(|calls| calls.get()), DISPLAY_LIMIT + 4);
        for (index, entry) in listing.entries[1..].iter().enumerate() {
            assert_eq!(entry.name, format!("file-{index}"));
            let actual = decode_identity(entry.identity.as_deref().unwrap()).unwrap();
            let expected = FileIdentity::of(&fs::metadata(&entry.path).unwrap());
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn delayed_identity_uses_the_open_directory_and_refuses_replaced_links_and_special_files() {
        let fixture = Fixture::new();
        let folder = fixture.root.join("folder");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("selected"), "original").unwrap();
        let expected = FileIdentity::of(&fs::metadata(folder.join("selected")).unwrap());
        let (directory, _) = file_preview::open_directory_in(&fixture.root, &folder).unwrap();
        fs::rename(&folder, fixture.root.join("old-folder")).unwrap();
        symlink(&fixture.outside, &folder).unwrap();
        fs::write(fixture.outside.join("selected"), "replacement outside").unwrap();
        let name = CString::new("selected").unwrap();
        let actual = entry_identity(&directory, &name).unwrap();
        assert_eq!(decode_identity(&actual).unwrap(), expected);
        let original = fixture.root.join("old-folder/selected");
        fs::remove_file(&original).unwrap();
        symlink(fixture.outside.join("selected"), &original).unwrap();
        assert!(entry_identity(&directory, &name).is_none());
        fs::remove_file(&original).unwrap();
        fs::create_dir(&original).unwrap();
        assert!(entry_identity(&directory, &name).is_none());
        fs::remove_dir(&original).unwrap();
        let fifo = CString::new(original.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(entry_identity(&directory, &name).is_none());
    }

    #[test]
    #[ignore = "manual displayed-file identity benchmark"]
    fn displayed_identity_performance() {
        let fixture = Fixture::new();
        let names: Vec<_> = (0..20_000)
            .map(|index| CString::new(format!("file-{index}")).unwrap())
            .collect();
        for name in &names {
            fs::write(fixture.root.join(name.to_str().unwrap()), b"").unwrap();
        }
        let (directory, _) = file_preview::open_directory_in(&fixture.root, &fixture.root).unwrap();
        let mut samples = [Vec::new(), Vec::new()];
        for pass in 0..5 {
            for displayed_only in if pass % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                IDENTITY_OPENS.with(|opens| opens.set(0));
                let start = Instant::now();
                let count = if displayed_only {
                    DISPLAY_LIMIT
                } else {
                    names.len()
                };
                let mut identities: Vec<_> = names[..count]
                    .iter()
                    .map(|name| entry_identity(&directory, name))
                    .collect();
                identities.truncate(DISPLAY_LIMIT);
                samples[usize::from(displayed_only)].push(start.elapsed());
                assert_eq!(IDENTITY_OPENS.with(|opens| opens.get()), count);
                assert!(identities.iter().all(Option::is_some));
                std::hint::black_box(identities);
            }
        }
        for values in &mut samples {
            values.sort();
        }
        eprintln!(
            "Files identity phase (20k scanned/5k displayed): {:?} -> {:?}; opens 20000 -> 5000",
            samples[0][2], samples[1][2]
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

    /// `somtus native invoke`'s path policy: another device names a path relative to a root
    /// the Mac takes from its store, and nothing outside that root is listed, previewed or read.
    #[test]
    fn another_devices_paths_stay_inside_the_project_root() {
        let fixture = Fixture::new();
        let root = &fixture.root;
        fs::create_dir(root.join("folder")).unwrap();
        fs::write(root.join("folder/inside.txt"), "inside").unwrap();
        symlink(&fixture.outside, root.join("linked")).unwrap();
        let identity =
            |path: &Path| encode_identity(FileIdentity::of(&fs::metadata(path).unwrap()));
        let inside = identity(&root.join("folder/inside.txt"));
        let secret = identity(&fixture.outside.join("secret"));
        assert_eq!(
            read_text_relative(root, "folder/inside.txt", &inside).unwrap(),
            "inside"
        );
        assert_eq!(confined_path(root, "", true).unwrap(), *root);
        assert!(confined_path(root, "", false).is_err());
        let absolute = fixture
            .outside
            .join("secret")
            .to_string_lossy()
            .into_owned();
        for path in [
            absolute.as_str(),
            "/etc/passwd",
            "../outside/secret",
            "folder/../../outside/secret",
            "folder/..",
            "./folder/inside.txt",
            "folder//inside.txt",
            "folder/inside.txt\0",
            "folder\0/inside.txt",
            // A link inside the root that points out of it.
            "linked/secret",
            "linked",
        ] {
            assert!(list_relative(root, path, true).is_err(), "{path:?}");
            assert!(
                preview_relative(root, path, Some(&secret), false, 1).is_err(),
                "{path:?}"
            );
            assert!(read_text_relative(root, path, &secret).is_err(), "{path:?}");
        }
        // A project root that is itself a symbolic link is refused, as on the Mac's own panel.
        let alias = root.parent().unwrap().join("alias");
        symlink(root, &alias).unwrap();
        assert!(list_relative(&alias, "", true).is_err());
        assert!(read_text_relative(&alias, "folder/inside.txt", &inside).is_err());
        assert!(preview_relative(&alias, "folder/inside.txt", Some(&inside), false, 1).is_err());
        assert!(confined_path(Path::new("relative/root"), "folder", true).is_err());

        // Outside every project: roots come from the store, for its projects and their
        // worktrees only.
        let home = root.parent().unwrap().join("home");
        let store = crate::backend::CoreStoreBackend::new(home);
        use crate::backend::StoreBackend;
        let project = store
            .create_project(crate::dto::ProjectRequest {
                root: root.to_string_lossy().into_owned(),
                name: None,
                init_git: Some(false),
            })
            .unwrap();
        let state = store.snapshot().unwrap();
        assert_eq!(
            crate::invoke::project_root(&state, &project.id, None).unwrap(),
            *root
        );
        for (project_id, worktree_id) in [
            ("00000000-0000-4000-8000-000000000001", None),
            (
                project.id.as_str(),
                Some("00000000-0000-4000-8000-000000000002"),
            ),
            ("not-a-uuid", None),
            (project.id.as_str(), Some("../outside")),
        ] {
            assert!(
                crate::invoke::project_root(&state, project_id, worktree_id).is_err(),
                "{project_id} {worktree_id:?}"
            );
        }
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
