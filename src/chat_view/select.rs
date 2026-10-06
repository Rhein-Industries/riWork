//! One persistent Base participant for the virtual transcript document.
//! Base owns all gestures/endpoints/multi-click/Shift/autoscroll. The renderer
//! reports glyph runs; stable content keys and copy_with export current source,
//! including leaves whose elements have genuinely left the virtual list.
use super::{ChatView, selection_document::SourceLeaf, widgets::Look};
use gpui::{
    AnyElement, AnyWindowHandle, App, Bounds, Context, Element, ElementId, FocusHandle, Focusable,
    GlobalElementId, HighlightStyle, HitboxBehavior, Hsla, InspectorElementId, InteractiveText,
    IntoElement, LayoutId, ListState, Pixels, Point, SharedString, StyledText, Subscription,
    TextLayout, Window, div, prelude::*, rgb,
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
};

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
    participant: Option<gpui::EntityId>,
}

impl Document {
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

pub(super) struct TranscriptSelection {
    pub scope: TextSelectionScopeId,
    handle: TextSelectionHandle,
    data: Rc<RefCell<Document>>,
    dirty: Cell<bool>,
    _refresh: Subscription,
}
impl TranscriptSelection {
    pub fn new(
        focus: FocusHandle,
        window: AnyWindowHandle,
        owner: gpui::WeakEntity<ChatView>,
        cx: &mut App,
    ) -> Self {
        let handle = TextSelectionHandle::new("", cx);
        let participant = handle.entity_id();
        let data = Rc::new(RefCell::new(Document::default()));
        data.borrow_mut().participant = Some(handle.entity_id());
        handle.focus_with(move |window, cx| focus.focus(window, cx), cx);
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
                let selection = &owner.read(cx).transcript_selection;
                if selection.handle.entity_id() != participant {
                    return String::new();
                }
                selection
                    .data
                    .borrow()
                    .export(selection.handle.snapshot(cx))
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
            _refresh: refresh,
        }
    }
    pub fn changed(&self) {
        self.dirty.set(true);
    }
    pub fn selected_rows(&self, cx: &App) -> Vec<usize> {
        self.data.borrow().rows(self.handle.snapshot(cx))
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
}

impl ChatView {
    pub(super) fn refresh_selection_document(&mut self, look: Look, cx: &mut Context<Self>) {
        if !self.transcript_selection.dirty.replace(false)
            && self.transcript_selection.data.borrow().native == Some(look.native)
        {
            return;
        }
        let source = super::selection_document::leaves(self, look);
        let changed_selected = {
            let old = self.transcript_selection.data.borrow();
            old.rows(self.transcript_selection.handle.snapshot(cx))
                .into_iter()
                .any(|row| {
                    old.source
                        .iter()
                        .filter(|leaf| leaf.row == row)
                        .ne(source.iter().filter(|leaf| leaf.row == row))
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
        let text = self.transcript_selection.data.borrow().export(snapshot);
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
                if highlights.is_empty() {
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
        links.sort_by_key(|(range, _)| range.start);
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
            let handle = self.transcript_selection.handle.clone();
            let data = self.transcript_selection.data.clone();
            InteractiveText::new(ElementId::Name(format!("links:{key}").into()), styled)
                .on_click(ranges, move |at, window, cx| {
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
                            let _ = view.update(cx, |_, cx| {
                                cx.emit(super::ChatViewEvent::OpenFile {
                                    target: target.clone(),
                                })
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
}

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
    type PrepaintState = ();
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
    ) {
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
        self.data.borrow_mut().frame.clear();
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
    let (Some(start), Some(end)) = (
        layout.position_for_index(range.start),
        layout.position_for_index(range.end),
    ) else {
        return;
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
    for quad in quads {
        window.paint_quad(gpui::fill(quad, color));
        window.with_content_mask(Some(gpui::ContentMask { bounds: quad }), |window| {
            let _ = layout.paint_foreground(window, cx);
        });
    }
}
