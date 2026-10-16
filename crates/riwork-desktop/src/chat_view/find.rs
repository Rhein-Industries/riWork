//! Find in the chat: ⌘F in a focused chat opens a bar above the transcript that finds what
//! the transcript shows, without regard to case, as the words are typed.
//!
//! What is searched is the transcript selection's source (`selection_document`): the text
//! each drawn leaf shows, byte for byte, for every row of the virtual list whether it is drawn
//! now or not. A match lies inside one leaf. The matches are painted under the glyphs of the
//! leaves that are drawn (`select`), and the current one is brought into view over the next
//! frames, by the list and by a code or output box it sits in (`reveal_find`).
//!
//! Each chat tab keeps its own bar: it stays open, with its words, while another tab is shown.

use std::{collections::HashMap, ops::Range, rc::Rc};

use gpui::{
    AnyElement, Bounds, Context, Entity, FocusHandle, Focusable, Hsla, ListOffset, MouseButton,
    Pixels, Window, div, point, prelude::*, px, rgb,
};
use gpui_kit::base::TestSupportExt as _;

use super::{
    ChatView, FindInChat, FindNext, FindPrevious,
    composer::{BAR_GAP, BAR_INSET},
    selection_document::SourceLeaf,
    widgets::Look,
};
use crate::{
    behavior_controls as behavior, icons,
    text_input::{self, InputEvent, InputState},
    tooltip, ui_text,
};

/// How many times a reveal looks at a frame before it gives up on a match it cannot bring
/// into view.
const REVEAL_TRIES: u8 = 24;

/// A match: bytes of one leaf of the selection document's source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Hit {
    /// The leaf's place in the source, in reading order.
    pub leaf: usize,
    pub range: Range<usize>,
}

/// What the transcript paints of an open find bar.
#[derive(Clone)]
pub(super) struct FindPaint {
    /// The source the matches were found in; another source paints none of them.
    pub generation: u64,
    /// In reading order.
    pub hits: Rc<[Hit]>,
    pub current: Option<usize>,
    pub fill: Hsla,
    pub strong: Hsla,
    pub edge: Hsla,
}

/// One leaf as it is searched: its text in lower case, and its matches of the last query.
struct Folded {
    text: String,
    folded: String,
    /// For text whose lower case changed a character's length: the byte of `text` where the
    /// character each byte of `folded` came from starts.
    origin: Option<Vec<u32>>,
    /// The query generation `hits` are of.
    query: u64,
    hits: Vec<Range<usize>>,
    /// The last search that met this leaf.
    seen: u64,
}

impl Folded {
    fn new(text: &str) -> Self {
        let (folded, origin) = fold(text);
        Self {
            text: text.to_owned(),
            folded,
            origin,
            query: 0,
            hits: Vec::new(),
            seen: 0,
        }
    }

    fn find(&self, query: &str) -> Vec<Range<usize>> {
        self.folded
            .match_indices(query)
            .map(|(at, _)| match &self.origin {
                None => at..at + query.len(),
                Some(origin) => {
                    let start = origin[at] as usize;
                    let last = origin[at + query.len() - 1] as usize;
                    let end = last + self.text[last..].chars().next().map_or(0, char::len_utf8);
                    start..end
                }
            })
            .collect()
    }
}

/// `text` in lower case, and where its bytes came from when that is not byte for byte.
fn fold(text: &str) -> (String, Option<Vec<u32>>) {
    if text.is_ascii() {
        return (text.to_ascii_lowercase(), None);
    }
    let mut folded = String::with_capacity(text.len());
    let mut aligned = true;
    let mut rest = text;
    while !rest.is_empty() {
        // Runs of ASCII are lowered whole; only the characters between them one by one.
        let ascii = rest
            .bytes()
            .position(|b| !b.is_ascii())
            .unwrap_or(rest.len());
        let from = folded.len();
        folded.push_str(&rest[..ascii]);
        folded[from..].make_ascii_lowercase();
        rest = &rest[ascii..];
        if let Some(c) = rest.chars().next() {
            let mut lower = c.to_lowercase();
            aligned &= lower.len() == 1 && lower.clone().all(|l| l.len_utf8() == c.len_utf8());
            folded.extend(&mut lower);
            rest = &rest[c.len_utf8()..];
        }
    }
    if aligned {
        return (folded, None);
    }
    let mut origin = Vec::with_capacity(folded.len());
    for (at, c) in text.char_indices() {
        let len: usize = c.to_lowercase().map(char::len_utf8).sum();
        origin.extend(std::iter::repeat_n(at as u32, len));
    }
    (folded, Some(origin))
}

/// What is typed, in the lower case the leaves are searched in.
fn fold_query(query: &str) -> String {
    query.chars().flat_map(char::to_lowercase).collect()
}

/// The leaves as last searched, by key, so that typing and a streaming turn only fold and
/// search the text that changed. A query that contains the last one is not looked for again
/// in a leaf the last one was not found in.
#[derive(Default)]
struct Index {
    leaves: HashMap<String, Folded>,
    searches: u64,
    /// The last query, folded, and its generation.
    last: Option<(u64, String)>,
    queries: u64,
}

impl Index {
    /// Every match of `query` (folded, not empty) in `source`, in reading order.
    fn search(&mut self, source: &[SourceLeaf], query: &str) -> Vec<Hit> {
        self.searches += 1;
        let (generation, narrows) = match &self.last {
            Some((generation, last)) if last == query => (*generation, None),
            last => {
                let narrows = last
                    .as_ref()
                    .filter(|(_, last)| query.contains(last.as_str()))
                    .map(|(generation, _)| *generation);
                self.queries += 1;
                (self.queries, narrows)
            }
        };
        self.last = Some((generation, query.to_owned()));
        let mut hits = Vec::new();
        for (at, leaf) in source.iter().enumerate() {
            if self
                .leaves
                .get(&leaf.key)
                .is_none_or(|folded| folded.text != leaf.text)
            {
                self.leaves
                    .insert(leaf.key.clone(), Folded::new(&leaf.text));
            }
            let folded = self.leaves.get_mut(&leaf.key).expect("inserted above");
            if folded.query != generation {
                if !(narrows == Some(folded.query) && folded.hits.is_empty()) {
                    folded.hits = folded.find(query);
                }
                folded.query = generation;
            }
            folded.seen = self.searches;
            hits.extend(folded.hits.iter().map(|range| Hit {
                leaf: at,
                range: range.clone(),
            }));
        }
        let searches = self.searches;
        self.leaves.retain(|_, folded| folded.seen == searches);
        hits
    }
}

/// A current match on its way into view: each step waits for a frame laid out after the
/// last scroll.
struct Reveal {
    /// The transcript frame count a step needs to see.
    ready_at: u64,
    /// Whether the match's row was scrolled to without knowing where in it the match is: it
    /// is then placed a third of the way down even though it is already in view.
    place: bool,
    tries: u8,
}

/// A chat's find bar.
pub(super) struct Find {
    pub(super) open: bool,
    pub(super) input: Entity<InputState>,
    /// Holds the focus of everything in the bar.
    bar: FocusHandle,
    hits: Rc<[Hit]>,
    current: Option<usize>,
    /// The current match by leaf key and byte, kept while the transcript changes.
    anchor: Option<(String, usize)>,
    /// The source generation `hits` were found in.
    searched: Option<u64>,
    index: Index,
    /// What had the keys when the bar opened; they go back there when it closes.
    restore: Option<FocusHandle>,
    /// The transcript frame count when the bar opened: the bar takes height from the list,
    /// so what the frames before it showed is not what is in view.
    opened_at: u64,
    /// The frame count the last scroll a reveal made is first drawn in. Until then a frame
    /// shows places from before it, and a new reveal (the next letter typed in the same
    /// frame) must not move by them again.
    scrolled_at: u64,
    reveal: Option<Reveal>,
    reveal_scheduled: bool,
}

impl Find {
    pub(super) fn new(window: &mut Window, cx: &mut Context<ChatView>) -> Self {
        Self {
            open: false,
            input: text_input::single_line("", "Find in chat", window, cx),
            bar: cx.focus_handle(),
            hits: Rc::from([]),
            current: None,
            anchor: None,
            searched: None,
            index: Index::default(),
            restore: None,
            opened_at: 0,
            scrolled_at: 0,
            reveal: None,
            reveal_scheduled: false,
        }
    }

    /// The view moved to another window: its transcript frames are counted afresh, and a
    /// next-frame callback of the old window will never come.
    pub(super) fn new_window(&mut self) {
        self.opened_at = 0;
        self.scrolled_at = 0;
        self.reveal_scheduled = false;
    }

    pub(super) fn field_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.input.read(cx).focus_handle(cx).is_focused(window)
    }

    /// The current match's place and the number of matches, for a window under test.
    #[cfg(test)]
    pub(super) fn status(&self) -> (Option<usize>, usize) {
        (self.current, self.hits.len())
    }

    #[cfg(test)]
    pub(super) fn current_hit(&self) -> Option<Hit> {
        self.hits.get(self.current?).cloned()
    }

    #[cfg(test)]
    pub(super) fn revealing(&self) -> bool {
        self.reveal.is_some()
    }
}

impl ChatView {
    pub(super) fn find_action(
        &mut self,
        _: &FindInChat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_find(window, cx);
    }

    /// ⌘G and ⌘⇧G step while the bar is open; otherwise the keys are the window's.
    pub(super) fn find_next_action(
        &mut self,
        _: &FindNext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.find.open {
            self.step_find(true, window, cx);
        } else {
            cx.propagate();
        }
    }

    pub(super) fn find_previous_action(
        &mut self,
        _: &FindPrevious,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.find.open {
            self.step_find(false, window, cx);
        } else {
            cx.propagate();
        }
    }

    /// ⌘F: the bar opens with its field focused and its words selected. One line of selected
    /// transcript text becomes the words, as on the Mac. A chat with no transcript yet leaves
    /// the key to the window.
    pub(super) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.chat_id.is_none() || self.deleted() {
            cx.propagate();
            return;
        }
        let selected = self.transcript_selection.selected_text(cx);
        let seed = (!selected.trim().is_empty() && !selected.contains('\n')).then_some(selected);
        let opening = !self.find.open;
        if opening {
            self.find.open = true;
            self.find.restore = window.focused(cx);
            self.find.opened_at = self.transcript_selection.paints();
        }
        let input = self.find.input.clone();
        let seeded = match seed {
            Some(seed) if input.read(cx).value().as_ref() != seed.as_str() => {
                input.update(cx, |state, cx| state.set_value(seed, window, cx));
                true
            }
            _ => false,
        };
        input.update(cx, |state, cx| {
            state.focus(window, cx);
            state.select_all(window, cx);
        });
        if opening || seeded {
            self.find_matches(true, window, cx);
        }
        cx.notify();
    }

    /// Esc or the close button: the matches go, and the keys go back to where they were when
    /// the bar opened if they are still in it.
    pub(super) fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.find.open {
            return;
        }
        let had_keys = self.find.bar.contains_focused(window, cx) || window.focused(cx).is_none();
        let restore = self.find.restore.take();
        self.find.open = false;
        self.find.hits = Rc::from([]);
        self.find.current = None;
        self.find.anchor = None;
        self.find.searched = None;
        self.find.reveal = None;
        self.find.reveal_scheduled = false;
        self.find.index = Index::default();
        self.transcript_selection.set_find(None);
        if had_keys {
            restore
                .unwrap_or_else(|| self.focus.clone())
                .focus(window, cx);
        }
        cx.notify();
    }

    /// Whether Esc closes the bar: it is open and the keys are in it or in the transcript.
    pub(super) fn find_takes_escape(&self, window: &Window, cx: &gpui::App) -> bool {
        self.find.open
            && (self.find.bar.contains_focused(window, cx) || self.focus.is_focused(window))
    }

    pub(super) fn find_event(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                self.find_matches(true, window, cx);
                cx.notify();
            }
            InputEvent::PressEnter { shift, .. } => self.step_find(!shift, window, cx),
            // As the chat's other fields do: the keys are not in the transcript's selection.
            InputEvent::Focus => gpui_kit::base::TextSelection::clear(window, cx),
            _ => {}
        }
    }

    /// Keep the matches to the transcript shown, while the bar is open: a new source (a turn
    /// streaming, a row expanded) is searched again with the current match kept, and nothing
    /// scrolls.
    pub(super) fn refresh_find(&mut self, look: Look, window: &mut Window, cx: &mut Context<Self>) {
        if !self.find.open {
            return;
        }
        if self.find.searched != Some(self.transcript_selection.generation()) {
            self.find_matches(false, window, cx);
        }
        self.paint_find(look);
    }

    /// Search the transcript for the field's words. With `jump`, the first match at or below
    /// the top of the view becomes the current one (else the first) and is brought into view;
    /// without, the current match stays the one it was.
    fn find_matches(&mut self, jump: bool, window: &mut Window, cx: &mut Context<Self>) {
        let look = Look::of(cx);
        self.refresh_selection_document(look, cx);
        let query = fold_query(&self.find.input.read(cx).value());
        let hits: Rc<[Hit]> = if query.is_empty() {
            Rc::from([])
        } else {
            let index = &mut self.find.index;
            self.transcript_selection
                .with_source(|source| index.search(source, &query))
                .into()
        };
        self.find.searched = Some(self.transcript_selection.generation());
        self.find.current = if hits.is_empty() {
            None
        } else if jump || self.find.anchor.is_none() {
            Some(self.first_in_view(&hits))
        } else {
            self.kept_current(&hits)
        };
        self.find.hits = hits;
        self.remember_current();
        self.paint_find(look);
        if jump {
            self.find.reveal = None;
            if self.find.current.is_some() {
                self.start_reveal(window, cx);
            }
        }
    }

    /// The first match whose row is at or below the top of the view, and within the top row
    /// one that is drawn below its top edge.
    fn first_in_view(&self, hits: &[Hit]) -> usize {
        let top = self.list.logical_scroll_top();
        let edge = self.list.viewport_bounds().top();
        let selection = &self.transcript_selection;
        selection
            .with_source(|source| {
                let from = hits.partition_point(|hit| source[hit.leaf].row < top.item_ix);
                hits[from..]
                    .iter()
                    .position(|hit| {
                        source[hit.leaf].row > top.item_ix
                            || selection
                                .painted_rect(hit.leaf, hit.range.clone())
                                .is_none_or(|rect| rect.bottom() > edge)
                    })
                    .map(|at| from + at)
            })
            .unwrap_or(0)
    }

    /// The match the current one is in `hits`: the same, or the next one after where it was.
    fn kept_current(&self, hits: &[Hit]) -> Option<usize> {
        let (key, start) = self.find.anchor.as_ref()?;
        let last = hits.len() - 1;
        let leaf = self
            .transcript_selection
            .with_source(|source| source.iter().position(|leaf| &leaf.key == key));
        Some(match leaf {
            Some(leaf) => hits
                .partition_point(|hit| (hit.leaf, hit.range.start) < (leaf, *start))
                .min(last),
            None => self.find.current.unwrap_or(0).min(last),
        })
    }

    fn remember_current(&mut self) {
        let hit = self.find.current.and_then(|at| self.find.hits.get(at));
        self.find.anchor = hit.and_then(|hit| {
            self.transcript_selection.with_source(|source| {
                source
                    .get(hit.leaf)
                    .map(|leaf| (leaf.key.clone(), hit.range.start))
            })
        });
    }

    fn paint_find(&self, look: Look) {
        let colors = look.colors;
        self.transcript_selection.set_find(Some(FindPaint {
            generation: self.find.searched.unwrap_or_default(),
            hits: self.find.hits.clone(),
            current: self.find.current,
            fill: rgb(look.tint(colors.gold, 0.3)).into(),
            strong: rgb(look.tint(colors.gold, 0.6)).into(),
            edge: rgb(colors.gold).into(),
        }));
    }

    /// ⏎, ⇧⏎, ⌘G, ⌘⇧G and the arrows: the next or previous match, round from the last to the
    /// first and back, brought into view.
    fn step_find(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.find.hits.len();
        if count == 0 {
            return;
        }
        self.find.current = Some(match self.find.current {
            Some(at) if forward => (at + 1) % count,
            Some(at) => (at + count - 1) % count,
            None if forward => 0,
            None => count - 1,
        });
        self.remember_current();
        self.paint_find(Look::of(cx));
        self.start_reveal(window, cx);
        cx.notify();
    }

    fn start_reveal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find.reveal = Some(Reveal {
            ready_at: self
                .transcript_selection
                .paints()
                .max(self.find.opened_at + 1)
                .max(self.find.scrolled_at),
            place: false,
            tries: 0,
        });
        self.reveal_find(window, cx);
    }

    /// One step of bringing the current match into view, from where the last frame drew it.
    /// A match that is fully in view does not move. One in a row the list did not draw has
    /// its row scrolled to first; one in a code or output box has the box scrolled; then the
    /// list scrolls so the match sits a third of the way down.
    fn reveal_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut reveal) = self.find.reveal.take() else {
            return;
        };
        reveal.tries += 1;
        let hit = self
            .find
            .current
            .and_then(|at| self.find.hits.get(at))
            .cloned();
        let (Some(hit), true) = (hit, self.find.open && reveal.tries <= REVEAL_TRIES) else {
            return;
        };
        let paints = self.transcript_selection.paints();
        if paints < reveal.ready_at {
            self.find.reveal = Some(reveal);
            self.schedule_reveal(window, cx);
            return;
        }
        let Some((row, key)) = self.transcript_selection.with_source(|source| {
            source
                .get(hit.leaf)
                .map(|leaf| (leaf.row, leaf.key.clone()))
        }) else {
            return;
        };
        match self
            .transcript_selection
            .painted_rect(hit.leaf, hit.range.clone())
        {
            None => {
                self.list.scroll_to(ListOffset {
                    item_ix: row,
                    offset_in_item: px(0.),
                });
                reveal.place = true;
            }
            Some(rect) if self.scroll_box_to(&key, rect) => {}
            Some(rect) => {
                let view = self.list.viewport_bounds();
                let shown = rect.top() >= view.top() && rect.bottom() <= view.bottom();
                if shown && !reveal.place {
                    return;
                }
                self.list.pause_following_tail();
                self.list
                    .scroll_by(rect.top() - (view.top() + view.size.height * 0.3));
                reveal.place = false;
            }
        }
        reveal.ready_at = paints + 1;
        self.find.scrolled_at = paints + 1;
        self.find.reveal = Some(reveal);
        self.schedule_reveal(window, cx);
    }

    fn schedule_reveal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !std::mem::replace(&mut self.find.reveal_scheduled, true) {
            cx.on_next_frame(window, |view, window, cx| {
                view.find.reveal_scheduled = false;
                view.reveal_find(window, cx);
            });
        }
        cx.notify();
    }

    /// Scroll the code, output or diff box leaf `key` is drawn in, if any, so `rect` is in
    /// it. Whether it moved.
    fn scroll_box_to(&self, key: &str, rect: Bounds<Pixels>) -> bool {
        let handle = {
            let caches = self.caches.borrow();
            let scroller = caches.scrollers.get(key).or_else(|| {
                let file = key.strip_prefix("diff:")?.rsplit_once(':')?.0;
                caches.scrollers.get(&format!("diff:{file}"))
            });
            let Some(scroller) = scroller else {
                return false;
            };
            scroller.handle().clone()
        };
        let bounds = handle.bounds();
        let margin = ui_text::space(12.);
        // A match wider than the box shows its start and stays there.
        let shift = |start: Pixels, end: Pixels, low: Pixels, high: Pixels| {
            if start < low {
                low - start + margin
            } else if end > high {
                (high - end - margin).max(low + margin - start).min(px(0.))
            } else {
                px(0.)
            }
        };
        let offset = handle.offset();
        let max = handle.max_offset();
        let next = point(
            (offset.x + shift(rect.left(), rect.right(), bounds.left(), bounds.right()))
                .max(-max.x)
                .min(px(0.)),
            (offset.y + shift(rect.top(), rect.bottom(), bounds.top(), bounds.bottom()))
                .max(-max.y)
                .min(px(0.)),
        );
        if next == offset {
            return false;
        }
        handle.set_offset(next);
        true
    }

    fn focus_find_field(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.find.input.read(cx).focus_handle(cx).focus(window, cx);
    }

    /// The bar under the chat's header: the field, where the current match is among how
    /// many, the previous and next arrows and the close button.
    pub(super) fn find_bar(
        &self,
        look: Look,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.find.open {
            return None;
        }
        let colors = look.colors;
        let input = self.find.input.clone();
        let focused = self.find.field_focused(window, cx);
        let typed = !input.read(cx).value().is_empty();
        let count = self.find.hits.len();
        let status = match (typed, count, self.find.current) {
            (false, _, _) => String::new(),
            (true, 0, _) => "No results".to_owned(),
            (true, count, current) => format!("{} of {count}", current.map_or(0, |at| at + 1)),
        };
        let edge: Hsla = if focused {
            rgb(colors.focus).into()
        } else if look.native {
            gpui::transparent_black()
        } else {
            rgb(colors.divider).into()
        };
        let field = div()
            .id("chat-find-field")
            .flex_1()
            .min_w_0()
            .max_w(ui_text::space(320.))
            .h(ui_text::space(24.))
            .px(ui_text::space(8.))
            .flex()
            .items_center()
            .gap(ui_text::space(5.))
            .cursor_text()
            .border_1()
            .border_color(edge)
            .map(|field| {
                if look.native {
                    field.rounded_full().bg(rgb(colors.panel_active))
                } else {
                    field
                        .rounded(ui_text::space(if look.hermes() { 5. } else { 3. }))
                        .bg(rgb(colors.bg))
                }
            })
            .text_color(rgb(if focused { colors.text } else { colors.muted }))
            .child(icons::mark("⌕", 10.0, colors.muted))
            .child(
                crate::form_input::search_frame("chat-find-input", &input, window, cx)
                    .accessibility_label("Find in chat"),
            );
        let none = count == 0;
        div()
            .id("chat-find")
            .track_focus(&self.find.bar)
            .w_full()
            .flex()
            .items_center()
            .gap(ui_text::space(BAR_GAP))
            .px(ui_text::space(BAR_INSET))
            .py(ui_text::space(5.))
            .border_b_1()
            .border_color(rgb(colors.divider))
            .bg(rgb(colors.panel))
            .text_size(ui_text::text(11.))
            // A click anywhere in the bar leaves the keys in the field, so ⏎ goes on stepping.
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                input.read(cx).focus_handle(cx).focus(window, cx);
                window.prevent_default();
            })
            .child(field)
            .child(
                div()
                    .id("chat-find-count")
                    .flex_none()
                    .text_color(rgb(if typed && none {
                        look.error()
                    } else {
                        colors.muted
                    }))
                    .role(gpui::Role::Label)
                    .aria_label(status.clone())
                    .child(status)
                    .test_support(),
            )
            .child(
                bar_button("chat-find-previous", "▴", "Previous match", none, look).on_click(
                    cx.listener(|view, _, window, cx| {
                        view.step_find(false, window, cx);
                        view.focus_find_field(window, cx);
                    }),
                ),
            )
            .child(
                bar_button("chat-find-next", "▾", "Next match", none, look).on_click(cx.listener(
                    |view, _, window, cx| {
                        view.step_find(true, window, cx);
                        view.focus_find_field(window, cx);
                    },
                )),
            )
            .child(div().flex_1())
            .child(
                bar_button("chat-find-close", "×", "Close find", false, look)
                    .on_click(cx.listener(|view, _, window, cx| view.close_find(window, cx))),
            )
            .test_support()
            .into_any_element()
            .into()
    }
}

/// A round arrow or close button of the bar: its mark, or Native's symbol, named in a tooltip.
fn bar_button(
    id: &'static str,
    mark: &'static str,
    name: &'static str,
    disabled: bool,
    look: Look,
) -> behavior::Button {
    let colors = look.colors;
    behavior::button_content(id, name, icons::text_mark(mark, 10.))
        .disabled(disabled)
        .flex_none()
        .size(ui_text::space(22.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .text_color(rgb(if disabled {
            colors.divider
        } else {
            colors.muted
        }))
        .when(!disabled, |button| {
            button
                .cursor_pointer()
                .hover(move |style| style.bg(rgb(colors.divider)).text_color(rgb(colors.text)))
        })
        .focus_visible(move |style| style.bg(rgb(colors.divider)).text_color(rgb(colors.focus)))
        .child(tooltip::anchor(name, tooltip::Look::Control))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(key: &str, text: &str, row: usize) -> SourceLeaf {
        SourceLeaf {
            key: key.into(),
            text: text.into(),
            row,
        }
    }

    fn found(source: &[SourceLeaf], hits: &[Hit]) -> Vec<String> {
        hits.iter()
            .map(|hit| source[hit.leaf].text[hit.range.clone()].to_owned())
            .collect()
    }

    #[test]
    fn matches_are_case_insensitive_substrings_in_reading_order() {
        let source = vec![
            leaf("a/0", "Alpha beta ALPHA", 0),
            leaf("code:a/1", "let alpha = 1;\nalphabet()", 0),
            leaf("b/0", "nothing here", 1),
        ];
        let mut index = Index::default();
        let hits = index.search(&source, "alpha");
        assert_eq!(
            hits.iter()
                .map(|hit| (hit.leaf, hit.range.clone()))
                .collect::<Vec<_>>(),
            vec![(0, 0..5), (0, 11..16), (1, 4..9), (1, 15..20)]
        );
        assert_eq!(found(&source, &hits), ["Alpha", "ALPHA", "alpha", "alpha"]);
        assert!(index.search(&source, "zeta").is_empty());
    }

    #[test]
    fn unicode_whose_lower_case_changes_length_maps_back_to_its_own_bytes() {
        // 'İ' lowers to two characters, the Kelvin sign to a one-byte 'k'.
        let source = vec![leaf("u/0", "İstanbul café \u{212A}ELVIN Ⅻ 🦀 CAFÉ", 0)];
        let mut index = Index::default();
        assert_eq!(
            found(&source, &index.search(&source, "café")),
            ["café", "CAFÉ"]
        );
        assert_eq!(
            found(&source, &index.search(&source, "kelvin")),
            ["\u{212A}ELVIN"]
        );
        assert_eq!(
            found(&source, &index.search(&source, "stanbul")),
            ["stanbul"]
        );
        assert_eq!(found(&source, &index.search(&source, "i̇s")), ["İs"]);
        assert_eq!(found(&source, &index.search(&source, "🦀")), ["🦀"]);
    }

    #[test]
    fn a_longer_query_skips_leaves_and_a_changed_leaf_is_searched_again() {
        let mut source = vec![leaf("a/0", "find me", 0), leaf("b/0", "fine", 1)];
        let mut index = Index::default();
        assert_eq!(index.search(&source, "fin").len(), 2);
        assert_eq!(index.search(&source, "find").len(), 1);
        // A streaming leaf that grew is folded and searched again under the same query.
        source[1].text = "fine, find it".into();
        let hits = index.search(&source, "find");
        assert_eq!(found(&source, &hits), ["find", "find"]);
        // Leaves that left the source leave the index.
        source.remove(0);
        index.search(&source, "find");
        assert_eq!(index.leaves.len(), 1);
    }
}
