//! One persistent Base participant for the virtual transcript document.
//! Base owns all gestures/endpoints/multi-click/Shift/autoscroll. The renderer
//! reports glyph runs; stable content keys and copy_with export current source,
//! including leaves whose elements have genuinely left the virtual list.
use super::{ChatView, find::FindPaint, selection_document::SourceLeaf, widgets::Look};
use gpui::{
    AnyElement, AnyWindowHandle, App, BorderStyle, Bounds, Context, Element, ElementId, Focusable,
    GlobalElementId, HighlightStyle, HitboxBehavior, Hsla, InspectorElementId, InteractiveText,
    IntoElement, LayoutId, ListState, Pixels, Point, SharedString, StyledText, Subscription,
    TextLayout, Window, div, prelude::*, px, rgb,
};
use gpui_kit::base::{
    TestSupportExt as _, TextSelection, TextSelectionContentKey, TextSelectionEvent,
    TextSelectionHandle, TextSelectionRegistration, TextSelectionRun, TextSelectionScopeId,
    TextSelectionSnapshot,
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ops::Range,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};

/// Content revisions are unique across chats and windows. Mounting another virtual
/// row does not change this revision; changing disclosed text does.
static GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
struct PaintedLeaf {
    key: String,
    text: SharedString,
    layout: TextLayout,
    bounds: Bounds<Pixels>,
    clip: Bounds<Pixels>,
    order: u64,
}
#[derive(Default)]
struct Document {
    source: Vec<SourceLeaf>,
    ids: HashMap<String, u32>,
    next_id: u32,
    frame: Vec<PaintedLeaf>,
    origin: Point<Pixels>,
    scroll: Point<Pixels>,
    native: Option<bool>,
    container_width: f32,
    participant: Option<gpui::EntityId>,
    generation: u64,
    paints: u64,
    /// Complete disclosed text, built on demand while Find is open. Selection
    /// itself retains only mounted bodies in `source`.
    search_source: Vec<SourceLeaf>,
    find: Option<FindPaint>,
    #[cfg(test)]
    found: Vec<(String, Range<usize>, bool)>,
}

/// Rendered UTF-8 byte ranges, anchored to stable transcript leaf keys. Only the
/// selected bytes are copied; quoting never retains an unbounded document snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SelectionFragment {
    pub key: String,
    pub range: Range<usize>,
    pub text: String,
}

impl Document {
    fn changed(&mut self) {
        self.generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    }

    fn painted_rect(&self, leaf: &SourceLeaf, range: Range<usize>) -> Option<Bounds<Pixels>> {
        let painted = self
            .frame
            .iter()
            .find(|painted| painted.key == leaf.key && painted.text.as_ref() == leaf.text)?;
        range_quads(&painted.layout, range)
            .into_iter()
            .reduce(|a, b| a.union(&b))
    }

    fn endpoint(&self, key: TextSelectionContentKey) -> Option<(usize, usize)> {
        let id = (key.value() >> 32) as u32;
        let byte = key.value() as u32 as usize;
        let at = self
            .source
            .iter()
            .position(|leaf| self.ids.get(&leaf.key) == Some(&id))?;
        let text = &self.source[at].text;
        (byte <= text.len() && text.is_char_boundary(byte)).then_some((at, byte))
    }
    fn endpoints(
        &self,
        snapshot: TextSelectionSnapshot,
    ) -> Option<((usize, usize), (usize, usize))> {
        if snapshot.anchor().entity_id() != self.participant
            || snapshot.cursor().entity_id() != self.participant
        {
            return None;
        }
        let a = self.endpoint(snapshot.anchor().content_key()?)?;
        let b = self.endpoint(snapshot.cursor().content_key()?)?;
        Some((a.min(b), a.max(b)))
    }
    fn export(&self, snapshot: Option<TextSelectionSnapshot>) -> String {
        let Some((a, b)) = snapshot.and_then(|s| self.endpoints(s)) else {
            return String::new();
        };
        if a == b {
            return String::new();
        }
        // One newline between displayed leaves/cells/messages; code's embedded
        // newlines and all indentation/trailing/blank-only bytes stay exact.
        (a.0..=b.0)
            .map(|at| {
                let text = &self.source[at].text;
                let start = if at == a.0 { a.1 } else { 0 };
                let end = if at == b.0 { b.1 } else { text.len() };
                &text[start..end]
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    fn export_complete(
        &self,
        snapshot: Option<TextSelectionSnapshot>,
        source: &[SourceLeaf],
    ) -> String {
        let Some((a, b)) = snapshot.and_then(|s| self.endpoints(s)) else {
            return String::new();
        };
        let Some(start) = source
            .iter()
            .position(|leaf| leaf.key == self.source[a.0].key)
        else {
            return String::new();
        };
        let Some(end) = source
            .iter()
            .position(|leaf| leaf.key == self.source[b.0].key)
        else {
            return String::new();
        };
        (start..=end)
            .map(|at| {
                let text = &source[at].text;
                let from = if at == start { a.1 } else { 0 };
                let to = if at == end { b.1 } else { text.len() };
                text.get(from..to).unwrap_or(text)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    fn rows(&self, snapshot: Option<TextSelectionSnapshot>) -> Vec<usize> {
        let Some((a, b)) = snapshot.and_then(|s| self.endpoints(s)) else {
            return Vec::new();
        };
        self.source[a.0..=b.0].iter().map(|leaf| leaf.row).collect()
    }
    fn content_key(&self, point: Point<Pixels>) -> Option<TextSelectionContentKey> {
        let window_point = point + self.origin + self.scroll;
        // Renderer mapping only: native TextLayout supplies Unicode/wrap-safe
        // glyph hit testing. No mouse history, word or gesture algorithm here.
        let leaf = self
            .frame
            .iter()
            .filter(|leaf| leaf.bounds.intersect(&leaf.clip).contains(&window_point))
            .min_by_key(|leaf| leaf.order)
            .or_else(|| {
                self.frame
                    .iter()
                    .filter(|leaf| leaf.bounds.top() <= window_point.y)
                    .max_by_key(|leaf| leaf.order)
            })
            .or_else(|| self.frame.iter().min_by_key(|leaf| leaf.order))?;
        let byte = leaf
            .layout
            .index_for_position(window_point)
            .unwrap_or_else(|byte| byte)
            .min(leaf.text.len());
        if !leaf.text.is_char_boundary(byte) {
            return None;
        }
        let id = *self.ids.get(&leaf.key)?;
        Some(TextSelectionContentKey::new(
            (u64::from(id) << 32) | u64::from(u32::try_from(byte).ok()?),
        ))
    }
}

type SourceSections = HashMap<usize, (super::display::ListRow, Vec<SourceLeaf>)>;

pub(super) struct TranscriptSelection {
    pub scope: TextSelectionScopeId,
    handle: TextSelectionHandle,
    data: Rc<RefCell<Document>>,
    dirty: Cell<bool>,
    source_cache: RefCell<HashMap<String, SourceSections>>,
    #[cfg(test)]
    pub(super) source_builds: Cell<usize>,
    _refresh: Subscription,
}
impl TranscriptSelection {
    pub fn new(window: AnyWindowHandle, owner: gpui::WeakEntity<ChatView>, cx: &mut App) -> Self {
        let handle = TextSelectionHandle::new("", cx);
        let participant = handle.entity_id();
        let data = Rc::new(RefCell::new(Document::default()));
        data.borrow_mut().participant = Some(handle.entity_id());
        // This participant covers the viewport, including child controls. Base
        // defers its focus callback for any point inside that viewport, even
        // outside glyphs; focusing ChatView there would steal a button's focus.
        // ChatView's native track_focus already transfers text clicks while
        // respecting the child's native prevent_default when it takes focus.
        let source = data.clone();
        let weak = Rc::downgrade(&source);
        handle.resolve_content_key_with(
            move |point, _| weak.upgrade()?.borrow().content_key(point),
            cx,
        );
        handle.copy_with(
            move |cx| {
                let Some(owner) = owner.upgrade() else {
                    return String::new();
                };
                let view = owner.read(cx);
                let selection = &view.transcript_selection;
                if selection.handle.entity_id() != participant {
                    return String::new();
                }
                let Some(snapshot) = selection.handle.snapshot(cx) else {
                    return String::new();
                };
                let source = super::selection_document::leaves(view, Look::of(cx));
                selection
                    .data
                    .borrow()
                    .export_complete(Some(snapshot), &source)
            },
            cx,
        );
        let refresh = handle.subscribe(
            move |event, cx| {
                if matches!(event, TextSelectionEvent::SelectionChanged(_)) {
                    let _ = window.update(cx, |_, window, _| window.refresh());
                }
            },
            cx,
        );
        Self {
            scope: TextSelectionScopeId::new(),
            handle,
            data,
            dirty: Cell::new(true),
            source_cache: RefCell::new(HashMap::new()),
            #[cfg(test)]
            source_builds: Cell::new(0),
            _refresh: refresh,
        }
    }
    pub fn changed(&self) {
        self.dirty.set(true);
        self.invalidate_search();
    }
    pub(super) fn invalidate_search(&self) {
        self.data.borrow_mut().changed();
    }
    pub fn generation(&self) -> u64 {
        self.data.borrow().generation
    }
    pub fn paints(&self) -> u64 {
        self.data.borrow().paints
    }
    pub(super) fn set_search_source(&self, source: Vec<SourceLeaf>) {
        self.data.borrow_mut().search_source = source;
    }
    pub fn with_source<R>(&self, read: impl FnOnce(&[SourceLeaf]) -> R) -> R {
        read(&self.data.borrow().search_source)
    }
    /// The selection document itself (mounted bodies, or a focused review's patch),
    /// as Copy's endpoints and `selection_painted_rect` index it.
    pub(super) fn with_selection_source<R>(&self, read: impl FnOnce(&[SourceLeaf]) -> R) -> R {
        read(&self.data.borrow().source)
    }
    pub fn painted_rect(&self, leaf: usize, range: Range<usize>) -> Option<Bounds<Pixels>> {
        let data = self.data.borrow();
        data.painted_rect(data.search_source.get(leaf)?, range)
    }
    /// `painted_rect` for a leaf of `with_selection_source`.
    pub(super) fn selection_painted_rect(
        &self,
        leaf: usize,
        range: Range<usize>,
    ) -> Option<Bounds<Pixels>> {
        let data = self.data.borrow();
        data.painted_rect(data.source.get(leaf)?, range)
    }
    pub fn set_find(&self, find: Option<FindPaint>) {
        let mut data = self.data.borrow_mut();
        if find.is_none() {
            data.search_source.clear();
        }
        data.find = find;
    }
    pub fn selected_text(&self, cx: &App) -> String {
        self.data.borrow().export(self.handle.snapshot(cx))
    }
    pub(super) fn selected_fragments(
        &self,
        cx: &App,
        max_bytes: usize,
    ) -> Result<Vec<SelectionFragment>, ()> {
        let data = self.data.borrow();
        let Some((first, last)) = self.handle.snapshot(cx).and_then(|s| data.endpoints(s)) else {
            return Ok(Vec::new());
        };
        let mut total = 0usize;
        let mut fragments = Vec::new();
        for at in first.0..=last.0 {
            let leaf = &data.source[at];
            let start = if at == first.0 { first.1 } else { 0 };
            let end = if at == last.0 {
                last.1
            } else {
                leaf.text.len()
            };
            if start == end {
                continue;
            }
            total = total.saturating_add(end - start + usize::from(!fragments.is_empty()));
            if total > max_bytes || fragments.len() >= 128 {
                return Err(());
            }
            fragments.push(SelectionFragment {
                key: leaf.key.clone(),
                range: start..end,
                text: leaf.text[start..end].to_owned(),
            });
        }
        Ok(fragments)
    }
    #[cfg(test)]
    pub(super) fn painted_find(&self) -> Vec<(String, Range<usize>, bool)> {
        self.data.borrow().found.clone()
    }
    pub(super) fn source_was_mounted(&self, id: &str) -> bool {
        self.source_cache.borrow().contains_key(id)
    }
    /// Content or body-detail changes invalidate every section owned by the message.
    pub(super) fn invalidate_source(&self, id: &str) {
        self.source_cache.borrow_mut().remove(id);
    }
    /// Refreshing one list row must keep the other mounted sections of its owner.
    fn invalidate_source_section(&self, row: &super::display::ListRow) {
        let mut cache = self.source_cache.borrow_mut();
        if let Some(sections) = cache.get_mut(&row.row.id) {
            sections.remove(&row.block_start());
            if sections.is_empty() {
                cache.remove(&row.row.id);
            }
        }
    }
    pub(super) fn invalidate_sources(&self) {
        self.source_cache.borrow_mut().clear();
    }
    pub(super) fn retain_sources(&self, rows: &[super::display::ListRow]) {
        let mut visible: HashMap<&str, std::collections::HashSet<usize>> = HashMap::new();
        for entry in rows {
            visible
                .entry(&entry.row.id)
                .or_default()
                .insert(entry.block_start());
        }
        self.source_cache.borrow_mut().retain(|id, sections| {
            let Some(keep) = visible.get(id.as_str()) else {
                return false;
            };
            sections.retain(|start, _| keep.contains(start));
            !sections.is_empty()
        });
    }
    fn has_cached_source(&self, row: &super::display::ListRow) -> bool {
        self.source_cache
            .borrow()
            .get(&row.row.id)
            .and_then(|sections| sections.get(&row.block_start()))
            .is_some_and(|(known, _)| known == row)
    }
    pub(super) fn cached_source(
        &self,
        row: &super::display::ListRow,
        at: usize,
    ) -> Option<Vec<SourceLeaf>> {
        let cache = self.source_cache.borrow();
        let (known, source) = cache.get(&row.row.id)?.get(&row.block_start())?;
        if known != row {
            return None;
        }
        Some(
            source
                .iter()
                .cloned()
                .map(|mut leaf| {
                    leaf.row = at;
                    leaf
                })
                .collect(),
        )
    }
    pub(super) fn cache_source(&self, row: &super::display::ListRow, source: &[SourceLeaf]) {
        #[cfg(test)]
        self.source_builds.set(self.source_builds.get() + 1);
        self.source_cache
            .borrow_mut()
            .entry(row.row.id.clone())
            .or_default()
            .insert(row.block_start(), (row.clone(), source.to_vec()));
    }
    pub fn clear(&self, window: AnyWindowHandle, cx: &mut App) {
        if self.handle.snapshot(cx).is_some() || self.handle.has_local_selection(cx) {
            TextSelection::clear_for_window(window.window_id(), cx);
        }
    }
    pub fn retire(&mut self, window: AnyWindowHandle, cx: &mut App) {
        self.clear(window, cx);
        let scope = self.scope;
        cx.defer(move |cx| {
            let _ = window.update(cx, |_, window, cx| {
                crate::behavior_controls::retire_content_scope(scope, window, cx)
            });
        });
        self.data.borrow_mut().source.clear();
        self.data.borrow_mut().frame.clear();
        self.set_find(None);
        self.handle.update_runs(&[], cx);
        self.scope = TextSelectionScopeId::new();
        self.changed();
    }
    pub fn viewport(&self, body: impl IntoElement, list: ListState, look: Look) -> AnyElement {
        DocumentViewport {
            body: body.into_any_element(),
            data: self.data.clone(),
            handle: self.handle.clone(),
            scope: self.scope,
            list,
            color: rgb(look.tint(look.colors.cyan, 0.4)).into(),
        }
        .into_any_element()
    }
    #[cfg(test)]
    pub(super) fn glyph_point(&self, key: &str, byte: usize) -> Option<Point<Pixels>> {
        let data = self.data.borrow();
        let leaf = data.frame.iter().find(|leaf| leaf.key == key)?;
        let point = leaf.layout.position_for_index(byte)?;
        Some(point + gpui::point(gpui::px(0.), leaf.layout.line_height() / 2.))
    }
    #[cfg(test)]
    pub(super) fn glyph_span_points(&self, key: &str) -> (Point<Pixels>, Point<Pixels>) {
        let data = self.data.borrow();
        let leaf = data
            .frame
            .iter()
            .find(|leaf| leaf.key == key)
            .unwrap_or_else(|| panic!("missing painted glyph layout: {key}"));
        let caret = |byte| {
            leaf.layout
                .position_for_index(byte)
                .unwrap_or_else(|| panic!("missing native caret: {key} byte {byte}"))
        };
        // A Div's right edge minus one is inside the last glyph. Native hit
        // testing returns that glyph's starting byte, not its ending caret.
        // Dispatch at the real first glyph and just past the final caret.
        let offset = gpui::point(gpui::px(1.), leaf.layout.line_height() / 2.);
        let first = caret(0) + offset;
        let last = caret(leaf.text.len()) + offset;
        let source_at = data
            .source
            .iter()
            .position(|source| source.key == key)
            .unwrap();
        for (point, byte) in [(first, 0), (last, leaf.text.len())] {
            assert!(
                leaf.clip.contains(&point),
                "clipped glyph endpoint: {key} byte {byte}"
            );
            assert_eq!(
                leaf.layout
                    .index_for_position(point)
                    .unwrap_or_else(|byte| byte),
                byte,
                "native glyph endpoint: {key} at {point:?} layout {:?} clip {:?}",
                leaf.bounds,
                leaf.clip,
            );
            let resolved = data
                .content_key(point - data.origin - data.scroll)
                .and_then(|key| data.endpoint(key));
            assert_eq!(
                resolved,
                Some((source_at, byte)),
                "source endpoint: {key} at {point:?}"
            );
        }
        (first, last)
    }
}

impl ChatView {
    pub(super) fn refresh_selection_rows(
        &mut self,
        rows: &[usize],
        look: Look,
        cx: &mut Context<Self>,
    ) {
        self.refresh_selection_rows_inner(rows, look, true, cx);
    }

    /// A body detail (an output page, a diagram, a raw or expanded view) changed what
    /// a row exports while its projected row stayed equal: its cached source is stale.
    /// Detail keys name their owner's ID; an unrelated match only rebuilds that row.
    pub(super) fn invalidate_leaf_owner(&self, key: &str) {
        for entry in &self.list_rows {
            if key.contains(entry.row.id.as_str()) {
                self.transcript_selection.invalidate_source(&entry.row.id);
            }
        }
    }

    pub(super) fn ensure_selection_row(&mut self, at: usize, look: Look, cx: &mut Context<Self>) {
        if self
            .list_rows
            .get(at)
            .is_some_and(|entry| !self.transcript_selection.has_cached_source(entry))
        {
            self.refresh_selection_rows_inner(&[at], look, false, cx);
        }
    }

    fn refresh_selection_rows_inner(
        &mut self,
        rows: &[usize],
        look: Look,
        clear_changed: bool,
        cx: &mut Context<Self>,
    ) {
        // A focused review has its own row numbers and source. Its single row
        // must not be interpreted as row zero of the background transcript.
        if self.transcript_selection.dirty.get() || rows.is_empty() || self.file_review.is_some() {
            return;
        }
        for &at in rows {
            if let Some(entry) = self.list_rows.get(at) {
                self.transcript_selection.invalidate_source_section(entry);
            }
        }
        let mut replacements: Vec<_> = rows
            .iter()
            .map(|&row| (row, super::selection_document::row_leaves(self, row, look)))
            .collect();
        let changed_selected = {
            let data = self.transcript_selection.data.borrow();
            let selected = data.rows(self.transcript_selection.handle.snapshot(cx));
            replacements.iter().any(|(row, source)| {
                selected.contains(row)
                    && data
                        .source
                        .iter()
                        .filter(|leaf| leaf.row == *row)
                        .ne(source.iter())
            })
        };
        if clear_changed && changed_selected {
            self.transcript_selection.clear(self.window_handle, cx);
        }
        replacements.sort_by_key(|(row, _)| std::cmp::Reverse(*row));
        let mut data = self.transcript_selection.data.borrow_mut();
        let mut changed = false;
        for (row, source) in replacements {
            for leaf in &source {
                if !data.ids.contains_key(&leaf.key) {
                    data.next_id = data
                        .next_id
                        .checked_add(1)
                        .expect("transcript leaf identity exhausted");
                    let id = data.next_id;
                    data.ids.insert(leaf.key.clone(), id);
                }
            }
            let start = data.source.partition_point(|leaf| leaf.row < row);
            let end = data.source.partition_point(|leaf| leaf.row <= row);
            changed |= data.source[start..end] != source;
            data.source.splice(start..end, source);
        }
        if clear_changed && changed {
            data.changed();
        }
    }

    /// Elapsed activity text is the only selectable text changed by a timer.
    pub(super) fn refresh_selection_timing(&mut self, cx: &mut Context<Self>) {
        if self.transcript_selection.dirty.get() {
            return;
        }
        self.transcript_selection.invalidate_search();
        let updates: HashMap<_, _> = self
            .visible
            .iter()
            .filter(|row| row.running && row.kind == super::display::RowKind::Activity)
            .map(|row| (format!("header:{}", row.id), row.title.as_str()))
            .collect();
        let changed_selected = {
            let data = self.transcript_selection.data.borrow();
            let selected = data.rows(self.transcript_selection.handle.snapshot(cx));
            data.source.iter().any(|leaf| {
                selected.contains(&leaf.row)
                    && updates
                        .get(&leaf.key)
                        .is_some_and(|text| leaf.text != *text)
            })
        };
        if changed_selected {
            self.transcript_selection.clear(self.window_handle, cx);
        }
        let mut data = self.transcript_selection.data.borrow_mut();
        for leaf in &mut data.source {
            if let Some(text) = updates.get(&leaf.key) {
                if leaf.text != *text {
                    leaf.text = (*text).to_owned();
                }
            }
        }
    }

    pub(super) fn refresh_selection_document(&mut self, look: Look, cx: &mut Context<Self>) {
        self.validate_pending_feedback();
        if !self.transcript_selection.dirty.replace(false)
            && self.transcript_selection.data.borrow().native == Some(look.native)
            && self.transcript_selection.data.borrow().container_width == self.composer_width.get()
        {
            return;
        }
        if self.transcript_selection.data.borrow().native != Some(look.native)
            || self.transcript_selection.data.borrow().container_width != self.composer_width.get()
        {
            self.transcript_selection.invalidate_sources();
            self.transcript_selection.invalidate_search();
        }
        let _timing = super::perf::Span::new("selection-document");
        let source = super::selection_document::mounted_leaves(self);
        let changed_selected = {
            let old = self.transcript_selection.data.borrow();
            self.transcript_selection
                .handle
                .snapshot(cx)
                .and_then(|snapshot| old.endpoints(snapshot))
                .is_some_and(|(first, last)| {
                    let before = &old.source[first.0..=last.0];
                    let bounds = source
                        .iter()
                        .position(|leaf| leaf.key == before[0].key)
                        .zip(
                            source
                                .iter()
                                .position(|leaf| leaf.key == before[before.len() - 1].key),
                        );
                    let Some((start, end)) = bounds.filter(|(start, end)| start <= end) else {
                        return true;
                    };
                    // Disclosing another Activity group changes row numbers, not
                    // this selection. New/changed text inside its span still clears it.
                    !before
                        .iter()
                        .map(|leaf| (&leaf.key, &leaf.text))
                        .eq(source[start..=end]
                            .iter()
                            .map(|leaf| (&leaf.key, &leaf.text)))
                })
        };
        if changed_selected {
            self.transcript_selection.clear(self.window_handle, cx);
        }
        let mut data = self.transcript_selection.data.borrow_mut();
        for leaf in &source {
            if !data.ids.contains_key(&leaf.key) {
                data.next_id = data
                    .next_id
                    .checked_add(1)
                    .expect("transcript leaf identity exhausted");
                let id = data.next_id;
                data.ids.insert(leaf.key.clone(), id);
            }
        }
        data.source = source;
        data.native = Some(look.native);
        data.container_width = self.composer_width.get();
        drop(data);
        self.validate_pending_feedback();
    }
    pub(super) fn copy_transcript(
        &mut self,
        _: &gpui_kit::base::input::Copy,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.composer.read(cx).focus_handle(cx).is_focused(window)
            || self
                .model_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
            || self.find.field_focused(window, cx)
            || self.focused_reference_editor(window, cx).is_some()
            || self.focused_output_editor(window, cx).is_some()
            || self
                .answers
                .values()
                .any(|answer| answer.state.read(cx).focus_handle(cx).is_focused(window))
        {
            TextSelection::clear(window, cx);
            return;
        }
        // Query our source projection directly: Base's multi-participant
        // compositor filters whitespace-only contributions before Root Copy.
        let snapshot = self.transcript_selection.handle.snapshot(cx);
        if snapshot.is_none() {
            cx.propagate();
            return;
        }
        let source = super::selection_document::leaves(self, Look::of(cx));
        let text = self
            .transcript_selection
            .data
            .borrow()
            .export_complete(snapshot, &source);
        if text.is_empty() {
            cx.propagate();
        } else {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        }
    }
    pub(super) fn selectable(
        &self,
        key: &str,
        text: String,
        mut highlights: Vec<(Range<usize>, HighlightStyle)>,
        mut links: Vec<(Range<usize>, String)>,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        for (range, target) in super::links::detect(&text, key.starts_with("code:")) {
            if links
                .iter()
                .all(|(other, _)| other.end <= range.start || other.start >= range.end)
            {
                if highlights
                    .iter()
                    .all(|(other, _)| other.end <= range.start || other.start >= range.end)
                {
                    highlights.push((
                        range.clone(),
                        HighlightStyle {
                            color: Some(rgb(look.colors.cyan).into()),
                            ..Default::default()
                        },
                    ));
                }
                links.push((range, target));
            }
        }
        // GPUI derives text runs from highlights in order; a link highlight appended
        // after a later markdown highlight would overlap runs and split a character.
        highlights.sort_by_key(|(range, _)| range.start);
        links.sort_by_key(|(range, _)| range.start);
        let file_actions = self.file_actions_button(
            key,
            links.iter().map(|(_, target)| target.clone()).collect(),
            look,
            cx,
        );
        let text: SharedString = text.into();
        let styled = StyledText::new(text.clone()).with_highlights(highlights);
        let layout = styled.layout().clone();
        let body = if links.is_empty() {
            styled.into_any_element()
        } else {
            let ranges = links.iter().map(|(range, _)| range.clone()).collect();
            let targets = links
                .into_iter()
                .map(|(_, target)| target)
                .collect::<Vec<_>>();
            let view = cx.weak_entity();
            // The leaf's source item disambiguates repeated paths in different turns.
            let owner = self.selection_owner(key).map(str::to_owned);
            let handle = self.transcript_selection.handle.clone();
            let data = self.transcript_selection.data.clone();
            InteractiveText::new(ElementId::Name(format!("links:{key}").into()), styled)
                .on_click(ranges, move |at, _window, cx| {
                    let snapshot = handle.snapshot(cx);
                    if !data.borrow().export(snapshot).is_empty()
                        || snapshot
                            .and_then(|s| s.window_points())
                            .is_some_and(|p| p.anchor() != p.cursor())
                    {
                        return;
                    }
                    if let Some(target) = targets.get(at) {
                        if crate::terminal_links::is_openable_url(target) {
                            cx.open_url(target);
                        } else {
                            let _ = view.update(cx, |view, cx| {
                                view.preview_reference(target, owner.as_deref(), cx)
                            });
                        }
                    }
                })
                .into_any_element()
        };
        div()
            .id(ElementId::Name(format!("transcript:{key}").into()))
            .min_w_0()
            .cursor_text()
            .role(gpui::Role::Label)
            .aria_label(text.clone())
            .children(file_actions)
            .child(LeafRenderer {
                body,
                key: key.into(),
                text,
                layout,
                data: self.transcript_selection.data.clone(),
            })
            .test_support()
            .into_any_element()
    }

    /// Keys carry the owner's stable ID. Resolve bounded key prefixes through
    /// the model index, rather than formatting every item ID on every frame.
    pub(super) fn selection_owner<'a>(&self, key: &'a str) -> Option<&'a str> {
        [
            "",
            "user:",
            "code:",
            "code:user:",
            "header:",
            "thought:",
            "command:",
            "file-header:",
            "output:",
            "tool-input:",
            "tool-output:",
            "diff:",
            "artifact:",
            "attachment:",
            "callout:",
            "tool-view:",
            "path:answer-file:",
            "diff:answer-file:",
        ]
        .into_iter()
        .filter_map(|prefix| key.strip_prefix(prefix))
        .filter_map(|candidate| {
            if self.projected_item(candidate).is_some() {
                return Some(candidate);
            }
            candidate
                .char_indices()
                .rev()
                .filter(|(_, c)| matches!(c, '/' | '#' | ':'))
                .map(|(at, _)| &candidate[..at])
                .find(|id| self.projected_item(id).is_some())
        })
        .max_by_key(|id| id.len())
    }
}

/// Preserve link/error styling while giving GPUI separate runs for a font span.
struct LeafRenderer {
    body: AnyElement,
    key: String,
    text: SharedString,
    layout: TextLayout,
    data: Rc<RefCell<Document>>,
}
impl IntoElement for LeafRenderer {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for LeafRenderer {
    type RequestLayoutState = ();
    type PrepaintState = Option<usize>;
    fn id(&self) -> Option<ElementId> {
        Some(ElementId::Name(format!("leaf:{}", self.key).into()))
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.body.request_layout(window, cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Option<usize> {
        self.body.prepaint(window, cx);
        let mut data = self.data.borrow_mut();
        if let Some(order) = data.source.iter().position(|leaf| leaf.key == self.key) {
            debug_assert_eq!(
                data.source[order].text.as_str(),
                self.text.as_ref(),
                "renderer/source export drift"
            );
            data.frame.push(PaintedLeaf {
                key: self.key.clone(),
                text: self.text.clone(),
                layout: self.layout.clone(),
                bounds: self.layout.bounds(),
                clip: window.content_mask().bounds,
                order: order as u64,
            });
        }
        data.search_source
            .iter()
            .position(|leaf| leaf.key == self.key && leaf.text == self.text.as_ref())
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        order: &mut Option<usize>,
        window: &mut Window,
        cx: &mut App,
    ) {
        // Paint Find under glyphs but over the surrounding output/container.
        if let Some(order) = *order {
            let mut data = self.data.borrow_mut();
            let data = &mut *data;
            if let Some(find) = data
                .find
                .as_ref()
                .filter(|f| f.generation == data.generation)
            {
                let first = find.hits.partition_point(|hit| hit.leaf < order);
                for (at, hit) in find.hits[first..]
                    .iter()
                    .take_while(|hit| hit.leaf == order)
                    .enumerate()
                {
                    let current = find.current == Some(first + at);
                    for quad in range_quads(&self.layout, hit.range.clone()) {
                        window.paint_quad(gpui::quad(
                            quad,
                            px(2.),
                            if current { find.strong } else { find.fill },
                            px(if current { 1. } else { 0. }),
                            find.edge,
                            BorderStyle::Solid,
                        ));
                    }
                    #[cfg(test)]
                    data.found
                        .push((self.key.clone(), hit.range.clone(), current));
                }
            }
        }
        self.body.paint(window, cx);
    }
}

struct DocumentViewport {
    body: AnyElement,
    data: Rc<RefCell<Document>>,
    handle: TextSelectionHandle,
    scope: TextSelectionScopeId,
    list: ListState,
    color: Hsla,
}
impl IntoElement for DocumentViewport {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for DocumentViewport {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        Some("transcript-document".into())
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.body.request_layout(window, cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        {
            let mut data = self.data.borrow_mut();
            data.frame.clear();
            data.paints += 1;
            #[cfg(test)]
            data.found.clear();
        }
        self.body.prepaint(window, cx);
        let bounds = self.list.viewport_bounds();
        let scroll = self.list.scroll_px_offset_for_scrollbar();
        let text_bounds = {
            let mut data = self.data.borrow_mut();
            data.origin = bounds.origin;
            data.scroll = scroll;
            data.frame
                .iter()
                .map(|leaf| leaf.bounds.intersect(&leaf.clip))
                .collect()
        };
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        self.handle.register(
            TextSelectionRegistration::new(hitbox, bounds)
                .with_scope(self.scope)
                .with_scroll_offset(scroll)
                .with_document_order(0)
                .with_text_bounds(text_bounds)
                .with_rendered_element(&self.handle, window, cx),
            window,
            cx,
        );
        let runs = self
            .data
            .borrow()
            .frame
            .iter()
            .map(|leaf| {
                TextSelectionRun::new(
                    leaf.text.clone(),
                    leaf.layout.clone(),
                    leaf.bounds.intersect(&leaf.clip),
                )
                .with_document_order(leaf.order)
            })
            .collect::<Vec<_>>();
        self.handle.update_runs(&runs, cx);
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.body.paint(window, cx);
        let data = self.data.borrow();
        let runs = data
            .frame
            .iter()
            .map(|leaf| {
                TextSelectionRun::new(
                    leaf.text.clone(),
                    leaf.layout.clone(),
                    leaf.bounds.intersect(&leaf.clip),
                )
                .with_document_order(leaf.order)
            })
            .collect::<Vec<_>>();
        let projection = self.handle.update_runs(&runs, cx);
        let endpoints = self
            .handle
            .snapshot(cx)
            .and_then(|snapshot| data.endpoints(snapshot));
        window.with_content_mask(
            Some(gpui::ContentMask {
                bounds: self.list.viewport_bounds(),
            }),
            |window| {
                for (leaf, range) in data.frame.iter().zip(projection.ranges()) {
                    // Source endpoints keep the decoration anchored when earlier
                    // variable-height virtual rows are measured, or nested code
                    // scrolling moves their glyphs. This is the same source copy
                    // projection; Base still owns endpoint creation/extension.
                    let source_range = endpoints.and_then(|(a, b)| {
                        let order = leaf.order as usize;
                        (a.0 <= order && order <= b.0).then(|| {
                            (if order == a.0 { a.1 } else { 0 })..(if order == b.0 {
                                b.1
                            } else {
                                leaf.text.len()
                            })
                        })
                    });
                    let range = if endpoints.is_some() {
                        source_range
                    } else {
                        range.clone()
                    };
                    if let Some(range) = range {
                        if !range.is_empty() {
                            window.with_content_mask(
                                Some(gpui::ContentMask { bounds: leaf.clip }),
                                |window| {
                                    paint_projection(&leaf.layout, range, self.color, window, cx)
                                },
                            );
                        }
                    }
                }
            },
        );
    }
}

fn paint_projection(
    layout: &TextLayout,
    range: Range<usize>,
    color: Hsla,
    window: &mut Window,
    cx: &mut App,
) {
    for quad in range_quads(layout, range) {
        window.paint_quad(gpui::fill(quad, color));
        window.with_content_mask(Some(gpui::ContentMask { bounds: quad }), |window| {
            let _ = layout.paint_foreground(window, cx);
        });
    }
}

/// The boxes `range` of a laid-out text covers, one per line it runs over, in window
/// coordinates.
fn range_quads(layout: &TextLayout, range: Range<usize>) -> Vec<Bounds<Pixels>> {
    let (Some(start), Some(end)) = (
        layout.position_for_index(range.start),
        layout.position_for_index(range.end),
    ) else {
        return Vec::new();
    };
    let bounds = layout.bounds();
    let height = layout.line_height();
    let mut quads = Vec::new();
    if start.y == end.y {
        quads.push(Bounds::from_corners(
            start,
            Point::new(end.x, end.y + height),
        ));
    } else {
        quads.push(Bounds::from_corners(
            start,
            Point::new(bounds.right(), start.y + height),
        ));
        if end.y > start.y + height {
            quads.push(Bounds::from_corners(
                Point::new(bounds.left(), start.y + height),
                Point::new(bounds.right(), end.y),
            ));
        }
        quads.push(Bounds::from_corners(
            Point::new(bounds.left(), end.y),
            Point::new(end.x, end.y + height),
        ));
    }
    quads
}
