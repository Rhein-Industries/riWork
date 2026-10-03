//! The mouse side of terminal links: while ⌘ is held, find what the pointer is over and show the
//! pointing hand; on a ⌘-click, open it.
//!
//! Ghostty gets its mouse events from GPUI (the adapter forwards them), so a click that opened
//! a link must not also reach it, or it would start a selection and, on release, open the raw
//! text with `open`. The wrapper below sees events first (the capture phase) and keeps those
//! from the terminal. A click that is not on a link passes through untouched, so Ghostty's own
//! OSC 8 links still work.
//!
//! Deciding takes a tmux call, so it is done ahead of the click: ⌘ held over a terminal reads the
//! screen (at most every half second) and works out what the cell under the pointer links to,
//! off the UI thread. The click uses that answer. Only a click with no answer yet waits, for a
//! moment, for its own.

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use gpui::{
    AnyElement, Bounds, Context, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, Window, canvas, div, prelude::*,
};

use super::{
    Bases, CellHint, Link, OpenMode, PaneView, ResolvedPath, open_mode, open_refusal, short_name,
    within,
};
use crate::{
    PaneId, TabId, Workspace, file_explorer_root, file_preview,
    layouts::PanelKind,
    theme::{self, GhosttyPadding},
};

/// How long a capture of the screen answers for. The screen moves while an agent writes, but a
/// pointer being moved over it for a second sees little of that.
const VIEW_LIFETIME: Duration = Duration::from_millis(500);
/// How long what the pointer was over stands for a click on the same cell. Output can move the
/// text under a pointer that is still, so an old answer is worked out again.
const HOVER_LIFETIME: Duration = Duration::from_millis(600);
/// How long a click with no answer ready waits for one before it is given to the terminal.
const CLICK_WAIT: Duration = Duration::from_millis(400);
/// A failed capture (the session is gone, tmux is wedged) is not asked again at once.
const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(2);
/// How long after a click that was kept from the terminal its release is kept too. It follows at
/// once; the window only bounds the harm if it never comes to the same tab.
const RELEASE_WINDOW: Duration = Duration::from_secs(2);

/// What a window knows about links: where the pointer is, what it was last found over, and the
/// screens read for it.
#[derive(Default)]
pub struct LinkState {
    /// The last mouse event over a terminal. A ⌘ press brings no position of its own.
    pointer: Option<Pointer>,
    views: HashMap<TabId, CachedView>,
    /// Tabs whose screen is being read.
    fetching: HashSet<TabId>,
    failed_at: HashMap<TabId, Instant>,
    hover: Option<Hover>,
    /// Numbers the answers being worked out, so that a late one for an old position is dropped.
    probe: u64,
    /// A click on this tab was kept from the terminal; its release must be too.
    swallow_release: Option<(TabId, Instant)>,
    padding: Option<(Instant, GhosttyPadding)>,
    /// What the window's terminals have shown about their cell size.
    cells: CellHint,
    /// Each terminal's bounds as laid out, written while painting and read by events.
    bounds: RefCell<HashMap<TabId, Rc<Cell<Bounds<Pixels>>>>>,
}

#[derive(Clone)]
struct Pointer {
    tab_id: TabId,
    pane_id: PaneId,
    position: Point<Pixels>,
    bounds: Bounds<Pixels>,
    scale: f64,
    modifiers: Modifiers,
}

impl Pointer {
    /// The cell under the pointer in a terminal laid out as `view` says.
    fn cell(
        &self,
        view: &PaneView,
        padding: &GhosttyPadding,
        hint: &mut CellHint,
    ) -> Option<(u32, u32)> {
        let points = |pixels: Pixels| f64::from(pixels.as_f32());
        super::cell_under_pointer(
            (
                points(self.bounds.size.width),
                points(self.bounds.size.height),
            ),
            (
                points(self.position.x - self.bounds.origin.x),
                points(self.position.y - self.bounds.origin.y),
            ),
            self.scale,
            (view.cols, view.rows),
            padding,
            hint,
        )
    }
}

struct CachedView {
    view: Arc<PaneView>,
    at: Instant,
}

/// What the pointer was found over: a link, or nothing worth one.
struct Hover {
    tab_id: TabId,
    /// Column and row.
    cell: (u32, u32),
    link: Option<Link>,
    at: Instant,
}

impl LinkState {
    fn bounds_cell(&self, tab_id: TabId) -> Rc<Cell<Bounds<Pixels>>> {
        self.bounds.borrow_mut().entry(tab_id).or_default().clone()
    }

    /// Whether the pointer is over a link in this tab, which is when it shows a hand.
    pub fn over_link(&self, tab_id: TabId) -> bool {
        self.hover
            .as_ref()
            .is_some_and(|hover| hover.tab_id == tab_id && hover.link.is_some())
    }

    fn padding(&mut self) -> GhosttyPadding {
        if let Some((at, padding)) = self.padding
            && at.elapsed() < Duration::from_secs(5)
        {
            return padding;
        }
        let padding = theme::read_ghostty_padding();
        self.padding = Some((Instant::now(), padding));
        padding
    }

    /// Whether a release on this tab is the end of a click that was kept from the terminal. Asking
    /// uses the answer up.
    fn take_release(&mut self, tab_id: TabId) -> bool {
        self.swallow_release
            .take()
            .is_some_and(|(tab, at)| tab == tab_id && at.elapsed() < RELEASE_WINDOW)
    }

    /// What a recent hover found for this very cell, whether a link or nothing.
    fn answer_for(&self, tab_id: TabId, cell: (u32, u32)) -> Option<Option<Link>> {
        self.hover
            .as_ref()
            .filter(|hover| {
                hover.tab_id == tab_id && hover.cell == cell && hover.at.elapsed() < HOVER_LIFETIME
            })
            .map(|hover| hover.link.clone())
    }
}

/// What a worker needs to turn a screen and a pointer into a link.
#[derive(Clone)]
struct LinkContext {
    pointer: Pointer,
    padding: GhosttyPadding,
    cells: CellHint,
    /// Where relative paths start when tmux does not say where the shell is.
    fallback_cwd: PathBuf,
    root: Option<PathBuf>,
    home: Option<PathBuf>,
}

impl LinkContext {
    /// The cell under the pointer in a screen that may have just been read, and what it links to.
    fn resolve(&self, view: &PaneView) -> Option<Link> {
        let mut cells = self.cells;
        let cell = self.pointer.cell(view, &self.padding, &mut cells)?;
        self.resolve_cell(view, cell)
    }

    /// What a cell links to.
    fn resolve_cell(&self, view: &PaneView, cell: (u32, u32)) -> Option<Link> {
        let cwd = if view.cwd.as_os_str().is_empty() {
            self.fallback_cwd.as_path()
        } else {
            view.cwd.as_path()
        };
        let bases = Bases {
            cwd,
            root: self.root.as_deref(),
            home: self.home.as_deref(),
        };
        view.link_at(cell.1, cell.0, &bases)
    }
}

impl Workspace {
    /// A live terminal of a local shell, wrapped to find links under the pointer and to take
    /// ⌘-clicks on them. Another Mac's terminal is not wrapped: its tmux is not this one.
    pub(crate) fn terminal_link_layer(
        &self,
        pane_id: PaneId,
        tab_id: TabId,
        terminal: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let bounds = self.terminal_links.bounds_cell(tab_id);
        let measured = bounds.clone();
        div()
            .id(("terminal-links", tab_id))
            .size_full()
            .min_w_0()
            .min_h_0()
            .relative()
            .when(self.terminal_links.over_link(tab_id), |layer| {
                layer.cursor_pointer()
            })
            .child(terminal)
            .child(
                canvas(move |bounds, _, _| measured.set(bounds), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .capture_any_mouse_down(cx.listener(
                move |workspace, event: &MouseDownEvent, window, cx| {
                    workspace.terminal_link_pressed(pane_id, tab_id, event, window, cx);
                },
            ))
            .capture_any_mouse_up(cx.listener(move |workspace, event: &MouseUpEvent, _, cx| {
                workspace.terminal_link_released(tab_id, event, cx);
            }))
            .on_mouse_move(
                cx.listener(move |workspace, event: &MouseMoveEvent, window, cx| {
                    let pointer = Pointer {
                        tab_id,
                        pane_id,
                        position: event.position,
                        bounds: bounds.get(),
                        scale: f64::from(window.scale_factor()),
                        modifiers: event.modifiers,
                    };
                    workspace.terminal_link_moved(pointer, cx);
                }),
            )
            // Typing must not end the hover: ⌘ is a key, and the pointer rests where it was.
            .hover_listener_mode(gpui::HoverListenerMode::InputModalityIndependent)
            .on_hover(cx.listener(move |workspace, hovered: &bool, _, cx| {
                if !*hovered {
                    workspace.terminal_link_left(tab_id, cx);
                }
            }))
            .into_any_element()
    }

    /// The shell a tab shows, if it is one of this Mac's.
    fn link_shell(&self, pane_id: PaneId, tab_id: TabId) -> Option<String> {
        self.panes
            .get(&pane_id)?
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)?
            .shell_id()
            .map(str::to_owned)
    }

    fn link_context(&mut self, pointer: Pointer) -> LinkContext {
        let root = file_explorer_root(
            &self.state,
            &self.project_id,
            self.selected_worktree_id.as_deref(),
        )
        .map(|root| root.path);
        LinkContext {
            pointer,
            padding: self.terminal_links.padding(),
            cells: self.terminal_links.cells,
            fallback_cwd: root.clone().unwrap_or_else(|| self.cwd.clone()),
            root,
            home: std::env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .map(PathBuf::from),
        }
    }

    fn terminal_link_moved(&mut self, pointer: Pointer, cx: &mut Context<Self>) {
        let held = open_mode(pointer.modifiers).is_some();
        self.terminal_links.pointer = Some(pointer);
        if held {
            self.probe_link(cx);
        } else {
            // Without ⌘ held nothing is read: the pointer only moves.
            self.clear_link_hover(cx);
        }
    }

    /// ⌘ was pressed or released. Called for the whole window (from its root element, which keys
    /// reach whichever terminal or panel has them), because a press brings no position of its own:
    /// the pointer is where it was last seen over a terminal.
    pub(crate) fn terminal_link_modifiers(
        &mut self,
        modifiers: Modifiers,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab_id) = self
            .terminal_links
            .pointer
            .as_ref()
            .map(|pointer| pointer.tab_id)
        else {
            return;
        };
        // The layout may have moved under a pointer that rests.
        let bounds = self.terminal_links.bounds_cell(tab_id).get();
        if let Some(pointer) = self.terminal_links.pointer.as_mut() {
            pointer.modifiers = modifiers;
            pointer.bounds = bounds;
            pointer.scale = f64::from(window.scale_factor());
        }
        if open_mode(modifiers).is_some() {
            self.probe_link(cx);
        } else {
            self.clear_link_hover(cx);
        }
    }

    fn terminal_link_left(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if self
            .terminal_links
            .pointer
            .as_ref()
            .is_some_and(|pointer| pointer.tab_id == tab_id)
        {
            self.terminal_links.pointer = None;
            self.clear_link_hover(cx);
        }
    }

    /// Forget what the pointer was over; repaint if that was a link, so the hand goes.
    fn clear_link_hover(&mut self, cx: &mut Context<Self>) {
        // A result still being worked out is for a position the pointer has left.
        self.terminal_links.probe += 1;
        if let Some(hover) = self.terminal_links.hover.take()
            && hover.link.is_some()
        {
            cx.notify();
        }
    }

    /// Find out what the pointer is over, reading the screen first if it has not been read
    /// lately. Nothing here waits: answers arrive later and come back through this function.
    fn probe_link(&mut self, cx: &mut Context<Self>) {
        let Some(pointer) = self.terminal_links.pointer.clone() else {
            return;
        };
        if open_mode(pointer.modifiers).is_none() {
            return;
        }
        let Some(shell_id) = self.link_shell(pointer.pane_id, pointer.tab_id) else {
            return;
        };
        let padding = self.terminal_links.padding();
        // The cell comes from the layout of the last capture, however old: only its text goes
        // stale, and the pointer often stays in one cell.
        let Some(cached) = self.terminal_links.views.get(&pointer.tab_id) else {
            self.fetch_link_view(pointer.tab_id, shell_id, cx);
            return;
        };
        let (view, view_at) = (cached.view.clone(), cached.at);
        let Some(cell) = pointer.cell(&view, &padding, &mut self.terminal_links.cells) else {
            self.set_link_hover(pointer.tab_id, None, None, cx);
            return;
        };
        if self
            .terminal_links
            .answer_for(pointer.tab_id, cell)
            .is_some()
        {
            return;
        }
        if view_at.elapsed() >= VIEW_LIFETIME {
            self.fetch_link_view(pointer.tab_id, shell_id, cx);
            return;
        }
        self.terminal_links.probe += 1;
        let probe = self.terminal_links.probe;
        let tab_id = pointer.tab_id;
        let context = self.link_context(pointer);
        cx.spawn(async move |workspace, cx| {
            let link = cx
                .background_executor()
                .spawn(async move { context.resolve_cell(&view, cell) })
                .await;
            let _ = workspace.update(cx, |workspace, cx| {
                // A newer position is being worked out, or the pointer left.
                if workspace.terminal_links.probe == probe {
                    workspace.set_link_hover(tab_id, Some(cell), link, cx);
                    // The pointer may have moved while this was worked out.
                    workspace.probe_link(cx);
                }
            });
        })
        .detach();
    }

    fn set_link_hover(
        &mut self,
        tab_id: TabId,
        cell: Option<(u32, u32)>,
        link: Option<Link>,
        cx: &mut Context<Self>,
    ) {
        let had_link = self.terminal_links.over_link(tab_id);
        self.terminal_links.hover = cell.map(|cell| Hover {
            tab_id,
            cell,
            link,
            at: Instant::now(),
        });
        if had_link != self.terminal_links.over_link(tab_id) {
            cx.notify();
        }
    }

    /// Read the screen of a tab's shell off the UI thread, then look again at the pointer.
    fn fetch_link_view(&mut self, tab_id: TabId, shell_id: String, cx: &mut Context<Self>) {
        if self
            .terminal_links
            .failed_at
            .get(&tab_id)
            .is_some_and(|at| at.elapsed() < RETRY_AFTER_FAILURE)
            || !self.terminal_links.fetching.insert(tab_id)
        {
            return;
        }
        let sessions = self.sessions.clone();
        cx.spawn(async move |workspace, cx| {
            let view = cx
                .background_executor()
                .spawn(async move {
                    sessions
                        .capture_link_view(&shell_id)
                        .and_then(|raw| PaneView::parse(&raw))
                })
                .await;
            let _ = workspace.update(cx, |workspace, cx| {
                let links = &mut workspace.terminal_links;
                links.fetching.remove(&tab_id);
                match view {
                    Ok(view) => {
                        links.failed_at.remove(&tab_id);
                        links.views.insert(
                            tab_id,
                            CachedView {
                                view: Arc::new(view),
                                at: Instant::now(),
                            },
                        );
                        // Closed tabs leave their screens behind; drop the ones nobody asks for.
                        links
                            .views
                            .retain(|_, cached| cached.at.elapsed() < Duration::from_secs(30));
                        workspace.probe_link(cx);
                    }
                    Err(_) => {
                        links.failed_at.insert(tab_id, Instant::now());
                    }
                }
            });
        })
        .detach();
    }

    /// A mouse press on a terminal. If it is a ⌘-click on a link, the terminal never sees it.
    fn terminal_link_pressed(
        &mut self,
        pane_id: PaneId,
        tab_id: TabId,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.terminal_links.swallow_release = None;
        if event.button != MouseButton::Left {
            return;
        }
        let Some(mode) = open_mode(event.modifiers) else {
            return;
        };
        let Some(shell_id) = self.link_shell(pane_id, tab_id) else {
            return;
        };
        let pointer = Pointer {
            tab_id,
            pane_id,
            position: event.position,
            bounds: self.terminal_links.bounds_cell(tab_id).get(),
            scale: f64::from(window.scale_factor()),
            modifiers: event.modifiers,
        };
        let padding = self.terminal_links.padding();
        let links = &mut self.terminal_links;
        let hovered = links
            .views
            .get(&tab_id)
            .and_then(|cached| pointer.cell(&cached.view, &padding, &mut links.cells))
            .and_then(|cell| links.answer_for(tab_id, cell));
        let link = match hovered {
            Some(answer) => answer,
            // A tmux that just failed to answer is not waited on again by a click.
            None if self
                .terminal_links
                .failed_at
                .get(&tab_id)
                .is_some_and(|at| at.elapsed() < RETRY_AFTER_FAILURE) =>
            {
                None
            }
            None => {
                let context = self.link_context(pointer);
                self.link_now(shell_id, context)
            }
        };
        let Some(link) = link else {
            return;
        };
        cx.stop_propagation();
        self.terminal_links.swallow_release = Some((tab_id, Instant::now()));
        self.terminal_links.hover = None;
        if self.active_pane != pane_id {
            self.select_pane(pane_id, window, cx);
        }
        self.open_terminal_link(link, mode, window, cx);
        cx.notify();
    }

    /// The release of a click that was kept from the terminal is kept from it too.
    fn terminal_link_released(
        &mut self,
        tab_id: TabId,
        event: &MouseUpEvent,
        cx: &mut Context<Self>,
    ) {
        if event.button == MouseButton::Left && self.terminal_links.take_release(tab_id) {
            cx.stop_propagation();
        }
    }

    /// Work out a link now, for a click nothing has answered yet. The work is a worker's, and
    /// the click waits only briefly for it: a tmux that is slow gives the click to the terminal.
    fn link_now(&self, shell_id: String, context: LinkContext) -> Option<Link> {
        let sessions = self.sessions.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let link = sessions
                .capture_link_view(&shell_id)
                .and_then(|raw| PaneView::parse(&raw))
                .ok()
                .and_then(|view| context.resolve(&view));
            let _ = sender.send(link);
        });
        receiver.recv_timeout(CLICK_WAIT).ok().flatten()
    }

    // -----------------------------------------------------------------------------------
    // Opening
    // -----------------------------------------------------------------------------------

    fn open_terminal_link(
        &mut self,
        link: Link,
        mode: OpenMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match link {
            Link::Url(url) => {
                // Only what a browser or a mail program handles; a screen can say anything.
                let scheme = url
                    .split(':')
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if matches!(scheme.as_str(), "http" | "https" | "mailto") {
                    cx.open_url(&url);
                }
            }
            Link::Path(target) => self.open_terminal_path(target, mode, window, cx),
        }
    }

    fn open_terminal_path(
        &mut self,
        target: ResolvedPath,
        mode: OpenMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root = file_explorer_root(
            &self.state,
            &self.project_id,
            self.selected_worktree_id.as_deref(),
        );
        let inside = root.and_then(|root| within(&root.path, &target.path).map(|at| (root, at)));
        match inside {
            Some((root, path)) if target.is_dir => self.reveal_folder(root.path, path, window, cx),
            Some((root, path)) if mode == OpenMode::Edit => {
                match fs::metadata(&path).map(|metadata| file_preview::FileIdentity::of(&metadata))
                {
                    Ok(identity) => {
                        self.open_file_editor(root, path, identity, target.line, window, cx);
                    }
                    Err(error) => {
                        self.notice = Some(format!("Cannot edit {}: {error}", short_name(&path)));
                    }
                }
            }
            Some((_, path)) => self.show_in_preview(path, target.line, window, cx),
            None => {
                if mode == OpenMode::Edit && !target.is_dir {
                    self.notice = Some(format!(
                        "{} is outside this worktree, so it opens with its default app.",
                        short_name(&target.path)
                    ));
                }
                self.open_outside_project(&target, cx);
            }
        }
    }

    /// A file or folder outside the project: Finder for a folder, the default app for a file.
    /// A program or an installer is shown in Finder instead of run.
    fn open_outside_project(&mut self, target: &ResolvedPath, cx: &mut Context<Self>) {
        if target.is_dir {
            cx.reveal_path(&target.path);
        } else if let Some(reason) = open_refusal(&target.path) {
            cx.reveal_path(&target.path);
            self.notice = Some(format!(
                "Showed {} in Finder. {reason}",
                short_name(&target.path)
            ));
        } else {
            cx.open_with_system(&target.path);
        }
    }

    /// A file that exists but that Files does not list (it is past the cap on a folder's rows, say)
    /// is opened the way a file outside the project is, and the click does not go unanswered.
    pub(crate) fn open_unlisted(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let target = ResolvedPath {
            is_dir: path.is_dir(),
            path,
            line: None,
            col: None,
        };
        self.notice = Some(format!(
            "{} is not listed in Files, so it is opened directly.",
            short_name(&target.path)
        ));
        self.open_outside_project(&target, cx);
        cx.notify();
    }

    /// Select a file of the project in Files, so that Preview shows it, at `line` when given.
    fn show_in_preview(
        &mut self,
        path: PathBuf,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.ensure_layout(window, cx) {
            return;
        }
        self.ensure_file_explorer(cx);
        let Some(explorer) = self.file_explorer.clone() else {
            self.notice = Some("Files are not available for this project.".to_owned());
            return;
        };
        if let Err(error) = explorer.update(cx, |explorer, cx| explorer.reveal(&path, line, cx)) {
            self.notice = Some(error);
            return;
        }
        if self.focus_mode {
            // Panes are not rearranged behind a single focused one.
            self.notice = Some(format!(
                "Selected {} in Files. Leave focus mode to see it.",
                short_name(&path)
            ));
        } else if self.explorer_pane().is_none() {
            // Files is not open anywhere, so there is nothing to put the preview beside.
            self.open_preview(window, cx);
        }
    }

    /// Show a folder of the project in Files.
    fn reveal_folder(
        &mut self,
        root: PathBuf,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_file_explorer(cx);
        let Some(explorer) = self.file_explorer.clone() else {
            return;
        };
        if path != root
            && let Err(error) = explorer.update(cx, |explorer, cx| explorer.reveal(&path, None, cx))
        {
            self.notice = Some(error);
            return;
        }
        let pane = self.active_pane;
        self.open_panel(PanelKind::Files, pane, window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hover(tab_id: TabId, cell: (u32, u32), link: Option<Link>) -> Hover {
        Hover {
            tab_id,
            cell,
            link,
            at: Instant::now(),
        }
    }

    #[test]
    fn a_recent_answer_stands_for_its_own_cell_and_tab_only() {
        let url = Link::Url("https://example.com".into());
        let links = LinkState {
            hover: Some(hover(3, (4, 5), Some(url.clone()))),
            ..LinkState::default()
        };
        assert_eq!(links.answer_for(3, (4, 5)), Some(Some(url)));
        assert_eq!(links.answer_for(3, (5, 5)), None);
        assert_eq!(links.answer_for(3, (4, 6)), None);
        assert_eq!(links.answer_for(4, (4, 5)), None);
        // The hand shows in the tab the link is in.
        assert!(links.over_link(3));
        assert!(!links.over_link(4));
    }

    #[test]
    fn nothing_there_is_an_answer_too_until_it_is_old() {
        let mut links = LinkState {
            hover: Some(hover(3, (1, 1), None)),
            ..LinkState::default()
        };
        assert_eq!(links.answer_for(3, (1, 1)), Some(None));
        assert!(!links.over_link(3));
        // Output may have moved the text under a pointer that has not: ask again.
        let old = Instant::now()
            .checked_sub(HOVER_LIFETIME + Duration::from_millis(1))
            .expect("the clock has run for a second");
        links.hover.as_mut().unwrap().at = old;
        assert_eq!(links.answer_for(3, (1, 1)), None);
    }

    #[test]
    fn a_kept_click_keeps_only_its_own_release_and_only_once() {
        let mut links = LinkState::default();
        assert!(!links.take_release(3));
        // The release comes to the tab the click was on.
        links.swallow_release = Some((3, Instant::now()));
        assert!(links.take_release(3));
        assert!(!links.take_release(3));
        // A release on another tab is an ordinary one, and it ends the claim.
        links.swallow_release = Some((3, Instant::now()));
        assert!(!links.take_release(4));
        assert!(!links.take_release(3));
        // A claim nobody came for runs out.
        let old = Instant::now()
            .checked_sub(RELEASE_WINDOW + Duration::from_millis(1))
            .expect("the clock has run for a few seconds");
        links.swallow_release = Some((3, old));
        assert!(!links.take_release(3));
    }

    #[test]
    fn a_tabs_bounds_are_one_cell_that_painting_and_events_share() {
        let links = LinkState::default();
        let painted = links.bounds_cell(7);
        let bounds = Bounds::new(
            Point::new(gpui::px(10.0), gpui::px(20.0)),
            gpui::size(gpui::px(300.0), gpui::px(200.0)),
        );
        painted.set(bounds);
        assert_eq!(links.bounds_cell(7).get(), bounds);
        assert_eq!(links.bounds_cell(8).get(), Bounds::default());
    }
}
