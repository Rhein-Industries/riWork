//! A lazy filesystem browser scoped to the workspace's selected worktree, and the preview of
//! the file selected in it.
//!
//! The browser (`FileExplorer`) owns every piece of state, the selection and the preview of
//! it included. It draws only the tree. The Preview panel is a second view of the same
//! entity (`FilePreview`) that draws the preview and its actions in a pane of its own, so
//! there is one selection per window and the two panes cannot disagree.

use std::{
    cell::RefCell,
    cmp::Ordering,
    collections::{BTreeMap, HashSet},
    ffi::OsStr,
    fs,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::text_input::{self, EnterBehavior, InputEvent, InputState};
use gpui::{
    AnyElement, App, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, IntoElement,
    KeyDownEvent, Render, RenderImage, ScrollStrategy, SharedString, Task, UniformListScrollHandle,
    Window, div, img, prelude::*, px, rgb, uniform_list,
};
use gpui::{Focusable, Subscription};

use crate::{
    controls,
    file_preview::{self, FileIdentity, PreviewContent},
    icons::{self, ActionGlyph, Icon},
    settings::Settings,
    theme,
    tooltip::{self, Look},
    ui_text,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExplorerRoot {
    pub path: PathBuf,
    pub label: String,
    pub worktree_id: Option<String>,
}

pub enum FileExplorerEvent {
    Edit {
        root: ExplorerRoot,
        path: PathBuf,
        identity: FileIdentity,
    },
    Open(PathBuf),
    Reveal(PathBuf),
    /// The path is relative to the current worktree root.
    CopyRelativePath(PathBuf),
    /// The user chose a file or link: with a click or the keyboard, not by the tree
    /// picking its first row. The window shows the preview if it is set to.
    Selected,
    /// A file was selected for a link in a terminal. The window shows the preview even if it
    /// is set not to open it for a plain selection: the link was asked for by name.
    Revealed,
    /// A path a terminal link named, inside the root and known to exist, is not in any listing:
    /// it is past the cap on a folder's rows, or ignored.
    NotListed(PathBuf),
    RevealFailed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryKind {
    Directory,
    File,
    Symlink,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    path: PathBuf,
    name: String,
    kind: EntryKind,
    hidden: bool,
    identity: Option<FileIdentity>,
}

/// Longest listing shown for one folder, and how many names are read before
/// giving up, so a folder with millions of files cannot stall or bloat the UI.
const MAX_LISTED_ENTRIES: usize = 5_000;
const MAX_SCANNED_ENTRIES: usize = 50_000;

/// One folder's contents, already sorted and capped.
#[derive(Debug)]
struct Listing {
    entries: Vec<Entry>,
    /// Entries left out by the cap; only a lower bound when `scan_capped`.
    omitted: usize,
    scan_capped: bool,
}

/// Compare digit runs by magnitude, without parsing them into a bounded integer.
/// Both names must already be case-folded, which callers do once per entry
/// rather than on every comparison.
fn natural_cmp(left: &str, right: &str) -> Ordering {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < left.len() && j < right.len() {
        if left[i].is_ascii_digit() && right[j].is_ascii_digit() {
            let (start_i, start_j) = (i, j);
            while i < left.len() && left[i].is_ascii_digit() {
                i += 1;
            }
            while j < right.len() && right[j].is_ascii_digit() {
                j += 1;
            }
            let a = &left[start_i..i];
            let b = &right[start_j..j];
            let a_value = &a[a.iter().position(|ch| *ch != b'0').unwrap_or(a.len())..];
            let b_value = &b[b.iter().position(|ch| *ch != b'0').unwrap_or(b.len())..];
            let order = a_value.len().cmp(&b_value.len()).then(a_value.cmp(b_value));
            if order != Ordering::Equal {
                return order;
            }
            let order = a.len().cmp(&b.len());
            if order != Ordering::Equal {
                return order;
            }
        } else {
            let order = left[i].cmp(&right[j]);
            if order != Ordering::Equal {
                return order;
            }
            i += 1;
            j += 1;
        }
    }
    left.len().cmp(&right.len())
}

fn read_directory(path: &Path) -> Result<Listing, String> {
    read_directory_capped(path, MAX_LISTED_ENTRIES, MAX_SCANNED_ENTRIES)
}

fn read_directory_capped(
    path: &Path,
    max_listed: usize,
    max_scanned: usize,
) -> Result<Listing, String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| format!("Cannot read this folder: {error}"))?;
    if metadata.file_type().is_symlink() {
        return Err("Symbolic links are not expanded. Use Reveal instead.".into());
    }
    if !metadata.is_dir() {
        return Err("This folder is no longer a directory.".into());
    }
    let directory =
        fs::read_dir(path).map_err(|error| format!("Cannot read this folder: {error}"))?;
    // Names are folded once here instead of twice per comparison while sorting.
    let mut scanned: Vec<(String, Entry)> = Vec::new();
    let mut scan_capped = false;
    for entry in directory {
        if scanned.len() >= max_scanned {
            scan_capped = true;
            break;
        }
        let entry = entry.map_err(|error| format!("Cannot list this folder: {error}"))?;
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            // A file removed while listing should not invalidate the whole directory.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("Cannot inspect this folder's files: {error}")),
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        scanned.push((
            name.to_lowercase(),
            Entry {
                path: entry.path(),
                hidden: name.starts_with('.'),
                name,
                identity: None,
                kind: if file_type.is_symlink() {
                    EntryKind::Symlink
                } else if file_type.is_dir() {
                    EntryKind::Directory
                } else if file_type.is_file() {
                    EntryKind::File
                } else {
                    EntryKind::Other
                },
            },
        ));
    }
    scanned.sort_by(|(a_key, a), (b_key, b)| {
        (a.kind != EntryKind::Directory)
            .cmp(&(b.kind != EntryKind::Directory))
            .then_with(|| natural_cmp(a_key, b_key))
            .then_with(|| a.path.cmp(&b.path))
    });
    let omitted = scanned.len().saturating_sub(max_listed);
    scanned.truncate(max_listed);
    let mut entries = scanned
        .into_iter()
        .map(|(_, entry)| entry)
        .collect::<Vec<_>>();
    // Only listed files need a stat for their identity.
    for entry in entries
        .iter_mut()
        .filter(|entry| entry.kind == EntryKind::File)
    {
        entry.identity = fs::symlink_metadata(&entry.path)
            .ok()
            .filter(|metadata| metadata.is_file())
            .map(|metadata| FileIdentity::of(&metadata));
    }
    Ok(Listing {
        entries,
        omitted,
        scan_capped,
    })
}

fn read_directory_in(root: &Path, path: &Path) -> Result<Listing, String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "This folder is outside the selected worktree.".to_owned())?;
    // Recheck ancestors when loading cached paths: a directory may have been
    // replaced by a symlink since its parent was listed.
    let mut current = root.to_owned();
    for component in std::iter::once(None).chain(relative.components().map(Some)) {
        if let Some(component) = component {
            match component {
                std::path::Component::Normal(name) => current.push(name),
                std::path::Component::CurDir => continue,
                _ => return Err("This folder is outside the selected worktree.".into()),
            }
        }
        let metadata = fs::symlink_metadata(&current)
            .map_err(|error| format!("Cannot read this folder: {error}"))?;
        if metadata.file_type().is_symlink() {
            return Err("Symbolic links are not expanded. Use Reveal instead.".into());
        }
    }
    read_directory(path)
}

#[derive(Clone, Default)]
struct DirectoryState {
    entries: Vec<Entry>,
    omitted: usize,
    scan_capped: bool,
    loading: bool,
    /// A listing or an error has arrived at least once, so a reload does not
    /// flash "Loading…" over rows that are already on screen.
    loaded: bool,
    error: Option<String>,
    request: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowKind {
    Entry(EntryKind),
    Status,
    Error,
}

#[derive(Clone)]
struct TreeRow {
    path: PathBuf,
    label: String,
    depth: usize,
    kind: RowKind,
    identity: Option<FileIdentity>,
}

fn omitted_label(omitted: usize, scan_capped: bool) -> String {
    let more = if scan_capped { "+" } else { "" };
    if omitted == 1 && !scan_capped {
        "1 more entry not shown".into()
    } else {
        format!("{omitted}{more} more entries not shown")
    }
}

enum Pending<'a> {
    Entry(&'a Entry, usize),
    Omitted {
        directory: &'a Path,
        label: String,
        depth: usize,
    },
}

fn push_children<'a>(
    stack: &mut Vec<Pending<'a>>,
    directory: &'a Path,
    state: &'a DirectoryState,
    depth: usize,
    show_omitted: bool,
) {
    // The stack is popped in reverse, so the notice goes in first to come last.
    if show_omitted && state.omitted > 0 {
        stack.push(Pending::Omitted {
            directory,
            label: omitted_label(state.omitted, state.scan_capped),
            depth,
        });
    }
    for entry in state.entries.iter().rev() {
        stack.push(Pending::Entry(entry, depth));
    }
}

fn visible_rows(
    root: &Path,
    directories: &BTreeMap<PathBuf, DirectoryState>,
    expanded: &HashSet<PathBuf>,
    show_hidden: bool,
    filter: &str,
) -> Vec<TreeRow> {
    let query = filter.trim().to_lowercase();
    let mut matches = HashSet::new();
    if !query.is_empty() {
        for entry in directories.values().flat_map(|state| &state.entries) {
            if !show_hidden
                && entry.path.strip_prefix(root).ok().is_some_and(|relative| {
                    relative
                        .components()
                        .any(|part| part.as_os_str().to_string_lossy().starts_with('.'))
                })
            {
                continue;
            }
            let relative = entry.path.strip_prefix(root).unwrap_or(&entry.path);
            if relative.to_string_lossy().to_lowercase().contains(&query) {
                let mut path = entry.path.as_path();
                while path != root && path.starts_with(root) {
                    matches.insert(path.to_owned());
                    let Some(parent) = path.parent() else { break };
                    path = parent;
                }
            }
        }
    }
    // Iterative traversal also bounds stack use in unusually deep worktrees.
    let mut rows = Vec::new();
    let mut stack = Vec::new();
    if let Some((directory, state)) = directories.get_key_value(root) {
        push_children(&mut stack, directory, state, 0, query.is_empty());
    }
    while let Some(item) = stack.pop() {
        let (entry, depth) = match item {
            Pending::Entry(entry, depth) => (entry, depth),
            Pending::Omitted {
                directory,
                label,
                depth,
            } => {
                rows.push(TreeRow {
                    path: directory.to_owned(),
                    label,
                    depth,
                    kind: RowKind::Status,
                    identity: None,
                });
                continue;
            }
        };
        if (!show_hidden && entry.hidden) || (!query.is_empty() && !matches.contains(&entry.path)) {
            continue;
        }
        rows.push(TreeRow {
            path: entry.path.clone(),
            label: entry.name.clone(),
            depth,
            kind: RowKind::Entry(entry.kind),
            identity: entry.identity,
        });
        if entry.kind != EntryKind::Directory
            || (query.is_empty() && !expanded.contains(&entry.path))
        {
            continue;
        }
        if let Some(state) = directories.get(&entry.path) {
            if (state.loading && !state.loaded) || state.error.is_some() {
                rows.push(TreeRow {
                    path: entry.path.clone(),
                    label: state.error.clone().unwrap_or_else(|| "Loading…".into()),
                    depth: depth + 1,
                    kind: if state.error.is_some() {
                        RowKind::Error
                    } else {
                        RowKind::Status
                    },
                    identity: None,
                });
            }
            push_children(&mut stack, &entry.path, state, depth + 1, query.is_empty());
        }
    }
    rows
}

struct RowsCache {
    version: u64,
    show_hidden: bool,
    filter: String,
    rows: Rc<Vec<TreeRow>>,
}

/// What a finished folder load changed.
struct LoadOutcome {
    /// The visible rows may differ from before.
    changed: bool,
    /// Expanded subfolders to poll next.
    reload: Vec<PathBuf>,
}

/// Loaded folders plus the visible-row list derived from them. The list is
/// cached because render, key handlers and load completions all need it, and
/// rebuilding it clones every visible entry.
#[derive(Default)]
struct TreeModel {
    directories: BTreeMap<PathBuf, DirectoryState>,
    expanded: HashSet<PathBuf>,
    /// Bumped by every mutable borrow, so a cached list cannot outlive a change.
    version: u64,
    cache: RefCell<Option<RowsCache>>,
}

impl TreeModel {
    fn directories(&self) -> &BTreeMap<PathBuf, DirectoryState> {
        &self.directories
    }

    fn directories_mut(&mut self) -> &mut BTreeMap<PathBuf, DirectoryState> {
        self.version = self.version.wrapping_add(1);
        &mut self.directories
    }

    fn expanded(&self) -> &HashSet<PathBuf> {
        &self.expanded
    }

    fn expanded_mut(&mut self) -> &mut HashSet<PathBuf> {
        self.version = self.version.wrapping_add(1);
        &mut self.expanded
    }

    fn clear(&mut self) {
        self.directories_mut().clear();
        self.expanded_mut().clear();
    }

    fn rows(&self, root: &Path, show_hidden: bool, filter: &str) -> Rc<Vec<TreeRow>> {
        let cached = self
            .cache
            .borrow()
            .as_ref()
            .filter(|cache| {
                cache.version == self.version
                    && cache.show_hidden == show_hidden
                    && cache.filter == filter
            })
            .map(|cache| cache.rows.clone());
        if let Some(rows) = cached {
            return rows;
        }
        let rows = Rc::new(visible_rows(
            root,
            &self.directories,
            &self.expanded,
            show_hidden,
            filter,
        ));
        *self.cache.borrow_mut() = Some(RowsCache {
            version: self.version,
            show_hidden,
            filter: filter.to_owned(),
            rows: rows.clone(),
        });
        rows
    }

    /// Returns whether the rows changed, which only a first load does: a
    /// reload keeps showing the previous listing until the new one arrives.
    fn begin_load(&mut self, path: &Path, request: u64) -> bool {
        if let Some(state) = self.directories.get_mut(path) {
            state.loading = true;
            state.request = request;
            return false;
        }
        self.directories_mut().insert(
            path.to_owned(),
            DirectoryState {
                loading: true,
                request,
                ..Default::default()
            },
        );
        true
    }

    fn finish_load(
        &mut self,
        path: &Path,
        request: u64,
        result: Result<Listing, String>,
    ) -> LoadOutcome {
        // The agent-written folders this polls every couple of seconds mostly
        // come back identical, and then nothing needs rebuilding or repainting.
        let unchanged = self.directories.get(path).is_some_and(|old| {
            old.loaded
                && match &result {
                    Ok(listing) => {
                        old.error.is_none()
                            && old.entries == listing.entries
                            && old.omitted == listing.omitted
                            && old.scan_capped == listing.scan_capped
                    }
                    Err(error) => old.error.as_ref() == Some(error),
                }
        });
        if unchanged {
            if let Some(state) = self.directories.get_mut(path) {
                state.loading = false;
                state.request = request;
            }
        } else {
            let state = match result {
                Ok(listing) => DirectoryState {
                    entries: listing.entries,
                    omitted: listing.omitted,
                    scan_capped: listing.scan_capped,
                    loaded: true,
                    request,
                    ..Default::default()
                },
                Err(error) => DirectoryState {
                    error: Some(error),
                    loaded: true,
                    request,
                    ..Default::default()
                },
            };
            let children = state
                .entries
                .iter()
                .filter(|entry| entry.kind == EntryKind::Directory)
                .map(|entry| entry.path.clone())
                .collect::<HashSet<_>>();
            // Removed folders, and folders replaced by symlinks, lose their cached subtree.
            let stale = self
                .directories
                .keys()
                .filter(|child| child.parent() == Some(path) && !children.contains(*child))
                .cloned()
                .collect::<Vec<_>>();
            if !stale.is_empty() {
                self.directories_mut()
                    .retain(|child, _| !stale.iter().any(|stale| child.starts_with(stale)));
                self.expanded_mut()
                    .retain(|child| !stale.iter().any(|stale| child.starts_with(stale)));
            }
            self.directories_mut().insert(path.to_owned(), state);
        }
        // Collapsed folders keep their cached listing but are not polled; they
        // reload when expanded again.
        let reload = self
            .directories
            .get(path)
            .map(|state| {
                state
                    .entries
                    .iter()
                    .filter(|entry| {
                        entry.kind == EntryKind::Directory && self.expanded.contains(&entry.path)
                    })
                    .map(|entry| entry.path.clone())
                    .collect()
            })
            .unwrap_or_default();
        LoadOutcome {
            changed: !unchanged,
            reload,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Search,
    Tree,
    Preview,
    Refresh,
    Hidden,
    RevealRoot,
    Copy,
    CopyContents,
    Reveal,
    Edit,
    Open,
}

/// The file actions in the preview header, left to right.
const TOOLBAR: [ToolbarAction; 5] = [
    ToolbarAction {
        id: "file-explorer-edit",
        label: "Edit in Vim ↗",
        name: "Edit in Vim",
        glyph: ActionGlyph::EditInVim,
        mode: Mode::Edit,
    },
    ToolbarAction {
        id: "file-explorer-copy",
        label: "Copy path",
        name: "Copy path",
        glyph: ActionGlyph::CopyPath,
        mode: Mode::Copy,
    },
    ToolbarAction {
        id: "file-explorer-copy-contents",
        label: "Copy contents",
        name: "Copy contents",
        glyph: ActionGlyph::CopyContents,
        mode: Mode::CopyContents,
    },
    ToolbarAction {
        id: "file-explorer-reveal",
        label: "Reveal",
        name: "Reveal in Finder",
        glyph: ActionGlyph::Reveal,
        mode: Mode::Reveal,
    },
    ToolbarAction {
        id: "file-explorer-open",
        label: "Open externally",
        name: "Open externally",
        glyph: ActionGlyph::OpenExternally,
        mode: Mode::Open,
    },
];

/// One button of the preview header: its words, or its glyph with `name` as the tooltip.
struct ToolbarAction {
    id: &'static str,
    label: &'static str,
    name: &'static str,
    glyph: ActionGlyph,
    mode: Mode,
}

/// What Copy Contents does when it is available; the reason it is not otherwise.
const COPY_CONTENTS_HINT: &str = "Copy the whole file as text (up to 1 MiB)";

/// The keyboard shortcut or explanation an icon button adds to its name.
fn tooltip_hint(mode: Mode) -> Option<&'static str> {
    match mode {
        Mode::Edit => Some("⌘E"),
        Mode::Open => Some("⌘O"),
        Mode::CopyContents => Some(COPY_CONTENTS_HINT),
        _ => None,
    }
}

/// The SF Symbol and name of a button under Native, which draws them all as symbols.
fn native_face(mode: Mode, face: &Face, show_hidden: bool) -> (&'static str, &'static str) {
    match (mode, face) {
        (_, Face::Glyph(glyph, name)) => (Icon::Action(*glyph).symbol(), name),
        (Mode::Refresh, _) => ("arrow.clockwise", "Refresh"),
        (Mode::Hidden, _) if show_hidden => ("eye", "Hide hidden files"),
        (Mode::Hidden, _) => ("eye.slash", "Show hidden files"),
        (Mode::RevealRoot, _) => ("folder", "Reveal worktree in Finder"),
        _ => ("questionmark", "Action"),
    }
}

/// An icon has no words, so its tooltip is the name, then the hint or the reason
/// the button is unavailable.
fn icon_tooltip(name: &str, detail: Option<&str>) -> String {
    match detail {
        Some(detail) => format!("{name} · {detail}"),
        None => name.to_owned(),
    }
}

/// What Tab visits in the Files pane, in order. The preview has its own pane and its own
/// focus, so Tab stays within the pane it was pressed in.
const TREE_FOCUS_ORDER: [Mode; 5] = [
    Mode::Search,
    Mode::Tree,
    Mode::Refresh,
    Mode::Hidden,
    Mode::RevealRoot,
];

/// What Tab visits in the Preview pane, in order.
const PREVIEW_FOCUS_ORDER: [Mode; 6] = [
    Mode::Preview,
    Mode::Copy,
    Mode::CopyContents,
    Mode::Reveal,
    Mode::Edit,
    Mode::Open,
];

impl Mode {
    /// Whether the control is in the Preview pane rather than the Files pane.
    fn in_preview_pane(self) -> bool {
        PREVIEW_FOCUS_ORDER.contains(&self)
    }

    /// The controls Tab cycles through in the pane that holds this one.
    fn focus_order(self) -> &'static [Mode] {
        if self.in_preview_pane() {
            &PREVIEW_FOCUS_ORDER
        } else {
            &TREE_FOCUS_ORDER
        }
    }
}

#[derive(Default)]
struct FilterInput {
    // Domain snapshot only. The retained Kit state owns all editing and selection.
    text: String,
}

/// How a row came to be selected. Only deliberate choices may parse a PDF;
/// passive ones (arrow keys, auto-selection) also wait out a short delay so
/// stepping through a folder does not decode every file it passes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Intent {
    /// The tree picked a row itself, such as the first one after a load or a filter. It
    /// previews like `Passive`, but nobody chose it, so it never opens the Preview pane.
    Auto,
    Passive,
    Explicit,
}

const PREVIEW_DEBOUNCE: Duration = Duration::from_millis(150);

impl Intent {
    fn delay(self) -> Duration {
        match self {
            Self::Auto | Self::Passive => PREVIEW_DEBOUNCE,
            Self::Explicit => Duration::ZERO,
        }
    }
}

/// Whether choosing this row asks the window to show the preview: a file or link that a
/// person selected, not a folder and not a row the tree picked by itself.
fn reveals_preview(intent: Intent, kind: Option<RowKind>) -> bool {
    intent != Intent::Auto
        && matches!(
            kind,
            Some(RowKind::Entry(EntryKind::File | EntryKind::Symlink))
        )
}

/// What the tree knows about the entry at `path`: the kind and identity its preview is
/// planned from. A row that is gone or filtered out has none, and the preview empties.
fn entry_of(rows: &[TreeRow], path: &Path) -> Option<(EntryKind, Option<FileIdentity>)> {
    rows.iter().find_map(|row| match row.kind {
        RowKind::Entry(kind) if row.path == path => Some((kind, row.identity)),
        _ => None,
    })
}

/// The name above the preview. It follows the content on screen, which trails the
/// selection while a replacement loads, and falls back to the selection and then to
/// "PREVIEW".
fn preview_title(shown: Option<&Path>, selected: Option<&Path>) -> String {
    shown
        .or(selected)
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "PREVIEW".into())
}

/// The PDF the user has asked to render after this selection, if any.
/// Selecting anything else forgets the request.
fn explicit_pdf_after(path: Option<&Path>, intent: Intent) -> Option<PathBuf> {
    path.filter(|path| intent == Intent::Explicit && file_preview::is_pdf(path))
        .map(Path::to_owned)
}

/// The folders that have to be open for `path` to show in a tree rooted at `root`, outermost
/// first, and whether the path is hidden (a dot name anywhere below the root). `None` for the
/// root itself and for anything outside it.
fn reveal_plan(root: &Path, path: &Path) -> Option<(Vec<PathBuf>, bool)> {
    let relative = path.strip_prefix(root).ok()?;
    let mut folders = Vec::new();
    let mut current = root.to_owned();
    let mut hidden = false;
    let mut parts = relative.components().peekable();
    parts.peek()?;
    while let Some(part) = parts.next() {
        let Component::Normal(name) = part else {
            return None;
        };
        hidden |= name.to_string_lossy().starts_with('.');
        current.push(name);
        if parts.peek().is_some() {
            folders.push(current.clone());
        }
    }
    Some((folders, hidden))
}

/// What a line number asked for does to the preview that is showing, or will.
#[derive(Debug, PartialEq, Eq)]
enum LineOutcome {
    /// The file is still loading.
    Wait,
    /// Scroll to this line (counted from 0).
    Show { index: usize },
    /// The file has fewer lines than that (or the preview stopped before it).
    PastEnd { last: usize, truncated: bool },
    /// This preview has no lines: a picture, a PDF, a message.
    NoLines,
    /// The preview failed; it says so itself.
    Failed,
}

fn line_outcome(preview: &PreviewState, line: usize) -> LineOutcome {
    match preview {
        PreviewState::Empty | PreviewState::Loading => LineOutcome::Wait,
        PreviewState::Error(_) => LineOutcome::Failed,
        PreviewState::Ready(PreviewContent::Text {
            lines, truncated, ..
        }) => {
            let last = lines.len().saturating_sub(1);
            if line > lines.len() {
                LineOutcome::PastEnd {
                    last,
                    truncated: *truncated,
                }
            } else {
                LineOutcome::Show {
                    index: line.saturating_sub(1),
                }
            }
        }
        PreviewState::PdfPending | PreviewState::Ready(_) => LineOutcome::NoLines,
    }
}

/// A file or folder a terminal link asked the tree to select, waiting for the listings that
/// show it.
struct PendingReveal {
    external_fallback: bool,
    path: PathBuf,
    line: Option<usize>,
    /// Folders asked to list themselves for this; the file cannot be called missing before
    /// they have answered.
    awaiting: HashSet<PathBuf>,
    started: Instant,
}

/// How long a terminal link may wait for the tree. A listing that never answers (the folder
/// went away, or the tree was refreshed under it) must not select the file much later.
const REVEAL_PATIENCE: Duration = Duration::from_secs(10);

enum PreviewPlan {
    Show(PreviewState),
    Load,
}

fn plan_preview(
    kind: EntryKind,
    identity: Option<FileIdentity>,
    path: &Path,
    explicit_pdf: Option<&Path>,
) -> PreviewPlan {
    let message =
        |text: &str| PreviewPlan::Show(PreviewState::Ready(PreviewContent::Message(text.into())));
    match kind {
        EntryKind::Directory => message("Expand this folder to browse its files."),
        EntryKind::Symlink => {
            message("Symbolic links cannot be previewed or opened here. Use Reveal instead.")
        }
        EntryKind::Other => message("This item cannot be previewed."),
        EntryKind::File if identity.is_none() => PreviewPlan::Show(PreviewState::Error(
            "Cannot verify this file. Refresh and try again.".into(),
        )),
        EntryKind::File if file_preview::is_pdf(path) && explicit_pdf != Some(path) => {
            PreviewPlan::Show(PreviewState::PdfPending)
        }
        EntryKind::File => PreviewPlan::Load,
    }
}

/// Extensions macOS runs, mounts or installs when opened, whatever their
/// permission bits say.
pub(crate) const LAUNCHER_EXTENSIONS: &[&str] = &[
    "app", "command", "tool", "terminal", "workflow", "action", "scpt", "scptd", "pkg", "mpkg",
    "dmg", "jar", "prefpane", "saver", "webloc", "inetloc",
];

/// Why the system `open` must not be handed this item, if it must not. The
/// explorer is a viewer: it opens documents and folders, but never launches
/// programs or follows a link out of the worktree.
fn open_refusal(root: &Path, path: &Path) -> Option<String> {
    let Ok(relative) = path.strip_prefix(root) else {
        return Some("This item is outside the selected worktree.".into());
    };
    // `open` follows links, so every ancestor is checked, not just the item.
    let mut current = root.to_owned();
    let mut metadata = None;
    for component in std::iter::once(None).chain(relative.components().map(Some)) {
        match component {
            None | Some(Component::CurDir) => {}
            Some(Component::Normal(name)) => current.push(name),
            Some(_) => return Some("This item is outside the selected worktree.".into()),
        }
        match fs::symlink_metadata(&current) {
            Ok(found) if found.file_type().is_symlink() => {
                return Some(
                    "Symbolic links are not opened externally. Use Reveal instead.".into(),
                );
            }
            Ok(found) => metadata = Some(found),
            Err(error) => return Some(format!("Cannot inspect this item: {error}")),
        }
    }
    let metadata = metadata?;
    let launcher = path
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            LAUNCHER_EXTENSIONS
                .iter()
                .any(|launcher| extension.eq_ignore_ascii_case(launcher))
        });
    if launcher {
        Some("Applications and scripts are not opened externally. Use Reveal instead.".into())
    } else if metadata.is_file() && metadata.mode() & 0o111 != 0 {
        Some("Executable files are not opened externally. Use Reveal instead.".into())
    } else if !metadata.is_file() && !metadata.is_dir() {
        Some("Only files and folders can be opened externally.".into())
    } else {
        None
    }
}

/// A message under the selected path. Refusals show in gold; a confirmation
/// that something was done shows in cyan and clears itself.
struct Notice {
    text: String,
    confirmation: bool,
}

impl Notice {
    fn refusal(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            confirmation: false,
        }
    }
}

const CONFIRMATION_LIFETIME: Duration = Duration::from_secs(4);

/// Why Copy Contents is unavailable for this entry, if it is. `preview` is
/// what the pane shows for this same file, once it has arrived; the read at
/// click time still decides, so a preview that is missing or stale never
/// allows anything the read would refuse.
fn copy_contents_refusal(
    kind: EntryKind,
    identity: Option<FileIdentity>,
    path: &Path,
    preview: Option<&PreviewState>,
) -> Option<&'static str> {
    match kind {
        EntryKind::File => {}
        EntryKind::Directory => return Some("Select a file to copy its contents"),
        EntryKind::Symlink => return Some("Symbolic links cannot be copied"),
        EntryKind::Other => return Some("Only regular files can be copied"),
    }
    let Some(identity) = identity else {
        return Some("Cannot verify this file. Refresh and try again.");
    };
    if let Some(reason) = file_preview::copy_block(path, identity) {
        return Some(reason);
    }
    // Images, PDFs and oversize files are ruled out above, so the only message
    // the preview shows for a regular file is its binary-content one.
    match preview {
        Some(PreviewState::Ready(PreviewContent::Message(_))) => Some("Binary file"),
        _ => None,
    }
}

/// "Copied 214 lines (6.1 KiB)" for the confirmation after a copy.
fn copy_summary(text: &str) -> String {
    let bytes = text.len() as u64;
    let size = if bytes < 1024 {
        format!("{bytes} B")
    } else {
        // Tenths of a KiB, rounded, so nearly 1 MiB reads as 1.0 MiB and not
        // 1024.0 KiB.
        let tenths = (bytes * 10 + 512) / 1024;
        if tenths < 10_240 {
            format!("{}.{} KiB", tenths / 10, tenths % 10)
        } else {
            let tenths = (bytes * 10 + 524_288) / 1_048_576;
            format!("{}.{} MiB", tenths / 10, tenths % 10)
        }
    };
    match text.lines().count() {
        0 => format!("Copied an empty file ({size})"),
        1 => format!("Copied 1 line ({size})"),
        lines => format!("Copied {lines} lines ({size})"),
    }
}

/// GPUI keeps an image's decoded atlas tiles until told to drop them.
/// Deferred so it also reaches the window whose update is running.
fn release_image(image: Arc<RenderImage>, cx: &mut App) {
    cx.defer(move |cx| cx.drop_image(image, None));
}

pub struct FileExplorer {
    root: Option<ExplorerRoot>,
    tree: TreeModel,
    selected: Option<PathBuf>,
    show_hidden: bool,
    filter: FilterInput,
    filter_state: Entity<InputState>,
    base_filter_state: Entity<InputState>,
    filter_synced: BTreeMap<gpui::EntityId, String>,
    filter_sync_events: BTreeMap<gpui::EntityId, usize>,
    filter_surfaces: BTreeMap<u64, (Entity<InputState>, Subscription)>,
    _input_subscription: Subscription,
    focus: FocusHandle,
    mode: Mode,
    scroll: UniformListScrollHandle,
    preview_scroll: UniformListScrollHandle,
    preview: PreviewState,
    /// The file `preview` shows. It trails `selected` while a replacement loads.
    preview_path: Option<PathBuf>,
    preview_loading: bool,
    /// Dropping this cancels a pending debounce or an unfinished load.
    preview_task: Option<Task<()>>,
    preview_request: u64,
    preview_identity: Option<FileIdentity>,
    preview_kind: Option<EntryKind>,
    /// The one PDF the user has asked to render.
    explicit_pdf: Option<PathBuf>,
    /// Why the last action was refused, or that it worked.
    notice: Option<Notice>,
    /// The Copy Contents read in flight. Starting another drops it.
    copy_task: Option<Task<()>>,
    /// Keys for the Preview pane. `focus` belongs to the Files pane, and `mode` says which
    /// control of the focused one is current.
    preview_focus: FocusHandle,
    generation: u64,
    next_request: u64,
    /// A link in a terminal that the tree has not finished showing.
    pending_reveal: Option<PendingReveal>,
    /// The line of the selected file a link named, until the preview can scroll to it.
    pending_line: Option<(PathBuf, usize)>,
    /// The line the preview scrolled to for a link, marked until another file is shown.
    highlight_line: Option<(PathBuf, usize)>,
}

#[derive(Clone)]
enum PreviewState {
    Empty,
    Loading,
    /// A PDF waiting for the user to ask for it.
    PdfPending,
    Ready(PreviewContent),
    Error(String),
}

impl PreviewState {
    fn render_image(&self) -> Option<&Arc<RenderImage>> {
        match self {
            Self::Ready(content) => content.render_image(),
            _ => None,
        }
    }
}

impl EventEmitter<FileExplorerEvent> for FileExplorer {}

impl FileExplorer {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe_global::<theme::Appearance>(|_, cx| cx.notify())
            .detach();
        // The preview toolbar swaps between words and glyphs with this setting.
        cx.observe_global::<Settings>(|_, cx| cx.notify()).detach();
        cx.on_release(|explorer, cx| {
            if let Some(image) = explorer.preview.render_image() {
                release_image(image.clone(), cx);
            }
        })
        .detach();
        let filter_state = text_input::single_line(
            "",
            if ui_text::is_native() {
                "Filter"
            } else {
                "Filter loaded files…"
            },
            window,
            cx,
        );
        let subscription = cx.subscribe_in(&filter_state, window, Self::filter_event);
        Self {
            base_filter_state: filter_state.clone(),
            filter_synced: BTreeMap::from([(filter_state.entity_id(), String::new())]),
            filter_sync_events: BTreeMap::new(),
            filter_state,
            filter_surfaces: BTreeMap::new(),
            _input_subscription: subscription,
            root: None,
            tree: TreeModel::default(),
            selected: None,
            show_hidden: false,
            filter: FilterInput::default(),
            focus: cx.focus_handle(),
            mode: Mode::Tree,
            scroll: UniformListScrollHandle::new(),
            preview_scroll: UniformListScrollHandle::new(),
            preview: PreviewState::Empty,
            preview_path: None,
            preview_loading: false,
            preview_task: None,
            preview_request: 0,
            preview_identity: None,
            preview_kind: None,
            explicit_pdf: None,
            notice: None,
            copy_task: None,
            preview_focus: cx.focus_handle(),
            generation: 0,
            next_request: 0,
            pending_reveal: None,
            pending_line: None,
            highlight_line: None,
        }
    }

    fn filter_event(
        &mut self,
        state: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.owns_filter(state) {
            return;
        }
        let id = state.entity_id();
        if matches!(event, InputEvent::Change)
            && let Some(count) = self.filter_sync_events.get_mut(&id)
            && *count > 0
        {
            *count -= 1;
            return;
        }
        match event {
            InputEvent::Change => {
                self.filter.text = state.read(cx).value().to_string();
                self.filter_synced.insert(id, self.filter.text.clone());
                let siblings: Vec<_> = self
                    .filter_surfaces
                    .values()
                    .map(|(input, _)| input.clone())
                    .collect();
                for input in siblings {
                    let other = input.entity_id();
                    if other != id {
                        let previous = self.filter_synced.get(&other).cloned().unwrap_or_default();
                        if crate::form_input::sync_value(
                            &input,
                            &previous,
                            &self.filter.text,
                            window,
                            cx,
                        ) {
                            *self.filter_sync_events.entry(other).or_default() += 1;
                            self.filter_synced.insert(other, self.filter.text.clone());
                        }
                    }
                }
                self.ensure_selection(cx);
                cx.notify();
            }
            InputEvent::Focus => {
                if !state.read(cx).focus_handle(cx).is_focused(window) {
                    return;
                }
                self.filter_state = state.clone();
                if !crate::form_input::is_composing(state, window, cx) {
                    self.filter.text = state.read(cx).value().to_string();
                    self.filter_synced.insert(id, self.filter.text.clone());
                    self.ensure_selection(cx);
                }
                self.mode = Mode::Search;
                cx.notify();
            }
            _ if text_input::is_submit(event, EnterBehavior::Submit) => {
                self.filter.text = state.read(cx).value().to_string();
                self.filter_synced.insert(id, self.filter.text.clone());
                self.mode = Mode::Tree;
                self.ensure_selection(cx);
                self.focus.focus(window, cx);
                cx.notify();
            }
            _ => {}
        }
    }

    /// Create tab geometry once at the Workspace's tab/layout boundary.
    pub fn surface(
        &mut self,
        id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<FileExplorerSurface> {
        let input = if let Some((input, _)) = self.filter_surfaces.get(&id) {
            input.clone()
        } else {
            let input = text_input::single_line(
                self.filter.text.clone(),
                if ui_text::is_native() {
                    "Filter"
                } else {
                    "Filter loaded files…"
                },
                window,
                cx,
            );
            let subscription = cx.subscribe_in(&input, window, Self::filter_event);
            self.filter_synced
                .insert(input.entity_id(), self.filter.text.clone());
            self.filter_surfaces
                .insert(id, (input.clone(), subscription));
            input
        };
        let explorer = cx.entity();
        cx.new(|cx| {
            cx.observe(&explorer, |_, _, cx| cx.notify()).detach();
            FileExplorerSurface { explorer, input }
        })
    }

    fn owns_filter(&self, state: &Entity<InputState>) -> bool {
        (self.filter_surfaces.is_empty() && state.entity_id() == self.base_filter_state.entity_id())
            || self
                .filter_surfaces
                .values()
                .any(|(input, _)| input.entity_id() == state.entity_id())
    }

    pub fn retain_surfaces(&mut self, ids: &[u64], cx: &mut Context<Self>) {
        self.filter_surfaces.retain(|id, _| ids.contains(id));
        let live: Vec<_> = self
            .filter_surfaces
            .values()
            .map(|(input, _)| input.entity_id())
            .chain(std::iter::once(self.base_filter_state.entity_id()))
            .collect();
        self.filter_synced.retain(|id, _| live.contains(id));
        self.filter_sync_events.retain(|id, _| live.contains(id));
        if !self.owns_filter(&self.filter_state) {
            self.filter_state = self
                .filter_surfaces
                .values()
                .next()
                .map(|(input, _)| input.clone())
                .unwrap_or_else(|| self.base_filter_state.clone());
            if self.mode == Mode::Search {
                self.mode = Mode::Tree;
            }
        }
        cx.notify();
    }

    fn clear_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter = FilterInput::default();
        let inputs: Vec<_> = self
            .filter_surfaces
            .values()
            .map(|(input, _)| input.clone())
            .chain(std::iter::once(self.base_filter_state.clone()))
            .collect();
        for input in inputs {
            crate::form_input::set_value(&input, String::new(), window, cx);
            self.filter_synced.insert(input.entity_id(), String::new());
        }
    }

    pub fn set_root(
        &mut self,
        root: Option<ExplorerRoot>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.root == root {
            return;
        }
        if self.root.as_ref().map(|root| &root.path) == root.as_ref().map(|root| &root.path) {
            self.root = root;
            cx.notify();
            return;
        }
        self.root = root;
        self.generation = self.generation.wrapping_add(1);
        self.tree.clear();
        self.selected = None;
        self.pending_reveal = None;
        self.pending_line = None;
        self.highlight_line = None;
        self.explicit_pdf = None;
        self.notice = None;
        self.preview_request = self.preview_request.wrapping_add(1);
        self.preview_task = None;
        self.preview_loading = false;
        self.preview_identity = None;
        self.preview_kind = None;
        self.set_preview(PreviewState::Empty, None, cx);
        self.clear_filter(window, cx);
        self.scroll = UniformListScrollHandle::new();
        if let Some(root) = &self.root {
            self.load(root.path.clone(), cx);
        }
        cx.notify();
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        // A slow/network filesystem must not be restarted by every refresh tick.
        if self.tree.directories().values().any(|state| state.loading) {
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        if let Some(root) = &self.root {
            self.load(root.path.clone(), cx);
        }
    }

    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = Mode::Tree;
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Give the keys to the Preview pane.
    pub fn focus_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = Mode::Preview;
        self.preview_focus.focus(window, cx);
        cx.notify();
    }

    /// The focus handle of the pane that holds the control `mode` stands for.
    fn focus_of(&self, mode: Mode) -> &FocusHandle {
        if mode.in_preview_pane() {
            &self.preview_focus
        } else {
            &self.focus
        }
    }

    /// Whether `mode` is the current control of the pane that has the keys.
    fn is_current(&self, mode: Mode, window: &Window, cx: &App) -> bool {
        self.mode == mode
            && if mode == Mode::Search {
                self.filter_state
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window)
            } else {
                self.focus_of(mode).is_focused(window)
            }
    }

    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = Mode::Search;
        self.filter_state
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }

    fn load(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self
            .tree
            .directories()
            .get(&path)
            .is_some_and(|state| state.loading)
        {
            return;
        }
        let Some(root) = &self.root else { return };
        if !path.starts_with(&root.path) {
            return;
        }
        self.next_request = self.next_request.wrapping_add(1);
        let request = self.next_request;
        let generation = self.generation;
        let root_path = root.path.clone();
        if self.tree.begin_load(&path, request) {
            cx.notify();
        }
        let work_path = path.clone();
        let work = cx
            .background_executor()
            .spawn(async move { read_directory_in(&root_path, &work_path) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |explorer, cx| {
                if explorer.generation != generation
                    || explorer
                        .tree
                        .directories()
                        .get(&path)
                        .is_none_or(|state| state.request != request)
                {
                    return;
                }
                let outcome = explorer.tree.finish_load(&path, request, result);
                for path in outcome.reload {
                    explorer.load(path, cx);
                }
                explorer.ensure_selection(cx);
                explorer.reload_preview_if_changed(&path, cx);
                explorer.continue_reveal(Some(&path), cx);
                if outcome.changed {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn rows(&self) -> Rc<Vec<TreeRow>> {
        match &self.root {
            Some(root) => self
                .tree
                .rows(&root.path, self.show_hidden, &self.filter.text),
            None => Rc::default(),
        }
    }

    fn ensure_selection(&mut self, cx: &mut Context<Self>) {
        let rows = self.rows();
        if !rows.iter().any(|row| {
            matches!(row.kind, RowKind::Entry(_)) && Some(&row.path) == self.selected.as_ref()
        }) {
            let selected = rows
                .iter()
                .find(|row| matches!(row.kind, RowKind::Entry(_)));
            self.select_row(selected, Intent::Auto, cx);
        }
    }

    fn select_row(&mut self, row: Option<&TreeRow>, intent: Intent, cx: &mut Context<Self>) {
        self.select_row_announcing(row, intent, true, cx);
    }

    /// Select a row. `announce` says whether choosing a file also tells the window to show its
    /// preview as a click in the tree does (`Selected`); a link announces itself (`Revealed`)
    /// because the tree it was chosen from may not be on screen.
    fn select_row_announcing(
        &mut self,
        row: Option<&TreeRow>,
        intent: Intent,
        announce: bool,
        cx: &mut Context<Self>,
    ) {
        let path = row.map(|row| row.path.clone());
        let reveals = announce && reveals_preview(intent, row.map(|row| row.kind));
        if self.selected == path {
            // Clicking the selected PDF is how its placeholder is dismissed.
            if intent == Intent::Explicit {
                self.open_pdf_preview(cx);
                // Choosing the same file again also brings back a preview that was closed.
                if reveals {
                    cx.emit(FileExplorerEvent::Selected);
                }
            }
            return;
        }
        self.notice = None;
        if self.pending_line.as_ref().map(|(line_path, _)| line_path) != path.as_ref() {
            self.pending_line = None;
        }
        self.explicit_pdf = explicit_pdf_after(path.as_deref(), intent);
        self.selected = path;
        self.reload_preview(intent.delay(), cx);
        if reveals {
            cx.emit(FileExplorerEvent::Selected);
        }
    }

    /// Refresh the preview when the selected entry's listing no longer matches
    /// what the preview was loaded from.
    fn reload_preview_if_changed(&mut self, listed: &Path, cx: &mut Context<Self>) {
        let Some(selected) = &self.selected else {
            return;
        };
        if selected.parent() != Some(listed) {
            return;
        }
        let current =
            entry_of(&self.rows(), selected).map(|(kind, identity)| (identity, Some(kind)));
        if current != Some((self.preview_identity, self.preview_kind)) {
            self.reload_preview(Duration::ZERO, cx);
        }
    }

    fn reload_preview(&mut self, delay: Duration, cx: &mut Context<Self>) {
        // Dropping the previous task cancels its debounce or unfinished load.
        self.preview_task = None;
        self.preview_loading = false;
        self.preview_request = self.preview_request.wrapping_add(1);
        let request = self.preview_request;
        self.preview_identity = None;
        self.preview_kind = None;
        let entry = self
            .selected
            .as_ref()
            .and_then(|selected| entry_of(&self.rows(), selected));
        let root_path = self.root.as_ref().map(|root| root.path.clone());
        let (Some(root_path), Some(path), Some((kind, identity))) =
            (root_path, self.selected.clone(), entry)
        else {
            self.set_preview(PreviewState::Empty, None, cx);
            return;
        };
        self.preview_identity = identity;
        self.preview_kind = Some(kind);
        match plan_preview(kind, identity, &path, self.explicit_pdf.as_deref()) {
            PreviewPlan::Show(state) => {
                self.set_preview(state, Some(path), cx);
                return;
            }
            PreviewPlan::Load => {}
        }
        // Whatever is on screen stays until its replacement is ready, so a file
        // an agent keeps rewriting neither flashes "Loading…" nor loses its
        // scroll position.
        if matches!(self.preview, PreviewState::Empty | PreviewState::Loading) {
            self.set_preview(PreviewState::Loading, Some(path.clone()), cx);
        }
        self.preview_loading = true;
        self.preview_task = Some(cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let load_path = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { file_preview::load(&root_path, &load_path, identity) })
                .await;
            let _ = this.update(cx, |explorer, cx| {
                if explorer.preview_request != request || explorer.selected.as_ref() != Some(&path)
                {
                    return;
                }
                explorer.preview_loading = false;
                let state = match result {
                    Ok(content) => PreviewState::Ready(content),
                    Err(error) => PreviewState::Error(error),
                };
                explorer.set_preview(state, Some(path), cx);
            });
        }));
        cx.notify();
    }

    /// Replace what the preview shows and release the bitmap it held.
    fn set_preview(&mut self, state: PreviewState, path: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.preview_path != path {
            self.preview_scroll = UniformListScrollHandle::new();
            self.highlight_line = None;
        }
        self.preview_path = path;
        let old = std::mem::replace(&mut self.preview, state);
        if let Some(image) = old.render_image() {
            release_image(image.clone(), cx);
        }
        self.apply_pending_line(cx);
        cx.notify();
    }

    /// Select `path`, a file or folder under the root, the way a click would: open the folders
    /// above it, clear a filter that would hide it, show hidden files if it is one, and select
    /// it once the listings that hold it have arrived. A file named by a terminal link also
    /// scrolls its preview to `line`. Folders listed long ago are listed again, because the
    /// file may be one the agent just wrote.
    pub fn reveal(
        &mut self,
        path: &Path,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.reveal_inner(path, line, true, window, cx)
    }

    /// Chat links always stay inside the read-only preview, even if listings change.
    pub fn reveal_read_only(
        &mut self,
        path: &Path,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.reveal_inner(path, line, false, window, cx)
    }

    fn reveal_inner(
        &mut self,
        path: &Path,
        line: Option<u32>,
        external_fallback: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let root = self
            .root
            .as_ref()
            .map(|root| root.path.clone())
            .ok_or("No folder is open in Files.")?;
        let (folders, hidden) = reveal_plan(&root, path)
            .ok_or("That is not inside the folder Files shows.".to_owned())?;
        if hidden {
            self.show_hidden = true;
        }
        self.clear_filter(window, cx);
        if self.mode == Mode::Search {
            self.mode = Mode::Tree;
        }
        let mut awaiting = HashSet::new();
        for folder in std::iter::once(root.clone()).chain(folders) {
            if folder != root {
                self.tree.expanded_mut().insert(folder.clone());
            }
            self.load(folder.clone(), cx);
            awaiting.insert(folder);
        }
        self.pending_reveal = Some(PendingReveal {
            external_fallback,
            path: path.to_owned(),
            line: line.map(|line| line as usize),
            awaiting,
            started: Instant::now(),
        });
        self.continue_reveal(None, cx);
        cx.notify();
        Ok(())
    }

    /// Try to finish a reveal: after it was asked for, and after each listing that arrives.
    /// `listed` is the folder whose listing just arrived.
    fn continue_reveal(&mut self, listed: Option<&Path>, cx: &mut Context<Self>) {
        let Some(pending) = &mut self.pending_reveal else {
            return;
        };
        if pending.started.elapsed() > REVEAL_PATIENCE {
            self.pending_reveal = None;
            return;
        }
        if let Some(listed) = listed {
            pending.awaiting.remove(listed);
        }
        let target = pending.path.clone();
        let rows = self.rows();
        let found = rows
            .iter()
            .find(|row| matches!(row.kind, RowKind::Entry(_)) && row.path == target);
        if let Some(row) = found {
            let line = self.pending_reveal.take().and_then(|pending| pending.line);
            self.select_for_link(row, line, cx);
        } else if self
            .pending_reveal
            .as_ref()
            .is_some_and(|pending| pending.awaiting.is_empty())
        {
            // Every folder above it has answered and it is not there: gone, or ignored.
            let external = self
                .pending_reveal
                .take()
                .is_some_and(|pending| pending.external_fallback);
            if external {
                cx.emit(FileExplorerEvent::NotListed(target));
            } else {
                cx.emit(FileExplorerEvent::RevealFailed(format!(
                    "Cannot preview {}: the file is missing or not listed in Files.",
                    target.display()
                )));
            }
            cx.notify();
        }
    }

    fn select_for_link(&mut self, row: &TreeRow, line: Option<usize>, cx: &mut Context<Self>) {
        if let Some(index) = self.rows().iter().position(|other| other.path == row.path) {
            self.scroll.scroll_to_item(index, ScrollStrategy::Center);
        }
        self.pending_line = line
            .filter(|_| {
                matches!(
                    row.kind,
                    RowKind::Entry(EntryKind::File | EntryKind::Symlink)
                )
            })
            .map(|line| (row.path.clone(), line));
        self.highlight_line = None;
        self.select_row_announcing(Some(row), Intent::Explicit, false, cx);
        // The file may already have been the selection, and shown.
        self.apply_pending_line(cx);
        if matches!(
            row.kind,
            RowKind::Entry(EntryKind::File | EntryKind::Symlink)
        ) {
            cx.emit(FileExplorerEvent::Revealed);
        }
    }

    /// Scroll the preview to the line a link named, once the file is on screen.
    fn apply_pending_line(&mut self, cx: &mut Context<Self>) {
        let Some((path, line)) = self.pending_line.clone() else {
            return;
        };
        if self.preview_path.as_ref() != Some(&path) {
            return;
        }
        match line_outcome(&self.preview, line) {
            LineOutcome::Wait => return,
            LineOutcome::Show { index } => {
                self.preview_scroll
                    .scroll_to_item(index, ScrollStrategy::Center);
                self.highlight_line = Some((path, index));
            }
            LineOutcome::PastEnd { last, truncated } => {
                self.preview_scroll
                    .scroll_to_item(last, ScrollStrategy::Center);
                self.notice = Some(Notice::refusal(if truncated {
                    format!("Line {line} is past the end of this preview, which stops early.")
                } else {
                    format!("Line {line} is past the end of this file.")
                }));
            }
            LineOutcome::NoLines => {
                self.notice = Some(Notice::refusal(format!(
                    "Line {line} cannot be shown: this preview has no lines."
                )));
            }
            LineOutcome::Failed => {}
        }
        self.pending_line = None;
        cx.notify();
    }

    fn open_pdf_preview(&mut self, cx: &mut Context<Self>) {
        let Some(selected) = &self.selected else {
            return;
        };
        if !file_preview::is_pdf(selected) || self.explicit_pdf.as_ref() == Some(selected) {
            return;
        }
        self.explicit_pdf = Some(selected.clone());
        self.reload_preview(Duration::ZERO, cx);
    }

    fn pdf_page(&mut self, page: usize, cx: &mut Context<Self>) {
        let PreviewState::Ready(PreviewContent::Pdf { bytes, pages, .. }) = &self.preview else {
            return;
        };
        if page == 0 || page > *pages {
            return;
        }
        // A reload of this file is pending; paging would cancel it.
        if self.preview_loading {
            return;
        }
        let bytes = bytes.clone();
        let pages = *pages;
        self.preview_request = self.preview_request.wrapping_add(1);
        let request = self.preview_request;
        let selected = self.selected.clone();
        // The current page stays up until the next one is rendered.
        let work = cx.background_executor().spawn(async move {
            file_preview::render_pdf(&bytes, page).map(|(image, _)| (bytes, image))
        });
        self.preview_task = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |explorer, cx| {
                if explorer.preview_request != request || explorer.selected != selected {
                    return;
                }
                let state = match result {
                    Ok((bytes, image)) => PreviewState::Ready(PreviewContent::Pdf {
                        bytes,
                        image,
                        page,
                        pages,
                    }),
                    Err(error) => PreviewState::Error(error),
                };
                let path = explorer.preview_path.clone();
                explorer.set_preview(state, path, cx);
            });
        }));
        cx.notify();
    }

    fn toggle(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.tree.expanded().contains(path) {
            self.tree.expanded_mut().remove(path);
            self.ensure_selection(cx);
        } else {
            self.tree.expanded_mut().insert(path.to_owned());
            // Collapsed folders are not polled, so a cached listing may be stale.
            self.load(path.to_owned(), cx);
        }
        cx.notify();
    }

    fn activate(&mut self, row: &TreeRow, cx: &mut Context<Self>) {
        match row.kind {
            RowKind::Entry(EntryKind::Directory) => self.toggle(&row.path, cx),
            RowKind::Entry(_) => self.select_row(Some(row), Intent::Explicit, cx),
            RowKind::Error => self.load(row.path.clone(), cx),
            RowKind::Status => {}
        }
    }

    fn selected_editable(&self) -> Option<(PathBuf, FileIdentity)> {
        let selected = self.selected.as_ref()?;
        self.rows()
            .iter()
            .find(|row| &row.path == selected && row.kind == RowKind::Entry(EntryKind::File))
            .and_then(|row| row.identity.map(|identity| (row.path.clone(), identity)))
    }

    fn copy_contents_refusal(&self) -> Option<&'static str> {
        let Some(selected) = &self.selected else {
            return Some("Select a file to copy its contents");
        };
        let Some((kind, identity)) = entry_of(&self.rows(), selected) else {
            return Some("Select a file to copy its contents");
        };
        // A preview still loading for another file says nothing about this one.
        let preview = (self.preview_path.as_ref() == Some(selected)).then_some(&self.preview);
        copy_contents_refusal(kind, identity, selected, preview)
    }

    /// Copy the selected file's full text, not the clipped preview lines. The
    /// file is read again off the UI thread, and only if it is still the one
    /// that was selected.
    fn copy_contents(&mut self, cx: &mut Context<Self>) {
        if let Some(reason) = self.copy_contents_refusal() {
            self.notice = Some(Notice::refusal(reason));
            return;
        }
        let (Some(root), Some((path, identity))) = (&self.root, self.selected_editable()) else {
            return;
        };
        let root_path = root.path.clone();
        let (read_root, read_path) = (root_path.clone(), path.clone());
        let work = cx
            .background_executor()
            .spawn(async move { file_preview::read_text(&read_root, &read_path, identity) });
        self.copy_task = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            let confirmed = this
                .update(cx, |explorer, cx| {
                    // The user moved on; copying now would surprise them.
                    if explorer.root.as_ref().map(|root| &root.path) != Some(&root_path)
                        || explorer.selected.as_ref() != Some(&path)
                    {
                        return false;
                    }
                    let confirmed = result.is_ok();
                    explorer.notice = Some(match result {
                        Ok(text) => {
                            let summary = copy_summary(&text);
                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                            Notice {
                                text: summary,
                                confirmation: true,
                            }
                        }
                        Err(error) => Notice::refusal(error),
                    });
                    cx.notify();
                    confirmed
                })
                .unwrap_or(false);
            if confirmed {
                cx.background_executor().timer(CONFIRMATION_LIFETIME).await;
                let _ = this.update(cx, |explorer, cx| {
                    if explorer
                        .notice
                        .as_ref()
                        .is_some_and(|notice| notice.confirmation)
                    {
                        explorer.notice = None;
                        cx.notify();
                    }
                });
            }
        }));
    }

    fn action(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.notice = None;
        match mode {
            Mode::Refresh => {
                self.refresh(cx);
                self.reload_preview(Duration::ZERO, cx);
            }
            Mode::Hidden => {
                self.show_hidden = !self.show_hidden;
                self.ensure_selection(cx);
            }
            Mode::RevealRoot => {
                if let Some(root) = &self.root {
                    cx.emit(FileExplorerEvent::Reveal(root.path.clone()));
                }
            }
            Mode::Edit => {
                if let (Some(root), Some((path, identity))) = (&self.root, self.selected_editable())
                {
                    cx.emit(FileExplorerEvent::Edit {
                        root: root.clone(),
                        path,
                        identity,
                    });
                }
            }
            Mode::Open => {
                if let (Some(root), Some(path)) = (&self.root, &self.selected) {
                    match open_refusal(&root.path, path) {
                        Some(reason) => self.notice = Some(Notice::refusal(reason)),
                        None => cx.emit(FileExplorerEvent::Open(path.clone())),
                    }
                }
            }
            Mode::CopyContents => self.copy_contents(cx),
            Mode::Reveal | Mode::Copy => {
                if let Some(path) = &self.selected {
                    match mode {
                        Mode::Reveal => cx.emit(FileExplorerEvent::Reveal(path.clone())),
                        Mode::Copy => {
                            if let Some(relative) = self
                                .root
                                .as_ref()
                                .and_then(|root| path.strip_prefix(&root.path).ok())
                            {
                                cx.emit(FileExplorerEvent::CopyRelativePath(relative.to_owned()));
                            }
                        }
                        _ => {}
                    }
                }
            }
            Mode::Preview => {
                if matches!(self.preview, PreviewState::PdfPending) {
                    self.open_pdf_preview(cx);
                }
            }
            _ => {}
        }
        cx.notify();
    }

    /// Keys pressed in the Files pane.
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.handle_key(event, false, window, cx);
    }

    /// Keys pressed in the Preview pane.
    fn preview_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.handle_key(event, true, window, cx);
    }

    fn handle_key(
        &mut self,
        event: &KeyDownEvent,
        in_preview: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.mode == Mode::Search
            && matches!(event.keystroke.key.as_str(), "escape" | "tab" | "down")
            && crate::form_input::is_composing(&self.filter_state, window, cx)
        {
            return;
        }
        // A click on empty space gives a pane the keys without choosing one of its controls.
        if self.mode.in_preview_pane() != in_preview {
            self.mode = if in_preview {
                Mode::Preview
            } else {
                Mode::Tree
            };
        }
        let key = event.keystroke.key.as_str();
        let platform = event.keystroke.modifiers.platform;
        if platform && key == "f" && !in_preview {
            self.focus_search(window, cx);
        } else if platform && key == "r" {
            self.refresh(cx);
        } else if platform && key == "." {
            self.action(Mode::Hidden, cx);
        } else if platform && key == "o" && self.mode != Mode::Search {
            self.action(Mode::Open, cx);
        } else if platform && key == "e" && self.mode != Mode::Search {
            self.action(Mode::Edit, cx);
        } else if key == "tab" {
            let order = self.mode.focus_order();
            let index = order
                .iter()
                .position(|mode| *mode == self.mode)
                .unwrap_or(0);
            let step = if event.keystroke.modifiers.shift {
                order.len() - 1
            } else {
                1
            };
            self.mode = order[(index + step) % order.len()];
        } else if key == "escape" && self.mode == Mode::Search {
            self.clear_filter(window, cx);
            self.mode = Mode::Tree;
            self.ensure_selection(cx);
        } else if self.mode == Mode::Search {
            if key == "down" {
                self.mode = Mode::Tree;
                self.ensure_selection(cx);
            } else {
                return;
            }
        } else if self.mode == Mode::Tree {
            let rows = self.rows();
            let navigable = rows
                .iter()
                .enumerate()
                .filter(|(_, row)| matches!(row.kind, RowKind::Entry(_)))
                .collect::<Vec<_>>();
            let index = navigable
                .iter()
                .position(|(_, row)| Some(&row.path) == self.selected.as_ref())
                .unwrap_or(0);
            match key {
                "up" | "down" | "home" | "end" if !navigable.is_empty() => {
                    let index = match key {
                        "up" => index.saturating_sub(1),
                        "down" => (index + 1).min(navigable.len() - 1),
                        "home" => 0,
                        _ => navigable.len() - 1,
                    };
                    self.select_row(Some(navigable[index].1), Intent::Passive, cx);
                    self.scroll
                        .scroll_to_item(navigable[index].0, ScrollStrategy::Nearest);
                }
                "enter" | "space" => {
                    if let Some((_, row)) = navigable.get(index) {
                        // Enter renders a PDF that was only selected; once it is
                        // showing, Enter edits it like any other file.
                        let unopened_pdf = file_preview::is_pdf(&row.path)
                            && self.explicit_pdf.as_ref() != Some(&row.path);
                        if key == "enter"
                            && row.kind == RowKind::Entry(EntryKind::File)
                            && !unopened_pdf
                        {
                            self.action(Mode::Edit, cx);
                        } else {
                            self.activate(row, cx);
                        }
                    }
                }
                "right" => {
                    if let Some((_, row)) = navigable.get(index) {
                        if row.kind == RowKind::Entry(EntryKind::Directory) {
                            if !self.tree.expanded().contains(&row.path) {
                                self.toggle(&row.path, cx);
                            } else if let Some((row_index, child)) = navigable
                                .get(index + 1)
                                .filter(|(_, child)| child.depth > row.depth)
                            {
                                self.select_row(Some(child), Intent::Passive, cx);
                                self.scroll
                                    .scroll_to_item(*row_index, ScrollStrategy::Nearest);
                            }
                        }
                    }
                }
                "left" => {
                    if let Some((_, row)) = navigable.get(index) {
                        if self.tree.expanded().contains(&row.path) {
                            self.toggle(&row.path, cx);
                        } else if let Some(parent) = row
                            .path
                            .parent()
                            .filter(|parent| navigable.iter().any(|(_, row)| row.path == *parent))
                        {
                            let parent_row = navigable
                                .iter()
                                .find(|(_, row)| row.path == parent)
                                .map(|(_, row)| *row);
                            self.select_row(parent_row, Intent::Passive, cx);
                        }
                    }
                }
                "c" if platform => self.action(Mode::Copy, cx),
                _ => return,
            }
        } else if self.mode == Mode::Preview && matches!(key, "left" | "right") {
            if let PreviewState::Ready(PreviewContent::Pdf { page, .. }) = &self.preview {
                self.pdf_page(
                    if key == "left" {
                        page.saturating_sub(1)
                    } else {
                        page + 1
                    },
                    cx,
                );
            }
        } else if key == "enter" || key == "space" {
            self.action(self.mode, cx);
        } else {
            return;
        }
        if self.mode == Mode::Search {
            self.filter_state
                .read(cx)
                .focus_handle(cx)
                .focus(window, cx);
        } else {
            self.focus_of(self.mode).focus(window, cx);
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn button(
        &self,
        id: &'static str,
        label: &str,
        mode: Mode,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.button_face(id, Face::Text(label), mode, window, cx)
    }

    fn toolbar_button(
        &self,
        action: &ToolbarAction,
        as_icon: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let face = if as_icon {
            Face::Glyph(action.glyph, action.name)
        } else {
            Face::Text(action.label)
        };
        self.button_face(action.id, face, action.mode, window, cx)
    }

    fn button_face(
        &self,
        id: &'static str,
        face: Face,
        mode: Mode,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme::palette(cx);
        let active = self.is_current(mode, window, cx);
        let available = match mode {
            Mode::Edit => self.selected_editable().is_some(),
            Mode::CopyContents => self.copy_contents_refusal().is_none(),
            Mode::Copy | Mode::Reveal | Mode::Open => self.selected.is_some(),
            _ => self.root.is_some(),
        };
        let color = if active {
            colors.focus
        } else if available {
            colors.cyan
        } else {
            colors.muted
        };
        // Copy Contents has limits the label cannot show, so it always explains.
        let explanation = (mode == Mode::CopyContents)
            .then(|| self.copy_contents_refusal().unwrap_or(COPY_CONTENTS_HINT));
        let click = move |view: &mut Self,
                          _: &gpui::ClickEvent,
                          window: &mut Window,
                          cx: &mut Context<Self>| {
            // The preview panel also takes clicks to focus itself; this button
            // owns its own focus state.
            cx.stop_propagation();
            view.mode = mode;
            let focus = view.focus_of(mode).clone();
            focus.focus(window, cx);
            // A disabled Copy Contents says why on click, as it does on
            // Enter, since a tooltip needs a hover.
            if available || mode == Mode::CopyContents {
                view.action(mode, cx);
            }
            cx.notify();
        };
        // Native: every button is a bare symbol in the header, named by its tooltip; the
        // one the keyboard is on keeps a ring, and Hidden is filled while it is on.
        if ui_text::is_native() {
            let (symbol, name) = native_face(mode, &face, self.show_hidden);
            let detail = explanation.or_else(|| tooltip_hint(mode).filter(|_| available));
            let button = controls::toolbar_button(
                id,
                symbol,
                icon_tooltip(name, detail),
                available || mode == Mode::CopyContents,
                colors,
            );
            let button = if mode == Mode::Hidden && self.show_hidden {
                controls::toolbar_button_on(button, colors)
            } else {
                button
            };
            return button
                .border_1()
                .border_color(if active {
                    rgb(colors.focus).into()
                } else {
                    gpui::transparent_black()
                })
                .on_click(cx.listener(click))
                .into_any_element();
        }
        let button = div()
            .id(id)
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .border_1()
            .border_color(rgb(if active { colors.focus } else { colors.divider }))
            .text_color(rgb(color));
        // Native: capsule buttons in the system face; the one in use keeps its ring.
        let native = ui_text::is_native();
        let button = controls::native(button, |button| {
            let kind = if available {
                controls::Button::Secondary
            } else {
                controls::Button::Disabled
            };
            let button = controls::button(button, kind, colors)
                .font_family(ui_text::ui_family())
                .hover(move |style| style.bg(rgb(kind.hover(colors))));
            if active {
                button.border_color(rgb(colors.focus))
            } else {
                button
            }
        });
        let button = match face {
            Face::Text(label) => {
                let button = button
                    .px(ui_text::space(if native { 10.0 } else { 7.0 }))
                    .py(ui_text::space(if native { 4.0 } else { 5.0 }))
                    .child(ui_text::cased(label.to_owned()));
                match explanation {
                    Some(text) => button.child(tooltip::anchor(text, Look::Control)),
                    None => button,
                }
            }
            // Square, so the hit target stays at least 22 px; the glyph takes the text colour.
            Face::Glyph(glyph, name) => {
                let detail = explanation.or_else(|| tooltip_hint(mode).filter(|_| available));
                let tooltip: SharedString = icon_tooltip(name, detail).into();
                button
                    .size(ui_text::space(24.0))
                    .when(native, |button| button.px_0().w(ui_text::space(28.0)))
                    .child(icons::icon(Icon::Action(glyph), color))
                    .child(tooltip::anchor(tooltip, Look::Control))
            }
        };
        button.on_click(cx.listener(click)).into_any_element()
    }

    /// The file actions, right of the file name. They drop to their own line only
    /// once the name is down to a stub, and wrap among themselves only when even
    /// that line is too narrow.
    fn preview_toolbar(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        // Native draws the file's actions as symbols in the header, as Quick Look does.
        let as_icons = icons::labels_as_icons(cx) || ui_text::is_native();
        div()
            .id("file-preview-actions")
            .ml_auto()
            .flex()
            .flex_wrap()
            .gap(ui_text::space(4.0))
            .text_size(ui_text::text(9.0))
            .children(
                TOOLBAR
                    .iter()
                    .map(|action| self.toolbar_button(action, as_icons, window, cx)),
            )
            .into_any_element()
    }

    fn search(
        &self,
        input: &Entity<InputState>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        crate::form_input::plain_frame("file-explorer-filter", input, false, window, cx)
            .when(ui_text::is_native(), |frame| frame.rounded_full())
            .into_any_element()
    }

    fn row(
        &self,
        row: &TreeRow,
        index: usize,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme::palette(cx);
        let selected =
            matches!(row.kind, RowKind::Entry(_)) && self.selected.as_ref() == Some(&row.path);
        let expanded =
            self.tree.expanded().contains(&row.path) || !self.filter.text.trim().is_empty();
        let icon = match row.kind {
            RowKind::Entry(EntryKind::Directory) if expanded => "▾",
            RowKind::Entry(EntryKind::Directory) => "▸",
            RowKind::Entry(EntryKind::Symlink) => "↗",
            RowKind::Entry(_) => "·",
            RowKind::Error => "!",
            RowKind::Status => "…",
        };
        let label = row.label.clone();
        let depth = row.depth;
        let clicked_row = row.clone();
        let color = match row.kind {
            RowKind::Error => colors.gold,
            RowKind::Status => colors.muted,
            RowKind::Entry(EntryKind::Directory) => colors.cyan,
            RowKind::Entry(EntryKind::Symlink) => colors.magenta,
            _ => colors.text,
        };
        let row = div()
            .id(("file-explorer-row", index))
            .h(ui_text::space(27.0))
            .w_full()
            .pl(px(8.0 + row.depth as f32 * 14.0))
            .pr(ui_text::space(8.0))
            .flex()
            .items_center()
            .gap(ui_text::space(6.0))
            .bg(rgb(if selected {
                colors.panel_active
            } else {
                colors.panel
            }))
            .border_l_1()
            .border_color(rgb(if selected {
                if self.mode == Mode::Tree && self.focus.is_focused(window) {
                    colors.focus
                } else {
                    colors.cyan
                }
            } else {
                colors.panel
            }))
            .hover(move |style| {
                controls::hovered(style, controls::row_hover(selected, colors), |style| {
                    style.bg(rgb(colors.panel_active))
                })
            })
            .map(|row| {
                // Native: an inset rounded row, and the focus shown by a ring.
                controls::native(row, |row| {
                    controls::list_row(row, selected, colors)
                        .h(ui_text::space(24.0))
                        .pl(px(ui_text::space_f32(
                            controls::PANEL_INSET - controls::LIST_MARGIN,
                        ) + depth as f32 * 14.0))
                        // The focused list's selection is a deeper fill, not a ring.
                        .when(
                            selected && self.mode == Mode::Tree && self.focus.is_focused(window),
                            |row| {
                                row.border_color(gpui::transparent_black())
                                    .bg(rgb(colors.divider))
                            },
                        )
                })
            })
            .child(
                div()
                    .w(ui_text::space(10.0))
                    .flex_none()
                    .flex()
                    .justify_center()
                    .text_color(rgb(color))
                    .child(icons::mark(icon, 8.0, colors.muted)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_color(rgb(color))
                    .child(label),
            )
            .children((row.kind == RowKind::Entry(EntryKind::Symlink)).then(|| {
                div()
                    .text_size(ui_text::text(8.0))
                    .text_color(rgb(colors.muted))
                    .child(ui_text::cased("Link"))
            }))
            .on_click(cx.listener(move |view, event, window, cx| {
                view.mode = Mode::Tree;
                view.focus.focus(window, cx);
                if matches!(clicked_row.kind, RowKind::Entry(_)) {
                    view.select_row(Some(&clicked_row), Intent::Explicit, cx);
                }
                // Choosing a file is all `activate` would do for one, a second time.
                if !matches!(
                    clicked_row.kind,
                    RowKind::Entry(EntryKind::File | EntryKind::Symlink | EntryKind::Other)
                ) {
                    view.activate(&clicked_row, cx);
                }
                if matches!(event, gpui::ClickEvent::Mouse(mouse) if mouse.up.click_count == 2)
                    && clicked_row.kind == RowKind::Entry(EntryKind::File)
                {
                    view.action(Mode::Edit, cx);
                }
                cx.notify();
            }))
            .into_any_element();
        // Native insets the rounded row from the list's edges; a margin on a full-width
        // row would push it past the right edge, so a padded box holds it.
        if ui_text::is_native() {
            controls::list_inset(row).into_any_element()
        } else {
            row
        }
    }

    /// The Preview pane: the selected file's name and actions above its contents. It is
    /// drawn by `FilePreview`, but its clicks and keys act on this entity.
    fn preview_panel(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        if ui_text::is_native() {
            return self.native_preview_panel(window, cx);
        }
        let colors = theme::palette(cx);
        let title = preview_title(self.preview_path.as_deref(), self.selected.as_deref());
        let content: AnyElement = match &self.preview {
            PreviewState::Empty => div()
                .p(ui_text::space(18.0))
                .text_color(rgb(colors.muted))
                .child("Select a file to preview it here.")
                .into_any_element(),
            PreviewState::Loading => div()
                .p(ui_text::space(18.0))
                .text_color(rgb(colors.muted))
                .child("Loading preview…")
                .into_any_element(),
            PreviewState::PdfPending => div()
                .p(ui_text::space(18.0))
                .flex()
                .flex_col()
                .items_start()
                .gap(ui_text::space(10.0))
                .child(div().text_color(rgb(colors.muted)).child(
                    "PDFs are only rendered on request, because the PDF parser runs inside RiWork.",
                ))
                // While a newer selection loads, this placeholder is stale.
                .children((self.preview_path == self.selected).then(|| {
                    div()
                        .id("file-preview-open-pdf")
                        .px(ui_text::space(7.0))
                        .py(ui_text::space(5.0))
                        .border_1()
                        .border_color(rgb(colors.divider))
                        .text_color(rgb(colors.cyan))
                        .child("PREVIEW PDF")
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.mode = Mode::Preview;
                            view.preview_focus.focus(window, cx);
                            view.open_pdf_preview(cx);
                        }))
                }))
                .into_any_element(),
            PreviewState::Error(error) => div()
                .p(ui_text::space(18.0))
                .text_color(rgb(colors.gold))
                .child(error.clone())
                .into_any_element(),
            PreviewState::Ready(PreviewContent::Message(message)) => div()
                .p(ui_text::space(18.0))
                .text_color(rgb(colors.muted))
                .child(message.clone())
                .into_any_element(),
            PreviewState::Ready(PreviewContent::Text {
                lines,
                truncated,
                markdown,
            }) => {
                let lines = lines.clone();
                let count = lines.len();
                let gold = colors.gold;
                // A Markdown heading is a color only in the colorful themes; Native keeps
                // its one signal color for state and sets headings in semibold instead.
                let native = ui_text::is_native();
                let heading_color = if native { colors.text } else { gold };
                let muted = colors.muted;
                let text = colors.text;
                let markdown = *markdown;
                // The line a terminal link named, while this file is the one on screen.
                let marked = self
                    .highlight_line
                    .as_ref()
                    .filter(|(path, _)| Some(path) == self.preview_path.as_ref())
                    .map(|(_, index)| *index);
                let marker = colors.panel_active;
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex_none()
                            .px(ui_text::space(12.0))
                            .py(ui_text::space(6.0))
                            .text_size(ui_text::text(9.0))
                            .text_color(rgb(if *truncated { gold } else { muted }))
                            .child(ui_text::quiet(if *truncated {
                                "PREVIEW TRUNCATED AT 1 MiB OR 10,000 LINES"
                            } else if markdown {
                                "MARKDOWN SOURCE · READ ONLY"
                            } else {
                                "TEXT · READ ONLY"
                            })),
                    )
                    .child(
                        uniform_list("file-preview-lines", count, move |range, _, _| {
                            range
                                .map(|index| {
                                    let line = &lines[index];
                                    let heading = markdown && line.trim_start().starts_with('#');
                                    div()
                                        .h(ui_text::space(20.0))
                                        .w_full()
                                        .flex()
                                        .items_center()
                                        .px(ui_text::space(10.0))
                                        .when(marked == Some(index), |row| row.bg(rgb(marker)))
                                        .child(
                                            div()
                                                .w(ui_text::space(42.0))
                                                .flex_none()
                                                .text_color(rgb(muted))
                                                .text_size(ui_text::text(9.0))
                                                .child(format!("{}", index + 1)),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .overflow_hidden()
                                                .text_ellipsis()
                                                .text_color(rgb(if heading {
                                                    heading_color
                                                } else {
                                                    text
                                                }))
                                                .when(heading && native, |line| {
                                                    line.font_weight(gpui::FontWeight::SEMIBOLD)
                                                })
                                                .child(line.clone()),
                                        )
                                        .into_any_element()
                                })
                                .collect::<Vec<_>>()
                        })
                        .flex_1()
                        .min_h_0()
                        .font_family(accent_family())
                        .track_scroll(&self.preview_scroll),
                    )
                    .into_any_element()
            }
            PreviewState::Ready(PreviewContent::Image { image, description }) => div()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex_none()
                        .px(ui_text::space(12.0))
                        .py(ui_text::space(6.0))
                        .text_size(ui_text::text(9.0))
                        .text_color(rgb(colors.muted))
                        .child(ui_text::quiet(format!("{description} · READ ONLY"))),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .min_w_0()
                        .p(ui_text::space(12.0))
                        .child(img(image.clone()).size_full()),
                )
                .into_any_element(),
            PreviewState::Ready(PreviewContent::Pdf {
                image, page, pages, ..
            }) => {
                let previous = *page - 1;
                let next = *page + 1;
                let can_previous = *page > 1;
                let can_next = *page < *pages;
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex_none()
                            .px(ui_text::space(12.0))
                            .py(ui_text::space(6.0))
                            .flex()
                            .items_center()
                            .gap(ui_text::space(10.0))
                            .text_size(ui_text::text(10.0))
                            .child(
                                div()
                                    .id("file-preview-previous-page")
                                    .text_color(rgb(if can_previous {
                                        colors.cyan
                                    } else {
                                        colors.muted
                                    }))
                                    .child(ui_text::quiet("‹ PREV"))
                                    .on_click(cx.listener(move |view, _, window, cx| {
                                        view.mode = Mode::Preview;
                                        view.preview_focus.focus(window, cx);
                                        if can_previous {
                                            view.pdf_page(previous, cx);
                                        }
                                    })),
                            )
                            .child(
                                div()
                                    .text_color(rgb(colors.text))
                                    .child(ui_text::quiet(format!("PAGE {page} OF {pages}"))),
                            )
                            .child(
                                div()
                                    .id("file-preview-next-page")
                                    .text_color(rgb(if can_next {
                                        colors.cyan
                                    } else {
                                        colors.muted
                                    }))
                                    .child(ui_text::quiet("NEXT ›"))
                                    .on_click(cx.listener(move |view, _, window, cx| {
                                        view.mode = Mode::Preview;
                                        view.preview_focus.focus(window, cx);
                                        if can_next {
                                            view.pdf_page(next, cx);
                                        }
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .p(ui_text::space(12.0))
                            .child(img(image.clone()).size_full()),
                    )
                    .into_any_element()
            }
        };
        div()
            .id("file-preview-panel")
            .track_focus(&self.preview_focus)
            .key_context("FilePreview")
            .on_key_down(cx.listener(Self::preview_key_down))
            .size_full()
            .flex()
            .flex_col()
            .min_w_0()
            .min_h_0()
            .bg(rgb(colors.bg))
            .text_color(rgb(colors.text))
            .font_family(panel_family())
            .text_size(ui_text::text(11.0))
            .border_1()
            .border_color(rgb(if self.is_current(Mode::Preview, window, cx) {
                colors.focus
            } else {
                colors.divider
            }))
            .child(
                // The name gives way first: it truncates down to a stub before
                // the toolbar drops to a line of its own.
                div()
                    .id("file-preview-header")
                    .flex_none()
                    .px(ui_text::space(12.0))
                    .py(ui_text::space(8.0))
                    .border_b_1()
                    .border_color(rgb(colors.divider))
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_x(ui_text::space(10.0))
                    .gap_y(ui_text::space(6.0))
                    .child(
                        div()
                            .flex_grow(1.0)
                            .flex_basis(px(0.0))
                            .min_w(ui_text::space(40.0))
                            .flex()
                            .flex_col()
                            .gap(ui_text::space(2.0))
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .text_color(rgb(colors.cyan))
                                    .when(ui_text::is_native(), |title| {
                                        title.font_weight(gpui::FontWeight::SEMIBOLD)
                                    })
                                    .child(title),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .text_size(ui_text::text(9.0))
                                    .text_color(rgb(colors.muted))
                                    .font_family(ui_text::ui_family())
                                    .child(
                                        if self.preview_loading
                                            && self.preview_path != self.selected
                                        {
                                            ui_text::cased("Loading…")
                                        } else {
                                            ui_text::cased("Preview")
                                        },
                                    ),
                            ),
                    )
                    .child(self.preview_toolbar(window, cx)),
            )
            // Directly under the header, where the action that caused it just was.
            .children(self.notice.as_ref().map(|notice| {
                div()
                    .flex_none()
                    .px(ui_text::space(12.0))
                    .py(ui_text::space(6.0))
                    .border_b_1()
                    .border_color(rgb(colors.divider))
                    .text_size(ui_text::text(10.0))
                    .text_color(rgb(if notice.confirmation {
                        colors.cyan
                    } else {
                        colors.gold
                    }))
                    .child(notice.text.clone())
            }))
            .child(content)
            .on_click(cx.listener(|view, _, window, cx| {
                view.mode = Mode::Preview;
                view.preview_focus.focus(window, cx);
                cx.notify();
            }))
            .into_any_element()
    }
}

impl FileExplorer {
    /// The Preview pane under Native, laid out like every navigation panel: the file's name
    /// over its kind, size and path, its actions as symbols, then the contents on the
    /// panel's own background with nothing framing them.
    fn native_preview_panel(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme::palette(cx);
        let shown = self.preview_path.as_deref().or(self.selected.as_deref());
        let title = shown
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Preview".into());
        let path = shown.map(|path| {
            self.root
                .as_ref()
                .and_then(|root| path.strip_prefix(&root.path).ok())
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned()
        });
        let kind = if self.preview_loading && self.preview_path != self.selected {
            Some("Loading…".to_owned())
        } else {
            match &self.preview {
                PreviewState::Ready(PreviewContent::Text {
                    lines, markdown, ..
                }) => Some(format!(
                    "{} · {} {}",
                    if *markdown { "Markdown" } else { "Text" },
                    lines.len(),
                    if lines.len() == 1 { "line" } else { "lines" }
                )),
                PreviewState::Ready(PreviewContent::Image { description, .. }) => {
                    Some(format!("Image · {description}"))
                }
                PreviewState::Ready(PreviewContent::Pdf { pages, .. }) => Some(format!(
                    "PDF · {pages} {}",
                    if *pages == 1 { "page" } else { "pages" }
                )),
                PreviewState::PdfPending => Some("PDF".to_owned()),
                _ => None,
            }
        };
        let meta = [kind, path]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        let meta = (!meta.is_empty()).then(|| meta.into());
        let actions = TOOLBAR
            .iter()
            .map(|action| self.toolbar_button(action, true, window, cx))
            .collect::<Vec<_>>();
        let content: AnyElement = match &self.preview {
            PreviewState::Empty => {
                controls::empty_state("eye", "Select a file to preview it here.", colors)
                    .into_any_element()
            }
            PreviewState::Loading => {
                controls::empty_state("doc", "Loading preview…", colors).into_any_element()
            }
            PreviewState::PdfPending => controls::empty_state(
                "doc.richtext",
                "PDFs are only rendered on request, because the PDF parser runs inside RiWork.",
                colors,
            )
            // While a newer selection loads, this placeholder is stale.
            .children((self.preview_path == self.selected).then(|| {
                controls::button(
                    div().id("file-preview-open-pdf"),
                    controls::Button::Secondary,
                    colors,
                )
                .py(ui_text::space(3.0))
                .hover(move |style| style.bg(rgb(controls::Button::Secondary.hover(colors))))
                .child("Preview PDF")
                .on_click(cx.listener(|view, _, window, cx| {
                    view.mode = Mode::Preview;
                    view.preview_focus.focus(window, cx);
                    view.open_pdf_preview(cx);
                }))
            }))
            .into_any_element(),
            PreviewState::Error(error) => {
                controls::empty_state("exclamationmark.triangle", error.clone(), colors)
                    .into_any_element()
            }
            PreviewState::Ready(PreviewContent::Message(message)) => controls::empty_state(
                if message.starts_with("Expand this folder") {
                    "folder"
                } else {
                    "eye.slash"
                },
                message.clone(),
                colors,
            )
            .into_any_element(),
            PreviewState::Ready(PreviewContent::Text {
                lines,
                truncated,
                markdown,
            }) => {
                let lines = lines.clone();
                let count = lines.len();
                let markdown = *markdown;
                // A quiet gutter, wide enough for the last line's number.
                let digits = count.max(1).to_string().len().max(2) as f32;
                let gutter = ui_text::space(14.0 + digits * 6.5);
                let number = theme::mix(colors.muted, colors.panel, 0.35);
                let text = colors.text;
                let marked = self
                    .highlight_line
                    .as_ref()
                    .filter(|(path, _)| Some(path) == self.preview_path.as_ref())
                    .map(|(_, index)| *index);
                let marker = colors.panel_active;
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .children(truncated.then(|| {
                        controls::footnote("Preview truncated at 1 MiB or 10,000 lines", colors)
                            .text_color(rgb(colors.gold))
                    }))
                    .child(
                        uniform_list("file-preview-lines", count, move |range, _, _| {
                            range
                                .map(|index| {
                                    let line = &lines[index];
                                    let heading = markdown && line.trim_start().starts_with('#');
                                    div()
                                        .h(ui_text::space(18.0))
                                        .w_full()
                                        .flex()
                                        .items_center()
                                        .pr(ui_text::space(controls::PANEL_INSET))
                                        .when(marked == Some(index), |row| row.bg(rgb(marker)))
                                        .child(
                                            div()
                                                .w(gutter)
                                                .flex_none()
                                                .flex()
                                                .justify_end()
                                                .pr(ui_text::space(8.0))
                                                .text_color(rgb(number))
                                                .text_size(ui_text::text(9.0))
                                                .child(format!("{}", index + 1)),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .overflow_hidden()
                                                .whitespace_nowrap()
                                                .text_ellipsis()
                                                .text_size(ui_text::text(10.5))
                                                .text_color(rgb(text))
                                                .when(heading, |line| {
                                                    line.font_weight(gpui::FontWeight::SEMIBOLD)
                                                })
                                                .child(line.clone()),
                                        )
                                        .into_any_element()
                                })
                                .collect::<Vec<_>>()
                        })
                        .flex_1()
                        .min_h_0()
                        .pb(ui_text::space(controls::LIST_MARGIN))
                        .font_family(accent_family())
                        .track_scroll(&self.preview_scroll),
                    )
                    .into_any_element()
            }
            PreviewState::Ready(PreviewContent::Image { image, .. }) => div()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .px(ui_text::space(controls::PANEL_INSET + 4.0))
                .pt(ui_text::space(4.0))
                .pb(ui_text::space(controls::PANEL_INSET + 4.0))
                .flex()
                .items_center()
                .justify_center()
                .child(img(image.clone()).size_full())
                .into_any_element(),
            PreviewState::Ready(PreviewContent::Pdf {
                image, page, pages, ..
            }) => {
                let previous = *page - 1;
                let next = *page + 1;
                let can_previous = *page > 1;
                let can_next = *page < *pages;
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .px(ui_text::space(controls::PANEL_INSET + 4.0))
                            .pt(ui_text::space(4.0))
                            .child(img(image.clone()).size_full()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .py(ui_text::space(6.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap(ui_text::space(6.0))
                            .text_size(ui_text::text(controls::META_TEXT))
                            .text_color(rgb(colors.muted))
                            .child(
                                controls::toolbar_button(
                                    "file-preview-previous-page",
                                    "chevron.left",
                                    "Previous page",
                                    can_previous,
                                    colors,
                                )
                                .on_click(cx.listener(
                                    move |view, _, window, cx| {
                                        cx.stop_propagation();
                                        view.mode = Mode::Preview;
                                        view.preview_focus.focus(window, cx);
                                        if can_previous {
                                            view.pdf_page(previous, cx);
                                        }
                                    },
                                )),
                            )
                            .child(format!("Page {page} of {pages}"))
                            .child(
                                controls::toolbar_button(
                                    "file-preview-next-page",
                                    "chevron.right",
                                    "Next page",
                                    can_next,
                                    colors,
                                )
                                .on_click(cx.listener(
                                    move |view, _, window, cx| {
                                        cx.stop_propagation();
                                        view.mode = Mode::Preview;
                                        view.preview_focus.focus(window, cx);
                                        if can_next {
                                            view.pdf_page(next, cx);
                                        }
                                    },
                                )),
                            ),
                    )
                    .into_any_element()
            }
        };
        controls::panel(colors)
            .id("file-preview-panel")
            .track_focus(&self.preview_focus)
            .key_context("FilePreview")
            .on_key_down(cx.listener(Self::preview_key_down))
            .child(controls::panel_header(title, meta, actions, colors))
            // Directly under the header, where the action that caused it just was.
            .children(self.notice.as_ref().map(|notice| {
                controls::footnote(notice.text.clone(), colors)
                    .pt_0()
                    .text_size(ui_text::text(controls::META_TEXT))
                    .text_color(rgb(if notice.confirmation {
                        colors.text
                    } else {
                        colors.gold
                    }))
            }))
            .child(content)
            .on_click(cx.listener(|view, _, window, cx| {
                view.mode = Mode::Preview;
                view.preview_focus.focus(window, cx);
                cx.notify();
            }))
            .into_any_element()
    }
}

/// How a button shows its action.
enum Face<'a> {
    Text(&'a str),
    /// A glyph in the text colour; the name and hint move to the tooltip.
    Glyph(ActionGlyph, &'static str),
}

impl FileExplorer {
    fn panel_for(
        &mut self,
        input: &Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme::palette(cx);
        let rows = self.rows();
        let item_count = rows
            .iter()
            .filter(|row| matches!(row.kind, RowKind::Entry(_)))
            .count();
        let root_state = self
            .root
            .as_ref()
            .and_then(|root| self.tree.directories().get(&root.path));
        let message = if self.root.is_none() {
            Some("Select a worktree to browse its files.".to_owned())
        } else if root_state.is_some_and(|state| state.loading && !state.loaded) {
            Some("Loading worktree…".into())
        } else if let Some(error) = root_state.and_then(|state| state.error.clone()) {
            Some(error)
        } else if item_count == 0 {
            Some(if self.filter.text.trim().is_empty() {
                if root_state.is_some_and(|state| !state.entries.is_empty()) && !self.show_hidden {
                    "No visible files. Turn on Hidden to show dotfiles.".into()
                } else {
                    "This folder is empty.".into()
                }
            } else {
                "No loaded files match. Clear the filter and expand folders to load more.".into()
            })
        } else {
            None
        };
        let view = cx.entity();
        let tree = uniform_list(
            "file-explorer-tree",
            rows.len(),
            move |range, window, cx| {
                view.update(cx, |view, cx| {
                    range
                        .map(|index| view.row(&rows[index], index, window, cx))
                        .collect::<Vec<_>>()
                })
            },
        )
        .flex_1()
        .min_h_0()
        // File names are technical text: the monospace accent, in Native as elsewhere.
        .font_family(accent_family())
        .track_scroll(&self.scroll);
        let relative = self
            .selected
            .as_ref()
            .and_then(|path| {
                self.root
                    .as_ref()
                    .and_then(|root| path.strip_prefix(&root.path).ok())
            })
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Select a file or folder".into());
        if ui_text::is_native() {
            let error = root_state.is_some_and(|state| state.error.is_some());
            let meta = match &self.root {
                Some(root) => format!(
                    "{} · {item_count} {}",
                    root.label,
                    if item_count == 1 { "item" } else { "items" }
                ),
                None => "No worktree".to_owned(),
            };
            return controls::panel(colors)
                .id("file-explorer-browser")
                .child(controls::panel_header(
                    crate::layouts::PanelKind::Files.label(),
                    Some(meta.into()),
                    [
                        self.button("file-explorer-refresh", "", Mode::Refresh, window, cx),
                        self.button("file-explorer-hidden", "", Mode::Hidden, window, cx),
                        self.button(
                            "file-explorer-reveal-root",
                            "",
                            Mode::RevealRoot,
                            window,
                            cx,
                        ),
                    ],
                    colors,
                ))
                .child(
                    div()
                        .flex_none()
                        .pb(ui_text::space(6.0))
                        .child(self.search(input, window, cx)),
                )
                .children(message.map(|message| {
                    if error {
                        controls::footnote(message, colors)
                            .text_size(ui_text::text(10.0))
                            .text_color(rgb(colors.gold))
                    } else {
                        controls::empty_state("folder", message, colors)
                    }
                }))
                .child(tree.pb(ui_text::space(controls::LIST_MARGIN)))
                .child(
                    controls::footnote(relative, colors)
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .when(self.selected.is_some(), |path| {
                            path.font_family(accent_family())
                        }),
                )
                .track_focus(&self.focus)
                .key_context("FileExplorer")
                .capture_key_down(cx.listener(Self::key_down))
                .into_any_element();
        }
        let browser = div()
            .id("file-explorer-browser")
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(rgb(colors.panel))
            .text_color(rgb(colors.text))
            .font_family(panel_family())
            .text_size(ui_text::text(11.0))
            .child(
                div()
                    .flex_none()
                    .p(ui_text::space(10.0))
                    .flex()
                    .flex_col()
                    .gap(ui_text::space(8.0))
                    .border_b_1()
                    .border_color(rgb(colors.divider))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(ui_text::space(8.0))
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .text_color(rgb(colors.cyan))
                                    .when(ui_text::is_native(), |title| {
                                        title.font_weight(gpui::FontWeight::SEMIBOLD)
                                    })
                                    .child(
                                        self.root
                                            .as_ref()
                                            .map(|root| root.label.clone())
                                            .unwrap_or_else(|| "WORKTREE FILES".into()),
                                    ),
                            )
                            .child(
                                div()
                                    .text_color(rgb(colors.magenta))
                                    .text_size(ui_text::text(9.0))
                                    .flex_none()
                                    .child(if ui_text::is_native() {
                                        format!("{item_count} items")
                                    } else {
                                        format!("{item_count:02} ITEMS")
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(ui_text::space(5.0))
                            .text_size(ui_text::text(9.0))
                            .child(self.button(
                                "file-explorer-refresh",
                                "↻ Refresh",
                                Mode::Refresh,
                                window,
                                cx,
                            ))
                            .child(self.button(
                                "file-explorer-hidden",
                                if self.show_hidden {
                                    "● Hidden"
                                } else {
                                    "○ Hidden"
                                },
                                Mode::Hidden,
                                window,
                                cx,
                            ))
                            .child(self.button(
                                "file-explorer-reveal-root",
                                "↗ Worktree",
                                Mode::RevealRoot,
                                window,
                                cx,
                            )),
                    )
                    .child(self.search(input, window, cx)),
            )
            .children(message.map(|message| {
                div()
                    .p(ui_text::space(12.0))
                    .text_color(rgb(
                        if root_state.is_some_and(|state| state.error.is_some()) {
                            colors.gold
                        } else {
                            colors.muted
                        },
                    ))
                    .child(message)
            }))
            .child(tree)
            .child(
                div()
                    .flex_none()
                    .p(ui_text::space(8.0))
                    .border_t_1()
                    .border_color(rgb(colors.divider))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_color(rgb(colors.muted))
                            .text_size(ui_text::text(10.0))
                            .when(self.selected.is_some(), |path| {
                                path.font_family(accent_family())
                            })
                            .child(relative),
                    ),
            );
        browser
            .track_focus(&self.focus)
            .key_context("FileExplorer")
            .capture_key_down(cx.listener(Self::key_down))
            .into_any_element()
    }
}

impl Render for FileExplorer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let input = self.filter_state.clone();
        self.panel_for(&input, window, cx)
    }
}

/// A Files tab shares the tree/domain query while retaining its own input geometry.
pub struct FileExplorerSurface {
    explorer: Entity<FileExplorer>,
    input: Entity<InputState>,
}

impl FileExplorerSurface {
    pub fn focus_search(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.explorer.update(cx, |explorer, cx| {
            if !explorer.owns_filter(&self.input) {
                return;
            }
            explorer.filter_state = self.input.clone();
            explorer.focus_search(window, cx);
        });
    }
}

impl Render for FileExplorerSurface {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.explorer.update(cx, |explorer, cx| {
            explorer.panel_for(&self.input, window, cx)
        })
    }
}

/// The Preview panel shares the explorer's selection and preview lifecycle.
pub struct FilePreview {
    explorer: Entity<FileExplorer>,
}

impl FilePreview {
    pub fn new(explorer: Entity<FileExplorer>, cx: &mut Context<Self>) -> Self {
        // Whatever redraws the explorer (a new selection, a finished load, a setting)
        // redraws the preview too.
        cx.observe(&explorer, |_, _, cx| cx.notify()).detach();
        Self { explorer }
    }

    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.explorer
            .update(cx, |explorer, cx| explorer.focus_preview(window, cx));
    }
}

impl Render for FilePreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.explorer
            .update(cx, |explorer, cx| explorer.preview_panel(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    static NEXT_TEMP: AtomicUsize = AtomicUsize::new(0);

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "riwork-explorer-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, AtomicOrdering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn loader_lists_one_level_dirs_first_and_natural_case_insensitive_names() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.0.join("z-folder")).unwrap();
        fs::create_dir(fixture.0.join("node_modules")).unwrap();
        fs::write(fixture.0.join("node_modules/should-stay-unloaded"), "").unwrap();
        for name in ["file10", "FILE2", "file02", ".hidden", "file1"] {
            fs::write(fixture.0.join(name), "").unwrap();
        }
        let entries = read_directory(&fixture.0).unwrap().entries;
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            [
                "node_modules",
                "z-folder",
                ".hidden",
                "file1",
                "FILE2",
                "file02",
                "file10"
            ]
        );
        assert!(
            entries
                .iter()
                .find(|entry| entry.name == ".hidden")
                .unwrap()
                .hidden
        );
        assert!(
            !entries
                .iter()
                .any(|entry| entry.name == "should-stay-unloaded")
        );
        assert_eq!(
            natural_cmp("x999999999999999999999999", "x1000000000000000000000000"),
            Ordering::Less
        );
    }

    #[test]
    fn missing_or_non_directory_root_is_reported() {
        let fixture = Fixture::new();
        assert!(
            read_directory(&fixture.0.join("missing"))
                .unwrap_err()
                .contains("Cannot read")
        );
        fs::write(fixture.0.join("file"), "").unwrap();
        assert!(
            read_directory(&fixture.0.join("file"))
                .unwrap_err()
                .contains("no longer a directory")
        );
    }

    #[cfg(unix)]
    #[test]
    fn directory_symlinks_and_cycles_are_never_expanded() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let outside = Fixture::new();
        fs::write(outside.0.join("secret"), "").unwrap();
        symlink(&outside.0, fixture.0.join("outside")).unwrap();
        symlink(&fixture.0, fixture.0.join("cycle")).unwrap();
        let entries = read_directory(&fixture.0).unwrap().entries;
        assert!(entries.iter().all(|entry| entry.kind == EntryKind::Symlink));
        assert!(
            read_directory(&fixture.0.join("outside"))
                .unwrap_err()
                .contains("Symbolic links")
        );
        assert!(read_directory(&fixture.0.join("cycle")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn cached_descendants_cannot_escape_through_a_replaced_ancestor() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let outside = Fixture::new();
        fs::create_dir_all(fixture.0.join("parent/child")).unwrap();
        fs::create_dir_all(outside.0.join("child")).unwrap();
        fs::write(outside.0.join("child/secret"), "").unwrap();
        assert!(
            read_directory_in(&fixture.0, &fixture.0.join("parent/child"))
                .unwrap()
                .entries
                .is_empty()
        );
        fs::remove_dir_all(fixture.0.join("parent")).unwrap();
        symlink(&outside.0, fixture.0.join("parent")).unwrap();
        assert!(
            read_directory_in(&fixture.0, &fixture.0.join("parent/child"))
                .unwrap_err()
                .contains("Symbolic links")
        );
        assert!(
            read_directory_in(&fixture.0, &outside.0)
                .unwrap_err()
                .contains("outside")
        );
        assert!(
            read_directory_in(&fixture.0, &fixture.0.join("../outside"))
                .unwrap_err()
                .contains("outside")
        );
    }

    #[cfg(unix)]
    #[test]
    fn permission_denied_directory_returns_an_error() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let path = fixture.0.join("private");
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        let result = read_directory(&path);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        // Privileged test runners can bypass Unix mode bits.
        if fs::read_dir(&path).is_ok() && result.is_ok() {
            return;
        }
        assert!(result.unwrap_err().contains("Cannot read this folder"));
    }

    #[test]
    fn tree_filter_includes_loaded_ancestors_and_respects_hidden_paths() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.0.join("src")).unwrap();
        fs::create_dir(fixture.0.join(".git")).unwrap();
        fs::write(fixture.0.join("src/main.rs"), "").unwrap();
        fs::write(fixture.0.join(".git/main.rs"), "").unwrap();
        let mut directories = BTreeMap::new();
        for path in [&fixture.0, &fixture.0.join("src"), &fixture.0.join(".git")] {
            directories.insert(
                path.clone(),
                DirectoryState {
                    entries: read_directory(path).unwrap().entries,
                    ..Default::default()
                },
            );
        }
        let expanded = HashSet::new();
        let rows = visible_rows(&fixture.0, &directories, &expanded, false, "main");
        assert_eq!(
            rows.iter()
                .map(|row| row.label.as_str())
                .collect::<Vec<_>>(),
            ["src", "main.rs"]
        );
        assert_eq!(rows[1].depth, 1);
        let rows = visible_rows(&fixture.0, &directories, &expanded, true, "main");
        assert_eq!(rows.iter().filter(|row| row.label == "main.rs").count(), 2);
        // A cached child stays collapsed when there is no filter.
        let rows = visible_rows(&fixture.0, &directories, &expanded, false, "");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "src");
        let expanded = HashSet::from([fixture.0.join("src")]);
        assert_eq!(
            visible_rows(&fixture.0, &directories, &expanded, false, "").len(),
            2
        );
    }

    fn entry(root: &Path, name: &str, kind: EntryKind) -> Entry {
        Entry {
            path: root.join(name),
            name: name.to_owned(),
            kind,
            hidden: name.starts_with('.'),
            identity: None,
        }
    }

    fn listing(entries: Vec<Entry>) -> Listing {
        Listing {
            entries,
            omitted: 0,
            scan_capped: false,
        }
    }

    fn load_listing(tree: &mut TreeModel, path: &Path, request: u64, entries: Vec<Entry>) {
        tree.begin_load(path, request);
        tree.finish_load(path, request, Ok(listing(entries)));
    }

    #[test]
    fn huge_folders_are_capped_with_directories_first() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.0.join("dir")).unwrap();
        for index in 0..10 {
            fs::write(fixture.0.join(format!("file{index}")), "").unwrap();
        }
        let listing = read_directory_capped(&fixture.0, 5, 100).unwrap();
        assert_eq!(listing.entries.len(), 5);
        assert_eq!(listing.entries[0].name, "dir");
        assert_eq!((listing.omitted, listing.scan_capped), (6, false));
        // Only listed files are statted.
        assert!(
            listing
                .entries
                .iter()
                .filter(|entry| entry.kind == EntryKind::File)
                .all(|entry| entry.identity.is_some())
        );
        let listing = read_directory_capped(&fixture.0, 5, 8).unwrap();
        assert_eq!(listing.entries.len(), 5);
        assert_eq!((listing.omitted, listing.scan_capped), (3, true));
        let listing = read_directory_capped(&fixture.0, 100, 11).unwrap();
        assert_eq!((listing.entries.len(), listing.omitted), (11, 0));
        assert!(!listing.scan_capped);
    }

    #[test]
    fn omitted_entries_get_a_notice_row_after_the_folders_children() {
        let root = Path::new("/w");
        let mut directories = BTreeMap::new();
        directories.insert(
            root.to_owned(),
            DirectoryState {
                entries: vec![
                    entry(root, "a", EntryKind::Directory),
                    entry(root, "z", EntryKind::File),
                ],
                omitted: 3,
                loaded: true,
                ..Default::default()
            },
        );
        directories.insert(
            root.join("a"),
            DirectoryState {
                entries: vec![entry(&root.join("a"), "inner", EntryKind::File)],
                omitted: 40_000,
                scan_capped: true,
                loaded: true,
                ..Default::default()
            },
        );
        let expanded = HashSet::from([root.join("a")]);
        let rows = visible_rows(root, &directories, &expanded, false, "");
        assert_eq!(
            rows.iter()
                .map(|row| (row.label.as_str(), row.depth))
                .collect::<Vec<_>>(),
            [
                ("a", 0),
                ("inner", 1),
                ("40000+ more entries not shown", 1),
                ("z", 0),
                ("3 more entries not shown", 0),
            ]
        );
        assert!(rows[2].kind == RowKind::Status && rows[4].kind == RowKind::Status);
        // Filtering only looks at loaded names, so the notices stay out of it.
        let rows = visible_rows(root, &directories, &expanded, false, "inner");
        assert!(rows.iter().all(|row| row.kind != RowKind::Status));
        assert_eq!(omitted_label(1, false), "1 more entry not shown");
    }

    #[test]
    fn reloading_a_loaded_folder_never_shows_loading_again() {
        let root = Path::new("/w");
        let mut directories = BTreeMap::new();
        directories.insert(
            root.to_owned(),
            DirectoryState {
                entries: vec![entry(root, "empty", EntryKind::Directory)],
                loaded: true,
                ..Default::default()
            },
        );
        directories.insert(
            root.join("empty"),
            DirectoryState {
                loading: true,
                loaded: true,
                ..Default::default()
            },
        );
        let expanded = HashSet::from([root.join("empty")]);
        assert_eq!(
            visible_rows(root, &directories, &expanded, false, "").len(),
            1
        );
        directories.get_mut(&root.join("empty")).unwrap().loaded = false;
        assert_eq!(
            visible_rows(root, &directories, &expanded, false, "")[1].label,
            "Loading…"
        );
    }

    #[test]
    fn cached_rows_survive_identical_reloads_and_rebuild_on_change() {
        let root = Path::new("/w");
        let mut tree = TreeModel::default();
        let listed = || {
            vec![
                entry(root, "a", EntryKind::Directory),
                entry(root, "f", EntryKind::File),
            ]
        };
        assert!(tree.begin_load(root, 1));
        let outcome = tree.finish_load(root, 1, Ok(listing(listed())));
        assert!(outcome.changed);
        let first = tree.rows(root, false, "");
        assert_eq!(first.len(), 2);
        assert!(Rc::ptr_eq(&first, &tree.rows(root, false, "")));
        // A poll that finds the same listing changes nothing.
        assert!(!tree.begin_load(root, 2));
        assert!(tree.directories()[root].loading);
        let outcome = tree.finish_load(root, 2, Ok(listing(listed())));
        assert!(!outcome.changed);
        assert!(!tree.directories()[root].loading);
        assert!(Rc::ptr_eq(&first, &tree.rows(root, false, "")));
        // The view options are part of the cache key.
        assert!(!Rc::ptr_eq(&first, &tree.rows(root, true, "")));
        assert!(!Rc::ptr_eq(&first, &tree.rows(root, false, "f")));
        let second = tree.rows(root, false, "");
        // A new file, or a new identity for an old one, rebuilds the rows.
        tree.begin_load(root, 3);
        let mut changed = listed();
        changed.push(entry(root, "g", EntryKind::File));
        assert!(tree.finish_load(root, 3, Ok(listing(changed))).changed);
        assert_eq!(tree.rows(root, false, "").len(), 3);
        tree.begin_load(root, 4);
        let mut rewritten = listed();
        rewritten.push(entry(root, "g", EntryKind::File));
        rewritten[1].identity = Some(FileIdentity::of(&fs::metadata(".").unwrap()));
        assert!(tree.finish_load(root, 4, Ok(listing(rewritten))).changed);
        assert!(!Rc::ptr_eq(&second, &tree.rows(root, false, "")));
        // Expanding a folder is a change too.
        let before = tree.rows(root, false, "");
        tree.expanded_mut().insert(root.join("a"));
        assert!(!Rc::ptr_eq(&before, &tree.rows(root, false, "")));
    }

    #[test]
    fn repeated_errors_are_unchanged_and_recovery_is_a_change() {
        let root = Path::new("/w");
        let mut tree = TreeModel::default();
        tree.begin_load(root, 1);
        assert!(tree.finish_load(root, 1, Err("nope".into())).changed);
        tree.begin_load(root, 2);
        assert!(!tree.finish_load(root, 2, Err("nope".into())).changed);
        // The error stays on screen while a retry is running.
        tree.begin_load(root, 3);
        assert_eq!(tree.directories()[root].error.as_deref(), Some("nope"));
        assert!(tree.finish_load(root, 3, Ok(listing(Vec::new()))).changed);
        assert!(tree.directories()[root].error.is_none());
    }

    #[test]
    fn only_expanded_folders_are_polled_and_removed_ones_are_evicted() {
        let root = Path::new("/w");
        let mut tree = TreeModel::default();
        let dirs = || {
            vec![
                entry(root, "open", EntryKind::Directory),
                entry(root, "closed", EntryKind::Directory),
                entry(root, "gone", EntryKind::Directory),
            ]
        };
        load_listing(&mut tree, root, 1, dirs());
        for name in ["open", "closed", "gone"] {
            load_listing(&mut tree, &root.join(name), 2, Vec::new());
        }
        load_listing(
            &mut tree,
            &root.join("gone"),
            3,
            vec![entry(&root.join("gone"), "deep", EntryKind::Directory)],
        );
        load_listing(&mut tree, &root.join("gone/deep"), 4, Vec::new());
        tree.expanded_mut().insert(root.join("open"));
        tree.expanded_mut().insert(root.join("gone"));
        tree.begin_load(root, 5);
        let outcome = tree.finish_load(root, 5, Ok(listing(dirs())));
        // The collapsed folder keeps its cache but is not polled.
        assert_eq!(
            outcome.reload,
            [root.join("open"), root.join("gone")].to_vec()
        );
        assert!(tree.directories().contains_key(&root.join("closed")));
        tree.begin_load(root, 6);
        let outcome = tree.finish_load(
            root,
            6,
            Ok(listing(vec![
                entry(root, "open", EntryKind::Directory),
                entry(root, "closed", EntryKind::Directory),
            ])),
        );
        assert_eq!(outcome.reload, [root.join("open")].to_vec());
        assert!(!tree.directories().contains_key(&root.join("gone")));
        assert!(!tree.directories().contains_key(&root.join("gone/deep")));
        assert!(!tree.expanded().contains(&root.join("gone")));
        assert!(tree.expanded().contains(&root.join("open")));
    }

    #[test]
    fn open_externally_refuses_links_executables_and_bundles() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let fixture = Fixture::new();
        let outside = Fixture::new();
        let root = &fixture.0;
        fs::write(root.join("notes.txt"), "").unwrap();
        fs::create_dir(root.join("docs")).unwrap();
        assert_eq!(open_refusal(root, &root.join("notes.txt")), None);
        assert_eq!(open_refusal(root, &root.join("docs")), None);

        fs::write(root.join("run.sh"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(root.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            open_refusal(root, &root.join("run.sh"))
                .unwrap()
                .contains("Executable")
        );

        fs::write(root.join("Setup.COMMAND"), "").unwrap();
        fs::create_dir(root.join("Tool.app")).unwrap();
        for name in ["Setup.COMMAND", "Tool.app"] {
            assert!(
                open_refusal(root, &root.join(name))
                    .unwrap()
                    .contains("Applications and scripts"),
                "{name}"
            );
        }

        fs::write(outside.0.join("secret.txt"), "").unwrap();
        symlink(outside.0.join("secret.txt"), root.join("link.txt")).unwrap();
        symlink(&outside.0, root.join("linked-dir")).unwrap();
        for path in [
            root.join("link.txt"),
            root.join("linked-dir"),
            root.join("linked-dir/secret.txt"),
        ] {
            assert!(
                open_refusal(root, &path)
                    .unwrap()
                    .contains("Symbolic links"),
                "{}",
                path.display()
            );
        }
        assert!(
            open_refusal(root, &outside.0.join("secret.txt"))
                .unwrap()
                .contains("outside")
        );
        assert!(
            open_refusal(root, &root.join("../x"))
                .unwrap()
                .contains("outside")
        );
        assert!(
            open_refusal(root, &root.join("missing.txt"))
                .unwrap()
                .contains("Cannot inspect")
        );

        let fifo = std::ffi::CString::new(root.join("pipe").to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(
            open_refusal(root, &root.join("pipe"))
                .unwrap()
                .contains("Only files and folders")
        );
    }

    #[test]
    fn pdfs_are_parsed_only_after_an_explicit_request() {
        let id = Some(FileIdentity::of(&fs::metadata(".").unwrap()));
        let pdf = Path::new("/w/Paper.PDF");
        let other = Path::new("/w/other.pdf");
        assert!(matches!(
            plan_preview(EntryKind::File, id, pdf, None),
            PreviewPlan::Show(PreviewState::PdfPending)
        ));
        // A request for one PDF does not cover another.
        assert!(matches!(
            plan_preview(EntryKind::File, id, pdf, Some(other)),
            PreviewPlan::Show(PreviewState::PdfPending)
        ));
        assert!(matches!(
            plan_preview(EntryKind::File, id, pdf, Some(pdf)),
            PreviewPlan::Load
        ));
        assert!(matches!(
            plan_preview(EntryKind::File, id, Path::new("/w/a.png"), None),
            PreviewPlan::Load
        ));
        assert!(matches!(
            plan_preview(EntryKind::File, None, pdf, Some(pdf)),
            PreviewPlan::Show(PreviewState::Error(_))
        ));
        for kind in [EntryKind::Directory, EntryKind::Symlink, EntryKind::Other] {
            assert!(matches!(
                plan_preview(kind, id, pdf, Some(pdf)),
                PreviewPlan::Show(PreviewState::Ready(PreviewContent::Message(_)))
            ));
        }

        // Arrow keys and auto-selection never opt in; clicks and Enter do,
        // and selecting anything else forgets the request.
        assert_eq!(explicit_pdf_after(Some(pdf), Intent::Passive), None);
        assert_eq!(
            explicit_pdf_after(Some(pdf), Intent::Explicit),
            Some(pdf.to_owned())
        );
        assert_eq!(
            explicit_pdf_after(Some(Path::new("/w/a.txt")), Intent::Explicit),
            None
        );
        assert_eq!(explicit_pdf_after(None, Intent::Explicit), None);
    }

    #[test]
    fn keyboard_selection_is_debounced_and_clicks_are_not() {
        assert!(Intent::Passive.delay() >= Duration::from_millis(100));
        assert!(Intent::Auto.delay() >= Duration::from_millis(100));
        assert_eq!(Intent::Explicit.delay(), Duration::ZERO);
        // The tree picking a row never opts a PDF in.
        let pdf = Path::new("/w/Paper.PDF");
        assert_eq!(explicit_pdf_after(Some(pdf), Intent::Auto), None);
    }

    #[test]
    fn only_a_person_choosing_a_file_asks_for_the_preview_pane() {
        let row = |kind| Some(RowKind::Entry(kind));
        for intent in [Intent::Passive, Intent::Explicit] {
            assert!(reveals_preview(intent, row(EntryKind::File)));
            assert!(reveals_preview(intent, row(EntryKind::Symlink)));
            // A folder has nothing to preview, and neither do special files.
            assert!(!reveals_preview(intent, row(EntryKind::Directory)));
            assert!(!reveals_preview(intent, row(EntryKind::Other)));
            assert!(!reveals_preview(intent, Some(RowKind::Status)));
            assert!(!reveals_preview(intent, Some(RowKind::Error)));
            assert!(!reveals_preview(intent, None));
        }
        // The first row picked after a load or a filter is not a choice.
        assert!(!reveals_preview(Intent::Auto, row(EntryKind::File)));
    }

    #[test]
    fn the_preview_follows_whatever_the_tree_selects() {
        let id = Some(FileIdentity::of(&fs::metadata(".").unwrap()));
        let root = Path::new("/w");
        let mut tree = TreeModel::default();
        let file = |name: &str| Entry {
            identity: id,
            ..entry(root, name, EntryKind::File)
        };
        let entries = vec![
            entry(root, "src", EntryKind::Directory),
            file("main.rs"),
            file("Paper.pdf"),
            entry(root, "link", EntryKind::Symlink),
        ];
        load_listing(&mut tree, root, 1, entries);
        let rows = tree.rows(root, false, "");

        // What the preview pane would plan for each selection, from the same rows the tree
        // shows: there is no copy of the selection in the preview to fall out of step.
        let plan = |name: &str, explicit_pdf: Option<&Path>| {
            let path = root.join(name);
            let (kind, identity) = entry_of(&rows, &path)?;
            Some(plan_preview(kind, identity, &path, explicit_pdf))
        };
        let message = |plan: Option<PreviewPlan>| match plan {
            Some(PreviewPlan::Show(PreviewState::Ready(PreviewContent::Message(text)))) => {
                Some(text.to_string())
            }
            _ => None,
        };
        assert!(
            message(plan("src", None))
                .unwrap()
                .contains("Expand this folder")
        );
        assert!(matches!(plan("main.rs", None), Some(PreviewPlan::Load)));
        assert!(matches!(
            plan("Paper.pdf", None),
            Some(PreviewPlan::Show(PreviewState::PdfPending))
        ));
        assert!(matches!(
            plan("Paper.pdf", Some(&root.join("Paper.pdf"))),
            Some(PreviewPlan::Load)
        ));
        assert!(
            message(plan("link", None))
                .unwrap()
                .contains("Symbolic links")
        );
        // A selection that is no longer listed (deleted, filtered out) empties the preview.
        assert!(plan("gone.txt", None).is_none());
        let filtered = tree.rows(root, false, "main");
        assert!(entry_of(&filtered, &root.join("Paper.pdf")).is_none());
        assert!(entry_of(&filtered, &root.join("main.rs")).is_some());
    }

    #[test]
    fn a_link_opens_the_folders_above_its_file() {
        let root = Path::new("/w");
        let folders = |paths: &[&str]| paths.iter().map(PathBuf::from).collect::<Vec<_>>();
        assert_eq!(
            reveal_plan(root, Path::new("/w/src/a/b.rs")),
            Some((folders(&["/w/src", "/w/src/a"]), false))
        );
        assert_eq!(
            reveal_plan(root, Path::new("/w/a.rs")),
            Some((Vec::new(), false))
        );
        // A folder is revealed the same way; its own row is not among the ones to open.
        assert_eq!(
            reveal_plan(root, Path::new("/w/src/a")),
            Some((folders(&["/w/src"]), false))
        );
        // A dot name anywhere below the root hides the row until hidden files are shown.
        assert_eq!(
            reveal_plan(root, Path::new("/w/.github/ci.yml")),
            Some((folders(&["/w/.github"]), true))
        );
        assert_eq!(
            reveal_plan(root, Path::new("/w/src/.env")),
            Some((folders(&["/w/src"]), true))
        );
        // The root itself, other places and escapes are not the tree's.
        assert_eq!(reveal_plan(root, root), None);
        assert_eq!(reveal_plan(root, Path::new("/other/a.rs")), None);
        assert_eq!(reveal_plan(root, Path::new("/w/../other/a.rs")), None);
    }

    #[test]
    fn a_line_scrolls_a_text_preview_and_says_when_it_cannot() {
        let text = |lines: usize, truncated: bool| {
            PreviewState::Ready(PreviewContent::Text {
                lines: Arc::new((0..lines).map(|line| format!("line {line}")).collect()),
                truncated,
                markdown: false,
            })
        };
        // Counted from 1 for people, from 0 for the list.
        assert_eq!(
            line_outcome(&text(3, false), 1),
            LineOutcome::Show { index: 0 }
        );
        assert_eq!(
            line_outcome(&text(3, false), 3),
            LineOutcome::Show { index: 2 }
        );
        assert_eq!(
            line_outcome(&text(3, false), 0),
            LineOutcome::Show { index: 0 }
        );
        // Past the end: the last line is shown and the message says why it is not the one asked.
        assert_eq!(
            line_outcome(&text(3, false), 4),
            LineOutcome::PastEnd {
                last: 2,
                truncated: false
            }
        );
        assert_eq!(
            line_outcome(&text(3, true), 9_000),
            LineOutcome::PastEnd {
                last: 2,
                truncated: true
            }
        );
        // Not here yet, not text, or not shown at all.
        assert_eq!(line_outcome(&PreviewState::Loading, 5), LineOutcome::Wait);
        assert_eq!(line_outcome(&PreviewState::Empty, 5), LineOutcome::Wait);
        assert_eq!(
            line_outcome(&PreviewState::PdfPending, 5),
            LineOutcome::NoLines
        );
        let message = PreviewState::Ready(PreviewContent::Message("binary".into()));
        assert_eq!(line_outcome(&message, 5), LineOutcome::NoLines);
        assert_eq!(
            line_outcome(&PreviewState::Error("gone".into()), 5),
            LineOutcome::Failed
        );
    }

    #[test]
    fn the_preview_title_trails_the_selection_while_a_replacement_loads() {
        let old = Path::new("/w/old.txt");
        let new = Path::new("/w/new.txt");
        assert_eq!(preview_title(None, None), "PREVIEW");
        assert_eq!(preview_title(Some(old), Some(new)), "old.txt");
        assert_eq!(preview_title(Some(new), Some(new)), "new.txt");
        // Nothing on screen yet: the selection names the pane, so it is never anonymous.
        assert_eq!(preview_title(None, Some(new)), "new.txt");
    }

    #[test]
    fn copy_contents_is_offered_only_for_small_text_files() {
        let fixture = Fixture::new();
        let identity = |name: &str, bytes: &[u8]| {
            let path = fixture.0.join(name);
            fs::write(&path, bytes).unwrap();
            (
                path.clone(),
                Some(FileIdentity::of(&fs::metadata(&path).unwrap())),
            )
        };
        let text = PreviewState::Ready(PreviewContent::Text {
            lines: Arc::new(vec!["fn main() {}".into()]),
            truncated: false,
            markdown: false,
        });
        let binary = PreviewState::Ready(PreviewContent::Message(
            "Binary or non-UTF-8 content cannot be previewed safely.".into(),
        ));
        let (path, id) = identity("main.rs", b"fn main() {}\n");
        let refusal = |kind, id, path: &Path, preview: Option<&PreviewState>| {
            copy_contents_refusal(kind, id, path, preview)
        };
        assert_eq!(refusal(EntryKind::File, id, &path, Some(&text)), None);
        // The read decides while the preview is missing or still loading.
        assert_eq!(refusal(EntryKind::File, id, &path, None), None);
        assert_eq!(
            refusal(EntryKind::File, id, &path, Some(&PreviewState::Loading)),
            None
        );
        assert_eq!(
            refusal(EntryKind::File, id, &path, Some(&binary)),
            Some("Binary file")
        );

        // One byte over 1 MiB, without writing a megabyte of data.
        let big = fixture.0.join("big.log");
        fs::File::create(&big)
            .unwrap()
            .set_len(1024 * 1024 + 1)
            .unwrap();
        let big_id = Some(FileIdentity::of(&fs::metadata(&big).unwrap()));
        assert_eq!(
            refusal(EntryKind::File, big_id, &big, Some(&text)),
            Some("Too large to copy (over 1 MiB)")
        );
        let (limit, limit_id) = identity("limit.txt", &vec![b'a'; 1024 * 1024]);
        assert_eq!(refusal(EntryKind::File, limit_id, &limit, None), None);

        for name in ["pic.png", "scan.PDF"] {
            let (path, id) = identity(name, b"x");
            assert!(
                refusal(EntryKind::File, id, &path, None)
                    .unwrap()
                    .contains("Images and PDFs"),
                "{name}"
            );
        }
        assert!(refusal(EntryKind::File, None, &path, Some(&text)).is_some());
        for kind in [EntryKind::Directory, EntryKind::Symlink, EntryKind::Other] {
            assert!(refusal(kind, id, &path, Some(&text)).is_some());
        }
    }

    #[test]
    fn copy_summary_counts_lines_and_sizes_the_text() {
        assert_eq!(copy_summary(""), "Copied an empty file (0 B)");
        assert_eq!(copy_summary("one"), "Copied 1 line (3 B)");
        assert_eq!(copy_summary("one\r\ntwo\r\n"), "Copied 2 lines (10 B)");
        assert_eq!(copy_summary("\n\n\n"), "Copied 3 lines (3 B)");
        let text = "x".repeat(70) + "\n";
        assert_eq!(
            copy_summary(&text.repeat(88)),
            "Copied 88 lines (6.1 KiB)" // 6,248 bytes
        );
        assert_eq!(copy_summary(&"é".repeat(512)), "Copied 1 line (1.0 KiB)");
        // Just under 1 MiB must not read as 1024.0 KiB.
        assert_eq!(
            copy_summary(&"a".repeat(1024 * 1024 - 10)),
            "Copied 1 line (1.0 MiB)"
        );
        assert_eq!(
            copy_summary(&"a".repeat(1024 * 1024)),
            "Copied 1 line (1.0 MiB)"
        );
    }

    #[test]
    fn tab_cycles_within_the_pane_it_is_pressed_in() {
        let every_mode = [
            Mode::Search,
            Mode::Tree,
            Mode::Preview,
            Mode::Refresh,
            Mode::Hidden,
            Mode::RevealRoot,
            Mode::Copy,
            Mode::CopyContents,
            Mode::Reveal,
            Mode::Edit,
            Mode::Open,
        ];
        // Each control is in exactly one pane's cycle, so none is unreachable or doubled.
        for mode in every_mode {
            let in_tree = TREE_FOCUS_ORDER.contains(&mode);
            let in_preview = PREVIEW_FOCUS_ORDER.contains(&mode);
            assert!(in_tree != in_preview, "{mode:?}");
            assert_eq!(mode.in_preview_pane(), in_preview);
            assert_eq!(
                mode.focus_order(),
                if in_preview {
                    &PREVIEW_FOCUS_ORDER[..]
                } else {
                    &TREE_FOCUS_ORDER[..]
                }
            );
        }
        assert_eq!(
            TREE_FOCUS_ORDER.len() + PREVIEW_FOCUS_ORDER.len(),
            every_mode.len()
        );
        // The pane that receives the keys when it is focused starts on its main control.
        assert_eq!(Mode::Tree.focus_order()[1], Mode::Tree);
        assert_eq!(PREVIEW_FOCUS_ORDER[0], Mode::Preview);
        // Stepping forward from the last control wraps to the first, in both panes.
        for order in [&TREE_FOCUS_ORDER[..], &PREVIEW_FOCUS_ORDER[..]] {
            let last = *order.last().unwrap();
            assert_eq!(order[(order.len() - 1 + 1) % order.len()], order[0]);
            assert_eq!(last.focus_order(), order);
        }
    }

    #[test]
    fn toolbar_keeps_its_words_and_stays_reachable_with_tab() {
        let labels: Vec<_> = TOOLBAR.iter().map(|action| action.label).collect();
        assert_eq!(
            labels,
            [
                "Edit in Vim ↗",
                "Copy path",
                "Copy contents",
                "Reveal",
                "Open externally"
            ]
        );
        for (index, action) in TOOLBAR.iter().enumerate() {
            // The toolbar is in the Preview pane, so Tab there reaches it.
            assert!(
                PREVIEW_FOCUS_ORDER.contains(&action.mode),
                "{} cannot be reached with Tab",
                action.name
            );
            for other in &TOOLBAR[index + 1..] {
                assert_ne!(action.id, other.id);
                assert_ne!(action.mode, other.mode);
                assert_ne!(action.glyph, other.glyph);
                assert_ne!(action.name, other.name);
            }
        }
        for order in [&TREE_FOCUS_ORDER[..], &PREVIEW_FOCUS_ORDER[..]] {
            for (index, mode) in order.iter().enumerate() {
                assert!(!order[index + 1..].contains(mode), "{mode:?} twice");
            }
        }
    }

    #[test]
    fn icon_tooltips_name_the_action_and_carry_the_reason_it_is_unavailable() {
        assert_eq!(icon_tooltip("Reveal in Finder", None), "Reveal in Finder");
        assert_eq!(
            icon_tooltip("Edit in Vim", tooltip_hint(Mode::Edit)),
            "Edit in Vim · ⌘E"
        );
        assert_eq!(tooltip_hint(Mode::Copy), None);
        assert_eq!(tooltip_hint(Mode::Reveal), None);

        // A disabled Copy Contents shows the refusal that a click or Enter would.
        let fixture = Fixture::new();
        let big = fixture.0.join("big.log");
        fs::File::create(&big)
            .unwrap()
            .set_len(1024 * 1024 + 1)
            .unwrap();
        let identity = Some(FileIdentity::of(&fs::metadata(&big).unwrap()));
        let reason = copy_contents_refusal(EntryKind::File, identity, &big, None);
        assert_eq!(
            icon_tooltip("Copy contents", reason),
            "Copy contents · Too large to copy (over 1 MiB)"
        );
    }
}

/// The face a file panel is set in: SF Mono in the colorful themes, as they have always
/// drawn it; Native's interface face, with only file names, paths and file text in the
/// monospace accent.
fn panel_family() -> SharedString {
    if ui_text::is_native() {
        ui_text::ui_family()
    } else {
        "SF Mono".into()
    }
}

/// The face of a file panel's technical text: its file names, paths and file contents.
fn accent_family() -> SharedString {
    if ui_text::is_native() {
        ui_text::mono_family()
    } else {
        "SF Mono".into()
    }
}

#[cfg(test)]
mod kit_filter_tests {
    use super::*;
    use gpui::TestAppContext;
    use gpui_kit::test::TestWindowExt;

    struct VisibleFilters {
        explorer: Entity<FileExplorer>,
        left: Entity<FileExplorerSurface>,
        right: Entity<FileExplorerSurface>,
    }
    impl Render for VisibleFilters {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .flex()
                .gap(px(20.))
                .child(div().w(px(300.)).child(self.left.clone()))
                .child(div().w(px(300.)).child(self.right.clone()))
        }
    }

    #[gpui::test]
    fn kit_visible_filters_protect_composition_history_and_focus(cx: &mut TestAppContext) {
        use gpui::{ElementInputHandler, InputHandler};
        let (handle, pair) = crate::form_input::test_window(cx, |window, cx| {
            let explorer = cx.new(|cx| FileExplorer::new(window, cx));
            let left = explorer.update(cx, |explorer, cx| explorer.surface(301, window, cx));
            let right = explorer.update(cx, |explorer, cx| explorer.surface(302, window, cx));
            VisibleFilters {
                explorer,
                left,
                right,
            }
        });
        let (left, right) = cx
            .update_window(handle.into(), |_, window, app| {
                window.render_frame(app);
                let left = pair.read(app).left.read(app).input.clone();
                let right = pair.read(app).right.read(app).input.clone();
                assert_ne!(
                    left.read(app).input_bounds(),
                    right.read(app).input_bounds()
                );
                left.read(app).focus_handle(app).focus(window, app);
                window.render_frame(app);
                window.input("A", app);
                (left, right)
            })
            .unwrap();
        cx.run_until_parked();
        cx.background_executor.advance_clock(Duration::from_secs(3));
        cx.update_window(handle.into(), |_, window, app| {
            assert_eq!(right.read(app).value(), "A");
            window.render_frame(app);
            window.input("B", app);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, app| {
            assert_eq!(right.read(app).value(), "AB");
            right.read(app).focus_handle(app).focus(window, app);
            window.render_frame(app);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, app| {
            window.press("cmd-z", app);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, app| {
            assert_eq!(right.read(app).value(), "A");
            let mut handler =
                ElementInputHandler::new(right.read(app).input_bounds(), right.clone());
            handler.replace_and_mark_text_in_range(None, "日本", Some(2..2), window, app);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, app| {
            left.read(app).focus_handle(app).focus(window, app);
            window.render_frame(app);
        })
        .unwrap();
        cx.run_until_parked();
        let marked = cx
            .update_window(handle.into(), |_, window, app| {
                let marked = right.read(app).value();
                window.input("C", app);
                marked
            })
            .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, app| {
            assert_eq!(right.read(app).value(), marked);
            assert!(crate::form_input::is_composing(&right, window, app));
            assert!(left.read(app).focus_handle(app).is_focused(window));
            assert!(pair.read(app).explorer.read(app).root.is_none());
        })
        .unwrap();
    }

    #[gpui::test]
    fn kit_removed_filter_rejects_stale_events_and_reopens_without_subscriptions(
        cx: &mut TestAppContext,
    ) {
        let (handle, explorer) = crate::form_input::test_window(cx, FileExplorer::new);
        let stale = cx
            .update_window(handle.into(), |_, window, app| {
                let surface =
                    explorer.update(app, |explorer, cx| explorer.surface(201, window, cx));
                let state = surface.read(app).input.clone();
                explorer.update(app, |explorer, cx| {
                    explorer.filter_state = state.clone();
                    explorer.mode = Mode::Search;
                    explorer.retain_surfaces(&[], cx);
                    explorer.filter_event(&state, &InputEvent::Focus, window, cx);
                });
                state.update(app, |input, cx| {
                    input.replace_all("stale draft", window, cx)
                });
                state
            })
            .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, app| {
            explorer.update(app, |explorer, cx| {
                assert!(!explorer.owns_filter(&stale));
                assert_eq!(
                    explorer.filter_state.entity_id(),
                    explorer.base_filter_state.entity_id()
                );
                assert_eq!(explorer.filter.text, "");
                assert!(explorer.filter_surfaces.is_empty());
                assert!(matches!(explorer.mode, Mode::Tree));
                for id in 202..210 {
                    explorer.surface(id, window, cx);
                    explorer.retain_surfaces(&[], cx);
                    assert!(explorer.filter_surfaces.is_empty());
                    assert_eq!(explorer.filter_synced.len(), 1);
                    assert!(explorer.filter_sync_events.is_empty());
                }
            });
        })
        .unwrap();
    }

    #[gpui::test]
    fn kit_duplicate_filters_keep_distinct_persistent_geometry(cx: &mut TestAppContext) {
        let (handle, explorer) = crate::form_input::test_window(cx, FileExplorer::new);
        let (first_input, second_input) = cx
            .update_window(handle.into(), |_, window, app| {
                let first = explorer.update(app, |explorer, cx| explorer.surface(101, window, cx));
                let second = explorer.update(app, |explorer, cx| explorer.surface(102, window, cx));
                (
                    first.read(app).input.clone(),
                    second.read(app).input.clone(),
                )
            })
            .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, app| {
            assert_ne!(first_input.entity_id(), second_input.entity_id());
            first_input.update(app, |input, cx| input.replace("A🦀中", window, cx));
            first_input.update(app, |input, cx| input.set_selected_range(1..5, cx));
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, app| {
            assert_eq!(second_input.read(app).value(), "A🦀中");
            let remounted = explorer.update(app, |explorer, cx| explorer.surface(101, window, cx));
            assert_eq!(
                remounted.read(app).input.entity_id(),
                first_input.entity_id()
            );
            assert_eq!(first_input.read(app).selected_range(), 1..5);
            explorer.update(app, |explorer, cx| {
                explorer.retain_surfaces(&[101, 102], cx)
            });
            assert_eq!(explorer.read(app).filter_surfaces.len(), 2);
        })
        .unwrap();
    }

    #[gpui::test]
    fn kit_filter_persists_selection_and_owns_enter_escape_and_tab(cx: &mut TestAppContext) {
        let (handle, explorer) = crate::form_input::test_window(cx, FileExplorer::new);
        let state = cx
            .update_window(handle.into(), |_, window, app| {
                explorer.update(app, |explorer, cx| explorer.focus_search(window, cx));
                window.render_frame(app);
                window.input("A🦀中", app);
                let state = explorer.read(app).filter_state.clone();
                state.update(app, |state, cx| state.set_selected_range(1..5, cx));
                state
            })
            .unwrap();
        cx.run_until_parked();
        let identity = state.entity_id();
        cx.update_window(handle.into(), |_, window, app| {
            explorer.update(app, |_, cx| cx.notify());
            window.render_frame(app);
            assert_eq!(explorer.read(app).filter_state.entity_id(), identity);
            assert_eq!(state.read(app).selected_range(), 1..5);
            window.press("enter", app);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, app| {
            assert!(matches!(explorer.read(app).mode, Mode::Tree));
            assert_eq!(state.read(app).value(), "A🦀中");
            explorer.update(app, |explorer, cx| explorer.focus_search(window, cx));
            window.render_frame(app);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, app| {
            window.press("escape", app);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, app| {
            assert!(matches!(explorer.read(app).mode, Mode::Tree));
            assert_eq!(state.read(app).value(), "");
            explorer.update(app, |explorer, cx| explorer.focus_search(window, cx));
            window.render_frame(app);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, app| {
            window.press("tab", app);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, app| {
            assert!(!state.read(app).focus_handle(app).is_focused(window));
        })
        .unwrap();
    }
}
