//! A lazy filesystem browser scoped to the workspace's selected worktree.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashSet},
    fs,
    ops::Range,
    path::{Path, PathBuf},
};

use gpui::{
    AnyElement, Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler,
    EventEmitter, FocusHandle, HighlightStyle, IntoElement, KeyDownEvent, Pixels, Point, Render,
    ScrollStrategy, StyledText, UTF16Selection, UniformListScrollHandle, Window, canvas, div,
    prelude::*, px, rgb, uniform_list,
};

use crate::{theme, utf16_to_byte};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExplorerRoot {
    pub path: PathBuf,
    pub label: String,
}

pub enum FileExplorerEvent {
    Open(PathBuf),
    Reveal(PathBuf),
    /// The path is relative to the current worktree root.
    CopyRelativePath(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryKind {
    Directory,
    File,
    Symlink,
    Other,
}

#[derive(Clone, Debug)]
struct Entry {
    path: PathBuf,
    name: String,
    kind: EntryKind,
    hidden: bool,
}

/// Compare digit runs by magnitude, without parsing them into a bounded integer.
fn natural_cmp(left: &str, right: &str) -> Ordering {
    let left = left.to_lowercase();
    let right = right.to_lowercase();
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

fn read_directory(path: &Path) -> Result<Vec<Entry>, String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| format!("Cannot read this folder: {error}"))?;
    if metadata.file_type().is_symlink() {
        return Err("Symbolic links are not expanded. Use Open or Reveal instead.".into());
    }
    if !metadata.is_dir() {
        return Err("This folder is no longer a directory.".into());
    }
    let directory =
        fs::read_dir(path).map_err(|error| format!("Cannot read this folder: {error}"))?;
    let mut entries = Vec::new();
    for entry in directory {
        let entry = entry.map_err(|error| format!("Cannot list this folder: {error}"))?;
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            // A file removed while listing should not invalidate the whole directory.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("Cannot inspect this folder's files: {error}")),
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        entries.push(Entry {
            path: entry.path(),
            hidden: name.starts_with('.'),
            name,
            kind: if file_type.is_symlink() {
                EntryKind::Symlink
            } else if file_type.is_dir() {
                EntryKind::Directory
            } else if file_type.is_file() {
                EntryKind::File
            } else {
                EntryKind::Other
            },
        });
    }
    entries.sort_by(|a, b| {
        (a.kind != EntryKind::Directory)
            .cmp(&(b.kind != EntryKind::Directory))
            .then_with(|| natural_cmp(&a.name, &b.name))
            .then_with(|| a.path.cmp(&b.path))
    });
    Ok(entries)
}

fn read_directory_in(root: &Path, path: &Path) -> Result<Vec<Entry>, String> {
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
            return Err("Symbolic links are not expanded. Use Open or Reveal instead.".into());
        }
    }
    read_directory(path)
}

#[derive(Clone, Default)]
struct DirectoryState {
    entries: Vec<Entry>,
    loading: bool,
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
    if let Some(state) = directories.get(root) {
        for entry in state.entries.iter().rev() {
            stack.push((entry.clone(), 0));
        }
    }
    while let Some((entry, depth)) = stack.pop() {
        if (!show_hidden && entry.hidden) || (!query.is_empty() && !matches.contains(&entry.path)) {
            continue;
        }
        rows.push(TreeRow {
            path: entry.path.clone(),
            label: entry.name,
            depth,
            kind: RowKind::Entry(entry.kind),
        });
        if entry.kind != EntryKind::Directory
            || (query.is_empty() && !expanded.contains(&entry.path))
        {
            continue;
        }
        if let Some(state) = directories.get(&entry.path) {
            if (state.loading && state.entries.is_empty()) || state.error.is_some() {
                rows.push(TreeRow {
                    path: entry.path.clone(),
                    label: state.error.clone().unwrap_or_else(|| "Loading…".into()),
                    depth: depth + 1,
                    kind: if state.error.is_some() {
                        RowKind::Error
                    } else {
                        RowKind::Status
                    },
                });
            }
            for child in state.entries.iter().rev() {
                stack.push((child.clone(), depth + 1));
            }
        }
    }
    rows
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Search,
    Tree,
    Refresh,
    Hidden,
    RevealRoot,
    Copy,
    Reveal,
    Open,
}

const FOCUS_ORDER: [Mode; 8] = [
    Mode::Search,
    Mode::Tree,
    Mode::Refresh,
    Mode::Hidden,
    Mode::RevealRoot,
    Mode::Copy,
    Mode::Reveal,
    Mode::Open,
];

#[derive(Default)]
struct FilterInput {
    text: String,
    selection: Range<usize>,
    marked: Option<Range<usize>>,
}

impl FilterInput {
    fn replace(&mut self, range: Option<Range<usize>>, text: &str) {
        let range = range
            .map(|range| {
                utf16_to_byte(&self.text, range.start)..utf16_to_byte(&self.text, range.end)
            })
            .or(self.marked.take())
            .unwrap_or_else(|| self.selection.clone());
        let text = text.replace(['\n', '\r'], "");
        self.text.replace_range(range.clone(), &text);
        let end = range.start + text.len();
        self.selection = end..end;
    }
}

pub struct FileExplorer {
    root: Option<ExplorerRoot>,
    directories: BTreeMap<PathBuf, DirectoryState>,
    expanded: HashSet<PathBuf>,
    selected: Option<PathBuf>,
    show_hidden: bool,
    filter: FilterInput,
    focus: FocusHandle,
    mode: Mode,
    scroll: UniformListScrollHandle,
    generation: u64,
    next_request: u64,
}

impl EventEmitter<FileExplorerEvent> for FileExplorer {}

impl FileExplorer {
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.observe_global::<theme::Appearance>(|_, cx| cx.notify())
            .detach();
        Self {
            root: None,
            directories: BTreeMap::new(),
            expanded: HashSet::new(),
            selected: None,
            show_hidden: false,
            filter: FilterInput::default(),
            focus: cx.focus_handle(),
            mode: Mode::Tree,
            scroll: UniformListScrollHandle::new(),
            generation: 0,
            next_request: 0,
        }
    }

    pub fn set_root(&mut self, root: Option<ExplorerRoot>, cx: &mut Context<Self>) {
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
        self.directories.clear();
        self.expanded.clear();
        self.selected = None;
        self.filter = FilterInput::default();
        self.scroll = UniformListScrollHandle::new();
        if let Some(root) = &self.root {
            self.load(root.path.clone(), cx);
        }
        cx.notify();
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        // A slow/network filesystem must not be restarted by every refresh tick.
        if self.directories.values().any(|state| state.loading) {
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        if let Some(root) = &self.root {
            self.load(root.path.clone(), cx);
        }
        cx.notify();
    }

    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = Mode::Tree;
        self.focus.focus(window, cx);
        cx.notify();
    }

    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = Mode::Search;
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn load(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self
            .directories
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
        let state = self.directories.entry(path.clone()).or_default();
        state.loading = true;
        state.error = None;
        state.request = request;
        let work_path = path.clone();
        let work = cx
            .background_executor()
            .spawn(async move { read_directory_in(&root_path, &work_path) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |explorer, cx| {
                if explorer.generation != generation
                    || !explorer
                        .directories
                        .get(&path)
                        .is_some_and(|state| state.request == request)
                {
                    return;
                }
                let state = match result {
                    Ok(entries) => DirectoryState {
                        entries,
                        request,
                        ..Default::default()
                    },
                    Err(error) => DirectoryState {
                        error: Some(error),
                        request,
                        ..Default::default()
                    },
                };
                let reload = state
                    .entries
                    .iter()
                    .filter(|entry| {
                        entry.kind == EntryKind::Directory
                            && (explorer.expanded.contains(&entry.path)
                                || explorer.directories.contains_key(&entry.path))
                    })
                    .map(|entry| entry.path.clone())
                    .collect::<Vec<_>>();
                let children = state
                    .entries
                    .iter()
                    .filter(|entry| entry.kind == EntryKind::Directory)
                    .map(|entry| entry.path.clone())
                    .collect::<HashSet<_>>();
                // Removed folders, and folders replaced by symlinks, lose their cached subtree.
                let stale = explorer
                    .directories
                    .keys()
                    .filter(|child| {
                        child.parent() == Some(path.as_path()) && !children.contains(*child)
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                explorer
                    .directories
                    .retain(|child, _| !stale.iter().any(|stale| child.starts_with(stale)));
                explorer
                    .expanded
                    .retain(|child| !stale.iter().any(|stale| child.starts_with(stale)));
                explorer.directories.insert(path, state);
                for path in reload {
                    explorer.load(path, cx);
                }
                explorer.ensure_selection();
                cx.notify();
            });
        })
        .detach();
    }

    fn rows(&self) -> Vec<TreeRow> {
        self.root
            .as_ref()
            .map(|root| {
                visible_rows(
                    &root.path,
                    &self.directories,
                    &self.expanded,
                    self.show_hidden,
                    &self.filter.text,
                )
            })
            .unwrap_or_default()
    }

    fn ensure_selection(&mut self) {
        let rows = self.rows();
        if !rows.iter().any(|row| {
            matches!(row.kind, RowKind::Entry(_)) && Some(&row.path) == self.selected.as_ref()
        }) {
            self.selected = rows
                .iter()
                .find(|row| matches!(row.kind, RowKind::Entry(_)))
                .map(|row| row.path.clone());
        }
    }

    fn toggle(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.expanded.remove(path) {
            self.ensure_selection();
        } else {
            self.expanded.insert(path.to_owned());
            if !self.directories.contains_key(path) {
                self.load(path.to_owned(), cx);
            }
        }
        cx.notify();
    }

    fn activate(&mut self, row: &TreeRow, cx: &mut Context<Self>) {
        match row.kind {
            RowKind::Entry(EntryKind::Directory) => self.toggle(&row.path, cx),
            RowKind::Entry(_) => cx.emit(FileExplorerEvent::Open(row.path.clone())),
            RowKind::Error => self.load(row.path.clone(), cx),
            RowKind::Status => {}
        }
    }

    fn action(&mut self, mode: Mode, cx: &mut Context<Self>) {
        match mode {
            Mode::Refresh => self.refresh(cx),
            Mode::Hidden => {
                self.show_hidden = !self.show_hidden;
                self.ensure_selection();
            }
            Mode::RevealRoot => {
                if let Some(root) = &self.root {
                    cx.emit(FileExplorerEvent::Reveal(root.path.clone()));
                }
            }
            Mode::Open | Mode::Reveal | Mode::Copy => {
                if let Some(path) = &self.selected {
                    match mode {
                        Mode::Open => cx.emit(FileExplorerEvent::Open(path.clone())),
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
            _ => {}
        }
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let platform = event.keystroke.modifiers.platform;
        if platform && key == "f" {
            self.focus_search(window, cx);
        } else if platform && key == "r" {
            self.refresh(cx);
        } else if platform && key == "." {
            self.action(Mode::Hidden, cx);
        } else if key == "tab" {
            let index = FOCUS_ORDER
                .iter()
                .position(|mode| *mode == self.mode)
                .unwrap_or(0);
            let step = if event.keystroke.modifiers.shift {
                FOCUS_ORDER.len() - 1
            } else {
                1
            };
            self.mode = FOCUS_ORDER[(index + step) % FOCUS_ORDER.len()];
        } else if key == "escape" && self.mode == Mode::Search {
            self.filter = FilterInput::default();
            self.mode = Mode::Tree;
            self.ensure_selection();
        } else if self.mode == Mode::Search {
            if key == "enter" || key == "down" {
                self.mode = Mode::Tree;
                self.ensure_selection();
            } else if platform && key == "a" {
                self.filter.selection = 0..self.filter.text.len();
            } else if platform && (key == "c" || key == "x") {
                cx.write_to_clipboard(ClipboardItem::new_string(
                    self.filter.text[self.filter.selection.clone()].to_owned(),
                ));
                if key == "x" {
                    self.filter.replace(None, "");
                    self.ensure_selection();
                }
            } else if platform && key == "v" {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.filter.replace(None, &text);
                    self.ensure_selection();
                }
            } else if key == "backspace" || key == "delete" {
                if self.filter.selection.is_empty() {
                    let cursor = self.filter.selection.end;
                    self.filter.selection = if key == "backspace" {
                        self.filter.text[..cursor]
                            .char_indices()
                            .next_back()
                            .map(|(offset, _)| offset)
                            .unwrap_or(0)..cursor
                    } else {
                        cursor
                            ..self.filter.text[cursor..]
                                .chars()
                                .next()
                                .map(|ch| cursor + ch.len_utf8())
                                .unwrap_or(cursor)
                    };
                }
                self.filter.replace(None, "");
                self.ensure_selection();
            } else if matches!(key, "left" | "right" | "home" | "end") {
                let cursor = self.filter.selection.end;
                let offset = match key {
                    "home" => 0,
                    "end" => self.filter.text.len(),
                    "left" if platform => 0,
                    "right" if platform => self.filter.text.len(),
                    "left" => self.filter.text[..cursor]
                        .char_indices()
                        .next_back()
                        .map(|(offset, _)| offset)
                        .unwrap_or(0),
                    _ => self.filter.text[cursor..]
                        .chars()
                        .next()
                        .map(|ch| cursor + ch.len_utf8())
                        .unwrap_or(cursor),
                };
                self.filter.selection = offset..offset;
                self.filter.marked = None;
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
                    self.selected = Some(navigable[index].1.path.clone());
                    self.scroll
                        .scroll_to_item(navigable[index].0, ScrollStrategy::Nearest);
                }
                "enter" | "space" => {
                    if let Some((_, row)) = navigable.get(index) {
                        self.activate(row, cx);
                    }
                }
                "right" => {
                    if let Some((_, row)) = navigable.get(index) {
                        if row.kind == RowKind::Entry(EntryKind::Directory) {
                            if !self.expanded.contains(&row.path) {
                                self.toggle(&row.path, cx);
                            } else if let Some((row_index, child)) = navigable
                                .get(index + 1)
                                .filter(|(_, child)| child.depth > row.depth)
                            {
                                self.selected = Some(child.path.clone());
                                self.scroll
                                    .scroll_to_item(*row_index, ScrollStrategy::Nearest);
                            }
                        }
                    }
                }
                "left" => {
                    if let Some((_, row)) = navigable.get(index) {
                        if self.expanded.contains(&row.path) {
                            self.toggle(&row.path, cx);
                        } else if let Some(parent) = row
                            .path
                            .parent()
                            .filter(|parent| navigable.iter().any(|(_, row)| row.path == *parent))
                        {
                            self.selected = Some(parent.to_owned());
                        }
                    }
                }
                "c" if platform => self.action(Mode::Copy, cx),
                _ => return,
            }
        } else if key == "enter" || key == "space" {
            self.action(self.mode, cx);
        } else {
            return;
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
        let colors = theme::palette(cx);
        let active = self.focus.is_focused(window) && self.mode == mode;
        let available = match mode {
            Mode::Copy | Mode::Reveal | Mode::Open => self.selected.is_some(),
            _ => self.root.is_some(),
        };
        div()
            .id(id)
            .px(px(7.0))
            .py(px(5.0))
            .flex_none()
            .cursor_pointer()
            .border_1()
            .border_color(rgb(if active { colors.gold } else { colors.divider }))
            .text_color(rgb(if active {
                colors.gold
            } else if available {
                colors.cyan
            } else {
                colors.muted
            }))
            .child(label.to_owned())
            .on_click(cx.listener(move |view, _, window, cx| {
                view.mode = mode;
                view.focus.focus(window, cx);
                if available {
                    view.action(mode, cx);
                }
                cx.notify();
            }))
            .into_any_element()
    }

    fn search(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme::palette(cx);
        let active = self.focus.is_focused(window) && self.mode == Mode::Search;
        let mut text = if self.filter.text.is_empty() {
            "Filter loaded files…".to_owned()
        } else {
            self.filter.text.clone()
        };
        let mut highlights = Vec::new();
        if active && !self.filter.text.is_empty() {
            if self.filter.selection.is_empty() {
                let cursor = self.filter.selection.end;
                text.insert(cursor, '▌');
            } else {
                highlights.push((
                    self.filter.selection.clone(),
                    HighlightStyle {
                        color: Some(rgb(colors.cyan).into()),
                        background_color: Some(rgb(colors.divider).into()),
                        ..Default::default()
                    },
                ));
            }
        }
        let entity = cx.entity();
        let focus = self.focus.clone();
        let handler = active.then(|| {
            canvas(
                |_, _, _| {},
                move |bounds, _, window, cx| {
                    window.handle_input(
                        &focus,
                        ElementInputHandler::new(bounds, entity.clone()),
                        cx,
                    );
                },
            )
            .absolute()
            .inset_0()
        });
        div()
            .id("file-explorer-filter")
            .relative()
            .h(px(29.0))
            .px(px(8.0))
            .flex()
            .items_center()
            .min_w_0()
            .bg(rgb(colors.bg))
            .border_1()
            .border_color(rgb(if active { colors.cyan } else { colors.divider }))
            .text_color(rgb(if self.filter.text.is_empty() {
                colors.muted
            } else {
                colors.text
            }))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(StyledText::new(text).with_highlights(highlights)),
            )
            .children(handler)
            .on_click(cx.listener(|view, _, window, cx| view.focus_search(window, cx)))
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
        let expanded = self.expanded.contains(&row.path) || !self.filter.text.trim().is_empty();
        let icon = match row.kind {
            RowKind::Entry(EntryKind::Directory) if expanded => "▾",
            RowKind::Entry(EntryKind::Directory) => "▸",
            RowKind::Entry(EntryKind::Symlink) => "↗",
            RowKind::Entry(_) => "·",
            RowKind::Error => "!",
            RowKind::Status => "…",
        };
        let label = row.label.clone();
        let clicked_row = row.clone();
        let color = match row.kind {
            RowKind::Error => colors.gold,
            RowKind::Status => colors.muted,
            RowKind::Entry(EntryKind::Directory) => colors.cyan,
            RowKind::Entry(EntryKind::Symlink) => colors.magenta,
            _ => colors.text,
        };
        div()
            .id(("file-explorer-row", index))
            .h(px(27.0))
            .w_full()
            .pl(px(8.0 + row.depth as f32 * 14.0))
            .pr(px(8.0))
            .flex()
            .items_center()
            .gap(px(6.0))
            .cursor_pointer()
            .bg(rgb(if selected {
                colors.panel_active
            } else {
                colors.panel
            }))
            .border_l_1()
            .border_color(rgb(if selected {
                if self.mode == Mode::Tree && self.focus.is_focused(window) {
                    colors.gold
                } else {
                    colors.cyan
                }
            } else {
                colors.panel
            }))
            .hover(|style| style.bg(rgb(colors.panel_active)))
            .child(
                div()
                    .w(px(10.0))
                    .flex_none()
                    .text_color(rgb(color))
                    .child(icon),
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
                    .text_size(px(8.0))
                    .text_color(rgb(colors.muted))
                    .child("LINK")
            }))
            .on_click(cx.listener(move |view, _, window, cx| {
                view.mode = Mode::Tree;
                view.focus.focus(window, cx);
                if matches!(clicked_row.kind, RowKind::Entry(_)) {
                    view.selected = Some(clicked_row.path.clone());
                }
                view.activate(&clicked_row, cx);
                cx.notify();
            }))
            .into_any_element()
    }
}

impl Render for FileExplorer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        let rows = self.rows();
        let item_count = rows
            .iter()
            .filter(|row| matches!(row.kind, RowKind::Entry(_)))
            .count();
        let root_state = self
            .root
            .as_ref()
            .and_then(|root| self.directories.get(&root.path));
        let message = if self.root.is_none() {
            Some("Select a worktree to browse its files.".to_owned())
        } else if root_state.is_some_and(|state| state.loading && state.entries.is_empty()) {
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
        div()
            .id("file-explorer")
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .track_focus(&self.focus)
            .key_context("FileExplorer")
            .on_key_down(cx.listener(Self::key_down))
            .bg(rgb(colors.panel))
            .text_color(rgb(colors.text))
            .font_family("SF Mono")
            .text_size(px(11.0))
            .child(
                div()
                    .flex_none()
                    .p(px(10.0))
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .border_b_1()
                    .border_color(rgb(colors.divider))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .text_color(rgb(colors.cyan))
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
                                    .text_size(px(9.0))
                                    .flex_none()
                                    .child(format!("{item_count:02} ITEMS")),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(5.0))
                            .text_size(px(9.0))
                            .child(self.button(
                                "file-explorer-refresh",
                                "↻ REFRESH",
                                Mode::Refresh,
                                window,
                                cx,
                            ))
                            .child(self.button(
                                "file-explorer-hidden",
                                if self.show_hidden {
                                    "● HIDDEN"
                                } else {
                                    "○ HIDDEN"
                                },
                                Mode::Hidden,
                                window,
                                cx,
                            ))
                            .child(self.button(
                                "file-explorer-reveal-root",
                                "↗ WORKTREE",
                                Mode::RevealRoot,
                                window,
                                cx,
                            )),
                    )
                    .child(self.search(window, cx)),
            )
            .children(message.map(|message| {
                div()
                    .p(px(12.0))
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
                    .p(px(8.0))
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .border_t_1()
                    .border_color(rgb(colors.divider))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_color(rgb(colors.muted))
                            .text_size(px(10.0))
                            .child(relative),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(5.0))
                            .text_size(px(9.0))
                            .child(self.button(
                                "file-explorer-copy",
                                "COPY PATH",
                                Mode::Copy,
                                window,
                                cx,
                            ))
                            .child(self.button(
                                "file-explorer-reveal",
                                "REVEAL",
                                Mode::Reveal,
                                window,
                                cx,
                            ))
                            .child(self.button(
                                "file-explorer-open",
                                "OPEN",
                                Mode::Open,
                                window,
                                cx,
                            )),
                    ),
            )
    }
}

impl EntityInputHandler for FileExplorer {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let start = utf16_to_byte(&self.filter.text, range.start);
        let end = utf16_to_byte(&self.filter.text, range.end);
        *actual = Some(
            self.filter.text[..start].encode_utf16().count()
                ..self.filter.text[..end].encode_utf16().count(),
        );
        Some(self.filter.text[start..end].to_owned())
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.filter.text[..self.filter.selection.start]
                .encode_utf16()
                .count()
                ..self.filter.text[..self.filter.selection.end]
                    .encode_utf16()
                    .count(),
            reversed: false,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.filter.marked.as_ref().map(|range| {
            self.filter.text[..range.start].encode_utf16().count()
                ..self.filter.text[..range.end].encode_utf16().count()
        })
    }
    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.filter.marked = None;
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.mode != Mode::Search {
            return;
        }
        self.filter.replace(range, text);
        self.ensure_selection();
        cx.notify();
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.mode != Mode::Search {
            return;
        }
        self.filter.replace(range, text);
        let length = text.replace(['\n', '\r'], "").len();
        let end = self.filter.selection.end;
        self.filter.marked = (length > 0).then_some(end - length..end);
        self.ensure_selection();
        cx.notify();
    }
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(bounds)
    }
    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.filter.text.encode_utf16().count())
    }
    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.filter.text.encode_utf16().count())
    }
    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        self.mode == Mode::Search
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
        let entries = read_directory(&fixture.0).unwrap();
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
        let entries = read_directory(&fixture.0).unwrap();
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
        fs::set_permissions(&path, fs::Permissions::from_mode(0)).unwrap();
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
                    entries: read_directory(path).unwrap(),
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
}
