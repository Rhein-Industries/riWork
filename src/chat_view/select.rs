//! RiWork renderers participating in Base's window TextSelection.
//!
//! Base owns gestures, word/line selection, Shift extension, UTF-8 projection,
//! scrolling and clipboard ordering. This module only retains participants,
//! assigns transcript reading order and paints Base's projected ranges over
//! RiWork's rich text. The window owner installs one Base Root/selection layer.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ops::Range,
};

use gpui::{
    AnyElement, AnyWindowHandle, App, Bounds, Context, Element, ElementId, FocusHandle,
    GlobalElementId, HighlightStyle, HitboxBehavior, Hsla, InspectorElementId, InteractiveText,
    IntoElement, LayoutId, Pixels, Point, SharedString, StyledText, Subscription, TextLayout,
    Window, div, prelude::*, rgb,
};
use gpui_kit::base::{
    ElementExt as _, SelectableText, TestSupportExt as _, TextSelection, TextSelectionEvent,
    TextSelectionHandle, TextSelectionRegistration, TextSelectionRun, TextSelectionScopeId,
};

use super::{ChatView, widgets::Look};

struct Run {
    handle: TextSelectionHandle,
    text: SharedString,
    row: usize,
    _refresh: Subscription,
}

/// Membership and presentation metadata only; selected ranges/text live in Base.
pub(super) struct TranscriptSelection {
    pub scope: TextSelectionScopeId,
    row: Cell<usize>,
    ordinal: Cell<u32>,
    runs: RefCell<HashMap<String, Run>>,
}

impl Default for TranscriptSelection {
    fn default() -> Self {
        Self {
            scope: TextSelectionScopeId::new(),
            row: Cell::new(0),
            ordinal: Cell::new(0),
            runs: RefCell::new(HashMap::new()),
        }
    }
}

impl TranscriptSelection {
    /// Rows can render/cache in any order. Within a row, the renderer builds
    /// leaves in Vec/tree reading order, never HashMap or frame paint order.
    pub fn begin_row(&self, row: usize) {
        self.row.set(row);
        self.ordinal.set(0);
    }

    fn participant(
        &self,
        key: &str,
        text: SharedString,
        focus: FocusHandle,
        window: AnyWindowHandle,
        cx: &mut App,
    ) -> (TextSelectionHandle, u64) {
        let row = self.row.get();
        let ordinal = self.ordinal.get();
        self.ordinal
            .set(ordinal.checked_add(1).expect("transcript row too large"));
        let order = (u64::from(u32::try_from(row).expect("transcript too large")) << 32)
            | u64::from(ordinal);
        let key = format!("{row}:{key}");
        let changed_selection = self
            .runs
            .borrow()
            .get(&key)
            .is_some_and(|run| run.text != text && run.handle.snapshot(cx).is_some());
        if changed_selection {
            self.clear(window, cx);
        }
        let mut runs = self.runs.borrow_mut();
        let run = runs.entry(key).or_insert_with(|| {
            let handle = TextSelectionHandle::new("", cx);
            handle.focus_with(move |window, cx| focus.focus(window, cx), cx);
            let refresh = handle.subscribe(
                move |event, cx| {
                    if matches!(event, TextSelectionEvent::SelectionChanged(_)) {
                        let _ = window.update(cx, |_, window, _| window.refresh());
                    }
                },
                cx,
            );
            Run {
                handle,
                text: text.clone(),
                row,
                _refresh: refresh,
            }
        });
        if run.text != text {
            // An old projection must not be copied between Change and paint.
            run.handle.update_runs(&[], cx);
            run.handle.set_fallback_copy_text("", cx);
            run.text = text;
        }
        (run.handle.clone(), order)
    }

    pub fn selected_rows(&self, cx: &App) -> Vec<usize> {
        self.runs
            .borrow()
            .values()
            .filter_map(|run| {
                (run.handle.snapshot(cx).is_some() || run.handle.has_local_selection(cx))
                    .then_some(run.row)
            })
            .collect()
    }

    pub fn clear(&self, window: AnyWindowHandle, cx: &mut App) {
        // A background chat must not clear another chat/window's selection.
        if !self.selected_rows(cx).is_empty() {
            TextSelection::clear_for_window(window.window_id(), cx);
        }
    }

    pub fn retire(&mut self, window: AnyWindowHandle, cx: &mut App) {
        self.clear(window, cx);
        for run in self.runs.borrow().values() {
            run.handle.update_runs(&[], cx);
        }
        self.runs.get_mut().clear();
        self.scope = TextSelectionScopeId::new();
    }
}

impl ChatView {
    /// Bubble after Base editors have had Copy. Base owns the projection and
    /// ordering; this adapter only preserves code whitespace (stock Root trims).
    pub(super) fn copy_transcript(
        &mut self,
        _: &gpui_kit::base::input::Copy,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor_focused = self.composer.read(cx).focus_handle(cx).is_focused(window)
            || self
                .model_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
            || self
                .answers
                .values()
                .any(|answer| answer.state.read(cx).focus_handle(cx).is_focused(window));
        if editor_focused {
            // Normally the Base editor consumes Copy, including an empty
            // selection. Never let a disabled/unmounted editor fall through
            // to an unrelated window projection.
            TextSelection::clear(window, cx);
            return;
        }
        if self.transcript_selection.selected_rows(cx).is_empty() {
            cx.propagate();
            return;
        }
        let text = TextSelection::selected_text(window, cx);
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
                .all(|(existing, _)| existing.end <= range.start || existing.start >= range.end)
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
        let (handle, order) = self.transcript_selection.participant(
            key,
            text.clone(),
            self.focus.clone(),
            self.window_handle,
            cx,
        );
        let color = rgb(look.tint(look.colors.cyan, 0.4)).into();
        let body = if highlights.is_empty() && links.is_empty() {
            // Use the complete library element wherever rich styling is unnecessary.
            SelectableText::with_handle(
                ElementId::Name(format!("plain:{key}").into()),
                handle,
                text.clone(),
            )
            .document_order(order)
            .selection_color(color)
            .into_any_element()
        } else {
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
                let click_handle = handle.clone();
                InteractiveText::new(ElementId::Name(format!("links:{key}").into()), styled)
                    .on_click(ranges, move |at, window, cx| {
                        // InteractiveText's range click is not a selection gesture.
                        // Base's projected selection vetoes drag, multi-click and
                        // Shift extension before a link can activate.
                        let moved = click_handle
                            .snapshot(cx)
                            .and_then(|snapshot| snapshot.window_points())
                            .is_some_and(|points| points.anchor() != points.cursor());
                        if moved || !TextSelection::selected_text(window, cx).is_empty() {
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
            RichParticipant {
                id: ElementId::Name(format!("rich:{key}").into()),
                body,
                text: text.clone(),
                layout,
                handle,
                order,
                scope: self.transcript_selection.scope,
                color,
            }
            .into_any_element()
        };
        div()
            .id(ElementId::Name(format!("transcript:{key}").into()))
            .min_w_0()
            .cursor_text()
            .role(gpui::Role::StaticText)
            .aria_label(text)
            .child(body)
            .test_support()
            .text_selection_scope(self.transcript_selection.scope)
            .into_any_element()
    }
}

/// The documented participant seam for RiWork's styled/link renderer. No input
/// handlers, selection endpoints, word boundaries or clipboard cache live here.
struct RichParticipant {
    id: ElementId,
    body: AnyElement,
    text: SharedString,
    layout: TextLayout,
    handle: TextSelectionHandle,
    order: u64,
    scope: TextSelectionScopeId,
    color: Hsla,
}

impl IntoElement for RichParticipant {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for RichParticipant {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
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
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.body.prepaint(window, cx);
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        self.handle.register(
            TextSelectionRegistration::new(hitbox, bounds)
                .with_scope(self.scope)
                .with_document_order(self.order)
                .with_text_bounds(vec![self.layout.bounds()])
                .with_rendered_element(&self.handle, window, cx),
            window,
            cx,
        );
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let before = TextSelection::selected_text(window, cx);
        let projection = self.handle.update_runs(
            &[
                TextSelectionRun::new(self.text.clone(), self.layout.clone(), bounds)
                    .with_document_order(self.order),
            ],
            cx,
        );
        self.body.paint(window, cx);
        // Preserve all RiWork foreground styles. Selection geometry comes only
        // from Base's UTF-8 projection; these quads are renderer decoration.
        for range in projection.ranges().iter().flatten() {
            paint_projection(&self.layout, range.clone(), self.color, window, cx);
        }
        if before != TextSelection::selected_text(window, cx) {
            window.refresh();
        }
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
