//! The chrome around the message list: the toolbar above it, and under it the banners, the
//! approval bar, the questions and the message box.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt, AnyElement, Context, ElementId, Focusable, FollowMode, PathBuilder,
    SharedString, Stateful, Window, canvas, deferred, div, list, point, prelude::*,
    pulsating_between, px, relative, rgb, transparent_black,
};
use gpui_kit::base::TestSupportExt as _;

use crate::{
    chat::model::{ApprovalKind, ApprovalMode, ChatState, Decision, Provider, Question},
    controls::{self, Button},
    dictation::{self, Phase},
    icons::{self, Icon},
    text_input, theme,
    tooltip::{self, Look as TipLook},
    ui_text,
};

use super::{
    ChatView, ChatViewEvent, Creation, Draft, Menu, approval, cards, composer, dictate,
    state::provider_name,
    toolbar,
    widgets::{self, Look, button, capsule, dimmed},
};

/// The chat header's inset and control gap.
use super::composer::{BAR_GAP, BAR_INSET};
use super::usage_chip::Detail;
pub(super) const ATTACHMENT_MENU_KEY: &str = "composer-attachment-menu";

/// The padding inside the message box's menus and the height of their rows, in design
/// points.
const CHOICE_MENU_PADDING: f32 = 7.0;
const CHOICE_ROW: f32 = 27.0;

/// What a row of the model or effort menu does when it is clicked or chosen with ⏎.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum MenuChoice {
    Effort(String),
    /// A model from the driver's list.
    Model(String),
    /// A model named by hand, for a driver that has not listed its models.
    Named(String),
    /// Go on with the other provider, with its default model for `None`.
    Switch(Provider, Option<String>),
    NewChat,
}

/// One row of the model or effort menu. A row without a `choice` is shown and cannot be
/// chosen now.
struct ChoiceRow {
    name: String,
    label: String,
    detail: Option<String>,
    current: bool,
    choice: Option<MenuChoice>,
}

/// A run of rows under a heading (a provider's name), with a note after the heading.
struct ChoiceGroup {
    heading: Option<String>,
    note: Option<String>,
    rows: Vec<ChoiceRow>,
}

/// The chat header's measured pieces (see `toolbar`).
const SLOT_DISPLAY: usize = 0;
const SLOT_THREAD: usize = 6;
const SLOT_MORE: usize = 7;
const SLOT_BILLING: usize = 8;

/// A header control that folds into ⋯ when row 1 has no room for it, last first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fold {
    Mode,
    Model,
    Effort,
    Fast,
    Compact,
}

impl Fold {
    fn slot(self) -> usize {
        match self {
            Fold::Mode => 1,
            Fold::Model => 2,
            Fold::Effort => 3,
            Fold::Fast => 4,
            Fold::Compact => 5,
        }
    }
}

fn id(parts: impl Into<String>) -> ElementId {
    ElementId::Name(SharedString::from(parts.into()))
}

/// Continuous corners: the repeated control at each corner gives the cubic zero
/// curvature at its straight-edge joins, rather than a circular arc's abrupt join.
fn composer_path(path: &mut PathBuilder, bounds: gpui::Bounds<gpui::Pixels>, radius: gpui::Pixels) {
    let left = bounds.left();
    let right = bounds.right();
    let top = bounds.top();
    let bottom = bounds.bottom();
    let radius = radius
        .min(bounds.size.width / 2.0)
        .min(bounds.size.height / 2.0);
    path.move_to(point(left + radius, top));
    path.line_to(point(right - radius, top));
    path.cubic_bezier_to(
        point(right, top + radius),
        point(right, top),
        point(right, top),
    );
    path.line_to(point(right, bottom - radius));
    path.cubic_bezier_to(
        point(right - radius, bottom),
        point(right, bottom),
        point(right, bottom),
    );
    path.line_to(point(left + radius, bottom));
    path.cubic_bezier_to(
        point(left, bottom - radius),
        point(left, bottom),
        point(left, bottom),
    );
    path.line_to(point(left, top + radius));
    path.cubic_bezier_to(
        point(left + radius, top),
        point(left, top),
        point(left, top),
    );
    path.close();
}

/// The message box's card as the design draws it: Hermes's panel with a hairline edge and
/// small corners; every other design keeps the message field it had, the window's background
/// in a continuous-corner outline. The edge takes the focus color while the box has focus
/// (the colorful themes' cyan). The geometry is the same in every design: a one-pixel edge
/// around the card's padding.
fn composer_card(card: Stateful<gpui::Div>, look: Look, active: bool) -> Stateful<gpui::Div> {
    let colors = look.colors;
    let radius = look.card_radius();
    if look.hermes() {
        return card
            .rounded(radius)
            .border_1()
            .border_color(rgb(if active { colors.focus } else { colors.divider }))
            .bg(rgb(colors.panel));
    }
    let outline = match (active, look.native) {
        (false, _) => colors.divider,
        (true, true) => colors.focus,
        (true, false) => colors.cyan,
    };
    let surface = canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let bounds = bounds.inset(px(0.5));
            if bounds.size.width <= px(0.) || bounds.size.height <= px(0.) {
                return;
            }
            for (mut path, color) in [
                (PathBuilder::fill(), colors.bg),
                (PathBuilder::stroke(px(1.)), outline),
            ] {
                composer_path(&mut path, bounds, radius);
                if let Ok(path) = path.build() {
                    window.paint_path(path, rgb(color));
                }
            }
        },
    )
    .absolute()
    .inset_0();
    card.relative()
        .border_1()
        .border_color(transparent_black())
        .child(surface)
}

/// A bar above the message box (a notice, a request, a question) as a card in the same column
/// as the message box: its inset on the sides and above, the design's card corners.
fn above_composer<E: Styled>(bar: E, inset: gpui::Pixels, look: Look) -> E {
    bar.mx(inset)
        .mt(inset)
        .w_auto()
        .border_1()
        .rounded(look.card_radius())
}

/// The pressed state every quiet control of the message box shares: a step darker than
/// the fill under the pointer.
fn pressable<E: StatefulInteractiveElement>(control: E, look: Look) -> E {
    let pressed = theme::mix(look.colors.divider, look.colors.text, 0.12);
    control.active(move |style| style.bg(rgb(pressed)))
}

/// One of the message box's quiet controls (the model and effort pickers, Fast): its label
/// in a capsule `side` tall, as the round buttons beside it, padded as the header's pills.
/// Bare at rest, it fills under the pointer as those buttons do, a step darker while pressed,
/// and stays filled while its menu is open (`held`); one that is on is tinted with `on`.
fn quiet_control<E: Styled + StatefulInteractiveElement + FluentBuilder>(
    control: E,
    side: gpui::Pixels,
    held: bool,
    on: Option<u32>,
    look: Look,
) -> E {
    let colors = look.colors;
    // A control that is on (Fast) is tinted with `on` at rest and a step more under the
    // pointer, its label in `on` (Native) or the text color.
    let (rest, fill, ink) = match on {
        Some(on) => (
            Some(look.tint(on, 0.18)),
            look.tint(on, 0.28),
            if look.native { on } else { colors.text },
        ),
        None => (
            held.then_some(colors.divider),
            colors.divider,
            if held { colors.text } else { colors.muted },
        ),
    };
    let hover_ink = if on.is_some() { ink } else { colors.text };
    pressable(control, look)
        .line_height(relative(1.618_034))
        .flex_none()
        .flex()
        .items_center()
        .gap(ui_text::space(4.0))
        .h(side)
        .px(ui_text::space(if look.native { 12.0 } else { 8.0 }))
        .rounded_full()
        .border_1()
        .border_color(transparent_black())
        .text_size(ui_text::text(11.0))
        .font_family(look.chat_family())
        .text_color(rgb(ink))
        .when_some(rest, |control, rest| control.bg(rgb(rest)))
        .cursor_pointer()
        .hover(move |style| style.bg(rgb(fill)).text_color(rgb(hover_ink)))
        .focus_visible(move |style| style.border_color(rgb(colors.focus)))
}

/// A message box button in the colorful themes: a square of `side` with a one-character mark,
/// bordered like their other buttons, in `accent` for the primary or a signal one; its name
/// and keys in the tooltip. Native draws symbols in squares of the same side.
fn mark_button(
    id: &'static str,
    mark: &'static str,
    tooltip: impl Into<SharedString>,
    accent: Option<u32>,
    side: gpui::Pixels,
    look: Look,
) -> crate::behavior_controls::Button {
    let name = tooltip.into();
    mark_look(
        crate::behavior_controls::button_content(id, name.clone(), mark),
        name,
        accent,
        side,
        look,
    )
}

/// A `mark_button` that stays pressed while `pressed`, such as the colorful themes' mic.
/// The caller adds `on_change`.
fn mark_toggle(
    id: &'static str,
    mark: &'static str,
    tooltip: impl Into<SharedString>,
    accent: Option<u32>,
    pressed: bool,
    side: gpui::Pixels,
    look: Look,
) -> crate::behavior_controls::Toggle {
    let name = tooltip.into();
    mark_look(
        crate::behavior_controls::toggle_content(id, name.clone(), mark, pressed),
        name,
        accent,
        side,
        look,
    )
}

/// A `mark_button`'s look, its edge in the focus color while it has keyboard focus as the
/// colorful themes' other buttons have it.
fn mark_look<E: Styled + InteractiveElement + ParentElement>(
    control: E,
    name: SharedString,
    accent: Option<u32>,
    side: gpui::Pixels,
    look: Look,
) -> E {
    let colors = look.colors;
    control
        .line_height(gpui::relative(1.618_034))
        .flex_none()
        .size(side)
        .flex()
        .items_center()
        .justify_center()
        .border_1()
        .border_color(rgb(accent.unwrap_or(colors.divider)))
        .rounded(px(3.0))
        .bg(rgb(colors.panel))
        .text_size(ui_text::text(11.0).min(((side - px(4.0)).max(px(1.0))) / 1.618_034))
        .text_color(rgb(accent.unwrap_or(colors.text)))
        .cursor_pointer()
        .hover(move |style| style.bg(rgb(colors.panel_active)))
        .focus_visible(move |style| style.border_color(rgb(colors.focus)))
        .child(tooltip::anchor(name, TipLook::Control))
}

/// The headline of a tab with no chat to show, in the signal color when something `failed`.
/// Native sets it like a panel's title, in semibold.
fn headline_text(text: String, failed: bool, look: Look) -> gpui::Div {
    let colors = look.colors;
    div()
        .text_size(ui_text::text(13.0))
        .when(look.native, |headline| {
            headline.font_weight(gpui::FontWeight::SEMIBOLD)
        })
        .text_color(rgb(if failed { colors.gold } else { colors.text }))
        .child(text)
}

/// One answer per prompt of a question: the options picked, then the text typed.
pub(super) fn answers_of(
    question: &Question,
    draft: Option<&Draft>,
    typed: &[String],
) -> Vec<Vec<String>> {
    question
        .questions
        .iter()
        .enumerate()
        .map(|(at, prompt)| {
            let mut answer: Vec<String> = draft
                .and_then(|draft| draft.picked.get(at))
                .into_iter()
                .flatten()
                .filter_map(|option| prompt.options.get(*option))
                .map(|option| option.label.clone())
                .collect();
            if let Some(text) = typed
                .get(at)
                .map(|input| input.trim())
                .filter(|t| !t.is_empty())
            {
                answer.push(text.to_owned());
            }
            answer
        })
        .collect()
}

impl ChatView {
    /// The whole tab of a chat that exists.
    pub(super) fn render_chat(
        &mut self,
        look: Look,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = look.colors;
        let messages = list(
            self.list.clone(),
            cx.processor(|view, ix, window, cx| view.render_row(ix, window, cx)),
        )
        .size_full();
        let messages = self
            .transcript_selection
            .viewport(messages, self.list.clone(), look);
        // Scrolled up, not merely not yet following again after a scroll to the end.
        let behind = self.items_in_list > 0
            && !self.list.is_following_tail()
            && self.list.is_scrolled_to_end() != Some(true);
        let stack = div()
            .size_full()
            .flex()
            .flex_col()
            .child(self.toolbar(look, window, cx))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(messages)
                    .children(behind.then(|| {
                        div()
                            .absolute()
                            .bottom(ui_text::space(10.0))
                            .w_full()
                            .flex()
                            .justify_center()
                            .child(
                                if look.native {
                                    // A raised grey capsule with the arrow as its symbol.
                                    capsule("chat-latest", "Latest", Button::Secondary, look)
                                        .child(icons::text_mark("↓", 9.0))
                                        .shadow_md()
                                        .cursor_pointer()
                                        .hover(move |style| {
                                            style.bg(rgb(Button::Secondary.hover(colors)))
                                        })
                                } else {
                                    button("chat-latest", "↓ latest", Some(colors.cyan), look)
                                        .bg(rgb(colors.panel_active))
                                }
                                .on_click(cx.listener(
                                    |view, _, _, cx| {
                                        view.list.set_follow_mode(FollowMode::Tail);
                                        cx.notify();
                                    },
                                )),
                            )
                    })),
            )
            .children(self.banner(look, cx))
            .children(self.approval_bar(look, cx))
            .children(self.question_panel(look, window, cx))
            .child(self.composer_box(look, window, cx));
        div()
            .relative()
            .size_full()
            .child(stack)
            .children(self.image_viewer(look, cx))
            .into_any_element()
    }

    /// What a tab shows while its chat is being made, or when that did not work.
    pub(super) fn render_starting(&self, look: Look, cx: &mut Context<Self>) -> AnyElement {
        let colors = look.colors;
        let (headline, detail, failed) = match &self.creation {
            Some(Creation::Failed(chat, error)) => (
                format!("Could not start {} chat", provider_name(chat.provider)),
                Some(error.clone()),
                true,
            ),
            Some(Creation::Pending(chat)) => (
                format!("Starting {} chat…", provider_name(chat.provider)),
                Some(chat.cwd.display().to_string()),
                false,
            ),
            None => ("Connecting…".to_owned(), None, false),
        };
        // While it starts the detail is the chat's folder, a path.
        let path = matches!(self.creation, Some(Creation::Pending(_)));
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(ui_text::space(10.0))
            .p(ui_text::space(20.0))
            .children(
                (look.native && failed)
                    .then(|| icons::symbol("exclamationmark.triangle", 22.0, Some(colors.gold))),
            )
            .child(headline_text(headline, failed, look))
            .children(detail.map(|detail| {
                div()
                    .max_w(ui_text::space(560.0))
                    .text_size(ui_text::text(11.0))
                    .text_color(rgb(colors.muted))
                    .when(look.native && path, |detail| {
                        detail.font_family(ui_text::mono_family())
                    })
                    .child(detail)
            }))
            .children(failed.then(|| {
                div()
                    .flex()
                    .gap(ui_text::space(8.0))
                    .child(
                        button("chat-retry-create", "Retry", Some(colors.cyan), look).on_click(
                            cx.listener(|view, _, window, cx| view.retry_creation(window, cx)),
                        ),
                    )
                    .child(
                        button("chat-close-failed", "Close", None, look)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ChatViewEvent::Close))),
                    )
            }))
            .into_any_element()
    }

    /// What a tab shows when the host no longer has its chat.
    pub(super) fn render_deleted(&self, look: Look, cx: &mut Context<Self>) -> AnyElement {
        let colors = look.colors;
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(ui_text::space(10.0))
            .children(look.native.then(|| {
                icons::symbol(
                    "trash",
                    22.0,
                    Some(theme::mix(colors.muted, colors.bg, 0.25)),
                )
            }))
            .child(headline_text(
                "This chat was deleted".to_owned(),
                false,
                look,
            ))
            .child(
                div()
                    .text_size(ui_text::text(11.0))
                    .text_color(rgb(colors.muted))
                    .child("Its history is gone from the chat host."),
            )
            .child(
                button("chat-close-deleted", "Close", Some(colors.cyan), look)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ChatViewEvent::Close))),
            )
            .into_any_element()
    }

    // -----------------------------------------------------------------------------------
    // The toolbar
    // -----------------------------------------------------------------------------------

    fn toolbar(&self, look: Look, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = look.colors;
        let info = self.model.transcript.info.as_ref();
        let mode = info.map(|info| info.approval_mode).unwrap_or_default();
        let models = &self.model.transcript.models;
        let model = info.and_then(|info| info.model.clone());
        let effort = info.and_then(|info| info.effort.clone());
        let fast = info.is_some_and(|info| info.fast);
        let thread = info.and_then(|info| info.provider_thread_id.clone());
        let idle = matches!(self.model.transcript.state, ChatState::Idle);

        // A pop-up button: its choice and an arrow. Native's is a grey capsule with a chevron,
        // a step darker while its menu is open.
        let picker = |name: &'static str, label: String, menu: Menu, cx: &mut Context<Self>| {
            let open = self.menu == Some(menu);
            let accessible = format!(
                "{}: {label}",
                match menu {
                    Menu::Mode => "Approval mode",
                    Menu::Model => "Model",
                    Menu::Effort => "Reasoning effort",
                    _ => "Chat choices",
                }
            );
            let trigger = if look.native {
                let fill = if open {
                    colors.divider
                } else {
                    colors.panel_active
                };
                capsule(name, label, Button::Secondary, look)
                    .bg(rgb(fill))
                    .child(icons::text_mark("▾", 9.0))
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgb(Button::Secondary.hover(colors))))
            } else {
                button(
                    name,
                    format!("{label} ▾"),
                    open.then_some(colors.cyan),
                    look,
                )
            };
            div()
                .relative()
                .child(
                    trigger
                        .accessibility_label(accessible)
                        .aria_expanded(open)
                        .on_click(cx.listener(move |view, _, window, cx| {
                            view.open.remove(ATTACHMENT_MENU_KEY);
                            view.toggle_menu(menu, window, cx)
                        })),
                )
                .children(open.then(|| self.menu_popover(menu, &[], false, look, window, cx)))
        };

        let display = div().child(
            widgets::segments("chat-display-mode", "Transcript display", look)
                .children(
                    [super::DisplayMode::Normal, super::DisplayMode::Verbose]
                        .into_iter()
                        .map(|mode| {
                            widgets::segment(
                                if mode == super::DisplayMode::Normal {
                                    "chat-display-normal"
                                } else {
                                    "chat-display-verbose"
                                },
                                mode.label(),
                                self.display_mode == mode,
                                look,
                            )
                            .on_change({
                                let owner = cx.weak_entity();
                                move |_, _, _, cx| {
                                    let _ =
                                        owner.update(cx, |view, cx| view.choose_display(mode, cx));
                                }
                            })
                        }),
                )
                .child(tooltip::anchor(
                    "Normal shows results · Verbose shows all activity",
                    TipLook::Control,
                )),
        );
        let mode_picker = picker(
            "chat-mode",
            toolbar::mode_label(mode).to_owned(),
            Menu::Mode,
            cx,
        );
        let compact = div().child(if idle {
            button("chat-compact", "Compact", None, look)
                .on_click(cx.listener(|view, _, _, cx| view.compact(cx)))
                .into_any_element()
        } else {
            dimmed("chat-compact", "Compact", look).into_any_element()
        });
        let thread = thread.map(|thread| {
            let key = "thread".to_owned();
            let copied = self.copied.as_deref() == Some("thread");
            let whole = thread.clone();
            // Native's is a capsule with the id in the monospace accent and a copy symbol.
            let copy = if look.native {
                let id = if copied {
                    div().child("Copied")
                } else {
                    div()
                        .font_family(ui_text::mono_family())
                        .child(toolbar::short_thread_id(&thread))
                };
                controls::button(
                    crate::behavior_controls::button_content(
                        "chat-thread",
                        "Copy provider thread ID",
                        id,
                    )
                    .line_height(gpui::relative(1.618_034))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(ui_text::space(4.0))
                    .py(ui_text::space(3.0))
                    .text_size(ui_text::text(10.0))
                    .child(icons::symbol(
                        if copied { "checkmark" } else { "doc.on.doc" },
                        9.0,
                        None,
                    )),
                    Button::Secondary,
                    colors,
                )
                .cursor_pointer()
                .hover(move |style| style.bg(rgb(Button::Secondary.hover(colors))))
            } else {
                button(
                    "chat-thread",
                    if copied {
                        "copied".to_owned()
                    } else {
                        format!("# {} ⧉", toolbar::short_thread_id(&thread))
                    },
                    None,
                    look,
                )
            };
            div()
                .relative()
                .child(
                    copy.accessibility_label(if copied {
                        "Provider thread ID copied"
                    } else {
                        "Copy provider thread ID"
                    })
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.copy(key.clone(), whole.clone(), cx);
                    })),
                )
                .child(tooltip::anchor(
                    format!("Copy thread id {thread}"),
                    TipLook::Control,
                ))
        });
        // Row 1 never wraps: what does not fit beside ⋯ goes into its menu, from the row's end
        // (Compact, Mode; model choices live in the composer). The usage and the session id share it, before ⋯,
        // when they fit; otherwise they take a row of their own, the session id at its far end
        // under ⋯. Each piece's width is
        // its own, not the layout's, so this settles after one redraw. A folded one keeps the
        // width it was last drawn at while it shows the same; one whose value has changed since
        // counts as narrow, comes back and is measured again.
        let widths = self.header_widths.clone();
        let keys: [u64; 9] = {
            use std::hash::{Hash, Hasher};
            let key = |shows: &str| {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                (
                    shows,
                    ui_text::scale().to_bits(),
                    look.native,
                    look.hermes(),
                )
                    .hash(&mut hasher);
                hasher.finish()
            };
            [
                key(""),
                key(toolbar::mode_label(mode)),
                key(&toolbar::model_label(models, model.as_deref())),
                key(&toolbar::effort_label(
                    models,
                    model.as_deref(),
                    effort.as_deref(),
                )),
                key(toolbar::fast_label(fast)),
                key(""),
                key(info
                    .and_then(|info| info.provider_thread_id.as_deref())
                    .unwrap_or("")),
                key(""),
                key(
                    toolbar::api_key_badge(self.model.transcript.account.as_ref())
                        .map_or("", |(label, _)| label),
                ),
            ]
        };
        let last = {
            let measured = widths.get();
            std::array::from_fn::<f32, 9, _>(|slot| {
                let (width, key) = measured[slot];
                if key == keys[slot] { width } else { 0. }
            })
        };
        let space = |base: f32| f32::from(ui_text::space(base));
        let gap = space(BAR_GAP);
        let inner = self.composer_width.get() - 2. * space(BAR_INSET);
        let foldable = [
            (Fold::Mode, true),
            (Fold::Model, false),
            (Fold::Effort, false),
            (Fold::Fast, false),
            (Fold::Compact, true),
        ]
        .into_iter()
        .filter_map(|(fold, shown)| shown.then_some(fold))
        .collect::<Vec<_>>();
        let row_width = |count: usize| {
            last[SLOT_DISPLAY]
                + foldable[..count]
                    .iter()
                    .map(|fold| gap + last[fold.slot()])
                    .sum::<f32>()
        };
        let thread_room = if thread.is_some() {
            gap + last[SLOT_THREAD]
        } else {
            0.
        };
        let more_room = gap + last[SLOT_MORE];
        // Keep every control beside More when all pieces fit. Otherwise the usage,
        // billing badge and thread ID move below; only then fold controls from the end.
        let billing_room =
            if toolbar::api_key_badge(self.model.transcript.account.as_ref()).is_some() {
                last[SLOT_BILLING] + gap
            } else {
                0.
            };
        let beside_usage = row_width(foldable.len()) + gap + thread_room + more_room + billing_room;
        let usage_room = inner - beside_usage - gap;
        let one_line = inner <= 0.
            || match self.usage_width(Detail::NoCost, window) {
                Some(usage) => usage <= usage_room + 0.5,
                None => beside_usage <= inner + 0.5,
            };
        let kept = if one_line {
            foldable.len()
        } else {
            (0..=foldable.len())
                .rev()
                .find(|count| row_width(*count) + more_room <= inner + 0.5)
                .unwrap_or(0)
        };
        let folded = foldable[kept..].to_vec();
        // A folded picker's menu opens from ⋯, as the rest of its menu does.
        let from_more = |menu: Menu| match menu {
            Menu::Mode => folded.contains(&Fold::Mode),
            Menu::Model => folded.contains(&Fold::Model),
            Menu::Effort => folded.contains(&Fold::Effort),
            Menu::More | Menu::ConfirmDelete => true,
        };
        let more_menu = self.menu.filter(|menu| from_more(*menu));
        let folded_menus = [Menu::Mode, Menu::Model, Menu::Effort]
            .into_iter()
            .filter(|menu| from_more(*menu))
            .collect::<Vec<_>>();
        let more = div()
            .flex_none()
            .relative()
            .child({
                let open = more_menu.is_some();
                if look.native {
                    let more = widgets::symbol_button("chat-more", "ellipsis", "More", look);
                    if open {
                        more.text_color(rgb(colors.text)).bg(rgb(colors.divider))
                    } else {
                        more
                    }
                } else {
                    button("chat-more", "⋯", open.then_some(colors.cyan), look)
                        .accessibility_label("More chat actions")
                }
                .aria_expanded(open)
                .on_click(cx.listener(move |view, _, window, cx| {
                    // A folded picker's menu hangs from ⋯ too: pressing ⋯ closes it rather
                    // than opening the rest, also when the press outside it already has.
                    let open = view.menu.is_some_and(|menu| folded_menus.contains(&menu));
                    let just_closed = view.menu.is_none()
                        && view.menu_closed.is_some_and(|(menu, at)| {
                            folded_menus.contains(&menu)
                                && at.elapsed() < Duration::from_millis(300)
                        });
                    if open || just_closed {
                        view.menu_closed = None;
                        view.close_menu(cx);
                    } else {
                        view.toggle_menu(Menu::More, window, cx);
                    }
                }))
            })
            .children(
                more_menu.map(|menu| self.menu_popover(menu, &folded, true, look, window, cx)),
            );

        let measured = |slot: usize, piece: gpui::Div| {
            let widths = widths.clone();
            let key = keys[slot];
            piece.relative().child(
                canvas(
                    move |bounds, window, _| {
                        let mut all = widths.get();
                        let width = f32::from(bounds.size.width);
                        if (all[slot].0 - width).abs() >= 0.5 || all[slot].1 != key {
                            all[slot] = (width, key);
                            widths.set(all);
                            window.refresh();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .w_full()
                .h(px(0.)),
            )
        };
        let controls = div()
            .flex()
            .items_center()
            .gap(ui_text::space(BAR_GAP))
            .child(measured(SLOT_DISPLAY, display.flex_none()))
            .children(
                [
                    (Fold::Mode, Some(mode_picker)),
                    (Fold::Model, None),
                    (Fold::Effort, None),
                    (Fold::Fast, None),
                    (Fold::Compact, Some(compact)),
                ]
                .into_iter()
                .filter(|(fold, _)| !folded.contains(fold))
                .filter_map(|(fold, piece)| {
                    piece.map(|piece| measured(fold.slot(), piece.flex_none()))
                }),
            );
        let usage = self
            .usage_group(
                if one_line {
                    (inner > 0.).then_some(usage_room + 0.5)
                } else {
                    Some(inner - thread_room - billing_room)
                },
                look,
                window,
                cx,
            )
            .map(|usage| div().flex_none().child(usage));
        let usage = Some(
            div()
                .flex()
                .items_center()
                .gap(ui_text::space(BAR_GAP))
                .children(
                    toolbar::api_key_badge(self.model.transcript.account.as_ref()).map(
                        |(label, hint)| {
                            measured(
                                SLOT_BILLING,
                                div()
                                    .relative()
                                    .flex_none()
                                    .child(
                                        div()
                                            .id("chat-api-key")
                                            .child(widgets::badge(
                                                "chat-api-key-symbol",
                                                &cards::Badge {
                                                    label: label.into(),
                                                    tone: cards::Tone::Warning,
                                                    live: false,
                                                },
                                                look,
                                            ))
                                            .test_support(),
                                    )
                                    .child(tooltip::anchor(hint, TipLook::Control)),
                            )
                        },
                    ),
                )
                .children(usage),
        );
        let thread = thread.map(|thread| measured(SLOT_THREAD, thread.flex_none()));
        let more = measured(SLOT_MORE, more);

        let bar = div()
            .debug_selector(|| "chat-header".into())
            .w_full()
            .px(ui_text::space(BAR_INSET))
            .py(ui_text::space(5.0))
            .border_b_1()
            .border_color(rgb(colors.divider))
            .bg(rgb(colors.panel));
        if one_line {
            bar.flex()
                .items_center()
                .gap(ui_text::space(BAR_GAP))
                .child(controls.flex_none())
                .child(div().flex_1())
                .children(usage)
                .children(thread)
                .child(more)
                .into_any_element()
        } else {
            bar.flex()
                .flex_col()
                .gap(ui_text::space(BAR_GAP))
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .gap(ui_text::space(BAR_GAP))
                        .child(controls.flex_initial().min_w_0().overflow_hidden())
                        .child(div().flex_1())
                        .child(more),
                )
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .gap(ui_text::space(BAR_GAP))
                        .children(usage)
                        .child(div().flex_1())
                        .children(thread),
                )
                .into_any_element()
        }
    }

    /// The ⋯ menu's popover alone, for timing its build.
    #[cfg(test)]
    pub(super) fn menu_popover_for_profile(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let look = Look::of(cx);
        self.menu_popover(Menu::More, &[], true, look, window, cx)
    }

    /// The popover under a toolbar button, or under ⋯ (`from_more`) with the controls the
    /// header `folded` into it leading its menu.
    fn menu_popover(
        &self,
        menu: Menu,
        folded: &[Fold],
        from_more: bool,
        look: Look,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = look.colors;
        let info = self.model.transcript.info.as_ref();
        // Native's rows are its menus' rows: rounded fills inside a raised panel, the tick
        // drawn as its symbol.
        let row = |name: String, label: String, detail: Option<&str>, current: bool| {
            let row = div()
                .w_full()
                .flex()
                .items_start()
                .gap(ui_text::space(8.0))
                .px(ui_text::space(10.0))
                .py(ui_text::space(5.0))
                .text_size(ui_text::text(11.0))
                .text_color(rgb(colors.text))
                .cursor_pointer()
                .hover(move |style| {
                    controls::hovered(style, controls::menu_row_hover(colors), |style| {
                        style.bg(rgb(colors.panel_active))
                    })
                });
            let content = controls::native(row, |row| controls::menu_row(row, colors))
                .child(if look.native {
                    controls::on_first_line(
                        div()
                            .w(ui_text::space(12.0))
                            .text_color(rgb(colors.cyan))
                            .children(
                                current.then(|| icons::text_icon(Icon::Check, 11.0, colors.cyan)),
                            ),
                        widgets::BODY_LINE,
                    )
                    .into_any_element()
                } else {
                    div()
                        .flex_none()
                        .w(ui_text::space(10.0))
                        .text_color(rgb(colors.cyan))
                        .child(if current { "✓" } else { "" })
                        .into_any_element()
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(label.clone())
                        .children(detail.map(|detail| {
                            div()
                                .text_size(ui_text::text(10.0))
                                .text_color(rgb(colors.muted))
                                .child(detail.to_owned())
                        })),
                );
            widgets::content_button(id(name), label, content, look)
                .role(if menu == Menu::More {
                    gpui::Role::MenuItem
                } else {
                    gpui::Role::MenuItemRadio
                })
                .when(menu != Menu::More, |choice| {
                    choice.aria_toggled(if current {
                        gpui::accesskit::Toggled::True
                    } else {
                        gpui::accesskit::Toggled::False
                    })
                })
        };
        let content: Vec<AnyElement> = match menu {
            Menu::Mode => {
                let current = info.map(|info| info.approval_mode).unwrap_or_default();
                toolbar::MODES
                    .iter()
                    .map(|(mode, label, detail)| {
                        let mode: ApprovalMode = *mode;
                        row(
                            format!("mode-{label}"),
                            (*label).to_owned(),
                            Some(detail),
                            mode == current,
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.configure(None, None, Some(mode), None, cx);
                        }))
                        .into_any_element()
                    })
                    .collect()
            }
            Menu::Effort | Menu::Model => self.choice_menu(menu, look, window, cx),
            Menu::More => {
                // What the header folded in, with its value: a picker opens its own menu
                // here, Fast and Compact act at once.
                let models = &self.model.transcript.models;
                let model = info.and_then(|info| info.model.as_deref());
                let effort = info.and_then(|info| info.effort.as_deref());
                let fast = info.is_some_and(|info| info.fast);
                let idle = matches!(self.model.transcript.state, ChatState::Idle);
                let open = |menu: Menu| {
                    cx.listener(
                        move |view: &mut Self, event: &gpui::ClickEvent, window, cx| {
                            view.toggle_menu(menu, window, cx);
                            view.menu_armed = !event.is_keyboard();
                        },
                    )
                };
                let mut rows = folded
                    .iter()
                    .map(|fold| match fold {
                        Fold::Mode => row(
                            "more-mode".into(),
                            format!(
                                "Approval mode: {}",
                                toolbar::mode_label(
                                    info.map(|info| info.approval_mode).unwrap_or_default()
                                )
                            ),
                            None,
                            false,
                        )
                        .aria_expanded(false)
                        .on_click(open(Menu::Mode)),
                        Fold::Model => row(
                            "more-model".into(),
                            format!("Model: {}", toolbar::model_label(models, model)),
                            None,
                            false,
                        )
                        .aria_expanded(false)
                        .on_click(open(Menu::Model)),
                        Fold::Effort => row(
                            "more-effort".into(),
                            format!(
                                "Effort: {}",
                                widgets::sentence(
                                    &toolbar::effort_label(models, model, effort),
                                    look
                                )
                            ),
                            None,
                            false,
                        )
                        .aria_expanded(false)
                        .on_click(open(Menu::Effort)),
                        Fold::Fast => row(
                            "more-fast".into(),
                            "Fast mode".into(),
                            Some("Answers sooner and uses more of your limits"),
                            fast,
                        )
                        .role(gpui::Role::MenuItemCheckBox)
                        .aria_toggled(if fast {
                            gpui::accesskit::Toggled::True
                        } else {
                            gpui::accesskit::Toggled::False
                        })
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.menu = None;
                            view.toggle_fast(cx);
                            cx.notify();
                        })),
                        Fold::Compact => row(
                            "more-compact".into(),
                            "Compact".into(),
                            (!idle).then_some("Once the agent has finished"),
                            false,
                        )
                        .disabled(!idle)
                        .when(!idle, |row| row.text_color(rgb(colors.muted)))
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.menu = None;
                            view.compact(cx);
                            cx.notify();
                        })),
                    })
                    .map(IntoElement::into_any_element)
                    .collect::<Vec<_>>();
                if !rows.is_empty() {
                    rows.push(controls::menu_separator(colors));
                }
                rows.extend([
                    row(
                        "chat-notice-history".into(),
                        format!("Notices ({})", self.notices.count(&self.model.transcript)),
                        Some("Every notice of this chat, dismissed and resolved ones too"),
                        self.notices.history,
                    )
                    .on_click(cx.listener(|view, _, _, cx| view.toggle_notice_history(cx)))
                    .into_any_element(),
                    row(
                        "hand-off-chat".into(),
                        "Hand off…".into(),
                        Some("Continues this conversation in a new shell or chat"),
                        false,
                    )
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.menu = None;
                        view.hand_off(cx);
                        cx.notify();
                    }))
                    .into_any_element(),
                    row(
                        "stop-chat".into(),
                        "Stop chat".into(),
                        Some("Ends the agent's process; the next message resumes it"),
                        false,
                    )
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.menu = None;
                        view.stop_chat(cx);
                    }))
                    .into_any_element(),
                    row(
                        "delete-chat".into(),
                        "Delete chat…".into(),
                        Some("Removes the chat and its history"),
                        false,
                    )
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.menu = Some(Menu::ConfirmDelete);
                        cx.notify();
                    }))
                    .into_any_element(),
                ]);
                rows
            }
            Menu::ConfirmDelete => vec![
                div()
                    .px(ui_text::space(10.0))
                    .py(ui_text::space(6.0))
                    .text_size(ui_text::text(11.0))
                    .text_color(rgb(colors.gold))
                    .child("Delete this chat and its history?")
                    .into_any_element(),
                div()
                    .flex()
                    .gap(ui_text::space(6.0))
                    .px(ui_text::space(10.0))
                    .pb(ui_text::space(6.0))
                    .child(
                        // Native leads the group with it, as a confirmation leads with its
                        // action; the line above says what it does in the signal color.
                        button(
                            "confirm-delete",
                            "Delete",
                            Some(if look.native {
                                colors.cyan
                            } else {
                                look.diff.removed
                            }),
                            look,
                        )
                        .on_click(cx.listener(|view, _, _, cx| view.delete_chat(cx))),
                    )
                    .child(
                        button("cancel-delete", "Cancel", None, look).on_click(cx.listener(
                            |view, _, _, cx| {
                                view.menu = None;
                                cx.notify();
                            },
                        )),
                    )
                    .into_any_element(),
            ],
        };
        // Composer menus are positioned above their triggers by Base Popup. Header menus
        // keep their original placement. Both cap their content to the available pane.
        let in_composer = matches!(menu, Menu::Model | Menu::Effort) && !from_more;
        let toward_left = from_more;
        let available = (self.composer_width.get() - 32.0).max(1.0);
        let popover = div()
            .id("chat-choices-popover")
            .debug_selector(|| "composer-choices-menu".into())
            .when(!in_composer, |menu| {
                menu.absolute()
                    .top(relative(1.0))
                    .mt(ui_text::space(3.0))
                    .when(toward_left, |menu| menu.right(px(0.0)))
                    .when(!toward_left, |menu| menu.left(px(0.0)))
            })
            .w(ui_text::space(if in_composer { 300.0 } else { 340.0 }).min(px(available)))
            .max_h(ui_text::space(320.0).min(window.viewport_size().height * 0.65))
            .overflow_y_scroll()
            .track_scroll(&self.menu_scroll)
            .font_family(look.chat_family())
            .test_support();
        // The message box's menus are cut as the box is: its corners, a hairline in the
        // divider color (the colorful themes keep their accent edge), a soft shadow, and
        // their rows inset by the padding.
        let popover = if in_composer {
            popover
                .p(ui_text::space(CHOICE_MENU_PADDING))
                .rounded(look.card_radius())
                .border_1()
                .border_color(rgb(if look.native || look.hermes() {
                    colors.divider
                } else {
                    colors.magenta
                }))
                .bg(rgb(if look.native {
                    controls::raised(colors)
                } else if look.hermes() {
                    colors.panel
                } else {
                    colors.panel_active
                }))
                .shadow_lg()
        } else {
            controls::native(
                popover
                    .py(ui_text::space(4.0))
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(rgb(if look.hermes() {
                        colors.divider
                    } else {
                        colors.magenta
                    }))
                    .bg(rgb(if look.hermes() {
                        colors.panel
                    } else {
                        colors.panel_active
                    })),
                |menu| controls::menu(menu, colors),
            )
        };
        let content = popover
            .occlude()
            .role(gpui::Role::Menu)
            .aria_label("Chat choices")
            .on_mouse_down_out(cx.listener(|view, _, _, cx| view.close_menu(cx)))
            .children(content);
        if in_composer {
            content.into_any_element()
        } else {
            deferred(content).with_priority(10).into_any_element()
        }
    }

    // -----------------------------------------------------------------------------------
    // Above the message box
    // -----------------------------------------------------------------------------------

    /// What the user should know before typing: the chat is stopped or failed, or dictation
    /// stopped. Notices and the tab's errors are in `notice_stack`, inside the message box's bar.
    fn banner(&self, look: Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        let colors = look.colors;
        let inset = px(self.composer_layout(cx).inset);
        // Native leads the line with a symbol of what it says, in the line's color.
        let line = |text: String, color: u32| {
            let symbol = if color == colors.muted {
                "pause.circle"
            } else {
                "exclamationmark.triangle"
            };
            above_composer(div(), inset, look)
                .debug_selector(|| "chat-banner".into())
                .flex()
                .items_center()
                .gap(ui_text::space(8.0))
                .px(ui_text::space(12.0))
                .py(ui_text::space(5.0))
                .border_color(rgb(colors.divider))
                .bg(rgb(colors.panel))
                .text_size(ui_text::text(11.0))
                .text_color(rgb(color))
                .children(
                    look.native
                        .then(|| icons::symbol(symbol, 11.0, Some(color))),
                )
                .child(
                    div().flex_1().min_w_0().child(
                        // A long message scrolls here and leaves the buttons in reach.
                        div()
                            .id("chat-banner-text")
                            .max_h(ui_text::space(90.0))
                            .overflow_y_scroll()
                            .child(text),
                    ),
                )
        };
        // Why dictation stopped, with the way to System Settings when a permission is off.
        if let Phase::Failed(problem) = self.dictation.phase()
            && dictate::mic_shown(cx)
        {
            let settings = problem.settings_url();
            return Some(
                line(problem.message(), colors.gold)
                    .children(settings.map(|url| {
                        button(
                            "chat-dictation-settings",
                            "Open System Settings",
                            Some(colors.cyan),
                            look,
                        )
                        .on_click(cx.listener(
                            move |view, _, window, cx| {
                                cx.open_url(url);
                                view.dismiss_dictation(window, cx);
                            },
                        ))
                    }))
                    .child(
                        button("chat-dictation-dismiss", "Dismiss", None, look).on_click(
                            cx.listener(|view, _, window, cx| view.dismiss_dictation(window, cx)),
                        ),
                    )
                    .into_any_element(),
            );
        }
        match &self.model.transcript.state {
            ChatState::Stopped => Some(
                line(
                    "Stopped. The chat will resume when you send a message.".to_owned(),
                    colors.muted,
                )
                .into_any_element(),
            ),
            ChatState::Failed { message } => Some(
                line(format!("Failed: {message}"), colors.gold)
                    .child(
                        button("chat-retry", "Retry", Some(colors.cyan), look)
                            .on_click(cx.listener(|view, _, _, cx| view.resume(cx))),
                    )
                    .into_any_element(),
            ),
            _ => None,
        }
    }

    fn approval_bar(&self, look: Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        let colors = look.colors;
        let (pending, more) = self.pending_approval()?;
        let answered = self.answered.contains(&pending.request_id);
        let key = format!("approval:{}", pending.request_id);
        let expanded = self.open.contains(&key);
        let (lines, hidden) = approval::detail_lines(&pending.detail, expanded);
        let request = pending.request_id.clone();
        // A command is shown as the code it is.
        let command = pending.kind == ApprovalKind::Command;
        let inset = px(self.composer_layout(cx).inset);
        Some(
            above_composer(div(), inset, look)
                .flex()
                .flex_col()
                .gap(ui_text::space(6.0))
                .px(ui_text::space(12.0))
                .py(ui_text::space(8.0))
                // Native sets the request on the bar's grey under a hairline, and says it
                // waits with its heading alone, in the signal color.
                .when(look.native, |bar| {
                    bar.border_color(rgb(colors.divider))
                        .bg(rgb(colors.panel))
                })
                .when(!look.native, |bar| {
                    bar.border_color(rgb(colors.gold))
                        .bg(rgb(look.tint(colors.gold, 0.10)))
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(ui_text::space(8.0))
                        .text_size(ui_text::text(10.0))
                        .text_color(rgb(colors.gold))
                        .when(look.native, |heading| {
                            heading
                                .gap(ui_text::space(6.0))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(icons::symbol("hand.raised", 10.0, Some(colors.gold)))
                        })
                        .child(approval::kind_label(pending.kind))
                        .children((more > 0).then(|| {
                            div()
                                .text_color(rgb(colors.muted))
                                .child(format!("{more} more waiting"))
                        })),
                )
                .child(
                    div()
                        .max_h(ui_text::space(120.0))
                        .overflow_hidden()
                        .text_size(ui_text::text(11.0))
                        .text_color(rgb(colors.text))
                        .when(command, |title| title.font_family(ui_text::code_family()))
                        .child(pending.title.clone()),
                )
                .children((!lines.is_empty()).then(|| {
                    // The detail scrolls inside a cap, so the buttons below stay in reach.
                    div()
                        .id("approval-detail")
                        .max_h(ui_text::space(130.0))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .text_size(ui_text::text(10.0))
                        .text_color(rgb(colors.muted))
                        .children(lines.iter().map(|line| div().child((*line).to_owned())))
                }))
                .children((hidden > 0 || expanded).then(|| {
                    let key = key.clone();
                    crate::behavior_controls::button_content("approval-more", if expanded { "Show less approval detail".to_owned() } else { format!("Show {hidden} more approval lines") },
                            if expanded { widgets::sentence("show less", look) } else { widgets::sentence(&format!("show {hidden} more lines"), look) })
                        .aria_expanded(expanded)
                        .line_height(gpui::relative(1.618_034))
                        .cursor_pointer()
                        .text_size(ui_text::text(10.0))
                        .text_color(rgb(colors.cyan))
                        .on_click(cx.listener(move |view, _, _, cx| view.toggle(&key, None, cx)))
                }))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(ui_text::space(6.0))
                        .children(pending.choices.iter().map(|decision| {
                            let decision: Decision = *decision;
                            let label = match approval::decision_key(decision).filter(|_| decision != Decision::AcceptForSession) {
                                Some(key) => format!("{}  {key}", approval::decision_label(decision)),
                                None => approval::decision_label(decision).to_owned(),
                            };
                            let request = request.clone();
                            if answered {
                                return dimmed(
                                    id(format!("decide-{decision:?}")),
                                    label,
                                    look,
                                )
                                .into_any_element();
                            }
                            let accent = match decision {
                                Decision::Accept => Some(colors.cyan),
                                Decision::Decline | Decision::Cancel => Some(look.diff.removed),
                                Decision::AcceptForSession => None,
                            };
                            button(id(format!("decide-{decision:?}")), label, accent, look)
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.approve(request.clone(), decision, cx);
                                }))
                                .into_any_element()
                        })),
                )
                .children((!answered).then(|| {
                    div()
                        .text_size(ui_text::text(9.0))
                        .text_color(rgb(colors.muted))
                        .child("With the message box empty: ⏎ allows, ⎋ denies. ⇧⏎ and ⌥⏎ insert a new line; session approval uses its button.")
                }))
                .into_any_element(),
        )
    }

    fn question_panel(
        &self,
        look: Look,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let colors = look.colors;
        let question = self.pending_question()?.clone();
        let answered = self.answered.contains(&question.request_id);
        let draft = self.drafts.get(&question.request_id);
        let single = question.questions.len() == 1 && !question.questions[0].multi_select;
        let prompts = question
            .questions
            .iter()
            .enumerate()
            .map(|(prompt_at, prompt)| {
                let picked: &[usize] = draft
                    .and_then(|draft| draft.picked.get(prompt_at))
                    .map_or(&[], Vec::as_slice);
                div()
                    .flex()
                    .flex_col()
                    .gap(ui_text::space(4.0))
                    .children(prompt.header.clone().map(|header| {
                        // Native heads a prompt like a panel's section: semibold and muted.
                        div()
                            .text_size(ui_text::text(10.0))
                            .text_color(rgb(if look.native {
                                colors.muted
                            } else {
                                colors.cyan
                            }))
                            .when(look.native, |heading| {
                                heading.font_weight(gpui::FontWeight::SEMIBOLD)
                            })
                            .child(header)
                    }))
                    .child(
                        div()
                            .text_size(ui_text::text(11.0))
                            .child(self.ordinary_prose(
                                &format!(
                                    "question:{}:{prompt_at}:{}",
                                    question.request_id, prompt.question
                                ),
                                &prompt.question,
                                look,
                                cx,
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(ui_text::space(6.0))
                            .children(prompt.options.iter().enumerate().map(
                                |(option_at, option)| {
                                    let option_request = question.request_id.clone();
                                    let option_prompt = prompt.clone();
                                    let chosen = picked.contains(&option_at);
                                    // Native leads a chosen option with the tick symbol.
                                    let label = if chosen && !look.native {
                                        format!("✓ {}", option.label)
                                    } else {
                                        option.label.clone()
                                    };
                                    let tick = |option: crate::behavior_controls::Toggle| {
                                        option.when(chosen && look.native, |option| {
                                            option
                                                .flex_row_reverse()
                                                .pl(ui_text::space(9.0))
                                                .child(icons::symbol("checkmark", 9.0, None))
                                        })
                                    };
                                    tick(widgets::toggle_button_with_disabled(
                                        id(format!(
                                            "opt-{}-{prompt_at}-{}-{option_at}",
                                            question.request_id, prompt.question
                                        )),
                                        label,
                                        chosen.then_some(colors.cyan),
                                        chosen,
                                        answered,
                                        look,
                                    ))
                                    .accessibility_label(option.label.clone())
                                    .disabled(answered)
                                    .when(answered, |control| {
                                        if look.native {
                                            control
                                                .bg(rgb(colors.panel_active))
                                                .text_color(rgb(colors.muted))
                                        } else {
                                            control
                                                .bg(transparent_black())
                                                .text_color(rgb(colors.muted))
                                                .border_color(rgb(colors.divider))
                                        }
                                    })
                                    .on_change({
                                        let owner = cx.weak_entity();
                                        move |_, _, _, cx| {
                                            let _ = owner.update(cx, |view, cx| {
                                                view.pick(
                                                    &option_request,
                                                    &option_prompt,
                                                    prompt_at,
                                                    option_at,
                                                    cx,
                                                )
                                            });
                                        }
                                    })
                                    .into_any_element()
                                },
                            )),
                    )
                    .children(
                        prompt
                            .options
                            .iter()
                            .filter(|o| !o.description.is_empty())
                            .map(|option| {
                                div()
                                    .text_size(ui_text::text(10.0))
                                    .text_color(rgb(colors.muted))
                                    .child(format!("{}: {}", option.label, option.description))
                            }),
                    )
                    .children(
                        self.answers
                            .get(&super::editors::AnswerKey::of(&question, prompt_at))
                            .map(|editor| {
                                text_input::input(
                                    format!("chat-answer-{}", editor.state.entity_id().as_u64()),
                                    &editor.state,
                                    window,
                                    cx,
                                )
                            }),
                    )
            })
            .collect::<Vec<_>>();
        let inset = px(self.composer_layout(cx).inset);
        Some(
            above_composer(div().id("chat-questions"), inset, look)
                .max_h(relative(0.5))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(ui_text::space(8.0))
                .px(ui_text::space(12.0))
                .py(ui_text::space(8.0))
                .border_color(rgb(if look.native {
                    colors.divider
                } else {
                    colors.cyan
                }))
                .bg(rgb(colors.panel))
                .children(prompts)
                .children(self.answer_failures.get(&question.request_id).map(|failure| {
                    let command = &failure.snapshot.command;
                    let error = &failure.error;
                    let request = question.request_id.clone();
                    div().flex().flex_col().gap(ui_text::space(4.))
                        .child(div().text_color(rgb(colors.gold)).child(format!("Answer submission retained: {error}")))
                        .child(div().id("saved-question-answers").max_h(ui_text::space(120.)).overflow_y_scroll().child(match command {
                            crate::chat::model::ChatCommand::Answer { answers, .. } => format!("Saved answers: {answers:?}"),
                            _ => String::new(),
                        }))
                        .children(matches!(error, crate::chat::client::CallError::Broken(_)).then(|| div().child("Inspect the transcript before explicitly resending.")))
                        .child(button("retry-saved-answers", "Resend saved answers", Some(colors.cyan), look)
                            .on_click(cx.listener(move |view, _, _, cx| view.resend_answers(&request, cx))))
                }))
                .children((!single).then(|| {
                    div().flex().child(if answered {
                        dimmed("answers-sent", "Sent", look).into_any_element()
                    } else {
                        button("send-answers", "Send answers  ⏎", Some(colors.cyan), look)
                            .on_click(cx.listener(|view, _, _, cx| view.submit_answers(cx)))
                            .into_any_element()
                    })
                }))
                .into_any_element(),
        )
    }

    // -----------------------------------------------------------------------------------
    // The message box
    // -----------------------------------------------------------------------------------

    /// Same-provider choices stay with the retained chat and its authoritative model list.
    /// The model and effort pickers and Fast are one family of quiet controls as tall as the
    /// round buttons beside them (`side`), `gap` apart as every control of the box is.
    fn composer_choices(
        &self,
        look: Look,
        gap: gpui::Pixels,
        side: gpui::Pixels,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = look.colors;
        let info = self.model.transcript.info.as_ref();
        let models = &self.model.transcript.models;
        let model = info.and_then(|info| info.model.as_deref());
        let effort = info.and_then(|info| info.effort.as_deref());
        let fast = info.is_some_and(|info| info.fast);
        let provider = self.provider().unwrap_or(Provider::Codex);
        div()
            .min_w_0()
            .max_w(relative(1.0))
            .debug_selector(|| "composer-model-choices".into())
            .flex()
            .flex_wrap()
            .items_center()
            .justify_end()
            .gap(gap)
            .child(self.composer_picker(
                "chat-model",
                toolbar::model_label(models, model),
                Menu::Model,
                look,
                side,
                window,
                cx,
            ))
            .children(toolbar::effort_available(models, model, provider).then(|| {
                self.composer_picker(
                    "chat-effort",
                    toolbar::effort_label(models, model, effort),
                    Menu::Effort,
                    look,
                    side,
                    window,
                    cx,
                )
            }))
            .children(toolbar::fast_available(models, model).then(|| {
                let toggle = if look.native || look.hermes() {
                    // On, it takes the design's signal for a mode that is on: Native's working
                    // color, Hermes's cyan, as a tint under the label.
                    let on = if look.native {
                        colors.working
                    } else {
                        colors.cyan
                    };
                    let content = div()
                        .flex()
                        .items_center()
                        .gap(ui_text::space(4.0))
                        .child(icons::symbol(
                            if fast { "bolt.fill" } else { "bolt" },
                            10.0,
                            None,
                        ))
                        .child("Fast");
                    quiet_control(
                        crate::behavior_controls::toggle_content(
                            "chat-fast",
                            "Fast",
                            content,
                            fast,
                        ),
                        side,
                        false,
                        fast.then_some(on),
                        look,
                    )
                } else {
                    widgets::toggle_button(
                        "chat-fast",
                        toolbar::fast_label(fast),
                        fast.then_some(colors.cyan),
                        fast,
                        look,
                    )
                    .h(side)
                    .flex()
                    .items_center()
                };
                toggle
                    .debug_selector(|| "composer-fast".into())
                    .accessibility_label(toolbar::fast_label(fast))
                    .child(tooltip::anchor(
                        "Fast mode answers sooner and uses more of your limits",
                        TipLook::Control,
                    ))
                    .on_change({
                        let owner = cx.weak_entity();
                        move |_, _, _, cx| {
                            let _ = owner.update(cx, |view, cx| view.toggle_fast(cx));
                        }
                    })
            }))
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn composer_picker(
        &self,
        name: &'static str,
        label: String,
        menu: Menu,
        look: Look,
        side: gpui::Pixels,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.menu == Some(menu);
        let accessible = format!(
            "{}: {label}",
            if menu == Menu::Model {
                "Model"
            } else {
                "Reasoning effort"
            }
        );
        let trigger = quiet_control(
            crate::behavior_controls::button_content(
                name,
                accessible,
                div()
                    .min_w_0()
                    .max_w(ui_text::space(if menu == Menu::Model {
                        160.0
                    } else {
                        85.0
                    }))
                    .truncate()
                    .child(label),
            ),
            side,
            open,
            None,
            look,
        )
        .debug_selector(move || format!("composer-picker-{name}"))
        .min_w_0()
        .max_w(relative(1.0))
        .aria_expanded(open)
        // The header's pop-up chevron: Native's and Hermes's chevron.down at the label's
        // x-height, the colorful themes' ▾, muted and centered on the label, the control's
        // gap after it.
        .child(if look.native || look.hermes() {
            icons::symbol_in_box(
                "chevron.down",
                9.5,
                Some(look.colors.muted),
                ui_text::space(12.0),
            )
        } else {
            div()
                .flex_none()
                .text_color(rgb(look.colors.muted))
                .child("▾")
                .into_any_element()
        })
        .on_click(
            cx.listener(move |view, event: &gpui::ClickEvent, window, cx| {
                view.open.remove(ATTACHMENT_MENU_KEY);
                let opening = view.menu != Some(menu);
                if menu == Menu::Model {
                    view.sync_model_placeholder(window, cx);
                }
                view.toggle_menu(menu, window, cx);
                // The pointer holds no ⏎; a key that opened it must come up first.
                view.menu_armed = !event.is_keyboard();
                if opening
                    && view.menu == Some(Menu::Model)
                    && !view.model.transcript.models.is_empty()
                {
                    view.model_seeded = false;
                    view.model_input.read(cx).focus_handle(cx).focus(window, cx);
                }
            }),
        );
        crate::behavior_controls::popup(format!("{name}-popup"), trigger)
            .max_w(relative(1.0))
            // Above the box, its trailing edge on the picker's, so it never leaves the pane.
            .anchor(gpui::Anchor::BottomRight)
            .when(open, |popup| {
                popup.content(self.menu_popover(menu, &[], false, look, window, cx))
            })
            .into_any_element()
    }

    /// The model or effort menu's rows, in order: the efforts; or the chat's own provider's
    /// models (or names to type, for a driver that has not listed them), then the other
    /// provider's, which move the chat to it, then New chat.
    fn choice_groups(&self, menu: Menu, cx: &gpui::App) -> Vec<ChoiceGroup> {
        let info = self.model.transcript.info.as_ref();
        let models = &self.model.transcript.models;
        let model = info.and_then(|info| info.model.as_deref());
        let provider = self.provider();
        if menu == Menu::Effort {
            let effort = info.and_then(|info| info.effort.as_deref());
            let rows =
                toolbar::effort_rows(models, model, effort, provider.unwrap_or(Provider::Codex))
                    .into_iter()
                    .map(|line| ChoiceRow {
                        name: format!("effort-{}", line.effort),
                        label: line.label,
                        detail: None,
                        current: line.current,
                        choice: Some(MenuChoice::Effort(line.effort)),
                    })
                    .collect();
            return vec![ChoiceGroup {
                heading: None,
                note: None,
                rows,
            }];
        }
        let own = if models.is_empty() {
            // A driver that has not listed its models: type the name, or take a suggestion.
            provider
                .map(toolbar::model_suggestions)
                .unwrap_or_default()
                .iter()
                .map(|name| ChoiceRow {
                    name: format!("model-{name}"),
                    label: (*name).to_owned(),
                    detail: None,
                    current: model == Some(*name),
                    choice: Some(MenuChoice::Named((*name).to_owned())),
                })
                .collect()
        } else {
            let query = self.model_input.read(cx).value();
            toolbar::filtered_model_rows(models, model, &query)
                .into_iter()
                .map(|line| ChoiceRow {
                    name: format!("model-{}", line.id),
                    label: line.label,
                    detail: line.detail,
                    current: line.current,
                    choice: Some(MenuChoice::Model(line.id)),
                })
                .collect()
        };
        let Some(provider) = provider else {
            return vec![ChoiceGroup {
                heading: None,
                note: None,
                rows: own,
            }];
        };
        let mut groups = vec![ChoiceGroup {
            heading: Some(provider_name(provider).to_owned()),
            note: None,
            rows: own,
        }];
        // One chat, every provider's models: the other one's move the chat to it, once no
        // turn runs.
        let other = super::other_provider(provider);
        let busy = self.running();
        // Typing in the field names the chat's own provider's model; the field searches both
        // lists only when it is a search.
        let query = if models.is_empty() {
            String::new()
        } else {
            self.model_input.read(cx).value().trim().to_lowercase()
        };
        let mut rows: Vec<(String, String, Option<String>)> = vec![(
            String::new(),
            format!("{} default", provider_name(other)),
            None,
        )];
        if let Some(catalog) = self.other_models() {
            rows.extend(catalog.supported.iter().map(|model| {
                (
                    model.id.clone(),
                    model.name.clone(),
                    Some(model.description.clone()).filter(|text| !text.is_empty()),
                )
            }));
            rows.extend(catalog.configured.iter().map(|id| {
                (
                    id.clone(),
                    id.clone(),
                    Some("Previously configured".to_owned()),
                )
            }));
        }
        groups.push(ChoiceGroup {
            heading: Some(provider_name(other).to_owned()),
            note: Some(if busy {
                "Finish or interrupt the turn first".to_owned()
            } else {
                "Continues this conversation".to_owned()
            }),
            rows: rows
                .into_iter()
                .filter(|(id, label, more)| {
                    query.is_empty()
                        || id.is_empty()
                        || format!("{id} {label} {}", more.as_deref().unwrap_or(""))
                            .to_lowercase()
                            .contains(&query)
                })
                .map(|(id, label, detail)| ChoiceRow {
                    name: format!(
                        "switch-{}-{}",
                        provider_name(other).to_lowercase(),
                        if id.is_empty() { "default" } else { &id }
                    ),
                    label,
                    detail,
                    current: false,
                    choice: (!busy)
                        .then(|| MenuChoice::Switch(other, (!id.is_empty()).then_some(id))),
                })
                .collect(),
        });
        groups.push(ChoiceGroup {
            heading: None,
            note: None,
            rows: vec![ChoiceRow {
                name: "chat-new".into(),
                label: "New chat…".into(),
                detail: None,
                current: false,
                choice: Some(MenuChoice::NewChat),
            }],
        });
        groups
    }

    /// The rows that can be chosen now, in the menu's order: what ↑ and ↓ step through.
    fn menu_choices(&self, cx: &gpui::App) -> Vec<(String, MenuChoice)> {
        let Some(menu @ (Menu::Model | Menu::Effort)) = self.menu else {
            return Vec::new();
        };
        self.choice_groups(menu, cx)
            .into_iter()
            .flat_map(|group| group.rows)
            .filter_map(|row| row.choice.map(|choice| (row.name, choice)))
            .collect()
    }

    /// Do what a row of the model or effort menu does.
    fn pick_choice(&mut self, choice: MenuChoice, cx: &mut Context<Self>) {
        self.menu_cursor = None;
        match choice {
            MenuChoice::Effort(effort) => self.configure(None, Some(effort), None, None, cx),
            MenuChoice::Model(id) => self.choose_model(&id, cx),
            MenuChoice::Named(name) => self.configure(Some(name), None, None, None, cx),
            MenuChoice::Switch(provider, model) => self.switch_provider(provider, model, cx),
            MenuChoice::NewChat => {
                self.close_menu(cx);
                // The new-chat chooser owns focus after this request.
                self.focus_composer = false;
                cx.emit(ChatViewEvent::NewChat);
            }
        }
    }

    /// ↑ or ↓ while the model or effort menu is open: the highlight moves to the row before
    /// or after it, from the last or first row when none is highlighted, and the menu
    /// scrolls to keep it in sight. Anywhere else the key does what it does.
    pub(super) fn step_menu(&mut self, down: bool, cx: &mut Context<Self>) -> bool {
        let choices = self.menu_choices(cx);
        if choices.is_empty() {
            return false;
        }
        let at = self
            .menu_cursor
            .as_ref()
            .and_then(|name| choices.iter().position(|(row, _)| row == name));
        let next = match (at, down) {
            (None, true) => 0,
            (None, false) => choices.len() - 1,
            (Some(at), true) => (at + 1).min(choices.len() - 1),
            (Some(at), false) => at.saturating_sub(1),
        };
        let name = choices[next].0.clone();
        if let Some(child) = self
            .menu_children
            .borrow()
            .iter()
            .position(|row| *row == name)
        {
            self.menu_scroll.scroll_to_item(child);
        }
        self.menu_cursor = Some(name);
        cx.notify();
        true
    }

    /// ⏎ while the model or effort menu is open: the highlighted row is chosen, or, while
    /// the model field searches, the chat's own provider's first match (never a row that
    /// moves the chat to the other provider). Otherwise ⏎ does what it does.
    pub(super) fn choose_highlighted(&mut self, cx: &mut Context<Self>) -> bool {
        let searching = self.menu == Some(Menu::Model)
            && !self.model.transcript.models.is_empty()
            && !self.model_input.read(cx).value().trim().is_empty();
        let chosen = match &self.menu_cursor {
            Some(name) => self
                .menu_choices(cx)
                .into_iter()
                .find(|(row, _)| row == name),
            None if searching => self
                .choice_groups(Menu::Model, cx)
                .into_iter()
                .next()
                .and_then(|own| own.rows.into_iter().next())
                .and_then(|row| row.choice.map(|choice| (row.name, choice))),
            None => None,
        };
        match chosen {
            Some((_, choice)) => {
                self.pick_choice(choice, cx);
                true
            }
            None => false,
        }
    }

    /// The model or effort menu: rows of one line each, the current one ticked, the one
    /// under the pointer or the keyboard highlight filled with the design's active color,
    /// a muted heading per provider.
    fn choice_menu(
        &self,
        menu: Menu,
        look: Look,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let colors = look.colors;
        let highlight = if look.native {
            controls::menu_row_hover(colors)
        } else {
            colors.panel_active
        };
        let corner = (f32::from(look.card_radius())
            - f32::from(ui_text::space(CHOICE_MENU_PADDING)))
        .max(3.0);
        let mut content = Vec::new();
        // Which child each row is, for scrolling the keyboard highlight into sight.
        let mut children = Vec::new();
        if menu == Menu::Model {
            let listed = !self.model.transcript.models.is_empty();
            content.push(
                div()
                    .pb(ui_text::space(CHOICE_MENU_PADDING))
                    .child(if listed {
                        text_input::input("chat-model-search", &self.model_input, window, cx)
                            .font_family(look.chat_family())
                            .accessibility_label("Search models")
                    } else {
                        text_input::input("chat-model-input", &self.model_input, window, cx)
                            .font_family(look.chat_family())
                    })
                    .into_any_element(),
            );
            children.push(String::new());
        }
        let heading = |text: String, note: Option<String>| {
            div()
                .flex()
                .items_baseline()
                .gap(ui_text::space(6.0))
                .px(ui_text::space(8.0))
                .pt(ui_text::space(4.0))
                .pb(ui_text::space(2.0))
                .text_size(ui_text::text(10.0))
                .text_color(rgb(colors.muted))
                .child(
                    div()
                        .flex_none()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(text),
                )
                .children(note.map(|note| div().min_w_0().truncate().child(note)))
                .into_any_element()
        };
        for (at, group) in self.choice_groups(menu, cx).into_iter().enumerate() {
            if at > 0 {
                content.push(controls::menu_separator(colors));
                children.push(String::new());
            }
            if let Some(text) = group.heading {
                content.push(heading(text, group.note));
                children.push(String::new());
            }
            if at == 0 && menu == Menu::Model && group.rows.is_empty() {
                content.push(
                    div()
                        .px(ui_text::space(8.0))
                        .h(ui_text::space(CHOICE_ROW))
                        .flex()
                        .items_center()
                        .text_size(ui_text::text(11.0))
                        .text_color(rgb(colors.muted))
                        .child("No matching models")
                        .into_any_element(),
                );
                children.push(String::new());
            }
            for row in group.rows {
                let highlighted = self.menu_cursor.as_deref() == Some(row.name.as_str());
                let enabled = row.choice.is_some();
                let label = if menu == Menu::Effort {
                    widgets::sentence(&row.label, look)
                } else {
                    row.label.clone()
                };
                let check = if look.native || look.hermes() {
                    div().children(
                        row.current
                            .then(|| icons::text_icon(Icon::Check, 11.0, colors.cyan)),
                    )
                } else {
                    div()
                        .text_color(rgb(colors.cyan))
                        .child(if row.current { "✓" } else { "" })
                };
                let line = div()
                    .w_full()
                    .h(ui_text::space(CHOICE_ROW))
                    .flex()
                    .items_center()
                    .gap(ui_text::space(6.0))
                    .px(ui_text::space(8.0))
                    .rounded(px(corner))
                    .text_size(ui_text::text(11.0))
                    .text_color(rgb(colors.text))
                    .when(highlighted && enabled, |line| line.bg(rgb(highlight)))
                    .when(enabled, |line| {
                        line.cursor_pointer()
                            .hover(move |style| style.bg(rgb(highlight)))
                    })
                    .when(!enabled, |line| line.opacity(0.5))
                    .child(
                        check
                            .flex_none()
                            .w(ui_text::space(12.0))
                            .flex()
                            .items_center()
                            .justify_center(),
                    )
                    .child(
                        div()
                            .flex_none()
                            .max_w(relative(0.7))
                            .truncate()
                            .child(label.clone()),
                    )
                    .children(row.detail.map(|detail| {
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(ui_text::text(10.0))
                            .text_color(rgb(colors.muted))
                            .child(detail)
                    }));
                let name = row.name.clone();
                let item = widgets::content_button(id(row.name.clone()), label, line, look)
                    .w_full()
                    .debug_selector(move || format!("choice-{name}"))
                    .role(if row.choice == Some(MenuChoice::NewChat) {
                        gpui::Role::MenuItem
                    } else {
                        gpui::Role::MenuItemRadio
                    })
                    .when(row.choice != Some(MenuChoice::NewChat), |item| {
                        item.aria_toggled(if row.current {
                            gpui::accesskit::Toggled::True
                        } else {
                            gpui::accesskit::Toggled::False
                        })
                    })
                    .disabled(!enabled)
                    .on_hover(cx.listener({
                        let name = row.name.clone();
                        move |view, hovered: &bool, _, cx| {
                            if *hovered && enabled && view.menu_cursor.as_ref() != Some(&name) {
                                view.menu_cursor = Some(name.clone());
                                cx.notify();
                            }
                        }
                    }));
                let item = match row.choice {
                    Some(choice) => item
                        .on_click(
                            cx.listener(move |view, _, _, cx| view.pick_choice(choice.clone(), cx)),
                        )
                        .into_any_element(),
                    None => item.into_any_element(),
                };
                content.push(item);
                children.push(row.name);
            }
        }
        *self.menu_children.borrow_mut() = children;
        content
    }

    /// The card's layout for the pane as last drawn (see `composer::layout`) and the slots
    /// the action row keeps: the mic when shown, and one that Send and Stop share.
    fn composer_layout(&self, cx: &gpui::App) -> composer::Layout {
        let actions = 1 + usize::from(dictate::mic_shown(cx));
        composer::layout(self.composer_width.get(), ui_text::scale(), actions)
    }

    /// The + menu: only actions backed by existing attachment staging. Its trigger is drawn
    /// as the design draws the message box's other buttons.
    fn composer_attachment_menu(
        &self,
        look: Look,
        side: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.open.contains(ATTACHMENT_MENU_KEY);
        let trigger_bounds = std::rc::Rc::new(std::cell::Cell::new(gpui::Bounds::default()));
        let positioned = trigger_bounds.clone();
        let tip = "Attach files or paste an image · UTF-8 text, PNG or JPEG · or drop them";
        let trigger = if look.native || look.hermes() {
            pressable(
                widgets::symbol_button_sized("chat-attach", "plus", tip, look, side),
                look,
            )
        } else {
            mark_button("chat-attach", "+", tip, None, side, look)
        }
        .aria_expanded(open)
        .on_click(cx.listener(|view, _, _, cx| {
            view.menu = None;
            if !view.open.remove(ATTACHMENT_MENU_KEY) {
                view.open.insert(ATTACHMENT_MENU_KEY.to_owned());
            }
            cx.notify();
        }));
        crate::behavior_controls::popup("chat-attachment-popup", trigger)
            .anchor(gpui::Anchor::BottomLeft)
            .on_position(move |_, bounds| positioned.set(bounds))
            .on_key_down(cx.listener(move |view, event: &gpui::KeyDownEvent, window, cx| {
                if open && event.keystroke.key == "escape" {
                    view.open.remove(ATTACHMENT_MENU_KEY);
                    view.focus(window, cx);
                    window.prevent_default();
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .when(open, |popup| {
                let width = (self.composer_width.get() - 32.0).max(1.0);
                popup.content(
                    div()
                        .id("chat-attachment-menu")
                        .debug_selector(|| "composer-attachment-menu".into())
                        .role(gpui::Role::Menu)
                        .aria_label("Attach")
                        .w(ui_text::space(260.0).min(px(width)))
                        .flex()
                        .flex_col()
                        .p(ui_text::space(5.0))
                        .rounded(ui_text::space(4.0))
                        .border_1()
                        .border_color(rgb(look.colors.divider))
                        .bg(rgb(look.colors.panel))
                        .font_family(look.chat_family())
                        .on_mouse_down_out(cx.listener(move |view, event: &gpui::MouseDownEvent, _, cx| {
                            if trigger_bounds.get().contains(&event.position) {
                                return;
                            }
                            view.open.remove(ATTACHMENT_MENU_KEY);
                            cx.notify();
                        }))
                        .child(div().px(ui_text::space(8.0)).py(ui_text::space(4.0))
                            .text_size(ui_text::text(10.0)).text_color(rgb(look.colors.muted)).child(ui_text::cased("Attach")))
                        .child(button("chat-attach-files", "Files and images…", None, look)
                            .role(gpui::Role::MenuItem)
                            .accessibility_label("Attach UTF-8 text, PNG or JPEG files")
                            .border_color(transparent_black())
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.open.remove(ATTACHMENT_MENU_KEY);
                                view.attach_picker(window, cx);
                                cx.notify();
                            })))
                        .child(button("chat-attach-paste", "Paste image", None, look)
                            .role(gpui::Role::MenuItem)
                            .border_color(transparent_black())
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.open.remove(ATTACHMENT_MENU_KEY);
                                match super::attachment_ui::read_composer_clipboard(cx) {
                                    Ok(Some(item)) => {
                                        if !view.paste_attachments(&item, window, cx) {
                                            view.notices.set(super::notices::LocalKey::Attachment, crate::chat::model::NoticeLevel::Error, "The clipboard has no supported image or file attachment.");
                                        }
                                    }
                                    Ok(None) => view.notices.set(super::notices::LocalKey::Attachment, crate::chat::model::NoticeLevel::Error, "The clipboard has no image or file attachment."),
                                    Err(error) => view.notices.set(super::notices::LocalKey::Attachment, crate::chat::model::NoticeLevel::Error, format!("Clipboard attachment: {error}")),
                                }
                                view.focus(window, cx);
                                cx.notify();
                            }))),
                )
            })
            .into_any_element()
    }

    /// Send, drawn as the design draws a message field's send button: Native's filled round
    /// symbol, Hermes's filled disc, the colorful themes' bordered mark. It waits until there
    /// is something to send, taking no click or key meanwhile.
    fn composer_send(&self, look: Look, side: gpui::Pixels, cx: &mut Context<Self>) -> AnyElement {
        let colors = look.colors;
        let empty = self.draft_empty(cx) || self.pending_submission.is_some();
        // While a turn runs Stop holds this slot, and its tooltip says ⏎ steers the turn.
        let tip = "Send · ⏎ · ⇧⏎ new line";
        let send = if look.native {
            widgets::round_button_sized(
                "chat-send",
                "arrow.up",
                tip,
                if empty {
                    Button::Disabled
                } else {
                    Button::Primary
                },
                look,
                side,
            )
        } else if look.hermes() {
            crate::behavior_controls::button_content(
                "chat-send",
                tip,
                icons::symbol_in_box("arrow.up", 14.0, None, (side - px(4.0)).max(px(1.0))),
            )
            .disabled(empty)
            .size(side)
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(rgb(colors.text))
            .text_color(rgb(colors.bg))
            .border_1()
            .border_color(transparent_black())
            .focus_visible(move |style| style.border_color(rgb(colors.focus)))
            .when(!empty, |button| {
                button
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgb(colors.cyan)))
            })
            .child(tooltip::anchor(tip, TipLook::Control))
        } else {
            mark_button("chat-send", "↑", tip, Some(colors.cyan), side, look).disabled(empty)
        };
        send.on_click(cx.listener(|view, _, window, cx| view.send_message(window, cx)))
            .into_any_element()
    }

    /// Stop, in Send's slot while a turn runs: Native and Hermes
    /// draw it as the box's other quiet round buttons, its symbol and a tint in the warning
    /// color; the colorful themes their mark in the terminal's red.
    fn composer_stop(&self, look: Look, side: gpui::Pixels, cx: &mut Context<Self>) -> AnyElement {
        let tip = "Interrupt · ⌘. · ⏎ steers the turn";
        if look.native || look.hermes() {
            let warning = look.tone(cards::Tone::Warning);
            let colors = look.colors;
            pressable(
                crate::behavior_controls::button_content(
                    "chat-interrupt",
                    tip,
                    icons::symbol_in_box(
                        "stop.fill",
                        controls::TOOLBAR_SYMBOL,
                        None,
                        (side - px(4.0)).max(px(1.0)),
                    ),
                ),
                look,
            )
            .line_height(relative(1.618_034))
            .flex_none()
            .size(side)
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .border_1()
            .border_color(transparent_black())
            .text_color(rgb(warning))
            .bg(rgb(look.tint(warning, 0.18)))
            .cursor_pointer()
            .hover(move |style| style.bg(rgb(look.tint(warning, 0.3))))
            .focus_visible(move |style| style.border_color(rgb(colors.focus)))
            .child(tooltip::anchor(tip, TipLook::Control))
        } else {
            mark_button(
                "chat-interrupt",
                "■",
                tip,
                Some(look.diff.removed),
                side,
                look,
            )
        }
        .on_click(cx.listener(|view, _, _, cx| view.interrupt(cx)))
        .into_any_element()
    }

    /// The message box: one card in every design, inset from the pane's sides and bottom
    /// (see `composer::layout`). In a wide pane the box shares one row with Attach and the
    /// controls (the model choices, then the mic and the Send/Stop slot), which sit on its
    /// last line (the box is never less than two lines tall, so they start on its second); otherwise the box takes the card's whole width and Attach and the controls
    /// wrap in a row under it, and a narrow pane gives the controls a row of their own. The
    /// arrangement follows the pane alone, so the controls never move while the user types. Every design lays it out
    /// the same; each draws the card and its buttons its own way (`composer_card`,
    /// `composer_send`, `composer_stop`).
    fn composer_box(&self, look: Look, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = look.colors;
        let layout = self.composer_layout(cx);
        let gap = px(layout.gap);
        let side = px(layout.button);
        let mic = dictate::mic_shown(cx);
        // One line of the box: its text's line box, its padding and its hairline. Beside the
        // box the controls are lifted to sit centered on its last line, however many lines
        // the draft has.
        let line = ui_text::space(widgets::FIELD_PAD_Y) * 2. + widgets::field_line() + px(2.);
        let lift = ((line - side) / 2.).max(px(0.));
        let attach = div()
            .flex_none()
            .debug_selector(|| "composer-attach".into())
            .child(self.composer_attachment_menu(look, side, cx));
        let field = div()
            .flex_1()
            .min_w_0()
            .debug_selector(|| "composer-field".into())
            .child(
                self.composer_editor(look, layout.compact, window, cx)
                    .test_support(),
            );
        // Send and Stop share one slot, so neither moves the controls: Stop while a turn runs
        // (⏎ still steers it with a draft), Send otherwise.
        let action = if self.running() {
            div()
                .debug_selector(|| "composer-stop".into())
                .child(self.composer_stop(look, side, cx))
        } else {
            div()
                .debug_selector(|| "composer-send".into())
                .child(self.composer_send(look, side, cx))
        };
        let actions = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(gap)
            .children(mic.then(|| {
                div()
                    .debug_selector(|| "composer-mic".into())
                    .child(self.mic_button(look, side, cx))
            }))
            .child(
                div()
                    .debug_selector(|| "composer-action".into())
                    .flex_none()
                    .size(side)
                    .child(action),
            );
        let controls = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_end()
            .gap(gap)
            .child(
                div()
                    .max_w(relative(1.0))
                    .min_w_0()
                    .child(self.composer_choices(look, gap, side, window, cx)),
            )
            .child(actions);
        let content = if layout.compact {
            div()
                .w_full()
                .flex()
                .items_end()
                .gap(gap)
                .child(attach.mb(lift))
                .child(field)
                .child(controls.flex_none().mb(lift))
        } else {
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap(gap)
                .child(div().w_full().flex().child(field))
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(gap)
                        .child(attach)
                        .child(
                            controls.when(layout.narrow, |controls| controls.flex_none().w_full()),
                        ),
                )
        };
        // Where the text starts in the card's content: one gap after Attach beside it, the
        // field's inset over the control row.
        let text_lead = if layout.compact {
            layout.button + layout.gap
        } else {
            f32::from(ui_text::space(widgets::FIELD_PAD_Y))
        };
        let active = self.composer.read(cx).focus_handle(cx).is_focused(window);
        let card = composer_card(
            div()
                .id("chat-composer-bar")
                .debug_selector(|| "composer-card".into())
                .w_full()
                .flex()
                .flex_col()
                .gap(gap)
                .p(px(layout.padding))
                .font_family(look.chat_family()),
            look,
            active,
        )
        // The draft's attachments above the box, a staged image as its thumbnail.
        .children((!self.attachments.is_empty()).then(|| {
            div()
                .id("chat-attachment-queue")
                .role(gpui::Role::Group)
                .aria_label("Draft attachments")
                .max_h(ui_text::space(280.0))
                .flex_none()
                .overflow_y_scroll()
                // The cards start where the text does.
                .pl(px(text_lead))
                .pr(ui_text::space(widgets::FIELD_PAD_Y))
                .child(
                    self.attachment_chips(
                        look,
                        Some(
                            self.composer_width.get()
                                - 2. * (layout.inset + layout.padding)
                                - text_lead
                                - f32::from(ui_text::space(widgets::FIELD_PAD_Y)),
                        )
                        .filter(|_| self.composer_width.get() > 0.),
                        window,
                        cx,
                    ),
                )
                .test_support()
        }))
        .child(content)
        // Saved submissions to review, below the box.
        .children((!self.submissions.is_empty()).then(|| {
            div()
                .id("chat-submission-reviews")
                .max_h(ui_text::space(280.0))
                .overflow_y_scroll()
                .child(self.submission_cards(look, cx))
        }))
        .children(composer::status(self.dictation.phase(), mic).map(|status| {
            div()
                .text_size(ui_text::text(10.0))
                .text_color(rgb(colors.text))
                .child(status)
        }));
        // The pane's width as laid out, for the next frame's `layout`. It does not depend on
        // the layout, so it settles after one redraw.
        let measured = self.composer_width.clone();
        div()
            .id("chat-composer-pane")
            .flex()
            .flex_col()
            .gap(gap)
            .relative()
            .w_full()
            .debug_selector(|| "composer-pane".into())
            .p(px(layout.inset))
            // A drop target while files are dragged over the card or the space around it.
            .drag_over::<gpui::ExternalPaths>(move |style, _, _, _| {
                style.bg(rgb(look.tint(colors.focus, 0.12)))
            })
            .on_drop(
                cx.listener(|view, paths: &gpui::ExternalPaths, window, cx| {
                    view.dropped_attachments(paths, window, cx)
                }),
            )
            .child(
                canvas(
                    move |bounds, window, _| {
                        let width = f32::from(bounds.size.width);
                        if (measured.get() - width).abs() >= 0.5 {
                            measured.set(width);
                            window.refresh();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .w_full()
                .h(px(0.0)),
            )
            .children(self.notice_stack(look, cx))
            .child(card)
            .into_any_element()
    }

    /// The mic beside Send: click to dictate, click again to stop; ⌃⌥D does the same. Native
    /// draws it as a bare round symbol button, as the paperclip: a mic, filled in the working
    /// color while it listens and pulsing while it gets ready or settles. The colorful themes
    /// draw a ring, filled while it dictates.
    fn mic_button(&self, look: Look, side: gpui::Pixels, cx: &mut Context<Self>) -> AnyElement {
        let colors = look.colors;
        let phase = self.dictation.phase();
        let key = dictation::SHORTCUT_LABEL;
        let tooltip = match phase {
            Phase::Listening { .. } => format!("Stop dictating · {key} · ⎋ cancels"),
            Phase::Preparing { note: Some(note) } => note.clone(),
            Phase::Preparing { .. } | Phase::Finishing { .. } => "Stop dictating".to_owned(),
            Phase::Failed(_) => format!("Dictate again · {key}"),
            Phase::Idle => format!("Dictate · {key}"),
        };
        let busy = matches!(phase, Phase::Preparing { .. } | Phase::Finishing { .. });
        let listening = matches!(phase, Phase::Listening { .. });
        let mic = if look.native || look.hermes() {
            let symbol = match phase {
                Phase::Listening { .. } | Phase::Preparing { .. } | Phase::Finishing { .. } => {
                    "mic.fill"
                }
                Phase::Failed(_) => "mic.slash",
                Phase::Idle => "mic",
            };
            // Quiet at rest, as the paperclip: a bare symbol with a fill under the pointer.
            widgets::symbol_toggle_sized(
                "chat-dictate",
                symbol,
                tooltip,
                phase.is_active(),
                look,
                side,
            )
            .when(phase.is_active(), |mic| mic.text_color(rgb(colors.working)))
            .map(|mic| pressable(mic, look))
        } else {
            mark_toggle(
                "chat-dictate",
                if phase.is_active() { "●" } else { "○" },
                tooltip,
                phase.is_active().then_some(colors.working),
                phase.is_active(),
                side,
                look,
            )
        }
        .when(listening, |mic| {
            mic.bg(rgb(look.tint(colors.working, 0.18)))
        })
        .on_change({
            let owner = cx.weak_entity();
            move |_, _, window, cx| {
                let _ = owner.update(cx, |view, cx| view.toggle_dictation(window, cx));
            }
        });
        if busy {
            mic.with_animation(
                "chat-dictate-busy",
                Animation::new(Duration::from_millis(1000))
                    .repeat()
                    .with_easing(pulsating_between(0.35, 1.0))
                    .with_max_fps(20.0),
                |mic, level| mic.opacity(level),
            )
            .into_any_element()
        } else {
            mic.into_any_element()
        }
    }

    /// Kit's transparent textarea: the card around it (`composer_card`) draws its edge.
    fn composer_editor(
        &self,
        look: Look,
        inline: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Stateful<gpui::Div> {
        let weak = cx.weak_entity();
        let editor = text_input::on_paste_with_reader(
            text_input::textarea("chat-composer", &self.composer, window, cx),
            &self.composer,
            super::attachment_ui::read_composer_clipboard,
            move |item, window, cx| {
                weak.update(cx, |view, cx| match item {
                    Ok(item) => view.paste_attachments(item, window, cx),
                    Err(error) => {
                        view.notices.set(
                            super::notices::LocalKey::Attachment,
                            crate::chat::model::NoticeLevel::Error,
                            format!("Clipboard attachment: {error}"),
                        );
                        cx.notify();
                        true
                    }
                })
                .unwrap_or(false)
            },
        )
        .accessibility_label("Chat message")
        .border_0()
        .bg(transparent_black())
        .rounded(px(0.))
        .styles(|styles| {
            styles.focused(|style| {
                style
                    .border_color(transparent_black())
                    .bg(transparent_black())
            })
        })
        // Beside Attach (`inline`) the text starts one gap after it, as every control of the
        // box is one gap from the next; over the control row it is inset as far from the
        // card's edge as from its top.
        .py(ui_text::space(widgets::FIELD_PAD_Y))
        .px(if inline {
            px(0.0)
        } else {
            ui_text::space(widgets::FIELD_PAD_Y)
        })
        .text_size(ui_text::text(widgets::FIELD_TEXT))
        .line_height(widgets::field_line())
        .font_family(look.chat_family())
        .capture_action(cx.listener(Self::capture_enter));
        // Never less than two lines: the text (and the placeholder) starts on the first, the
        // controls beside the box sit on the second, and nothing moves when the first line
        // wraps. From three lines up the box grows, the controls staying on its last line. A
        // click below a short draft puts the cursor in it, as a click in the text does.
        let composer = self.composer.clone();
        div()
            .id("chat-composer-shell")
            .relative()
            .w_full()
            .min_h(ui_text::space(widgets::FIELD_PAD_Y) * 2. + widgets::field_line() * 2. + px(2.))
            .cursor_text()
            .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                composer.read(cx).focus_handle(cx).focus(window, cx);
            })
            .border_y_1()
            .bg(transparent_black())
            .border_color(transparent_black())
            .child(editor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::{QuestionOption, QuestionPrompt};
    use gpui_kit::test::TestWindowExt;

    fn hermes_fixture(
        cx: &mut gpui::TestAppContext,
        width: f32,
    ) -> (
        gpui::WindowHandle<gpui_kit::base::Root>,
        gpui::Entity<ChatView>,
    ) {
        design_fixture(cx, width, theme::ThemeChoice::Hermes)
    }

    /// Recording-only fixture in `design`, at the scale the test set: no provider, host
    /// staging, clipboard or socket access.
    fn design_fixture(
        cx: &mut gpui::TestAppContext,
        width: f32,
        design: theme::ThemeChoice,
    ) -> (
        gpui::WindowHandle<gpui_kit::base::Root>,
        gpui::Entity<ChatView>,
    ) {
        cx.update(|cx| {
            let mut settings = crate::settings::Settings {
                theme: design,
                ui_text_matches_terminal: false,
                dictation_mic: true,
                ..Default::default()
            };
            settings.ui_text_size = ui_text::TextPoints::new(
                ui_text::scale() * ui_text::REFERENCE_SIZE
                    - ui_text::Face::of(&settings).offset(false),
            );
            cx.set_global(settings);
            cx.set_global(theme::Appearance {
                selected: design,
                palette: match design {
                    theme::ThemeChoice::Native => theme::Palette::native(false),
                    theme::ThemeChoice::RiWork => theme::Palette::RIWORK,
                    theme::ThemeChoice::Hermes => theme::Palette::HERMES,
                    _ => panic!("the chat layout fixture draws Native, RiWork and Hermes"),
                },
                terminal: None,
                ghostty: None,
                error: None,
            });
            text_input::init(cx);
            ui_text::init(cx);
            let (feed, _) = super::super::feed::Feed::recording();
            let mut chat = None;
            let handle = cx
                .open_window(
                    gpui::WindowOptions {
                        window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds::new(
                            point(px(0.0), px(0.0)),
                            gpui::size(px(width), px(900.0)),
                        ))),
                        ..Default::default()
                    },
                    |window, cx| {
                        let view = cx.new(|cx| {
                            let mut view = ChatView::blank(
                                super::super::HostConfig {
                                    ensure: std::sync::Arc::new(|| {
                                        panic!("the fixture cannot stage or launch a provider")
                                    }),
                                },
                                window,
                                cx,
                            );
                            view.chat_id = Some("layout-fixture".into());
                            view.feed = Some(feed);
                            view.model.transcript.state = ChatState::Running;
                            view.model.transcript.models = vec![crate::chat::model::ModelOption {
                                id: "fixture-model".into(),
                                name: "A deliberately long supported model label for wrapping"
                                    .into(),
                                efforts: vec!["medium".into(), "high".into()],
                                default_effort: Some("medium".into()),
                                supports_fast: true,
                                is_default: true,
                                ..Default::default()
                            }];
                            view
                        });
                        chat = Some(view.clone());
                        cx.new(|cx| gpui_kit::base::Root::new(view, window, cx))
                    },
                )
                .unwrap();
            (handle, chat.unwrap())
        })
    }

    fn draw_hermes(
        cx: &mut gpui::TestAppContext,
        handle: gpui::WindowHandle<gpui_kit::base::Root>,
    ) {
        for _ in 0..3 {
            cx.update_window(handle.into(), |_, window, cx| window.render_frame(cx))
                .unwrap();
            cx.run_until_parked();
        }
    }

    #[gpui::test]
    fn the_model_menu_offers_the_other_providers_models_and_moves_the_chat_to_one(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::chat::{
            catalog::Catalog,
            model::{ChatCommand, ChatInfo, ModelOption},
        };
        let previous = ui_text::set_for_tests(1.0, ui_text::Face::Hermes);
        for state in [ChatState::Running, ChatState::Idle] {
            let (handle, view) = hermes_fixture(cx, 720.0);
            let (feed, recording) = super::super::feed::Feed::recording();
            let info = ChatInfo {
                parent_id: None,
                user_title: None,
                first_user_message: None,
                provider_title: None,
                id: uuid::Uuid::from_u128(1).to_string(),
                provider: Provider::Codex,
                project_id: Some("fixture-project".into()),
                worktree_id: None,
                cwd: "/inert-fixture".into(),
                title: "Existing conversation".into(),
                created_at_unix: 1,
                provider_thread_id: Some("existing-provider-thread".into()),
                model: Some("fixture-model".into()),
                effort: None,
                fast: false,
                approval_mode: ApprovalMode::Supervised,
                codex_account_id: None,
                state: state.clone(),
                orchestrator: None,
                carried_over: None,
            };
            cx.update_window(handle.into(), |_, _, cx| {
                view.update(cx, |view, cx| {
                    view.feed = Some(feed);
                    view.chat_id = Some(info.id.clone());
                    view.model.transcript.info = Some(info.clone());
                    view.model.transcript.state = state.clone();
                    // As the saved chats would tell it; the fixture reads no files.
                    view.others = Some((
                        Provider::Claude,
                        Catalog {
                            supported: vec![ModelOption {
                                id: "opus".into(),
                                name: "Opus".into(),
                                ..Default::default()
                            }],
                            ..Default::default()
                        },
                    ));
                    cx.notify();
                })
            })
            .unwrap();
            draw_hermes(cx, handle);
            cx.update_window(handle.into(), |_, window, cx| {
                view.update(cx, |view, cx| {
                    view.menu = Some(super::super::Menu::Model);
                    cx.notify();
                });
                let _ = window;
            })
            .unwrap();
            draw_hermes(cx, handle);
            cx.update_window(handle.into(), |_, window, cx| {
                for (id, label) in [
                    ("switch-claude-default", "Claude default"),
                    ("switch-claude-opus", "Opus"),
                ] {
                    assert_eq!(window.find(id).label(), Some(label), "{id}");
                }
                window.click("switch-claude-opus", cx);
            })
            .unwrap();
            draw_hermes(cx, handle);
            match state {
                // Nothing moves while a turn runs; the row says why.
                ChatState::Running => assert!(matches!(
                    recording.try_recv(),
                    Err(std::sync::mpsc::TryRecvError::Empty)
                )),
                _ => match recording.try_recv().unwrap() {
                    super::super::feed::Delivery::Command(command) => assert_eq!(
                        command,
                        ChatCommand::Switch {
                            provider: Provider::Claude,
                            model: Some("opus".into()),
                            effort: None,
                            fast: None,
                        }
                    ),
                    _ => panic!("a switch is an ordinary command"),
                },
            }
        }
        ui_text::set_for_tests(previous.0, previous.1);
    }

    /// A Claude chat in `state` on the fixture's model, as the host would describe it.
    fn chat_info(state: ChatState, fast: bool) -> crate::chat::model::ChatInfo {
        crate::chat::model::ChatInfo {
            parent_id: None,
            user_title: None,
            first_user_message: None,
            provider_title: None,
            id: uuid::Uuid::from_u128(2).to_string(),
            provider: Provider::Claude,
            project_id: None,
            worktree_id: None,
            cwd: "/inert-fixture".into(),
            title: "Composer".into(),
            created_at_unix: 1,
            provider_thread_id: None,
            model: Some("fixture-model".into()),
            effort: None,
            fast,
            approval_mode: ApprovalMode::Supervised,
            codex_account_id: None,
            state,
            orchestrator: None,
            carried_over: None,
        }
    }

    /// The fixture in `design` with a chat in `state`, drawn; and what its feed records.
    fn composer_fixture(
        cx: &mut gpui::TestAppContext,
        width: f32,
        design: theme::ThemeChoice,
        state: ChatState,
    ) -> (
        gpui::WindowHandle<gpui_kit::base::Root>,
        gpui::Entity<ChatView>,
        std::sync::mpsc::Receiver<super::super::feed::Delivery>,
    ) {
        let (handle, view) = design_fixture(cx, width, design);
        let (feed, recording) = super::super::feed::Feed::recording();
        cx.update_window(handle.into(), |_, _, cx| {
            view.update(cx, |view, cx| {
                let info = chat_info(state.clone(), false);
                view.feed = Some(feed);
                view.chat_id = Some(info.id.clone());
                view.model.transcript.info = Some(info);
                view.model.transcript.state = state;
                cx.notify();
            })
        })
        .unwrap();
        draw_hermes(cx, handle);
        (handle, view, recording)
    }

    fn set_draft(
        cx: &mut gpui::TestAppContext,
        handle: gpui::WindowHandle<gpui_kit::base::Root>,
        view: &gpui::Entity<ChatView>,
        text: &str,
    ) {
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                view.composer
                    .update(cx, |state, cx| state.set_value(text.to_owned(), window, cx));
                cx.notify();
            })
        })
        .unwrap();
        draw_hermes(cx, handle);
    }

    fn set_search(
        cx: &mut gpui::TestAppContext,
        handle: gpui::WindowHandle<gpui_kit::base::Root>,
        view: &gpui::Entity<ChatView>,
        text: &str,
    ) {
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                view.model_input
                    .update(cx, |state, cx| state.set_value(text.to_owned(), window, cx));
                cx.notify();
            })
        })
        .unwrap();
        draw_hermes(cx, handle);
    }

    fn update_info(
        cx: &mut gpui::TestAppContext,
        handle: gpui::WindowHandle<gpui_kit::base::Root>,
        view: &gpui::Entity<ChatView>,
        change: impl FnOnce(&mut ChatView),
    ) {
        cx.update_window(handle.into(), |_, _, cx| {
            view.update(cx, |view, cx| {
                change(view);
                cx.notify();
            })
        })
        .unwrap();
        draw_hermes(cx, handle);
    }

    /// Where each control of the message box is, relative to the bottom of the field: what
    /// must not move while the user types.
    fn composer_frames(
        cx: &mut gpui::TestAppContext,
        handle: gpui::WindowHandle<gpui_kit::base::Root>,
        slot: &str,
    ) -> (f32, Vec<(&'static str, gpui::Bounds<gpui::Pixels>)>) {
        cx.update_window(handle.into(), |_, window, _| {
            let field = window.find("chat-composer-shell").bounds();
            let bottom = field.bottom();
            let frames = [
                "chat-attach",
                "chat-model",
                "chat-effort",
                "chat-fast",
                "chat-dictate",
            ]
            .into_iter()
            .map(|id| (id, window.find(id).bounds()))
            .chain([("slot", window.find(slot.to_owned()).bounds())])
            .map(|(id, bounds)| {
                (
                    id,
                    gpui::Bounds::new(
                        point(bounds.origin.x, bounds.origin.y - bottom),
                        bounds.size,
                    ),
                )
            })
            .collect();
            (f32::from(field.size.width), frames)
        })
        .unwrap()
    }

    #[gpui::test]
    fn the_composer_controls_hold_still_while_the_draft_grows_and_shrinks(
        cx: &mut gpui::TestAppContext,
    ) {
        for (face, design) in [
            (ui_text::Face::System, theme::ThemeChoice::Native),
            (ui_text::Face::Hermes, theme::ThemeChoice::Hermes),
        ] {
            let previous = ui_text::set_for_tests(1.0, face);
            // One row beside the field, and the field over a row of controls.
            for width in [760.0, 520.0] {
                let (handle, view, _) = composer_fixture(cx, width, design, ChatState::Idle);
                let at_rest = composer_frames(cx, handle, "chat-send");
                let (_, frames) = &at_rest;
                // One family: every control as tall as Send, centered on one line, one gap
                // between each.
                let slot = frames.last().unwrap().1;
                let mut row: Vec<_> = frames[1..].iter().map(|(_, bounds)| *bounds).collect();
                row.sort_by(|a, b| a.left().partial_cmp(&b.left()).unwrap());
                let gap = row[1].left() - row[0].right();
                for (id, bounds) in frames {
                    assert!(
                        (bounds.size.height - slot.size.height).abs() < px(0.5),
                        "{design:?} {width}: {id} is {:?} tall, Send {:?}",
                        bounds.size.height,
                        slot.size.height
                    );
                    assert!(
                        (bounds.center().y - slot.center().y).abs() < px(0.5),
                        "{design:?} {width}: {id} is off the controls' line"
                    );
                }
                for pair in row.windows(2) {
                    assert!(
                        (pair[1].left() - pair[0].right() - gap).abs() < px(0.5),
                        "{design:?} {width}: uneven gaps {row:?}"
                    );
                }
                assert!(gap > px(0.0));
                if width > 700.0 {
                    // The text starts one gap after Attach, which sits at the card's padding.
                    cx.update_window(handle.into(), |_, window, _| {
                        let attach = window.find("chat-attach").bounds();
                        let text = window.find("chat-composer").bounds();
                        assert!(
                            (text.left() - attach.right() - gap).abs() < px(0.5),
                            "{design:?}: text starts {:?} after Attach, gap {gap:?}",
                            text.left() - attach.right()
                        );
                        // The card is inset from the pane, which spans the window, and
                        // edged by a hairline.
                        let padding = ui_text::space(composer::CARD_INSET)
                            + px(1.0)
                            + ui_text::space(composer::CARD_PADDING);
                        assert!(
                            (attach.left() - padding).abs() < px(0.5),
                            "{design:?}: Attach is at {:?}, not the card's padding",
                            attach.left()
                        );
                    })
                    .unwrap();
                    // Beside the field the controls sit on its last line of text, inside its
                    // hairline.
                    let line =
                        ui_text::space(widgets::FIELD_PAD_Y) + widgets::field_line() / 2. + px(1.0);
                    assert!(
                        (slot.center().y + line).abs() < px(1.0),
                        "{design:?}: the controls are centered {:?} above the field's bottom",
                        -slot.center().y
                    );
                }
                // At least two lines tall: empty, one line and two lines are the same box,
                // its text on the first line and the controls on the second.
                let field = |cx: &mut gpui::TestAppContext| {
                    cx.update_window(handle.into(), |_, window, _| {
                        window.find("chat-composer-shell").bounds()
                    })
                    .unwrap()
                };
                let two_lines = ui_text::space(widgets::FIELD_PAD_Y) * 2.
                    + widgets::field_line() * 2.
                    + px(2.0);
                let empty = field(cx);
                assert!(
                    (empty.size.height - two_lines).abs() < px(0.5),
                    "{design:?} {width}: the empty box is {:?} tall, not two lines",
                    empty.size.height
                );
                for draft in ["one line", "one\ntwo"] {
                    set_draft(cx, handle, &view, draft);
                    assert_eq!(
                        field(cx),
                        empty,
                        "{design:?} {width}: {draft:?} resized the box"
                    );
                }
                set_draft(cx, handle, &view, "one\ntwo\nthree");
                assert!(
                    field(cx).size.height > empty.size.height + widgets::field_line() / 2.,
                    "{design:?} {width}: three lines do not grow the box"
                );
                for draft in [
                    "one line",
                    "one\ntwo",
                    "one\ntwo\nthree\nfour",
                    &"a long pasted line that wraps ".repeat(20),
                    "",
                ] {
                    set_draft(cx, handle, &view, draft);
                    assert_eq!(
                        composer_frames(cx, handle, "chat-send"),
                        at_rest,
                        "{design:?} {width}: the controls moved for {draft:?}"
                    );
                }
                update_info(cx, handle, &view, |view| {
                    if let Some(info) = view.model.transcript.info.as_mut() {
                        info.fast = true;
                    }
                });
                assert_eq!(
                    composer_frames(cx, handle, "chat-send"),
                    at_rest,
                    "{design:?} {width}: Fast moved the controls"
                );
                // While a turn runs Stop takes Send's slot, and keeps it with a draft (⏎
                // steers the turn), so Interrupt stays in reach.
                update_info(cx, handle, &view, |view| {
                    view.model.transcript.state = ChatState::Running;
                });
                assert_eq!(
                    composer_frames(cx, handle, "chat-interrupt"),
                    at_rest,
                    "{design:?} {width}: Stop is not in Send's slot"
                );
                set_draft(cx, handle, &view, "steer");
                assert_eq!(composer_frames(cx, handle, "chat-interrupt"), at_rest);
                cx.update_window(handle.into(), |_, window, _| {
                    assert!(window.try_find("chat-send").is_none());
                })
                .unwrap();
            }
            ui_text::set_for_tests(previous.0, previous.1);
        }
    }

    #[gpui::test]
    fn the_composer_menus_open_over_their_picker_and_follow_the_pointer_and_keys(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::chat::model::ChatCommand;
        let configured =
            |recording: &std::sync::mpsc::Receiver<super::super::feed::Delivery>| match recording
                .try_recv()
            {
                Ok(super::super::feed::Delivery::Command(command)) => Some(command),
                _ => None,
            };
        for (face, design) in [
            (ui_text::Face::System, theme::ThemeChoice::Native),
            (ui_text::Face::Hermes, theme::ThemeChoice::Hermes),
        ] {
            let previous = ui_text::set_for_tests(1.0, face);
            let (handle, view, recording) = composer_fixture(cx, 760.0, design, ChatState::Idle);
            let cursor = |cx: &mut gpui::TestAppContext| {
                view.read_with(cx, |view, _| view.menu_cursor.clone())
            };
            let open = |cx: &mut gpui::TestAppContext| view.read_with(cx, |view, _| view.menu);

            // The effort menu: above its picker, its trailing edge on the picker's, in the
            // window; one-line rows.
            cx.update_window(handle.into(), |_, window, cx| {
                window.click("chat-effort", cx)
            })
            .unwrap();
            draw_hermes(cx, handle);
            assert_eq!(open(cx), Some(super::super::Menu::Effort));
            cx.update_window(handle.into(), |_, window, cx| {
                let picker = window.find("chat-effort").bounds();
                let menu = window.find("chat-choices-popover").bounds();
                assert!(
                    menu.bottom() <= picker.top(),
                    "{design:?}: {menu:?} over {picker:?}"
                );
                assert!(
                    (menu.right() - picker.right()).abs() < px(1.0),
                    "{design:?}: {menu:?}"
                );
                assert!(menu.left() >= px(0.0));
                assert_eq!(window.find("chat-effort").expanded(), Some(true));
                let row = window.find("effort-high").bounds();
                assert!((row.size.height - ui_text::space(CHOICE_ROW)).abs() < px(0.5));
                assert!(row.left() - menu.left() >= ui_text::space(CHOICE_MENU_PADDING));
                // The pointer highlights the row it is over.
                window.hover("effort-high", cx);
            })
            .unwrap();
            draw_hermes(cx, handle);
            assert_eq!(cursor(cx).as_deref(), Some("effort-high"));
            // ↑ and ↓ move the highlight, ⏎ chooses it and closes the menu.
            cx.update_window(handle.into(), |_, window, cx| window.press("up", cx))
                .unwrap();
            draw_hermes(cx, handle);
            assert_eq!(cursor(cx).as_deref(), Some("effort-medium"));
            cx.update_window(handle.into(), |_, window, cx| {
                window.press("up", cx);
                window.press("down", cx);
                window.press("enter", cx);
            })
            .unwrap();
            draw_hermes(cx, handle);
            assert_eq!(open(cx), None);
            assert_eq!(
                configured(&recording),
                Some(ChatCommand::Configure {
                    model: None,
                    effort: Some("high".into()),
                    approval_mode: None,
                    fast: None,
                })
            );

            // ⎋ closes the menu, and so does a click outside it.
            for close in ["escape", "outside"] {
                cx.update_window(handle.into(), |_, window, cx| {
                    window.click("chat-effort", cx)
                })
                .unwrap();
                draw_hermes(cx, handle);
                assert_eq!(open(cx), Some(super::super::Menu::Effort), "{close}");
                cx.update_window(handle.into(), |_, window, cx| {
                    if close == "escape" {
                        window.press("escape", cx);
                    } else {
                        window.click("chat-composer", cx);
                    }
                })
                .unwrap();
                draw_hermes(cx, handle);
                assert_eq!(open(cx), None, "{design:?}: {close} left the menu open");
                assert!(configured(&recording).is_none());
                // A click on the picker right after must open it again, not be swallowed.
                update_info(cx, handle, &view, |view| view.menu_closed = None);
            }

            // The model menu: the search field keeps the keys; ↓ steps from the first row
            // through this provider's models to the other's, ⏎ chooses.
            cx.update_window(handle.into(), |_, window, cx| {
                window.click("chat-model", cx)
            })
            .unwrap();
            draw_hermes(cx, handle);
            cx.update_window(handle.into(), |_, window, cx| {
                let picker = window.find("chat-model").bounds();
                let menu = window.find("chat-choices-popover").bounds();
                assert!(menu.bottom() <= picker.top());
                assert!((menu.right() - picker.right()).abs() < px(1.0));
                assert_eq!(
                    window.find("switch-codex-default").label(),
                    Some("Codex default")
                );
                window.press("down", cx);
            })
            .unwrap();
            draw_hermes(cx, handle);
            assert_eq!(cursor(cx).as_deref(), Some("model-fixture-model"));
            cx.update_window(handle.into(), |_, window, cx| window.press("down", cx))
                .unwrap();
            draw_hermes(cx, handle);
            assert_eq!(cursor(cx).as_deref(), Some("switch-codex-default"));
            cx.update_window(handle.into(), |_, window, cx| {
                window.press("up", cx);
                window.press("enter", cx);
            })
            .unwrap();
            draw_hermes(cx, handle);
            assert_eq!(open(cx), None);
            update_info(cx, handle, &view, |view| view.menu_closed = None);
            assert_eq!(
                configured(&recording),
                Some(ChatCommand::Configure {
                    model: Some("fixture-model".into()),
                    effort: None,
                    approval_mode: None,
                    fast: None,
                })
            );
            // ⏎ on a search takes this provider's first match; with none it does nothing,
            // never moving the chat to the other provider.
            cx.update_window(handle.into(), |_, window, cx| {
                window.click("chat-model", cx);
            })
            .unwrap();
            draw_hermes(cx, handle);
            cx.update_window(handle.into(), |_, window, cx| {
                window.input("no such model", cx);
                window.press("enter", cx);
            })
            .unwrap();
            draw_hermes(cx, handle);
            assert_eq!(open(cx), Some(super::super::Menu::Model));
            assert!(configured(&recording).is_none());
            set_search(cx, handle, &view, "fixture");
            cx.update_window(handle.into(), |_, window, cx| window.press("enter", cx))
                .unwrap();
            draw_hermes(cx, handle);
            assert_eq!(open(cx), None);
            assert_eq!(
                configured(&recording),
                Some(ChatCommand::Configure {
                    model: Some("fixture-model".into()),
                    effort: None,
                    approval_mode: None,
                    fast: None,
                })
            );
            ui_text::set_for_tests(previous.0, previous.1);
        }
    }

    /// A key going down (`held` for a repeat) or coming up, without its pair.
    fn key_event(
        cx: &mut gpui::TestAppContext,
        handle: gpui::WindowHandle<gpui_kit::base::Root>,
        key: &str,
        down: Option<bool>,
    ) {
        use gpui::InputEvent as _;
        let keystroke = gpui::Keystroke::parse(key).unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            let event = match down {
                Some(is_held) => gpui::KeyDownEvent {
                    keystroke,
                    is_held,
                    prefer_character_input: false,
                }
                .to_platform_input(),
                None => gpui::KeyUpEvent { keystroke }.to_platform_input(),
            };
            window.dispatch_event(event, cx);
        })
        .unwrap();
        draw_hermes(cx, handle);
    }

    #[gpui::test]
    fn an_open_composer_menu_owns_enter_and_ignores_held_keys(cx: &mut gpui::TestAppContext) {
        use crate::chat::model::ChatCommand;
        let previous = ui_text::set_for_tests(1.0, ui_text::Face::System);
        let (handle, view, recording) =
            composer_fixture(cx, 760.0, theme::ThemeChoice::Native, ChatState::Idle);
        let cursor =
            |cx: &mut gpui::TestAppContext| view.read_with(cx, |view, _| view.menu_cursor.clone());
        let open = |cx: &mut gpui::TestAppContext| view.read_with(cx, |view, _| view.menu);
        let draft = |cx: &mut gpui::TestAppContext| {
            view.read_with(cx, |view, cx| view.composer.read(cx).value().to_string())
        };
        set_draft(cx, handle, &view, "keep me");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("chat-effort", cx)
        })
        .unwrap();
        draw_hermes(cx, handle);
        assert_eq!(open(cx), Some(super::super::Menu::Effort));

        // ⏎ with nothing highlighted: the menu keeps it, the draft is not sent.
        cx.update_window(handle.into(), |_, window, cx| window.press("enter", cx))
            .unwrap();
        draw_hermes(cx, handle);
        assert_eq!(open(cx), Some(super::super::Menu::Effort));
        assert_eq!(draft(cx), "keep me");
        assert!(recording.try_recv().is_err(), "⏎ reached the message box");

        // A held ↓ moves the highlight once; the next press moves it again.
        key_event(cx, handle, "down", Some(false));
        key_event(cx, handle, "down", Some(true));
        key_event(cx, handle, "down", Some(true));
        assert_eq!(cursor(cx).as_deref(), Some("effort-medium"));
        key_event(cx, handle, "down", None);
        key_event(cx, handle, "down", Some(false));
        key_event(cx, handle, "down", None);
        assert_eq!(cursor(cx).as_deref(), Some("effort-high"));

        // A menu opened by a key: an ⏎ held through the opening chooses nothing, and its
        // repeats neither choose nor reach the message box; the next ⏎ chooses.
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                view.menu = None;
                view.menu_closed = None;
                view.toggle_menu(super::super::Menu::Effort, window, cx);
                view.menu_cursor = Some("effort-high".into());
            })
        })
        .unwrap();
        draw_hermes(cx, handle);
        key_event(cx, handle, "enter", Some(true));
        key_event(cx, handle, "enter", Some(true));
        assert_eq!(open(cx), Some(super::super::Menu::Effort));
        assert_eq!(draft(cx), "keep me");
        assert!(recording.try_recv().is_err());
        key_event(cx, handle, "enter", None);
        key_event(cx, handle, "enter", Some(false));
        assert_eq!(open(cx), None);
        match recording.try_recv() {
            Ok(super::super::feed::Delivery::Command(command)) => assert_eq!(
                command,
                ChatCommand::Configure {
                    model: None,
                    effort: Some("high".into()),
                    approval_mode: None,
                    fast: None,
                }
            ),
            _ => panic!("⏎ chose nothing"),
        }
        // Its repeats after the menu closed do not send the draft.
        key_event(cx, handle, "enter", Some(true));
        assert_eq!(draft(cx), "keep me");
        assert!(recording.try_recv().is_err());
        ui_text::set_for_tests(previous.0, previous.1);
    }

    fn prompt(question: &str, options: &[&str], multi_select: bool) -> QuestionPrompt {
        QuestionPrompt {
            header: None,
            question: question.to_owned(),
            options: options
                .iter()
                .map(|label| QuestionOption {
                    label: (*label).to_owned(),
                    description: String::new(),
                })
                .collect(),
            multi_select,
        }
    }

    fn question() -> Question {
        Question {
            request_id: "q1".to_owned(),
            questions: vec![
                prompt("Which part?", &["The list", "The footer"], false),
                prompt("Which checks?", &["fmt", "clippy", "tests"], true),
            ],
        }
    }

    #[test]
    fn an_answer_is_the_options_picked_then_the_text_typed() {
        let draft = Draft {
            picked: vec![vec![1], vec![0, 2]],
            ..Default::default()
        };
        let typed = [String::new(), "  and the docs  ".to_owned()];
        assert_eq!(
            answers_of(&question(), Some(&draft), &typed),
            vec![
                vec!["The footer".to_owned()],
                vec![
                    "fmt".to_owned(),
                    "tests".to_owned(),
                    "and the docs".to_owned()
                ],
            ]
        );
    }

    #[test]
    fn a_prompt_with_nothing_picked_or_typed_has_an_empty_answer_that_blocks_sending() {
        let answers = answers_of(&question(), None, &[]);
        assert_eq!(answers, vec![Vec::<String>::new(), Vec::new()]);
        assert!(answers.iter().any(Vec::is_empty));
        // Free text alone answers a prompt, and an option that is not there is ignored.
        let draft = Draft {
            picked: vec![vec![7], vec![]],
            ..Default::default()
        };
        let typed = ["something else".to_owned(), String::new()];
        assert_eq!(
            answers_of(&question(), Some(&draft), &typed),
            vec![vec!["something else".to_owned()], Vec::new()]
        );
    }
}
