//! A pane's content laid out on its own, once the pane's size is known.
//!
//! What a pane shows never changes the pane's size: the layout tree gives each pane its
//! share of the window, whatever is in it. Inside the window's layout, though, taffy sized
//! a pane's content again in each of the window's own sizing passes, and a pane's content
//! is a deep tree: a Projects list of 120 rows took about four times as long to lay out
//! inside the window as alone (`measure_a_busy_workspace_frame`), and it was most of
//! every frame the window drew, a chat's streaming included. So the window lays out an
//! empty box of the pane's size, and the content is laid out in that box afterwards, as
//! the cached Settings view is, only without the cache: it is drawn anew every frame.

use std::panic::Location;

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, Style, Window, relative, size,
};

/// `content`, filling the space its parent gives it, laid out apart from the parent.
/// `content`'s own size must not depend on what it holds (fill its parent, at most), or
/// the parent no longer grows with it.
pub(crate) fn laid_out_alone(content: impl IntoElement) -> LaidOutAlone {
    LaidOutAlone {
        content: content.into_any_element(),
    }
}

pub(crate) struct LaidOutAlone {
    content: AnyElement,
}

impl IntoElement for LaidOutAlone {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for LaidOutAlone {
    type RequestLayoutState = ();
    type PrepaintState = ();

    // No id of its own: the content keeps the ids, and so the state, it had without it.
    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size = size(relative(1.).into(), relative(1.).into());
        (window.request_layout(style, None, cx), ())
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
        self.content
            .prepaint_as_root(bounds.origin, bounds.size.into(), window, cx);
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
        self.content.paint(window, cx);
    }
}
