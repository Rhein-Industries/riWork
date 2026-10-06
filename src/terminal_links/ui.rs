//! The mouse side of terminal links: find what the pointer is over and underline it, show the
//! pointing hand while ⌘ is held, and on a ⌘-click, open it.
//!
//! Ghostty gets its mouse events from GPUI (the adapter forwards them), so a click that opened
//! a link must not also reach it, or it would start a selection and, on release, open the raw
//! text with `open`. The wrapper below sees events first (the capture phase) and keeps those
//! from the terminal. A click that is not on a link passes through untouched, so Ghostty's own
//! OSC 8 links still work.
//!
//! Deciding takes a tmux call, so it is done ahead of the click: the pointer over a terminal reads
//! the screen and works out what the cell under the pointer links to, off the UI thread. The
//! underline and the click use that answer. Only a click with no answer yet waits, for a moment,
//! for its own.
//!
//! A screen read stands until the terminal changes. While the pointer is over a terminal, a tmux
//! control-mode client (`SessionManager::watch_shell`) tells when its pane prints, resizes or
//! enters or leaves a mode, and the wheel says when it scrolls; only then is the screen read
//! again. A pointer resting on a link in a quiet terminal costs nothing.

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use gpui::{
    AnyElement, Bounds, Context, DragMoveEvent, ExternalPaths, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollWheelEvent, Task, Window,
    canvas, div, prelude::*,
};

use super::{
    Bases, CellHint, Link, LinkRow, OpenMode, PaneView, ResolvedPath, Strip, is_openable_url,
    open_mode, open_refusal, overlay::Overlay, short_name, underline_strips, within,
};
use crate::{
    PaneId, TabId, Workspace, file_explorer_root, file_preview,
    layouts::PanelKind,
    sessions::{SessionManager, Woke},
    theme::{self, GhosttyPadding},
};

/// How long a capture of the screen answers for when tmux cannot say whether it changed (no
/// watch is attached yet, or none could be). The screen moves while an agent writes, but a
/// pointer being moved over it for a second sees little of that.
const VIEW_LIFETIME: Duration = Duration::from_millis(500);
/// How long what the pointer was over stands for a click on the same cell, on the same terms.
/// Output can move the text under a pointer that is still, so an old answer is worked out again.
const HOVER_LIFETIME: Duration = Duration::from_millis(600);
/// The least time between two looks at a screen that keeps changing: while an agent writes, an
/// underline follows its output this late at worst, and the screen is read at most this often.
const CHANGE_GAP: Duration = VIEW_LIFETIME;
/// How long an underlined link goes unchecked while tmux tells of no change. Some changes it does
/// not announce (scrolling within copy mode by key or from the phone, a cleared history).
const QUIET_CHECK: Duration = Duration::from_secs(5);
/// How often a watch looks whether it is still wanted, so that it ends soon after the pointer
/// leaves.
const WATCH_STOP_CHECK: Duration = Duration::from_millis(500);
/// How long after the last turn of the wheel the screen is read again: scrolling moves the text
/// under a pointer that rests, and tmux does not tell of it.
const SCROLL_SETTLE: Duration = Duration::from_millis(150);
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
    /// The underline the terminal under the pointer painted this frame, in window points, and
    /// its color; taken when the window's last element is painted.
    painted: Rc<RefCell<Option<Underline>>>,
    overlay: Rc<RefCell<Overlay>>,
    /// Tells when the screen of the terminal under the pointer changes.
    watch: Option<ScreenWatch>,
    /// Counts the changes each tab's screen was told of. A screen read, and an answer worked out
    /// from it, stand only while the count is the one they were read at.
    changes: HashMap<TabId, u64>,
    /// Numbers turns of the wheel, so that only the last of a burst reads the screen.
    scrolled: u64,
}

/// A control-mode client on one tab's shell, kept on a thread of its own while the pointer is over
/// that tab, and the task that brings its news to the window.
struct ScreenWatch {
    tab_id: TabId,
    /// The client is attached: every change from then on is told of.
    attached: bool,
    /// Ends the task, which closes the channel, which ends the thread and its client.
    _news: Task<()>,
}

/// What a watch tells the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScreenNews {
    /// From here on every change is told of; what was read before may have missed one.
    Attached,
    /// The screen may have changed (or, with no client, it is time to look again).
    Changed,
    /// Nothing for a while; an underline is looked at in case of a change tmux kept quiet about.
    Quiet,
    /// The client is gone; screens are read on a timer, as before there was one.
    Lost,
}

/// An underline as painted: strips in window points, and their color.
type Underline = (Vec<Strip>, u32);

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
    /// When the read began, and the count of changes then.
    at: Instant,
    changes: u64,
}

/// What the pointer was found over: a link, or nothing worth one.
struct Hover {
    tab_id: TabId,
    /// Column and row.
    cell: (u32, u32),
    link: Option<Link>,
    /// The cells the link covers, one run per row, in a grid of `grid` columns and rows.
    underline: Vec<LinkRow>,
    grid: (u32, u32),
    at: Instant,
    /// The count of changes when the screen it was worked out from was read.
    changes: u64,
}

impl LinkState {
    fn bounds_cell(&self, tab_id: TabId) -> Rc<Cell<Bounds<Pixels>>> {
        self.bounds.borrow_mut().entry(tab_id).or_default().clone()
    }

    /// Whether the pointer is over a link in this tab, which is when the link is underlined.
    pub fn over_link(&self, tab_id: TabId) -> bool {
        self.hover
            .as_ref()
            .is_some_and(|hover| hover.tab_id == tab_id && hover.link.is_some())
    }

    /// Whether the pointer shows a hand in this tab: over a link, with ⌘ held.
    fn hand(&self, tab_id: TabId) -> bool {
        self.over_link(tab_id)
            && self
                .pointer
                .as_ref()
                .is_some_and(|pointer| open_mode(pointer.modifiers).is_some())
    }

    /// What to underline in this tab: the runs of cells, and the grid they are counted in.
    fn underline(&self, tab_id: TabId) -> Option<(Vec<LinkRow>, (u32, u32))> {
        self.hover
            .as_ref()
            .filter(|hover| hover.tab_id == tab_id && hover.link.is_some())
            .filter(|hover| !hover.underline.is_empty())
            .map(|hover| (hover.underline.clone(), hover.grid))
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
                hover.tab_id == tab_id
                    && hover.cell == cell
                    && self.current(tab_id, hover.changes, hover.at, HOVER_LIFETIME)
            })
            .map(|hover| hover.link.clone())
    }

    /// The count of changes this tab's screen was told of.
    fn changes(&self, tab_id: TabId) -> u64 {
        self.changes.get(&tab_id).copied().unwrap_or_default()
    }

    /// What was read of this tab's screen before now is out of date.
    fn changed(&mut self, tab_id: TabId) {
        *self.changes.entry(tab_id).or_default() += 1;
    }

    /// Whether every change to this tab's screen is told of.
    fn watched(&self, tab_id: TabId) -> bool {
        self.watch
            .as_ref()
            .is_some_and(|watch| watch.tab_id == tab_id && watch.attached)
    }

    /// Whether something read of a tab's screen at `at`, with the count of changes at `changes`,
    /// still holds: nothing changed since, and either a watch would have told of a change, or it
    /// is younger than `lifetime`.
    fn current(&self, tab_id: TabId, changes: u64, at: Instant, lifetime: Duration) -> bool {
        changes == self.changes(tab_id) && (self.watched(tab_id) || at.elapsed() < lifetime)
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
        self.resolve_cell(view, cell).map(|(link, _)| link)
    }

    /// What a cell links to, and the cells the link covers.
    fn resolve_cell(&self, view: &PaneView, cell: (u32, u32)) -> Option<(Link, Vec<LinkRow>)> {
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
        view.link_span_at(cell.1, cell.0, &bases)
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
        // A dialog over the window hides the terminals; the line would show through it.
        let underline = (!self.modal_open())
            .then(|| self.terminal_links.underline(tab_id))
            .flatten()
            .and_then(|(runs, grid)| {
                let padding = self.terminal_links.padding?.1;
                Some((runs, grid, padding, self.terminal_links.cells))
            });
        let painted = self.terminal_links.painted.clone();
        let color = theme::palette(cx).cyan;
        div()
            .id(("terminal-links", tab_id))
            .size_full()
            .min_w_0()
            .min_h_0()
            .relative()
            .when(self.terminal_links.hand(tab_id), |layer| {
                layer.cursor_pointer()
            })
            .child(terminal)
            .child(
                canvas(
                    move |bounds, _, _| measured.set(bounds),
                    move |bounds, _, window, _| {
                        let Some((runs, grid, padding, mut hint)) = underline else {
                            return;
                        };
                        // Where the line goes is worked out as the pointer's cell is, from the
                        // bounds the terminal has in this very frame.
                        let points = |pixels: Pixels| f64::from(pixels.as_f32());
                        let strips = underline_strips(
                            (points(bounds.size.width), points(bounds.size.height)),
                            f64::from(window.scale_factor()),
                            grid,
                            &padding,
                            &mut hint,
                            &runs,
                        )
                        .into_iter()
                        .map(|strip| Strip {
                            x: strip.x + points(bounds.origin.x),
                            y: strip.y + points(bounds.origin.y),
                            ..strip
                        })
                        .collect();
                        *painted.borrow_mut() = Some((strips, color));
                    },
                )
                .absolute()
                .top_0()
                .left_0()
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
            .on_scroll_wheel(cx.listener(move |workspace, _: &ScrollWheelEvent, _, cx| {
                workspace.terminal_link_scrolled(tab_id, cx);
            }))
            // Files dragged from Finder: their paths are pasted into this shell (terminal_drop).
            .on_drag_move(cx.listener(
                move |workspace, event: &DragMoveEvent<ExternalPaths>, _, cx| {
                    workspace.terminal_drag_moved(tab_id, event, cx);
                },
            ))
            .on_drop(
                cx.listener(move |workspace, paths: &ExternalPaths, window, cx| {
                    workspace.terminal_dropped(pane_id, tab_id, paths, window, cx);
                    cx.stop_propagation();
                }),
            )
            // ⌘V with files or a picture copied (terminal_drop).
            .on_action(
                cx.listener(move |workspace, _: &crate::PasteInTerminal, _, cx| {
                    workspace.terminal_paste(pane_id, tab_id, cx);
                }),
            )
            .hover_listener_mode(gpui::HoverListenerMode::InputModalityIndependent)
            .on_hover(cx.listener(move |workspace, hovered: &bool, _, cx| {
                if !*hovered {
                    workspace.terminal_link_left(tab_id, cx);
                }
            }))
            .into_any_element()
    }

    /// The window's last element: it puts on screen the underline a terminal painted in this
    /// frame, or takes it away when none did (the pointer left, the tab is no longer shown).
    pub(crate) fn terminal_link_underline(&self) -> AnyElement {
        let painted = self.terminal_links.painted.clone();
        let overlay = self.terminal_links.overlay.clone();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| match painted.borrow_mut().take() {
                Some((strips, color)) => overlay.borrow_mut().show(window, &strips, color),
                None => overlay.borrow_mut().hide(),
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_0()
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
        let hand = self.terminal_links.hand(pointer.tab_id);
        self.terminal_links.pointer = Some(pointer.clone());
        if hand != self.terminal_links.hand(pointer.tab_id) {
            cx.notify();
        }
        // With or without ⌘: a link under the pointer is underlined either way.
        self.probe_link(cx);
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
        let hand = self.terminal_links.hand(tab_id);
        if let Some(pointer) = self.terminal_links.pointer.as_mut() {
            pointer.modifiers = modifiers;
            pointer.bounds = bounds;
            pointer.scale = f64::from(window.scale_factor());
        }
        // The hand comes and goes with ⌘ on what the pointer was found over; the underline stays.
        if hand != self.terminal_links.hand(tab_id) {
            cx.notify();
        }
        self.probe_link(cx);
    }

    fn terminal_link_left(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if self
            .terminal_links
            .pointer
            .as_ref()
            .is_some_and(|pointer| pointer.tab_id == tab_id)
        {
            self.terminal_links.pointer = None;
            self.terminal_links.watch = None;
            self.clear_link_hover(cx);
        }
    }

    /// Forget what the pointer was over; repaint if that was a link, so the hand and the line go.
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
        let Some(shell_id) = self.link_shell(pointer.pane_id, pointer.tab_id) else {
            return;
        };
        self.watch_screen(pointer.tab_id, &shell_id, cx);
        let padding = self.terminal_links.padding();
        // The cell comes from the layout of the last capture, however old: only its text goes
        // stale, and the pointer often stays in one cell.
        let Some(cached) = self.terminal_links.views.get(&pointer.tab_id) else {
            self.fetch_link_view(pointer.tab_id, shell_id, cx);
            return;
        };
        let (view, view_at, changes) = (cached.view.clone(), cached.at, cached.changes);
        let Some(cell) = pointer.cell(&view, &padding, &mut self.terminal_links.cells) else {
            self.set_link_hover(
                pointer.tab_id,
                None,
                None,
                (view.cols, view.rows),
                changes,
                cx,
            );
            return;
        };
        if self
            .terminal_links
            .answer_for(pointer.tab_id, cell)
            .is_some()
        {
            return;
        }
        if !self
            .terminal_links
            .current(pointer.tab_id, changes, view_at, VIEW_LIFETIME)
        {
            self.fetch_link_view(pointer.tab_id, shell_id, cx);
            return;
        }
        self.terminal_links.probe += 1;
        let probe = self.terminal_links.probe;
        let tab_id = pointer.tab_id;
        let context = self.link_context(pointer);
        let grid = (view.cols, view.rows);
        cx.spawn(async move |workspace, cx| {
            let found = cx
                .background_executor()
                .spawn(async move { context.resolve_cell(&view, cell) })
                .await;
            let _ = workspace.update(cx, |workspace, cx| {
                // A newer position is being worked out, or the pointer left.
                if workspace.terminal_links.probe == probe {
                    workspace.set_link_hover(tab_id, Some(cell), found, grid, changes, cx);
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
        found: Option<(Link, Vec<LinkRow>)>,
        grid: (u32, u32),
        changes: u64,
        cx: &mut Context<Self>,
    ) {
        let had = (
            self.terminal_links.hand(tab_id),
            self.terminal_links.underline(tab_id),
        );
        let (link, underline) = found.map_or((None, Vec::new()), |(link, runs)| (Some(link), runs));
        self.terminal_links.hover = cell.map(|cell| Hover {
            tab_id,
            cell,
            link,
            underline,
            grid,
            at: Instant::now(),
            changes,
        });
        let now = (
            self.terminal_links.hand(tab_id),
            self.terminal_links.underline(tab_id),
        );
        if had != now {
            cx.notify();
        }
    }

    /// Watch the screen of the tab under the pointer, unless that is being done: a control-mode
    /// client on a thread of its own says when the pane changes, and only then is the screen read
    /// again. The watch of another tab ends.
    fn watch_screen(&mut self, tab_id: TabId, shell_id: &str, cx: &mut Context<Self>) {
        if self
            .terminal_links
            .watch
            .as_ref()
            .is_some_and(|watch| watch.tab_id == tab_id)
        {
            return;
        }
        let (sender, news) = async_channel::bounded(1);
        let sessions = self.sessions.clone();
        let shell_id = shell_id.to_owned();
        thread::spawn(move || watch_shell(&sessions, &shell_id, &sender));
        let task = cx.spawn(async move |workspace, cx| {
            while let Ok(news) = news.recv().await {
                let heard = workspace.update(cx, |workspace, cx| {
                    workspace.screen_news(tab_id, news, cx);
                });
                if heard.is_err() {
                    break;
                }
            }
        });
        self.terminal_links.watch = Some(ScreenWatch {
            tab_id,
            attached: false,
            _news: task,
        });
    }

    /// What the watch of a tab's screen told. A change makes what was read of the screen out of
    /// date, and an underline on it is looked at again at once: output or scrolling may have moved
    /// the text from under a pointer that rests. A tab that is no longer shown loses it.
    fn screen_news(&mut self, tab_id: TabId, news: ScreenNews, cx: &mut Context<Self>) {
        let links = &mut self.terminal_links;
        let Some(watch) = links.watch.as_mut().filter(|watch| watch.tab_id == tab_id) else {
            return;
        };
        match news {
            ScreenNews::Attached => watch.attached = true,
            ScreenNews::Lost => watch.attached = false,
            ScreenNews::Changed | ScreenNews::Quiet => {}
        }
        // What was read before the client attached may have missed a change, and the timer that
        // stands in for a lost one is a reason to look.
        links.changed(tab_id);
        self.recheck_link_hover(tab_id, cx);
    }

    /// Look again at an underlined link, from a screen read anew if it may have changed.
    fn recheck_link_hover(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if !self.terminal_links.over_link(tab_id) {
            return;
        }
        let on_screen = self
            .terminal_links
            .pointer
            .as_ref()
            .filter(|pointer| pointer.tab_id == tab_id)
            .and_then(|pointer| self.panes.get(&pointer.pane_id))
            .and_then(|pane| pane.tabs.get(pane.active))
            .is_some_and(|tab| tab.id == tab_id);
        if !on_screen {
            self.terminal_links.watch = None;
            self.clear_link_hover(cx);
            return;
        }
        // The layout may have moved under a pointer that rests (the window was resized).
        let bounds = self.terminal_links.bounds_cell(tab_id).get();
        if let Some(pointer) = self.terminal_links.pointer.as_mut() {
            pointer.bounds = bounds;
        }
        self.probe_link(cx);
    }

    /// The wheel turned over a terminal. The text moves under the pointer, so an underline goes
    /// at once and comes back, from a screen read anew, once the scrolling stops.
    fn terminal_link_scrolled(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let links = &mut self.terminal_links;
        if links
            .pointer
            .as_ref()
            .is_none_or(|pointer| pointer.tab_id != tab_id)
        {
            return;
        }
        links.changed(tab_id);
        links.scrolled += 1;
        let scrolled = links.scrolled;
        if links.over_link(tab_id) {
            self.clear_link_hover(cx);
        }
        cx.spawn(async move |workspace, cx| {
            cx.background_executor().timer(SCROLL_SETTLE).await;
            let _ = workspace.update(cx, |workspace, cx| {
                if workspace.terminal_links.scrolled == scrolled {
                    workspace.terminal_links.changed(tab_id);
                    workspace.probe_link(cx);
                }
            });
        })
        .detach();
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
        let (asked, changes) = (Instant::now(), self.terminal_links.changes(tab_id));
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
                                at: asked,
                                changes,
                            },
                        );
                        // Closed tabs leave their screens behind; drop the ones nobody asks for.
                        links
                            .views
                            .retain(|_, cached| cached.at.elapsed() < Duration::from_secs(30));
                        let (views, fetching) = (&links.views, &links.fetching);
                        links.changes.retain(|tab_id, _| {
                            views.contains_key(tab_id) || fetching.contains(tab_id)
                        });
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
                if is_openable_url(&url) {
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
        self.sync_file_explorer(window, cx);
        let Some(explorer) = self.file_explorer.clone() else {
            self.notice = Some("Files are not available for this project.".to_owned());
            return;
        };
        if let Err(error) =
            explorer.update(cx, |explorer, cx| explorer.reveal(&path, line, window, cx))
        {
            self.notice = Some(error);
            return;
        }
        if self.focus_mode {
            // Panes are not rearranged behind a single focused one.
            self.notice = Some(format!(
                "Selected {} in Files. Leave focus mode to see it.",
                short_name(&path)
            ));
        }
        // Where the Preview goes is settled when the file is selected (`FileExplorerEvent::
        // Revealed`), which may be after listings arrive: beside the work, never over the
        // terminal that was clicked, and without taking the keys from it.
    }

    /// Show a folder of the project in Files, in a pane beside the work like the Preview: the
    /// terminal that was clicked stays on screen and keeps the keys.
    fn reveal_folder(
        &mut self,
        root: PathBuf,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.ensure_layout(window, cx) {
            return;
        }
        self.sync_file_explorer(window, cx);
        let Some(explorer) = self.file_explorer.clone() else {
            return;
        };
        if path != root
            && let Err(error) =
                explorer.update(cx, |explorer, cx| explorer.reveal(&path, None, window, cx))
        {
            self.notice = Some(error);
            return;
        }
        if self.focus_mode {
            self.notice = Some(format!(
                "Selected {} in Files. Leave focus mode to see it.",
                short_name(&path)
            ));
        } else {
            self.reveal_panel_for_link(PanelKind::Files, cx);
        }
    }
}

/// The thread of a watch: attach a control-mode client to the shell's session and pass on what it
/// tells, a change at most every `CHANGE_GAP`, until the window stops listening. A tmux that cannot
/// attach one is looked at every `HOVER_LIFETIME` instead, as if it had told of a change.
fn watch_shell(
    sessions: &SessionManager,
    shell_id: &str,
    sender: &async_channel::Sender<ScreenNews>,
) {
    let mut watch = sessions.watch_shell(shell_id);
    if watch.is_some() && sender.send_blocking(ScreenNews::Attached).is_err() {
        return;
    }
    let mut quiet_since = Instant::now();
    while !sender.is_closed() {
        let news = match watch.as_mut().map(|watch| watch.wait(WATCH_STOP_CHECK)) {
            Some(Woke::Change) => Some(ScreenNews::Changed),
            Some(Woke::Silence) => {
                (quiet_since.elapsed() >= QUIET_CHECK).then_some(ScreenNews::Quiet)
            }
            Some(Woke::Lost) => {
                watch = None;
                Some(ScreenNews::Lost)
            }
            None => {
                thread::sleep(HOVER_LIFETIME);
                Some(ScreenNews::Changed)
            }
        };
        let Some(news) = news else {
            continue;
        };
        if sender.send_blocking(news).is_err() {
            return;
        }
        quiet_since = Instant::now();
        // Output that streams is looked at in steps; what comes meanwhile waits in the client.
        if news == ScreenNews::Changed && watch.is_some() {
            thread::sleep(CHANGE_GAP);
        }
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
            underline: vec![LinkRow {
                row: cell.1,
                cols: cell.0..cell.0 + 1,
            }],
            grid: (80, 24),
            at: Instant::now(),
            changes: 0,
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
        // The link is underlined in the tab it is in.
        assert!(links.over_link(3));
        assert!(!links.over_link(4));
    }

    #[test]
    fn a_link_is_underlined_without_command_and_shows_a_hand_only_with_it() {
        let pointer = |modifiers: Modifiers| Pointer {
            tab_id: 3,
            pane_id: 1,
            position: Point::default(),
            bounds: Bounds::default(),
            scale: 2.0,
            modifiers,
        };
        let mut links = LinkState {
            hover: Some(hover(
                3,
                (4, 5),
                Some(Link::Url("https://example.com".into())),
            )),
            pointer: Some(pointer(Modifiers::default())),
            ..LinkState::default()
        };
        assert!(links.underline(3).is_some());
        assert!(!links.hand(3));
        links.pointer = Some(pointer(Modifiers::command()));
        assert!(links.hand(3));
        assert!(!links.hand(4));
        assert_eq!(links.underline(4), None);
        // Nothing found is nothing to underline.
        links.hover = Some(hover(3, (4, 5), None));
        assert_eq!(links.underline(3), None);
        assert!(!links.hand(3));
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

    fn watching(tab_id: TabId, attached: bool) -> ScreenWatch {
        ScreenWatch {
            tab_id,
            attached,
            _news: Task::ready(()),
        }
    }

    #[test]
    fn a_watched_screen_stands_until_it_changes_however_old() {
        let old = Instant::now()
            .checked_sub(HOVER_LIFETIME + Duration::from_millis(1))
            .expect("the clock has run for a second");
        let url = Link::Url("https://example.com".into());
        let mut links = LinkState {
            hover: Some(Hover {
                at: old,
                ..hover(3, (4, 5), Some(url.clone()))
            }),
            ..LinkState::default()
        };
        // Nobody would tell of a change: an old answer is worked out again.
        assert_eq!(links.answer_for(3, (4, 5)), None);
        assert!(!links.current(3, 0, old, VIEW_LIFETIME));
        // A client that is still attaching vouches for nothing yet.
        links.watch = Some(watching(3, false));
        assert_eq!(links.answer_for(3, (4, 5)), None);
        // Attached, it does: a quiet terminal is not read again.
        links.watch = Some(watching(3, true));
        assert_eq!(links.answer_for(3, (4, 5)), Some(Some(url)));
        assert!(links.current(3, 0, old, VIEW_LIFETIME));
        // Only for its own tab.
        assert!(!links.current(4, 0, old, VIEW_LIFETIME));
        // A change makes what was read before it out of date, the answer and the screen alike,
        // while the screen read after it holds.
        links.changed(3);
        assert_eq!(links.answer_for(3, (4, 5)), None);
        assert!(!links.current(3, 0, old, VIEW_LIFETIME));
        assert!(links.current(3, 1, old, VIEW_LIFETIME));
        // Changes to one tab say nothing about another.
        assert_eq!(links.changes(4), 0);
    }

    #[test]
    fn a_change_voids_even_a_fresh_read_where_nothing_watches() {
        let mut links = LinkState {
            hover: Some(hover(3, (1, 1), None)),
            ..LinkState::default()
        };
        let now = Instant::now();
        assert!(links.current(3, 0, now, VIEW_LIFETIME));
        assert_eq!(links.answer_for(3, (1, 1)), Some(None));
        // The wheel turned, or a lost client's timer came round.
        links.changed(3);
        assert!(!links.current(3, 0, now, VIEW_LIFETIME));
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
