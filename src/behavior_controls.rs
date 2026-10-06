//! RiWork presentation over Base controls. Base owns activation and focus.
//! The transparent element refinement only supplies missing disabled AX state.
#![allow(dead_code)] // Shared consumer migrations land independently.

use gpui::{
    A11ySubtreeBuilder, App, Bounds, ClickEvent, Element, ElementId, GlobalElementId,
    InspectorElementId, InteractiveElement, Interactivity, IntoElement, LayoutId, ParentElement,
    Pixels, RenderOnce, SharedString, StatefulInteractiveElement, StyleRefinement, Styled, Window,
    accesskit, div, prelude::*, rgb,
};
use gpui_kit::base::{self, ElementExt, FocusTrapElement};
use crate::{controls, theme::Palette, ui_text};

pub type Button = Control<base::Button>;
pub type Toggle = Control<base::Toggle>;
pub type Switch = Control<base::Switch>;
pub type Link = Control<base::Link>;
pub type Segments = base::ToggleGroup;
pub type Popup = base::Popup;

/// Run after text_input::init; do not initialize Base a second time.
pub fn init(cx: &mut App) {
    // Suppress only Base Root traversal in editors/terminals. Input's own
    // indentation/navigation actions and owner raw composition policy remain.
    cx.bind_keys([
        gpui::KeyBinding::new("tab", gpui::Unbind("root::Tab".into()), Some("Root && (Terminal || Input)")),
        gpui::KeyBinding::new("shift-tab", gpui::Unbind("root::TabPrev".into()), Some("Root && (Terminal || Input)")),
        #[cfg(target_os = "macos")]
        gpui::KeyBinding::new("cmd-c", gpui::Unbind("input::Copy".into()), Some("Root && Terminal && !Input")),
        #[cfg(not(target_os = "macos"))]
        gpui::KeyBinding::new("ctrl-c", gpui::Unbind("input::Copy".into()), Some("Root && Terminal && !Input")),
    ]);
}

/// Preserve access to the application entity while Base owns the window root.
pub fn content_view<V: gpui::Render>(view: gpui::AnyView, cx: &App) -> Option<gpui::Entity<V>> {
    if let Ok(content) = view.clone().downcast::<V>() { return Some(content); }
    view.downcast::<base::Root>().ok()?.read(cx).view().clone().downcast::<V>().ok()
}

pub fn selection_scope(element: impl IntoElement + ParentElement, scope: base::TextSelectionScopeId) -> impl IntoElement {
    element.text_selection_scope(scope)
}
/// Keep Root traversal and transcript selection inside the active modal.
pub fn modal_scope<E>(
    element: E, scope: base::TextSelectionScopeId, id: impl Into<ElementId>,
    focus: &gpui::FocusHandle,
) -> impl IntoElement
where E: Element + ParentElement + Styled + InteractiveElement + 'static {
    element.focus_trap(id, focus).text_selection_scope(scope)
}

pub trait Primitive: RenderOnce + Styled + ParentElement + InteractiveElement {
    fn with_disabled(self, disabled: bool) -> Self;
    fn with_name(self, name: SharedString) -> Self;
    fn with_tab_stop(self, tab_stop: bool) -> Self;
}
macro_rules! primitive {
    ($($ty:ty),+) => { $(impl Primitive for $ty {
        fn with_disabled(self, disabled: bool) -> Self { self.disabled(disabled) }
        fn with_name(self, name: SharedString) -> Self { self.accessibility_label(name) }
        fn with_tab_stop(self, tab_stop: bool) -> Self { self.tab_stop(tab_stop) }
    })+ };
}
primitive!(base::Button, base::Toggle, base::Switch, base::Link);

#[derive(IntoElement)]
pub struct Control<B: Primitive> {
    base: B,
    disabled: bool,
}
impl<B: Primitive> Control<B> {
    fn new(base: B) -> Self { Self { base, disabled: false } }
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self.base = self.base.with_disabled(disabled);
        self
    }
    pub fn accessibility_label(mut self, name: impl Into<SharedString>) -> Self {
        self.base = self.base.with_name(name.into());
        self
    }
    pub fn tab_stop(mut self, tab_stop: bool) -> Self {
        self.base = self.base.with_tab_stop(tab_stop);
        self
    }
}
impl<B: Primitive> Styled for Control<B> {
    fn style(&mut self) -> &mut StyleRefinement { self.base.style() }
}
impl<B: Primitive> ParentElement for Control<B> {
    fn extend(&mut self, elements: impl IntoIterator<Item = gpui::AnyElement>) {
        self.base.extend(elements);
    }
}
impl<B: Primitive> InteractiveElement for Control<B> {
    fn interactivity(&mut self) -> &mut Interactivity { self.base.interactivity() }
}
impl<B: Primitive> StatefulInteractiveElement for Control<B> {}
impl<B: Primitive> RenderOnce for Control<B> {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        AccessibleState {
            inner: self.base.render(window, cx).into_element(),
            disabled: self.disabled,
        }
    }
}
impl Control<base::Button> {
    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.base = self.base.on_click(handler); self
    }
    pub fn selected(mut self, selected: bool) -> Self {
        self.base = self.base.selected(selected); self
    }
    pub fn track_focus(mut self, focus: &gpui::FocusHandle) -> Self {
        self.base = self.base.track_focus(focus); self
    }
}
macro_rules! change_control {
    ($ty:ty) => { impl Control<$ty> {
        pub fn on_change(mut self, handler: impl Fn(bool, &ClickEvent, &mut Window, &mut App) + 'static) -> Self {
            self.base = self.base.on_change(handler); self
        }
        pub fn track_focus(mut self, focus: &gpui::FocusHandle) -> Self {
            self.base = self.base.track_focus(focus); self
        }
    }};
}
change_control!(base::Toggle);
change_control!(base::Switch);
impl Control<base::Link> {
    pub fn href(mut self, href: impl Into<SharedString>) -> Self {
        self.base = self.base.href(href); self
    }
    pub fn open_with(mut self, handler: impl Fn(&str, &ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.base = self.base.open_with(handler); self
    }
    pub fn on_activate(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.base = self.base.on_activate(handler); self
    }
}

pub fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, kind: controls::Button, colors: Palette) -> Button {
    let label = label.into();
    let disabled = kind == controls::Button::Disabled;
    let base = base::Button::new(id).accessibility_label(label.clone())
        .child(label).px(ui_text::space(8.)).py(ui_text::space(4.))
        .border_1().border_color(rgb(colors.divider)).rounded(ui_text::space(3.))
        .bg(rgb(if kind == controls::Button::Primary { colors.cyan } else { colors.panel }))
        .text_color(rgb(if kind == controls::Button::Primary { colors.bg } else { colors.text }))
        .focus_visible(move |style| style.border_color(rgb(colors.focus)))
        .hover(move |style| style.bg(rgb(kind.hover(colors))))
        .styles(|styles| styles.disabled(|style| style.bg(rgb(colors.panel_active)).text_color(rgb(colors.muted))));
    Control::new(controls::native(base, |base| controls::button(base, kind, colors))).disabled(disabled)
}
pub fn disabled_button(id: impl Into<ElementId>, label: impl Into<SharedString>, colors: Palette) -> Button {
    button(id, label, controls::Button::Disabled, colors)
}
/// Existing text/icon layout with Base activation and a visible keyboard focus ring.
/// The owner supplies visible content and keeps its spacing/hover treatment.
pub fn action(id: impl Into<ElementId>, name: impl Into<SharedString>, colors: Palette) -> Button {
    Control::new(base::Button::new(id).accessibility_label(name)
        .border_1().border_color(gpui::transparent_black())
        .focus_visible(move |style| style.border_color(rgb(colors.focus)))
        .styles(|styles| styles.disabled(|style| style.opacity(0.45))))
}
pub fn toolbar_button(
    id: impl Into<ElementId>, symbol: &'static str, name: impl Into<SharedString>,
    enabled: bool, colors: Palette,
) -> Button {
    let name = name.into();
    let rest = if enabled { colors.muted } else { crate::theme::mix(colors.muted, colors.panel, 0.45) };
    action(id, name.clone(), colors).disabled(!enabled)
        .flex_none().size(ui_text::space(controls::TOOLBAR_BUTTON)).rounded_full()
        .text_color(rgb(rest))
        .when(enabled, |control| control.hover(move |style| style.bg(rgb(colors.divider)).text_color(rgb(colors.text))))
        .child(crate::icons::symbol(symbol, controls::TOOLBAR_SYMBOL, None))
        .child(crate::tooltip::anchor(name, crate::tooltip::Look::Control))
}
pub fn symbol(id: impl Into<ElementId>, name: impl Into<SharedString>, icon: impl IntoElement, colors: Palette) -> Button {
    let name = name.into();
    Control::new(base::Button::new(id).accessibility_label(name)
        .child(icon).min_w(ui_text::space(24.)).h(ui_text::space(24.))
        .rounded(ui_text::space(3.)).text_color(rgb(colors.muted))
        .border_1().border_color(gpui::transparent_black())
        .focus_visible(move |style| style.border_color(rgb(colors.focus)))
        .hover(move |style| style.bg(rgb(colors.divider)).text_color(rgb(colors.text)))
        .styles(|styles| styles.disabled(|style| style.text_color(rgb(colors.muted)))))
}
pub fn toggle(id: impl Into<ElementId>, label: impl Into<SharedString>, pressed: bool, colors: Palette) -> Toggle {
    let label = label.into();
    Control::new(base::Toggle::new(id).pressed(pressed).accessibility_label(label.clone())
        .child(label).px(ui_text::space(8.)).py(ui_text::space(4.))
        .border_1().border_color(rgb(if pressed { colors.cyan } else { colors.divider }))
        .rounded(ui_text::space(3.)).bg(rgb(colors.panel))
        .text_color(rgb(if pressed { colors.cyan } else { colors.text }))
        .focus_visible(move |style| style.border_color(rgb(colors.focus)))
        .hover(move |style| style.bg(rgb(colors.panel_active)))
        .styles(|styles| styles.disabled(|style| style.text_color(rgb(colors.muted)))))
}
pub fn switch(id: impl Into<ElementId>, name: impl Into<SharedString>, checked: bool, colors: Palette) -> Switch {
    let mark = if ui_text::is_native() {
        controls::switch(checked, colors)
    } else {
        div().size(ui_text::space(17.)).border_1()
            .border_color(rgb(if checked { colors.cyan } else { colors.divider }))
            .text_color(rgb(colors.cyan)).child(if checked { "✓" } else { "" }).into_any_element()
    };
    Control::new(base::Switch::new(id).checked(checked).accessibility_label(name)
        .child(mark).p(ui_text::space(2.))
        .border_1().border_color(gpui::transparent_black()).rounded(ui_text::space(3.))
        .focus_visible(move |style| style.border_color(rgb(colors.focus)))
        .styles(|styles| styles.disabled(|style| style.opacity(0.45).text_color(rgb(colors.muted)))))
}
pub fn segments(id: impl Into<ElementId>, name: impl Into<SharedString>, colors: Palette) -> Segments {
    base::ToggleGroup::new(id).aria_label(name).flex().gap(ui_text::space(2.))
        .p(ui_text::space(2.)).bg(rgb(colors.panel_active)).rounded(ui_text::space(3.))
}
pub fn segment(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool, colors: Palette) -> Toggle {
    let control = toggle(id, label, selected, colors);
    controls::native(control, |control| controls::segment(control, selected, colors))
}
pub fn link(id: impl Into<ElementId>, label: impl Into<SharedString>, colors: Palette) -> Link {
    let label = label.into();
    Control::new(base::Link::new(id).accessibility_label(label.clone()).child(label)
        .text_color(rgb(colors.cyan)).border_1().border_color(gpui::transparent_black())
        .focus_visible(move |style| style.border_color(rgb(colors.focus)))
        .styles(|styles| styles.disabled(|style| style.text_color(rgb(colors.muted)))))
}
pub fn popup(id: impl Into<ElementId>, trigger: impl IntoElement) -> Popup {
    base::Popup::new(id, trigger).offset(ui_text::space(4.))
}

// Base 0.7.1 disables activation/focus but does not write Node::disabled.
// Forward every element phase and identity; refine its existing AX node only.
// There is no extra hitbox, focus handle, accessibility node or click handler.
pub struct AccessibleState<E: Element> { inner: E, disabled: bool }
impl<E: Element> IntoElement for AccessibleState<E> {
    type Element = Self;
    fn into_element(self) -> Self { self }
}
impl<E: Element> Element for AccessibleState<E> {
    type RequestLayoutState = E::RequestLayoutState;
    type PrepaintState = E::PrepaintState;
    fn id(&self) -> Option<ElementId> { self.inner.id() }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> { self.inner.source_location() }
    fn request_layout(&mut self, id: Option<&GlobalElementId>, inspector: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, Self::RequestLayoutState) {
        self.inner.request_layout(id, inspector, window, cx)
    }
    fn prepaint(&mut self, id: Option<&GlobalElementId>, inspector: Option<&InspectorElementId>, bounds: Bounds<Pixels>, layout: &mut Self::RequestLayoutState, window: &mut Window, cx: &mut App) -> Self::PrepaintState {
        self.inner.prepaint(id, inspector, bounds, layout, window, cx)
    }
    fn paint(&mut self, id: Option<&GlobalElementId>, inspector: Option<&InspectorElementId>, bounds: Bounds<Pixels>, layout: &mut Self::RequestLayoutState, prepaint: &mut Self::PrepaintState, window: &mut Window, cx: &mut App) {
        self.inner.paint(id, inspector, bounds, layout, prepaint, window, cx)
    }
    fn a11y_role(&self) -> Option<gpui::Role> { self.inner.a11y_role() }
    fn write_a11y_info(&self, node: &mut accesskit::Node) {
        self.inner.write_a11y_info(node);
        if self.disabled { node.set_disabled(); }
    }
    fn a11y_synthetic_children(&mut self, prepaint: &mut Self::PrepaintState, builder: &mut A11ySubtreeBuilder) {
        self.inner.a11y_synthetic_children(prepaint, builder);
    }
}

#[cfg(test)]
mod tests;
