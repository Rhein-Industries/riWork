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
use std::collections::HashMap;

pub type Button = Control<base::Button>;
pub type Toggle = Control<base::Toggle>;
pub type Switch = Control<base::Switch>;
pub type Radio = Control<base::Radio>;
pub type Link = Control<base::Link>;
pub type Segments = base::ToggleGroup;
pub type Popup = base::Popup;

/// Run after text_input::init; do not initialize Base a second time.
pub fn init(cx: &mut App) {
    // `&&` tests one context. Root and its focused descendant are separate
    // contexts, so use `>` to suppress only the ancestor Base bindings.
    // Input indentation/navigation and user/domain bindings remain intact.
    cx.bind_keys([
        gpui::KeyBinding::new("tab", gpui::Unbind("root::Tab".into()), Some("Root > (Terminal || Input)")),
        gpui::KeyBinding::new("shift-tab", gpui::Unbind("root::TabPrev".into()), Some("Root > (Terminal || Input)")),
        #[cfg(target_os = "macos")]
        gpui::KeyBinding::new("cmd-c", gpui::Unbind("input::Copy".into()), Some("(Root > Terminal) && !Input")),
        #[cfg(not(target_os = "macos"))]
        gpui::KeyBinding::new("ctrl-c", gpui::Unbind("input::Copy".into()), Some("(Root > Terminal) && !Input")),
    ]);
    cx.on_window_closed(|cx, id| {
        if cx.has_global::<SelectionScopes>() {
            cx.global_mut::<SelectionScopes>().0.remove(&id);
        }
    }).detach();
}

#[derive(Default)]
struct SelectionScopes(HashMap<gpui::WindowId, ScopeState>);
impl gpui::Global for SelectionScopes {}
#[derive(Default)]
struct ScopeState {
    content: base::TextSelectionScopeId,
    modal: Option<base::TextSelectionScopeId>,
}

/// Chat/content owners call this from pointer capture before Base's bubble gesture.
/// Remember content scope per window; a modal cannot be displaced by background input.
pub fn activate_content_scope(scope: base::TextSelectionScopeId, window: &mut Window, cx: &mut App) {
    let id = window.window_handle().window_id();
    let state = cx.default_global::<SelectionScopes>().0.entry(id).or_default();
    if state.modal.is_some() { return; }
    state.content = scope;
    base::TextSelection::activate_scope(scope, window, cx);
}

/// A content owner calls this when retiring/replacing its semantic scope.
pub fn retire_content_scope(scope: base::TextSelectionScopeId, window: &mut Window, cx: &mut App) {
    let id = window.window_handle().window_id();
    let state = cx.default_global::<SelectionScopes>().0.entry(id).or_default();
    if state.content != scope { return; }
    state.content = Default::default();
    if state.modal.is_none() {
        base::TextSelection::activate_scope(Default::default(), window, cx);
    }
}

/// Owner lifecycle glue; Base focus traps own traversal, domain owners own dismissal.
#[derive(Default)]
pub struct FocusReturn(Option<gpui::FocusHandle>);
impl FocusReturn {
    pub fn capture(&mut self, window: &Window, cx: &App) { self.0 = window.focused(cx); }
    pub fn restore(&mut self, window: &mut Window, cx: &mut App) -> bool {
        if let Some(focus) = self.0.take() { focus.focus(window, cx); true } else { false }
    }
    pub fn restore_within(&mut self, owner: &gpui::FocusHandle, window: &mut Window, cx: &mut App) -> bool {
        if let Some(focus) = self.0.take().filter(|focus| owner.contains(focus, window)) {
            focus.focus(window, cx);
            true
        } else { false }
    }
    pub fn forget(&mut self) { self.0 = None; }
}

/// Called by the window owner on render. Ordinary renders never write a scope.
/// Closing a modal clears its selection and restores the last registered content scope.
pub fn sync_modal_scope(modal: Option<base::TextSelectionScopeId>, window: &mut Window, cx: &mut App) {
    let id = window.window_handle().window_id();
    let state = cx.default_global::<SelectionScopes>().0.entry(id).or_default();
    if state.modal == modal { return; }
    state.modal = modal;
    let scope = modal.unwrap_or(state.content);
    base::TextSelection::activate_scope(scope, window, cx);
}

/// Native menu dispatch uses the same editor actions as Base's existing shortcuts.
/// This installs no application-wide key bindings, including in terminals.
pub fn edit_menu() -> gpui::Menu {
    gpui::Menu::new("Edit").items([
        gpui::MenuItem::os_action("Cut", base::input::Cut, gpui::OsAction::Cut),
        gpui::MenuItem::os_action("Copy", base::input::Copy, gpui::OsAction::Copy),
        gpui::MenuItem::os_action("Paste", base::input::Paste, gpui::OsAction::Paste),
        gpui::MenuItem::separator(),
        gpui::MenuItem::os_action("Select All", base::input::SelectAll, gpui::OsAction::SelectAll),
    ])
}

/// Bubble-only: Ghostty already received this unbound raw shortcut. Keep AppKit
/// from dispatching the same gesture again through an editor menu equivalent.
/// Bound terminal paste/file actions and nested Input actions run before this.
pub fn protect_terminal_edit_menu_fallback(event: &gpui::KeyDownEvent, window: &Window, cx: &mut App) -> bool {
    let modifiers = event.keystroke.modifiers;
    let edit_shortcut = modifiers.platform && !modifiers.control && !modifiers.alt && !modifiers.shift
        && matches!(event.keystroke.key.as_str(), "x" | "c" | "v" | "a");
    let contexts = window.context_stack();
    if edit_shortcut && contexts.iter().any(|context| context.contains("Terminal"))
        && !contexts.iter().any(|context| context.contains("Input")) {
        cx.stop_propagation();
        true
    } else { false }
}

/// Preserve access to the application entity while Base owns the window root.
pub fn content_view<V: gpui::Render>(view: gpui::AnyView, cx: &App) -> Option<gpui::Entity<V>> {
    if let Ok(content) = view.clone().downcast::<V>() { return Some(content); }
    view.downcast::<base::Root>().ok()?.read(cx).view().clone().downcast::<V>().ok()
}

pub fn selection_scope(element: impl IntoElement + ParentElement, scope: base::TextSelectionScopeId) -> impl IntoElement {
    element.text_selection_scope(scope)
}
pub fn focus_scope<E>(element: E, id: impl Into<ElementId>, focus: &gpui::FocusHandle) -> impl IntoElement + ParentElement
where E: Element + ParentElement + Styled + InteractiveElement + 'static {
    // Pinned FocusTrapContainer forwards layout/events but omits AX metadata.
    // Keep this container's existing metadata on the same trapped element.
    let metadata = element.a11y_role().map(|role| {
        let mut node = accesskit::Node::new(role);
        element.write_a11y_info(&mut node);
        node.add_action(accesskit::Action::Focus);
        node
    });
    AccessibleState { inner: element.focus_trap(id, focus), disabled: false, metadata }
}
/// Keep Root traversal and transcript selection inside the active modal.
pub fn modal_scope<E>(
    element: E, scope: base::TextSelectionScopeId, id: impl Into<ElementId>,
    focus: &gpui::FocusHandle,
) -> impl IntoElement
where E: Element + ParentElement + Styled + InteractiveElement + 'static {
    focus_scope(element, id, focus).text_selection_scope(scope)
}

pub trait Primitive: RenderOnce + Styled + ParentElement + InteractiveElement {
    fn with_disabled(self, disabled: bool) -> Self;
    fn with_name(self, name: SharedString) -> Self;
    fn with_tab_stop(self, tab_stop: bool) -> Self;
    fn prepare_disabled(self, _disabled: bool) -> Self { self }
}
macro_rules! primitive {
    ($($ty:ty),+) => { $(impl Primitive for $ty {
        fn with_disabled(self, disabled: bool) -> Self { self.disabled(disabled) }
        fn with_name(self, name: SharedString) -> Self { self.accessibility_label(name) }
        fn with_tab_stop(self, tab_stop: bool) -> Self { self.tab_stop(tab_stop) }
    })+ };
}
primitive!(base::Button, base::Toggle, base::Switch, base::Link);
impl Primitive for base::Radio {
    fn with_disabled(self, disabled: bool) -> Self { self.disabled(disabled) }
    fn with_name(self, name: SharedString) -> Self { self.accessibility_label(name) }
    fn with_tab_stop(self, tab_stop: bool) -> Self { self.tab_stop(tab_stop) }
    fn prepare_disabled(self, disabled: bool) -> Self {
        // Base Radio omits Button's disabled pointer suppression as well as its AX flag.
        self.when(disabled, |radio| radio.on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation()))
    }
}

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
    fn render(mut self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // Base's neutral relative(1.) must not change inherited RiWork row typography.
        // Explicit caller line height and alignment always win.
        if self.base.text_style().line_height.is_none() {
            self.base = self.base.line_height(window.text_style().line_height);
        }
        if self.base.style().display == Some(gpui::Display::Flex)
            && self.base.style().justify_content.is_none()
        {
            self.base = self.base.justify_start();
        }
        AccessibleState {
            inner: self.base.prepare_disabled(self.disabled).render(window, cx).into_element(),
            disabled: self.disabled,
            metadata: None,
        }
    }
}
impl Control<base::Button> {
    pub fn role(mut self, role: impl Into<base::RoleOverride>) -> Self {
        self.base = self.base.role(role); self
    }
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
change_control!(base::Radio);
impl Control<base::Radio> {
    pub fn set_position(mut self, position: usize, size: usize) -> Self {
        self.base = self.base.set_position(position, size); self
    }
}
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

/// Behavior only. The caller owns visible content, focus ring and every style.
pub fn button_content(id: impl Into<ElementId>, name: impl Into<SharedString>, content: impl IntoElement) -> Button {
    Control::new(base::Button::new(id).accessibility_label(name).child(content))
}
pub fn toggle_content(id: impl Into<ElementId>, name: impl Into<SharedString>, content: impl IntoElement, pressed: bool) -> Toggle {
    Control::new(base::Toggle::new(id).accessibility_label(name).pressed(pressed).child(content))
}
pub fn switch_content(id: impl Into<ElementId>, name: impl Into<SharedString>, content: impl IntoElement, checked: bool) -> Switch {
    Control::new(base::Switch::new(id).accessibility_label(name).checked(checked).child(content))
}
pub fn radio_content(id: impl Into<ElementId>, name: impl Into<SharedString>, content: impl IntoElement, checked: bool) -> Radio {
    Control::new(base::Radio::new(id).accessibility_label(name).checked(checked).child(content))
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

/// Native inactive-tab chrome stays in Base traversal while visually quiet.
pub fn tab_close_reveal(close: Button, group: impl Into<SharedString>) -> Button {
    close
        .opacity(0.)
        .group_hover(group, |style| style.opacity(1.))
        .focus(|style| style.opacity(1.))
}

/// Stop a close-target press from arming the parent tab's drag/selection engine.
/// This non-focusable ancestor runs after Base's child default focus handler.
pub fn tab_close_boundary(id: impl Into<ElementId>, close: Button) -> impl IntoElement {
    div().id(id).flex_none()
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(close)
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
pub struct AccessibleState<E: Element> { inner: E, disabled: bool, metadata: Option<accesskit::Node> }
impl<E: Element + ParentElement> ParentElement for AccessibleState<E> {
    fn extend(&mut self, elements: impl IntoIterator<Item = gpui::AnyElement>) { self.inner.extend(elements); }
}
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
    fn a11y_role(&self) -> Option<gpui::Role> { self.metadata.as_ref().map(|node| node.role()).or_else(|| self.inner.a11y_role()) }
    fn write_a11y_info(&self, node: &mut accesskit::Node) {
        if let Some(metadata) = &self.metadata { *node = metadata.clone(); } else { self.inner.write_a11y_info(node); }
        if self.disabled { node.set_disabled(); }
    }
    fn a11y_synthetic_children(&mut self, prepaint: &mut Self::PrepaintState, builder: &mut A11ySubtreeBuilder) {
        self.inner.a11y_synthetic_children(prepaint, builder);
    }
}

#[cfg(test)]
mod tests;
