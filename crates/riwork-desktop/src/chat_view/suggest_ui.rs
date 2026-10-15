//! The `/` and `@` suggestions over the message box, from `chat::suggest`: a list just above
//! the card, starting at the caret, and beside it (or above it, in a narrow pane) a card that
//! says what the highlighted entry is and what accepting it puts in. ↑ and ↓ move the
//! highlight, ⏎ or ⇥ accept it, ⎋ closes the list until that token is typed again; the
//! pointer highlights a row and a click accepts it. Accepting only edits the draft: sending is
//! still ⏎ or Send afterwards. The list stands wholly above the card, cut to the room there,
//! so it never covers Send, Stop or the pickers.
//!
//! `@` lists the chat's folder once (`suggest::list_files`, bounded) on a background thread
//! and keeps it for every tab of that folder (`Listings`, `suggest::FILE_TTL`); typing only
//! filters what is kept. A file's first lines are read when it is highlighted, a few KB at
//! most, also off the main thread, and kept while the list stays open.

use std::{
    cell::Cell,
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::Instant,
};

use gpui::{
    AnyElement, Bounds, Context, ElementId, EntityInputHandler, Focusable, Global, Pixels,
    ScrollHandle, SharedString, WeakEntity, Window, anchored, canvas, deferred, div, point,
    prelude::*, px, rgb,
};
use gpui_kit::base::{
    TestSupportExt as _,
    input::{Enter, Escape, IndentInline, MoveDown, MoveUp},
};

use super::{ChatView, widgets::Look};
use crate::{
    chat::suggest::{self, Command, FileEntry, Listing, Peek, Sigil, Trigger},
    ui_text,
};

/// The list's width, and the preview card's beside it, in design points.
const LIST_WIDTH: f32 = 340.0;
const PREVIEW_WIDTH: f32 = 300.0;
/// The most the list (and a preview beside it) may be tall, in design points.
const MAX_HEIGHT: f32 = 260.0;
/// How many previews a tab keeps.
const PEEKS: usize = 64;

/// One row of the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Item {
    Command(Command),
    File(FileEntry),
}

impl Item {
    /// What accepting it puts in the draft, which is also how its row names it.
    pub fn insertion(&self) -> String {
        match self {
            Self::Command(command) => command.insertion(),
            Self::File(entry) => entry.insertion(),
        }
    }

    /// The row's second line: what a command is, what kind of thing a file is.
    fn detail(&self) -> String {
        match self {
            Self::Command(command) => command.summary.clone(),
            Self::File(entry) => suggest::file_preview(entry, None).kind,
        }
    }
}

/// The list while it is open.
pub(super) struct Open {
    pub trigger: Trigger,
    pub items: Vec<Item>,
    pub highlight: usize,
    /// The chat's folder is still being listed for the first time.
    pub loading: bool,
    /// Its listing stopped at `suggest::FILE_BOUND`.
    pub truncated: bool,
}

/// A tab's suggestions.
#[derive(Default)]
pub(super) struct Suggestions {
    pub open: Option<Open>,
    /// The sigil (its byte offset in the draft) whose list ⎋ closed: it stays closed while
    /// that token is typed, and opens again once it is gone.
    dismissed: Option<usize>,
    /// The first lines of files, by folder and path, and the one being read. Dropped when the
    /// list opens again, so a file that changed meanwhile is read again.
    peeks: HashMap<(PathBuf, String), Peek>,
    peeking: Option<(PathBuf, String)>,
    /// How many times `peeks` was dropped: a read started before the last time is not kept.
    peeks_dropped: u64,
    /// The message box card as last drawn, in window coordinates: the list stands on it.
    pub card: Rc<Cell<Bounds<Pixels>>>,
    scroll: ScrollHandle,
}

/// The folders' listings, shared by every chat tab: one walk serves every tab of a folder.
#[derive(Default)]
struct Listings {
    cache: suggest::FileCache,
    /// The folders being walked, and the tabs waiting for them.
    reading: HashMap<PathBuf, Vec<WeakEntity<ChatView>>>,
}

impl Global for Listings {}

impl ChatView {
    /// The folder the chat works in, which `@` lists.
    fn suggestion_root(&self) -> Option<PathBuf> {
        self.info()
            .or(self.inventory_info.as_ref())
            .map(|info| info.cwd.clone())
            .or_else(|| {
                self.creation
                    .as_ref()
                    .map(|creation| creation.chat().cwd.clone())
            })
    }

    /// The token at the caret that opens a list: only in the focused message box, with no
    /// selection, no text being composed and no menu open.
    fn suggestion_trigger(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Trigger> {
        if !self.accepts_input() || self.menu.is_some() {
            return None;
        }
        let (focused, selection, caret, text) = {
            let state = self.composer.read(cx);
            (
                state.focus_handle(cx).is_focused(window),
                state.selected_range(),
                state.cursor(),
                state.value(),
            )
        };
        if !focused || !selection.is_empty() {
            return None;
        }
        let composing = self.composer.update(cx, |state, cx| {
            state.marked_text_range(window, cx).is_some()
        });
        if composing {
            return None;
        }
        suggest::trigger(&text, caret)
    }

    /// Open, change or close the list for what the message box holds now. Called whenever
    /// the box changes (its text, caret or focus) and before the tab is drawn.
    pub(super) fn sync_suggestions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let found = self.suggestion_trigger(window, cx);
        if found.is_none() {
            self.suggestions.dismissed = None;
        }
        let found = found.filter(|found| self.suggestions.dismissed != Some(found.start));
        let unchanged = match (&self.suggestions.open, &found) {
            (Some(open), Some(found)) => open.trigger == *found,
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            return;
        }
        let reopened = self.suggestions.open.is_none();
        self.suggestions.open = found.and_then(|found| self.rank_suggestions(found, cx));
        if reopened && self.suggestions.open.is_some() {
            let suggestions = &mut self.suggestions;
            suggestions.peeks.clear();
            suggestions.peeking = None;
            suggestions.peeks_dropped += 1;
        }
        self.suggestions.scroll.scroll_to_item(0);
        self.peek_highlighted(cx);
        cx.notify();
    }

    /// The list for `trigger`: the chat's commands, or its folder's files, that match. None
    /// when nothing matches (and the files are not still being listed).
    fn rank_suggestions(&mut self, trigger: Trigger, cx: &mut Context<Self>) -> Option<Open> {
        match trigger.sigil {
            Sigil::Slash => {
                let commands = self.provider().map(suggest::commands).unwrap_or_default();
                let items: Vec<_> = suggest::rank_commands(&trigger.query, &commands)
                    .into_iter()
                    .cloned()
                    .map(Item::Command)
                    .collect();
                (!items.is_empty()).then_some(Open {
                    trigger,
                    items,
                    highlight: 0,
                    loading: false,
                    truncated: false,
                })
            }
            Sigil::Mention => {
                let root = self.suggestion_root()?;
                let listing = self.listing(&root, cx);
                let items: Vec<_> = listing
                    .as_ref()
                    .map(|listing| {
                        suggest::rank_files(&trigger.query, &listing.entries, suggest::SHOWN)
                            .into_iter()
                            .cloned()
                            .map(Item::File)
                            .collect()
                    })
                    .unwrap_or_default();
                let loading = listing.is_none();
                (loading || !items.is_empty()).then(|| Open {
                    trigger,
                    items,
                    highlight: 0,
                    loading,
                    truncated: listing.is_some_and(|listing| listing.truncated),
                })
            }
        }
    }

    /// `root`'s listing as kept, walking the folder again (once, for every tab) when it is
    /// stale or missing. Never waits for the walk: a stale listing is used meanwhile, and none
    /// while the folder is listed for the first time.
    fn listing(&mut self, root: &Path, cx: &mut Context<Self>) -> Option<Arc<Listing>> {
        let me = cx.weak_entity();
        let listings = cx.default_global::<Listings>();
        if let Some(fresh) = listings.cache.fresh(root, Instant::now()) {
            return Some(fresh);
        }
        let kept = listings.cache.any(root);
        let waiting = listings.reading.entry(root.to_path_buf()).or_default();
        let start = waiting.is_empty();
        if !waiting.iter().any(|view| view == &me) {
            waiting.push(me);
        }
        if start {
            let root = root.to_path_buf();
            let walk = {
                let root = root.clone();
                cx.background_executor()
                    .spawn(async move { suggest::list_files(&root) })
            };
            cx.spawn(async move |_, cx| {
                let listing = walk.await;
                cx.update(|cx| {
                    let listings = cx.default_global::<Listings>();
                    listings
                        .cache
                        .put(root.clone(), Instant::now(), Arc::new(listing));
                    let waiting = listings.reading.remove(&root).unwrap_or_default();
                    for view in waiting {
                        let _ = view.update(cx, |view, cx| view.listing_arrived(cx));
                    }
                });
            })
            .detach();
        }
        kept
    }

    /// A folder's listing came: an open `@` list is worked out again from it.
    fn listing_arrived(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self
            .suggestions
            .open
            .take_if(|open| open.trigger.sigil == Sigil::Mention)
        else {
            return;
        };
        let highlighted = open.items.get(open.highlight).cloned();
        self.suggestions.open = self.rank_suggestions(open.trigger, cx).map(|mut fresh| {
            // The row that was highlighted stays so if it is still there.
            if let Some(at) =
                highlighted.and_then(|item| fresh.items.iter().position(|i| *i == item))
            {
                fresh.highlight = at;
            }
            fresh
        });
        self.peek_highlighted(cx);
        cx.notify();
    }

    /// Read the first lines of the highlighted file, unless they are known or being read.
    fn peek_highlighted(&mut self, cx: &mut Context<Self>) {
        let Some(Item::File(entry)) = self
            .suggestions
            .open
            .as_ref()
            .and_then(|open| open.items.get(open.highlight))
        else {
            return;
        };
        if entry.dir {
            return;
        }
        let Some(root) = self.suggestion_root() else {
            return;
        };
        let key = (root, entry.path.clone());
        if self.suggestions.peeks.contains_key(&key)
            || self.suggestions.peeking.as_ref() == Some(&key)
        {
            return;
        }
        self.suggestions.peeking = Some(key.clone());
        let dropped = self.suggestions.peeks_dropped;
        let read = {
            let (root, path) = key.clone();
            cx.background_executor()
                .spawn(async move { suggest::peek(&root, &path) })
        };
        cx.spawn(async move |view, cx| {
            let peek = read.await;
            let _ = view.update(cx, |view, cx| {
                let suggestions = &mut view.suggestions;
                if suggestions.peeks_dropped != dropped {
                    return;
                }
                if suggestions.peeks.len() >= PEEKS {
                    suggestions.peeks.clear();
                }
                if suggestions.peeking.as_ref() == Some(&key) {
                    suggestions.peeking = None;
                }
                suggestions.peeks.insert(key, peek);
                // The highlighted file's own preview may have just been dropped: read again.
                view.peek_highlighted(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Whether the list has the keys now: it is shown (open, with room for it), and the
    /// message box has the focus with no text being composed (an input method's keys are its
    /// own).
    fn suggestion_keys(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.suggestions.open.is_some()
            && self.suggestion_room().is_some()
            && self.composer.read(cx).focus_handle(cx).is_focused(window)
            && !self.composer.update(cx, |state, cx| {
                state.marked_text_range(window, cx).is_some()
            })
    }

    pub(super) fn suggestion_up(
        &mut self,
        _: &MoveUp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.suggestion_keys(window, cx) {
            cx.stop_propagation();
            self.step_suggestion(false, cx);
        }
    }

    pub(super) fn suggestion_down(
        &mut self,
        _: &MoveDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.suggestion_keys(window, cx) {
            cx.stop_propagation();
            self.step_suggestion(true, cx);
        }
    }

    /// ⏎ accepts the highlighted row and never sends nor answers a request while the list is
    /// shown. A held ⏎ accepts once: its repeats are taken for the held key that
    /// `capture_enter` already ignores, so they do not send the draft once the list closes.
    /// ⇧⏎ is still a new line.
    pub(super) fn suggestion_enter(
        &mut self,
        enter: &Enter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if enter.secondary || enter.shift || !self.suggestion_keys(window, cx) {
            return;
        }
        cx.stop_propagation();
        let repeat = self.enter_down;
        self.enter_down = true;
        if !repeat {
            self.accept_highlighted(window, cx);
        }
    }

    /// ⇥ accepts as ⏎ does.
    pub(super) fn suggestion_tab(
        &mut self,
        _: &IndentInline,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.suggestion_keys(window, cx) {
            cx.stop_propagation();
            self.accept_highlighted(window, cx);
        }
    }

    /// ⎋ closes the list, and nothing else: a request waiting is not denied by it.
    pub(super) fn suggestion_escape(
        &mut self,
        _: &Escape,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.suggestion_keys(window, cx) {
            cx.stop_propagation();
            self.escape_down = true;
            self.dismiss_suggestions(cx);
        }
    }

    pub(super) fn dismiss_suggestions(&mut self, cx: &mut Context<Self>) {
        if let Some(open) = self.suggestions.open.take() {
            self.suggestions.dismissed = Some(open.trigger.start);
            cx.notify();
        }
    }

    /// Move the highlight one row, round from the last to the first and back.
    fn step_suggestion(&mut self, down: bool, cx: &mut Context<Self>) {
        let Some(open) = self.suggestions.open.as_mut() else {
            return;
        };
        let rows = open.items.len();
        if rows == 0 {
            return;
        }
        open.highlight = if down {
            (open.highlight + 1) % rows
        } else {
            (open.highlight + rows - 1) % rows
        };
        self.suggestions.scroll.scroll_to_item(open.highlight);
        self.peek_highlighted(cx);
        cx.notify();
    }

    fn highlight_suggestion(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(open) = self.suggestions.open.as_mut() else {
            return;
        };
        if open.highlight != row && row < open.items.len() {
            open.highlight = row;
            self.peek_highlighted(cx);
            cx.notify();
        }
    }

    fn accept_highlighted(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(row) = self.suggestions.open.as_ref().map(|open| open.highlight) {
            self.accept_suggestion(row, window, cx);
        }
    }

    /// Put row `row`'s text in place of the token at the caret, and a space after it, as an
    /// edit of the box that ⌘Z takes back. Nothing is sent.
    pub(super) fn accept_suggestion(
        &mut self,
        row: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(open) = self.suggestions.open.take() else {
            return;
        };
        let Some(insert) = open.items.get(row).map(Item::insertion) else {
            self.suggestions.open = Some(open);
            return;
        };
        let (text, caret) = {
            let state = self.composer.read(cx);
            (state.value().to_string(), state.cursor())
        };
        // The draft may have changed since the list was drawn: accept into its token now.
        let Some(trigger) = suggest::trigger(&text, caret)
            .filter(|now| now.start == open.trigger.start && now.sigil == open.trigger.sigil)
        else {
            cx.notify();
            return;
        };
        let (after, caret) = suggest::accept(&text, &trigger, &insert);
        let replacement = after[trigger.start..after.len() - (text.len() - trigger.end)].to_owned();
        self.composer.update(cx, |state, cx| {
            state.set_selected_range(trigger.start..trigger.end, cx);
            state.replace(replacement, window, cx);
            state.set_selected_range(caret..caret, cx);
        });
        cx.notify();
    }

    /// Something that records the card's bounds as it is drawn, for the list to stand on. The
    /// list is placed before the card is laid out, from the frame before: when the card moved
    /// or changed size under an open list, the window is drawn again.
    pub(super) fn suggestion_anchor(&self) -> AnyElement {
        let card = self.suggestions.card.clone();
        let open = self.suggestions.open.is_some();
        canvas(
            move |bounds, window, cx| {
                if card.replace(bounds) != bounds && open {
                    window.defer(cx, |window, _| window.refresh());
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .into_any_element()
    }

    /// The list and its preview, over the chat and just above the message box card, if the
    /// list is open.
    pub(super) fn suggestion_popup(
        &self,
        look: Look,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let open = self.suggestions.open.as_ref()?;
        let room = self.suggestion_room()?;
        let card = self.suggestions.card.get();
        let gap = ui_text::space(4.0);
        let list_width = ui_text::space(LIST_WIDTH).min(card.size.width);
        let preview_width = ui_text::space(PREVIEW_WIDTH);
        let beside = card.size.width >= list_width + gap + preview_width;
        let width = if beside {
            list_width + gap + preview_width
        } else {
            list_width
        };
        // The list starts at the caret, as far as the card lets it.
        let caret = self
            .composer
            .read(cx)
            .cursor_layout()
            .map_or(card.left(), |(caret, _)| caret.left());
        let left = (caret - ui_text::space(12.0))
            .min(card.right() - width)
            .max(card.left());
        let tall = ui_text::space(MAX_HEIGHT).min(room);
        let (list_height, preview_height) = if beside {
            (tall, tall)
        } else {
            // A narrow pane stacks the preview over the list, the list next to the box.
            let list = (room * 0.55).min(ui_text::space(200.0));
            (list, (room - list - gap).max(px(0.)))
        };
        let list = self.suggestion_list(open, look, list_width, list_height, cx);
        let preview = (preview_height >= ui_text::space(48.0))
            .then(|| {
                let width = if beside { preview_width } else { list_width };
                self.suggestion_preview(open, look, width, preview_height)
            })
            .flatten();
        let body = div()
            .id("composer-suggestions")
            .debug_selector(|| "composer-suggestions".into())
            .occlude()
            .flex()
            .gap(gap)
            .when(beside, |body| body.flex_row().items_end())
            .when(!beside, |body| body.flex_col())
            .font_family(look.chat_family())
            .text_size(ui_text::text(11.0))
            // The pointer works the rows; it never takes the focus from the message box.
            .on_mouse_down(gpui::MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            });
        let body = if beside {
            body.child(list).children(preview)
        } else {
            body.children(preview).child(list)
        }
        .test_support();
        Some(
            deferred(
                anchored()
                    .anchor(gpui::Anchor::BottomLeft)
                    .position(point(left, card.top() - gap))
                    .child(body),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// The room above the message box card, as last drawn, that the list may stand in: all of
    /// it below the window's top edge. None while the card is not drawn or the room is too
    /// little for a row, when the list is not shown and has no keys.
    fn suggestion_room(&self) -> Option<Pixels> {
        let card = self.suggestions.card.get();
        if card.size.width <= px(0.) {
            return None;
        }
        let room = (card.top() - ui_text::space(8.0) - ui_text::space(4.0)).max(px(0.));
        (room >= ui_text::space(28.0)).then_some(room)
    }

    fn suggestion_list(
        &self,
        open: &Open,
        look: Look,
        width: Pixels,
        height: Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = look.colors;
        let title = match open.trigger.sigil {
            Sigil::Slash => "Commands".to_owned(),
            Sigil::Mention => match self.suggestion_root() {
                Some(root) => format!(
                    "Files in {}",
                    root.file_name().map_or_else(
                        || root.display().to_string(),
                        |name| name.to_string_lossy().into_owned()
                    )
                ),
                None => "Files".to_owned(),
            },
        };
        let note = if open.loading {
            Some("Listing files…".to_owned())
        } else if open.truncated {
            Some(format!(
                "First {} entries of the folder",
                suggest::FILE_BOUND
            ))
        } else {
            None
        };
        let rows = open.items.iter().enumerate().map(|(row, item)| {
            let highlighted = row == open.highlight;
            let label = item.insertion();
            div()
                .id(ElementId::NamedInteger(
                    "composer-suggestion".into(),
                    row as u64,
                ))
                .role(gpui::Role::ListBoxOption)
                .aria_label(SharedString::from(label.clone()))
                .aria_selected(highlighted)
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .px(ui_text::space(8.0))
                .py(ui_text::space(3.0))
                .rounded(ui_text::space(4.0))
                .cursor_pointer()
                .when(highlighted, |row| {
                    row.bg(rgb(look.tint(colors.focus, 0.22)))
                })
                .on_mouse_move(cx.listener(move |view, _: &gpui::MouseMoveEvent, _, cx| {
                    view.highlight_suggestion(row, cx);
                }))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |view, _: &gpui::MouseDownEvent, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                        view.accept_suggestion(row, window, cx);
                    }),
                )
                .child(
                    div()
                        .truncate()
                        .font_family(ui_text::code_family())
                        .text_color(rgb(colors.text))
                        .child(label),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(ui_text::text(10.0))
                        .text_color(rgb(colors.muted))
                        .child(item.detail()),
                )
                .test_support()
        });
        div()
            .id("composer-suggestion-list")
            .role(gpui::Role::ListBox)
            .aria_label(SharedString::from(title.clone()))
            .w(width)
            .max_h(height)
            .flex()
            .flex_col()
            .p(ui_text::space(4.0))
            .rounded(ui_text::space(6.0))
            .border_1()
            .border_color(rgb(colors.divider))
            .bg(rgb(colors.panel))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .justify_between()
                    .gap(ui_text::space(6.0))
                    .px(ui_text::space(8.0))
                    .py(ui_text::space(2.0))
                    .text_size(ui_text::text(10.0))
                    .text_color(rgb(colors.muted))
                    .child(div().truncate().child(title))
                    .children(note.map(|note| div().flex_none().child(note))),
            )
            .child(
                div()
                    .id("composer-suggestion-rows")
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .track_scroll(&self.suggestions.scroll)
                    .children(rows),
            )
            .test_support()
            .into_any_element()
    }

    fn suggestion_preview(
        &self,
        open: &Open,
        look: Look,
        width: Pixels,
        height: Pixels,
    ) -> Option<AnyElement> {
        let colors = look.colors;
        let preview = match open.items.get(open.highlight)? {
            Item::Command(command) => suggest::command_preview(command, self.provider()?),
            Item::File(entry) => {
                let peek = self
                    .suggestion_root()
                    .and_then(|root| self.suggestions.peeks.get(&(root, entry.path.clone())));
                let mut preview = suggest::file_preview(entry, peek);
                if peek.is_none() && !entry.dir {
                    preview.body = vec!["Reading…".to_owned()];
                }
                preview
            }
        };
        let code = matches!(open.items.get(open.highlight), Some(Item::File(_)));
        Some(
            div()
                .id("composer-suggestion-preview")
                .role(gpui::Role::Group)
                .aria_label(SharedString::from(format!("Preview of {}", preview.title)))
                .w(width)
                .max_h(height)
                .overflow_hidden()
                .flex()
                .flex_col()
                .gap(ui_text::space(4.0))
                .p(ui_text::space(8.0))
                .rounded(ui_text::space(6.0))
                .border_1()
                .border_color(rgb(colors.divider))
                .bg(rgb(colors.panel))
                .child(
                    div()
                        .truncate()
                        .font_family(ui_text::code_family())
                        .text_color(rgb(colors.text))
                        .child(preview.title.clone()),
                )
                .child(
                    div()
                        .text_size(ui_text::text(10.0))
                        .text_color(rgb(colors.muted))
                        .child(preview.kind.clone()),
                )
                .children((!preview.body.is_empty()).then(|| {
                    div()
                        .flex()
                        .flex_col()
                        .px(ui_text::space(6.0))
                        .py(ui_text::space(4.0))
                        .rounded(ui_text::space(4.0))
                        .bg(rgb(colors.bg))
                        .when(code, |body| {
                            body.font_family(ui_text::code_family())
                                .text_size(ui_text::text(10.0))
                        })
                        .children(
                            preview
                                .body
                                .iter()
                                .map(|line| div().truncate().child(line.clone())),
                        )
                }))
                .child(
                    div()
                        .flex()
                        .gap(ui_text::space(4.0))
                        .min_w_0()
                        .child(
                            div()
                                .flex_none()
                                .text_color(rgb(colors.muted))
                                .child("Inserts"),
                        )
                        .child(
                            div()
                                .truncate()
                                .font_family(ui_text::code_family())
                                .child(preview.inserts.clone()),
                        ),
                )
                .child(
                    div()
                        .text_size(ui_text::text(10.0))
                        .text_color(rgb(colors.muted))
                        .child(preview.effect.clone()),
                )
                .child(
                    div()
                        .text_size(ui_text::text(10.0))
                        .text_color(rgb(colors.muted))
                        .child("⏎ or ⇥ inserts · ⎋ closes · nothing is sent"),
                )
                .test_support()
                .into_any_element(),
        )
    }
}

#[cfg(test)]
impl ChatView {
    pub(super) fn suggestion_root_for_tests(&self) -> Option<PathBuf> {
        self.suggestion_root()
    }

    pub(super) fn suggestion_peek_for_tests(&self, root: &Path, path: &str) -> Option<Peek> {
        self.suggestions
            .peeks
            .get(&(root.to_path_buf(), path.to_owned()))
            .cloned()
    }

    /// Fill the kept previews up to their bound with ones of another folder.
    pub(super) fn fill_suggestion_peeks_for_tests(&mut self) {
        for n in 0.. {
            if self.suggestions.peeks.len() >= PEEKS {
                break;
            }
            self.suggestions
                .peeks
                .insert((PathBuf::from("/elsewhere"), format!("{n}")), Peek::Binary);
        }
    }
}
