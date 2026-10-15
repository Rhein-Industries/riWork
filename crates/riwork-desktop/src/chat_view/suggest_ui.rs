//! The `/`, `@` and `&` suggestions over the message box, from `chat::suggest`: a list just
//! above the card, starting at the caret, and beside it (or above it, in a narrow pane) a card
//! that says what the highlighted entry is and what accepting it does. ↑ and ↓ move the
//! highlight, ⏎ or ⇥ accept it, ⎋ closes the list until that token is typed again; the
//! pointer highlights a row and a click accepts it. Accepting only edits the draft: sending is
//! still ⏎ or Send afterwards. The list stands wholly above the card, cut to the room there,
//! so it never covers Send, Stop or the pickers.
//!
//! `&` lists every provider's models (`suggest::mention_entries`): the ones the chat's driver
//! listed (the model menu's own list), then what each provider offers as its saved chats tell
//! (`catalog::mention_lists`), each row naming its provider, the most used first
//! (`suggest::ModelUsage`); both are read from the chats' saved metadata on a background thread
//! and kept for every tab of that data folder and project as a listing is. The chat's own model
//! is marked. Accepting one puts in its mention (`&gpt-6.1-sol`), as text for an orchestrator to
//! read: the chat's model, effort and everything else stay as they are, and nothing is sent.
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
    chat::{
        model::{ModelOption, Provider},
        suggest::{
            self, Command, FileEntry, Listing, ModelEntry, ModelUsage, Peek, Sigil, Trigger,
        },
    },
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
    /// A model of the chat's provider, and whether the chat runs it now.
    Model {
        entry: ModelEntry,
        current: bool,
    },
}

impl Item {
    /// What accepting it puts in the draft, which is also how its row names it: a model's
    /// mention.
    pub fn insertion(&self) -> String {
        match self {
            Self::Command(command) => command.insertion(),
            Self::File(entry) => entry.insertion(),
            Self::Model { entry, .. } => entry.insertion(),
        }
    }

    /// Whether it is the same entry as `other`: a model by its id and provider, whatever its use
    /// or mark.
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Model { entry: a, .. }, Self::Model { entry: b, .. }) => {
                a.id == b.id && a.provider == b.provider
            }
            _ => self == other,
        }
    }

    /// The row's second line: what a command is, what kind of thing a file is, whether the
    /// chat runs a model and how much it is used.
    pub(super) fn detail(&self) -> String {
        match self {
            Self::Command(command) => command.summary.clone(),
            Self::File(entry) => suggest::file_preview(entry, None).kind,
            Self::Model { entry, current } => suggest::model_detail(entry, *current),
        }
    }
}

/// The list while it is open.
pub(super) struct Open {
    pub trigger: Trigger,
    pub items: Vec<Item>,
    pub highlight: usize,
    /// The chat's folder is still being listed for the first time, or the models' lists and use
    /// read.
    pub loading: bool,
    /// Its listing stopped at `suggest::FILE_BOUND`.
    pub truncated: bool,
    /// What a `&` list was ranked from: another one ranks it again.
    pub basis: Option<ModelBasis>,
    /// The person moved the highlight (keys or pointer): a `&` list ordered again by use keeps
    /// it on their row; one never moved keeps it on the first, the most used.
    pub moved: bool,
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

/// What a `&` list is ranked from, besides what is read in the background: the chat's provider,
/// the model it is set to (none: its provider's default; another one marks another row), and the
/// ids its driver listed (a list that arrives adds rows).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ModelBasis {
    provider: Provider,
    chosen: Option<String>,
    own: Vec<String>,
}

/// What `&` lists beyond the chat's own models: each provider's saved list and how much each
/// model is used.
struct ModelSources {
    lists: Vec<(Provider, Vec<ModelOption>)>,
    usage: ModelUsage,
}

/// The data folder and the project (which picks the Codex account a list is for) of a
/// `ModelSources`.
type SourcesKey = (PathBuf, Option<String>);

/// `ModelSources` by data folder and project, shared by every chat tab: read once from the
/// chats' saved metadata, kept `suggest::FILE_TTL`, and read again only once stale.
#[derive(Default)]
struct ModelReads {
    read: HashMap<SourcesKey, (Instant, Arc<ModelSources>)>,
    /// The ones being read, and the tabs waiting for them.
    reading: HashMap<SourcesKey, Vec<WeakEntity<ChatView>>>,
}

impl Global for ModelReads {}

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
        // A `&` list is ranked again when the chat's model or its driver's list changed under it.
        let basis = found
            .as_ref()
            .filter(|found| found.sigil == Sigil::Model)
            .and_then(|_| self.model_basis());
        let unchanged = match (&self.suggestions.open, &found) {
            (Some(open), Some(found)) => {
                open.trigger == *found && (found.sigil != Sigil::Model || open.basis == basis)
            }
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
                    basis: None,
                    moved: false,
                })
            }
            Sigil::Model => {
                let provider = self.provider()?;
                // Until the saved lists are read, the chat's own list; no model is made up.
                let sources = self.model_sources(cx);
                let reading = sources.is_none() && self.catalog_home.is_some();
                let entries = match &sources {
                    Some(sources) => suggest::mention_entries(
                        provider,
                        &self.model.transcript.models,
                        &sources.lists,
                        &sources.usage,
                    ),
                    None => suggest::mention_entries(
                        provider,
                        &self.model.transcript.models,
                        &[],
                        &ModelUsage::default(),
                    ),
                };
                let basis = self.model_basis();
                let current = suggest::current_model(
                    &entries,
                    provider,
                    basis.as_ref().and_then(|basis| basis.chosen.as_deref()),
                )
                .map(str::to_owned);
                let items: Vec<_> = suggest::rank_models(&trigger.query, &entries)
                    .into_iter()
                    .map(|entry| Item::Model {
                        current: entry.provider == Some(provider)
                            && current.as_deref() == Some(entry.id.as_str()),
                        entry: entry.clone(),
                    })
                    .collect();
                (reading || !items.is_empty()).then_some(Open {
                    trigger,
                    items,
                    highlight: 0,
                    loading: reading,
                    truncated: false,
                    basis,
                    moved: false,
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
                    basis: None,
                    moved: false,
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

    /// What a `&` list is ranked from now (see `ModelBasis`).
    fn model_basis(&self) -> Option<ModelBasis> {
        let transcript = &self.model.transcript;
        Some(ModelBasis {
            provider: self.provider()?,
            chosen: transcript.info.as_ref().and_then(|info| info.model.clone()),
            own: transcript
                .models
                .iter()
                .map(|model| model.id.clone())
                .collect(),
        })
    }

    /// The providers' saved lists and the models' use, as read for the chats' data folder and
    /// the chat's project, reading them again (once, for every tab) when they are stale or
    /// missing. Never waits: stale ones are used meanwhile, and none while they are read for the
    /// first time. None without a data folder.
    fn model_sources(&mut self, cx: &mut Context<Self>) -> Option<Arc<ModelSources>> {
        let home = self.catalog_home.clone()?;
        let project = self
            .model
            .transcript
            .info
            .as_ref()
            .and_then(|info| info.project_id.clone());
        let key = (home, project);
        let me = cx.weak_entity();
        let reads = cx.default_global::<ModelReads>();
        let kept = reads.read.get(&key).cloned();
        if let Some((read, sources)) = &kept
            && read.elapsed() < suggest::FILE_TTL
        {
            return Some(sources.clone());
        }
        let waiting = reads.reading.entry(key.clone()).or_default();
        let start = waiting.is_empty();
        if !waiting.iter().any(|view| view == &me) {
            waiting.push(me);
        }
        if start {
            let read = {
                let (home, project) = key.clone();
                cx.background_executor().spawn(async move {
                    let infos = crate::chat::log::read_infos(&home);
                    ModelSources {
                        lists: crate::chat::catalog::mention_lists(
                            &home,
                            project.as_deref(),
                            &infos,
                        ),
                        usage: ModelUsage::of(&infos),
                    }
                })
            };
            cx.spawn(async move |_, cx| {
                let sources = read.await;
                cx.update(|cx| {
                    let reads = cx.default_global::<ModelReads>();
                    reads
                        .read
                        .insert(key.clone(), (Instant::now(), Arc::new(sources)));
                    let waiting = reads.reading.remove(&key).unwrap_or_default();
                    for view in waiting {
                        let _ = view.update(cx, |view, cx| view.model_sources_arrived(cx));
                    }
                });
            })
            .detach();
        }
        kept.map(|(_, sources)| sources)
    }

    /// The providers' lists and the models' use were read: an open `&` list is worked out again
    /// from them, the highlighted model staying so once the person moved to it.
    pub(super) fn model_sources_arrived(&mut self, cx: &mut Context<Self>) {
        self.reopen_with(Sigil::Model, cx);
    }

    /// A folder's listing came: an open `@` list is worked out again from it.
    fn listing_arrived(&mut self, cx: &mut Context<Self>) {
        self.reopen_with(Sigil::Mention, cx);
    }

    /// Work an open list of `sigil` out again, from what has come since; the row that was
    /// highlighted stays so if it is still there.
    fn reopen_with(&mut self, sigil: Sigil, cx: &mut Context<Self>) {
        let Some(open) = self
            .suggestions
            .open
            .take_if(|open| open.trigger.sigil == sigil)
        else {
            return;
        };
        let highlighted = open
            .items
            .get(open.highlight)
            .cloned()
            .filter(|_| open.moved || sigil != Sigil::Model);
        let moved = open.moved;
        self.suggestions.open = self.rank_suggestions(open.trigger, cx).map(|mut fresh| {
            // The row that was highlighted stays so if it is still there.
            if let Some(at) =
                highlighted.and_then(|item| fresh.items.iter().position(|i| i.same(&item)))
            {
                fresh.highlight = at;
                fresh.moved = moved;
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
        open.moved = true;
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
            open.moved = true;
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
            Sigil::Model => "Models · most used first".to_owned(),
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
        let note = if open.loading && open.trigger.sigil == Sigil::Model {
            Some("Reading models…".to_owned())
        } else if open.loading {
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
            Item::Model { entry, current } => {
                suggest::model_preview(entry, *current, self.provider()?)
            }
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
