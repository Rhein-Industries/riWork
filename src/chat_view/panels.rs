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
    ChatView, ChatViewEvent, Creation, Draft, Menu, approval, composer, dictate,
    state::provider_name,
    toolbar,
    widgets::{self, Look, button, capsule, dimmed},
};

/// The chat header's inset and control gap, shared with the message box (see `composer`).
use super::composer::{BAR_GAP, BAR_INSET};
/// The space between the message box's edge and its text, clear of its round corners.
const FIELD_INSET: f32 = 12.0;
pub(super) const ATTACHMENT_MENU_KEY: &str = "composer-attachment-menu";

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
                .children(open.then(|| self.menu_popover(menu, look, window, cx)))
        };

        div()
            .debug_selector(|| "chat-header".into())
            .w_full()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(ui_text::space(BAR_GAP))
            .px(ui_text::space(BAR_INSET))
            .py(ui_text::space(5.0))
            .border_b_1()
            .border_color(rgb(colors.divider))
            .bg(rgb(colors.panel))
            .child(
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
                                        let _ = owner
                                            .update(cx, |view, cx| view.choose_display(mode, cx));
                                    }
                                })
                            }),
                    )
                    .child(tooltip::anchor(
                        "Normal shows results · Verbose shows all activity",
                        TipLook::Control,
                    )),
            )
            .child(picker(
                "chat-mode",
                toolbar::mode_label(mode).to_owned(),
                Menu::Mode,
                cx,
            ))
            .child(if idle {
                button("chat-compact", "Compact", None, look)
                    .on_click(cx.listener(|view, _, _, cx| view.compact(cx)))
                    .into_any_element()
            } else {
                dimmed("chat-compact", "Compact", look).into_any_element()
            })
            .child(div().flex_1())
            .children(self.model.transcript.usage.as_ref().map(|usage| {
                let fraction = toolbar::context_fraction(usage);
                div()
                    .relative()
                    .flex()
                    .items_center()
                    .gap(ui_text::space(6.0))
                    .text_size(ui_text::text(10.0))
                    .text_color(rgb(colors.muted))
                    // Counts, in the monospace accent where Native keeps one.
                    .font_family(ui_text::mono_family())
                    .children(fraction.map(|fraction| {
                        div()
                            .w(ui_text::space(44.0))
                            .h(ui_text::space(5.0))
                            .rounded(px(3.0))
                            .when(look.native, |track| track.rounded_full())
                            .bg(rgb(colors.divider))
                            .child(
                                div()
                                    .h_full()
                                    .rounded(px(3.0))
                                    .when(look.native, |fill| fill.rounded_full())
                                    .w(relative(fraction))
                                    .bg(rgb(if fraction >= 0.9 {
                                        look.error()
                                    } else if fraction >= 0.7 {
                                        colors.gold
                                    } else {
                                        colors.cyan
                                    })),
                            )
                    }))
                    .child(toolbar::usage_text(usage))
                    .children(usage.cost_usd.map(toolbar::cost_text))
                    .child(tooltip::anchor(
                        toolbar::usage_details(usage),
                        TipLook::Control,
                    ))
            }))
            .children(thread.map(|thread| {
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
            }))
            .child(
                div()
                    .relative()
                    .child({
                        let open = matches!(self.menu, Some(Menu::More | Menu::ConfirmDelete));
                        if look.native {
                            let more =
                                widgets::symbol_button("chat-more", "ellipsis", "More", look);
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
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.open.remove(ATTACHMENT_MENU_KEY);
                            view.toggle_menu(Menu::More, window, cx);
                        }))
                    })
                    .children(
                        matches!(self.menu, Some(Menu::More | Menu::ConfirmDelete)).then(|| {
                            self.menu_popover(self.menu.unwrap_or(Menu::More), look, window, cx)
                        }),
                    ),
            )
            .into_any_element()
    }

    /// The popover under a toolbar button.
    fn menu_popover(
        &self,
        menu: Menu,
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
            Menu::Effort => {
                let model = info.and_then(|info| info.model.as_deref());
                let effort = info.and_then(|info| info.effort.as_deref());
                let provider = self.provider().unwrap_or(Provider::Codex);
                toolbar::effort_rows(&self.model.transcript.models, model, effort, provider)
                    .into_iter()
                    .map(|line| {
                        let name = line.effort;
                        let label = widgets::sentence(&line.label, look);
                        row(format!("effort-{name}"), label, None, line.current)
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.configure(None, Some(name.clone()), None, None, cx);
                            }))
                            .into_any_element()
                    })
                    .collect()
            }
            Menu::Model if !self.model.transcript.models.is_empty() => {
                let current = info.and_then(|info| info.model.as_deref());
                let query = self.model_input.read(cx).value();
                let rows =
                    toolbar::filtered_model_rows(&self.model.transcript.models, current, &query);
                let mut content = vec![
                    div()
                        .px(ui_text::space(10.0))
                        .py(ui_text::space(6.0))
                        .child(
                            text_input::input("chat-model-search", &self.model_input, window, cx)
                                .font_family(look.chat_family())
                                .accessibility_label("Search models"),
                        )
                        .into_any_element(),
                ];
                if rows.is_empty() {
                    content.push(
                        div()
                            .px(ui_text::space(10.0))
                            .py(ui_text::space(6.0))
                            .text_size(ui_text::text(11.0))
                            .text_color(rgb(colors.muted))
                            .child("No matching models")
                            .into_any_element(),
                    );
                }
                content.extend(rows.into_iter().map(|line| {
                    let id = line.id;
                    row(
                        format!("model-{id}"),
                        line.label,
                        line.detail.as_deref(),
                        line.current,
                    )
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.choose_model(&id, cx);
                    }))
                    .into_any_element()
                }));
                content
            }
            Menu::Model => {
                // A driver that has not listed its models: type the name.
                let current = info.and_then(|info| info.model.clone());
                let suggestions = self
                    .provider()
                    .map(toolbar::model_suggestions)
                    .unwrap_or_default();
                let mut rows = vec![
                    div()
                        .px(ui_text::space(10.0))
                        .py(ui_text::space(6.0))
                        .child(
                            text_input::input("chat-model-input", &self.model_input, window, cx)
                                .font_family(look.chat_family()),
                        )
                        .into_any_element(),
                ];
                rows.extend(suggestions.iter().map(|model| {
                    let name = (*model).to_owned();
                    row(
                        format!("model-{model}"),
                        name.clone(),
                        None,
                        current.as_deref() == Some(model),
                    )
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.configure(Some(name.clone()), None, None, None, cx);
                    }))
                    .into_any_element()
                }));
                rows
            }
            Menu::More => vec![
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
            ],
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
        let in_composer = matches!(menu, Menu::Model | Menu::Effort);
        let toward_left = matches!(menu, Menu::More | Menu::ConfirmDelete);
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
            .w(ui_text::space(340.0).min(px(available)))
            .max_h(ui_text::space(320.0).min(window.viewport_size().height * 0.65))
            .overflow_y_scroll()
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
            }))
            .font_family(look.chat_family());
        let content = controls::native(popover, |menu| controls::menu(menu, colors))
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

    /// What the user should know before typing: the chat is stopped or failed, or the last
    /// command did not get through.
    fn banner(&self, look: Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        let colors = look.colors;
        // Native leads the line with a symbol of what it says, in the line's color.
        let line = |text: String, color: u32| {
            let symbol = if color == colors.muted {
                "pause.circle"
            } else {
                "exclamationmark.triangle"
            };
            div()
                .w_full()
                .flex()
                .items_center()
                .gap(ui_text::space(8.0))
                .px(ui_text::space(12.0))
                .py(ui_text::space(5.0))
                .border_t_1()
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
        if let Some(notice) = &self.notice {
            return Some(
                line(notice.clone(), colors.gold)
                    .child(
                        button("chat-dismiss", "Dismiss", None, look).on_click(cx.listener(
                            |view, _, _, cx| {
                                view.notice = None;
                                cx.notify();
                            },
                        )),
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
        Some(
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap(ui_text::space(6.0))
                .px(ui_text::space(12.0))
                .py(ui_text::space(8.0))
                .border_t_1()
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
        Some(
            div()
                .id("chat-questions")
                .w_full()
                .max_h(relative(0.5))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(ui_text::space(8.0))
                .px(ui_text::space(12.0))
                .py(ui_text::space(8.0))
                .border_t_1()
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
    fn composer_choices(&self, look: Look, window: &Window, cx: &mut Context<Self>) -> AnyElement {
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
            .gap(ui_text::space(4.0))
            .child(self.composer_picker(
                "chat-model",
                toolbar::model_label(models, model),
                Menu::Model,
                look,
                window,
                cx,
            ))
            .children(toolbar::effort_available(models, model, provider).then(|| {
                self.composer_picker(
                    "chat-effort",
                    toolbar::effort_label(models, model, effort),
                    Menu::Effort,
                    look,
                    window,
                    cx,
                )
            }))
            .children(toolbar::fast_available(models, model).then(|| {
                widgets::toggle_button("chat-fast", "Fast", None, fast, look)
                    .accessibility_label(toolbar::fast_label(fast))
                    .when(look.hermes(), |toggle| {
                        toggle
                            .border_color(transparent_black())
                            .bg(transparent_black())
                            .text_color(rgb(if fast {
                                look.colors.text
                            } else {
                                look.colors.muted
                            }))
                    })
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

    fn composer_picker(
        &self,
        name: &'static str,
        label: String,
        menu: Menu,
        look: Look,
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
        let trigger = crate::behavior_controls::button_content(
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
        )
        .flex()
        .min_w_0()
        .max_w(relative(1.0))
        .items_center()
        .gap(ui_text::space(4.0))
        .px(ui_text::space(6.0))
        .py(ui_text::space(3.0))
        .text_size(ui_text::text(11.0))
        .font_family(look.chat_family())
        .text_color(rgb(look.colors.muted))
        .border_1()
        .border_color(transparent_black())
        .rounded(ui_text::space(3.0))
        .bg(rgb(if open {
            look.colors.panel_active
        } else {
            look.colors.panel
        }))
        .hover(move |style| style.bg(rgb(look.colors.panel_active)))
        .focus_visible(move |style| style.border_color(rgb(look.colors.focus)))
        .cursor_pointer()
        .aria_expanded(open)
        .child(icons::text_mark("⌄", 9.0))
        .on_click(cx.listener(move |view, _, window, cx| {
            view.open.remove(ATTACHMENT_MENU_KEY);
            let opening = view.menu != Some(menu);
            view.toggle_menu(menu, window, cx);
            if opening && view.menu == Some(Menu::Model) && !view.model.transcript.models.is_empty()
            {
                view.model_seeded = false;
                view.model_input
                    .update(cx, |state, cx| state.set_value("", window, cx));
                view.model_input.read(cx).focus_handle(cx).focus(window, cx);
            }
        }));
        crate::behavior_controls::popup(format!("{name}-popup"), trigger)
            .max_w(relative(1.0))
            .anchor(gpui::Anchor::BottomRight)
            .when(open, |popup| {
                popup.content(self.menu_popover(menu, look, window, cx))
            })
            .into_any_element()
    }

    /// The reference's + menu contains only actions backed by existing attachment staging.
    fn composer_attachment_menu(
        &self,
        look: Look,
        side: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.open.contains(ATTACHMENT_MENU_KEY);
        let trigger_bounds = std::rc::Rc::new(std::cell::Cell::new(gpui::Bounds::default()));
        let positioned = trigger_bounds.clone();
        let trigger = widgets::symbol_button_sized(
            "chat-attach",
            "plus",
            "Attach files or paste an image",
            look,
            side,
        )
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
                            .text_size(ui_text::text(10.0)).text_color(rgb(look.colors.muted)).child("ATTACH"))
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
                                            view.notice = Some("The clipboard has no supported image or file attachment.".into());
                                        }
                                    }
                                    Ok(None) => view.notice = Some("The clipboard has no image or file attachment.".into()),
                                    Err(error) => view.notice = Some(format!("Clipboard attachment: {error}")),
                                }
                                view.focus(window, cx);
                                cx.notify();
                            }))),
                )
            })
            .into_any_element()
    }

    fn composer_box(&self, look: Look, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        if look.hermes() {
            return self.hermes_composer(look, window, cx);
        }
        let colors = look.colors;
        let running = self.running();
        let dictation = self.dictation.phase();
        // The mic, its key and its status only while Settings shows it in chats.
        let mic = dictate::mic_shown(cx);
        // No helper text at rest: only a dictation at work says what it does.
        let status = composer::status(dictation, mic);
        // Where the buttons go and how big they are, for the pane's width as last drawn (see
        // `composer::layout`): the box keeps its minimum and the buttons stay in the pane.
        let layout = composer::layout(
            self.composer_width.get(),
            ui_text::scale(),
            mic,
            dictation.is_active(),
            running,
        );
        let side = px(layout.button);
        let gap = px(layout.gap);
        // Every button is a square of `side`: Native's symbols, the colorful themes' marks,
        // their names and keys in the tooltips. Attach shows its fill only under the pointer
        // and its tooltip names the kinds and paste/drop.
        let attach = if look.native {
            widgets::symbol_button_sized(
                "chat-attach",
                "paperclip",
                "Attach files · UTF-8 text, PNG or JPEG · or paste or drop them",
                look,
                side,
            )
        } else {
            mark_button(
                "chat-attach",
                "+",
                "Attach files · UTF-8 text, PNG or JPEG · or paste or drop them",
                None,
                side,
                look,
            )
        }
        .on_click(cx.listener(|view, _, window, cx| view.attach_picker(window, cx)));
        let stop = running.then(|| {
            if look.native {
                widgets::round_button_sized(
                    "chat-interrupt",
                    "stop.fill",
                    "Interrupt · ⌘.",
                    Button::Secondary,
                    look,
                    side,
                )
            } else {
                mark_button(
                    "chat-interrupt",
                    "■",
                    "Interrupt · ⌘.",
                    Some(look.diff.removed),
                    side,
                    look,
                )
            }
            .on_click(cx.listener(|view, _, _, cx| view.interrupt(cx)))
        });
        let send_tip = if running {
            "Send · ⏎ steers the turn · ⇧⏎ new line"
        } else {
            "Send · ⏎ · ⇧⏎ new line"
        };
        // Send waits until there is something to send: Native's in grey, the colorful
        // themes' as it is drawn, in every theme taking no click or key meanwhile.
        let empty = self.draft_empty(cx) || self.pending_submission.is_some();
        let send = if look.native {
            widgets::round_button_sized(
                "chat-send",
                "arrow.up",
                send_tip,
                if empty {
                    Button::Disabled
                } else {
                    Button::Primary
                },
                look,
                side,
            )
        } else {
            mark_button("chat-send", "↑", send_tip, Some(colors.cyan), side, look).disabled(empty)
        }
        .on_click(cx.listener(|view, _, window, cx| view.send_message(window, cx)));
        let trailing: Vec<AnyElement> = [
            layout.mic.then(|| {
                widgets::beside_field(self.mic_button(look, side, cx))
                    .debug_selector(|| "composer-mic".into())
                    .into_any_element()
            }),
            stop.map(|stop| {
                widgets::beside_field(stop)
                    .debug_selector(|| "composer-stop".into())
                    .into_any_element()
            }),
            Some(
                widgets::beside_field(send)
                    .debug_selector(|| "composer-send".into())
                    .into_any_element(),
            ),
        ]
        .into_iter()
        .flatten()
        .collect();
        let attach = widgets::beside_field(attach).debug_selector(|| "composer-attach".into());
        let field = div()
            .flex_1()
            .min_w_0()
            .debug_selector(|| "composer-field".into())
            .child(self.composer_editor(look, window, cx));
        let buttons_and_field = if layout.stacked {
            // The box on its own row; Attach at the start of the row under it and the rest at
            // its end, as the row beside the box has them.
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
                        .items_center()
                        .gap(gap)
                        .child(attach)
                        .child(div().flex_1())
                        .children(trailing),
                )
        } else {
            // The buttons keep to the bottom, each centered on the box's last line (see
            // `widgets::beside_field`): on the box's center while it has one line, beside its
            // last line once it grows, as a message field keeps them.
            div()
                .w_full()
                .flex()
                .items_end()
                .gap(gap)
                .child(attach)
                .child(field)
                .children(trailing)
        };
        // The pane's width as laid out, for the next frame's `layout`. It does not depend on
        // the layout, so it settles after one redraw.
        let measured = self.composer_width.clone();
        let measure = canvas(
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
        .h(px(0.));
        let bar = div()
            .id("chat-composer-bar")
            .w_full()
            .flex()
            .flex_col()
            .gap(gap)
            // The header's inset on the sides, so the field and the buttons line up with its
            // controls, and the same above and below the field; less in a narrow pane.
            .p(px(layout.inset))
            .border_t_1()
            .border_color(rgb(colors.divider))
            .bg(rgb(colors.panel))
            // A drop target only while files are dragged over it.
            .drag_over::<gpui::ExternalPaths>(move |style, _, _, _| {
                style.bg(rgb(look.tint(colors.focus, 0.12)))
            })
            .on_drop(
                cx.listener(|view, paths: &gpui::ExternalPaths, window, cx| {
                    view.dropped_attachments(paths, window, cx)
                }),
            )
            // The draft's attachments above the box, a staged image as its thumbnail, inside
            // the bar's inset.
            .children((!self.attachments.is_empty()).then(|| {
                div()
                    .id("chat-attachment-queue")
                    .role(gpui::Role::Group)
                    .aria_label("Draft attachments")
                    .max_h(ui_text::space(280.))
                    .flex_none()
                    .overflow_y_scroll()
                    .child(self.attachment_chips(look, cx))
                    .test_support()
            }))
            .child(buttons_and_field)
            .child(self.composer_choices(look, window, cx))
            // Saved submissions to review, below the box.
            .children((!self.submissions.is_empty()).then(|| {
                div()
                    .id("chat-submission-reviews")
                    .max_h(ui_text::space(280.))
                    .overflow_y_scroll()
                    .child(self.submission_cards(look, cx))
            }))
            .children(status.map(|status| {
                div()
                    .text_size(ui_text::text(9.0))
                    .text_color(rgb(colors.text))
                    .child(status)
            }));
        div()
            .relative()
            .w_full()
            .debug_selector(|| "composer-pane".into())
            .child(measure)
            .child(bar)
            .into_any_element()
    }

    /// A single navy surface: compact at rest, full-width multiline text above its controls
    /// as soon as there is a draft. The retained Kit textarea still owns every edit.
    fn hermes_composer(&self, look: Look, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = look.colors;
        let width = self.composer_width.get();
        let compact = composer::hermes_compact(
            width,
            ui_text::scale(),
            self.composer.read(cx).value().is_empty() && self.attachments.is_empty(),
        );
        let narrow = width > 0.0 && width < ui_text::space_f32(360.0);
        let inset = if narrow {
            4.0
        } else {
            ui_text::space_f32(14.0)
        };
        let padding = if narrow { 4.0 } else { ui_text::space_f32(7.0) };
        let gap = if narrow { px(4.0) } else { ui_text::space(4.0) };
        let mic = dictate::mic_shown(cx);
        let action_count = 1 + usize::from(mic) + usize::from(self.running());
        let available = if width <= 0.0 {
            ui_text::space_f32(680.0)
        } else {
            (width - 2.0 * (inset + padding) - 2.0).max(1.0)
        };
        let side = ui_text::space(widgets::ROUND_BUTTON).min(px(((available
            - f32::from(gap) * (action_count - 1) as f32)
            / action_count as f32)
            .max(1.0)));
        let attach = div()
            .flex_none()
            .debug_selector(|| "composer-attach".into())
            .child(self.composer_attachment_menu(look, side, cx));
        let field = div()
            .flex_1()
            .min_w_0()
            .debug_selector(|| "composer-field".into())
            .child(self.composer_editor(look, window, cx));
        let empty = self.draft_empty(cx) || self.pending_submission.is_some();
        let send = crate::behavior_controls::button_content(
            "chat-send",
            "Send message · Enter sends · Shift Enter adds a line",
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
        .child(tooltip::anchor(
            if self.running() {
                "Send · Enter steers the turn · Shift Enter adds a line"
            } else {
                "Send · Enter · Shift Enter adds a line"
            },
            TipLook::Control,
        ))
        .on_click(cx.listener(|view, _, window, cx| view.send_message(window, cx)));
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
            .children(self.running().then(|| {
                div().debug_selector(|| "composer-stop".into()).child(
                    widgets::symbol_button_sized(
                        "chat-interrupt",
                        "stop.fill",
                        "Interrupt · ⌘.",
                        look,
                        side,
                    )
                    .on_click(cx.listener(|view, _, _, cx| view.interrupt(cx))),
                )
            }))
            .child(div().debug_selector(|| "composer-send".into()).child(send));
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
                    .child(self.composer_choices(look, window, cx)),
            )
            .child(actions);
        let content = if compact {
            div()
                .w_full()
                .flex()
                .items_center()
                .gap(gap)
                .child(attach)
                .child(field)
                .child(controls.flex_none())
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
                        .items_end()
                        .gap(gap)
                        .child(attach)
                        .child(controls.when(narrow, |controls| controls.flex_none().w_full())),
                )
        };
        let active = self.composer.read(cx).focus_handle(cx).is_focused(window);
        let surface = div()
            .id("chat-composer-bar")
            .w_full()
            .flex()
            .flex_col()
            .gap(gap)
            .p(px(padding))
            .font_family(look.chat_family())
            .rounded(ui_text::space(5.0))
            .border_1()
            .border_color(rgb(if active { colors.focus } else { colors.divider }))
            .bg(rgb(colors.panel))
            .drag_over::<gpui::ExternalPaths>(move |style, _, _, _| {
                style.bg(rgb(look.tint(colors.focus, 0.12)))
            })
            .on_drop(
                cx.listener(|view, paths: &gpui::ExternalPaths, window, cx| {
                    view.dropped_attachments(paths, window, cx)
                }),
            )
            .children((!self.attachments.is_empty()).then(|| {
                div()
                    .id("chat-attachment-queue")
                    .role(gpui::Role::Group)
                    .aria_label("Draft attachments")
                    .max_h(ui_text::space(280.0))
                    .flex_none()
                    .overflow_y_scroll()
                    .child(self.attachment_chips(look, cx))
                    .test_support()
            }))
            .child(content)
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
        let measured = self.composer_width.clone();
        div()
            .relative()
            .w_full()
            .debug_selector(|| "composer-pane".into())
            .p(px(inset))
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
            .child(surface)
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

    /// Kit's transparent textarea inside the existing continuous-corner shell.
    fn composer_editor(
        &self,
        look: Look,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Stateful<gpui::Div> {
        let colors = look.colors;
        let active = self.composer.read(cx).focus_handle(cx).is_focused(window);
        let radius = ui_text::space(16.);
        let outline = if active {
            if look.native {
                colors.focus
            } else {
                colors.cyan
            }
        } else {
            colors.divider
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
        let weak = cx.weak_entity();
        let editor = text_input::on_paste_with_reader(
            text_input::textarea("chat-composer", &self.composer, window, cx),
            &self.composer,
            super::attachment_ui::read_composer_clipboard,
            move |item, window, cx| {
                weak.update(cx, |view, cx| match item {
                    Ok(item) => view.paste_attachments(item, window, cx),
                    Err(error) => {
                        view.notice = Some(format!("Clipboard attachment: {error}"));
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
        .px(ui_text::space(FIELD_INSET))
        .py(ui_text::space(widgets::FIELD_PAD_Y))
        .text_size(ui_text::text(widgets::FIELD_TEXT))
        .line_height(widgets::field_line())
        .font_family(look.chat_family())
        .capture_action(cx.listener(Self::capture_enter));
        div()
            .id("chat-composer-shell")
            .relative()
            .w_full()
            .border_1()
            .bg(transparent_black())
            .border_color(transparent_black())
            .children((!look.hermes()).then_some(surface))
            .child(editor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::{QuestionOption, QuestionPrompt};
    use gpui_kit::test::TestWindowExt;

    /// Recording-only fixture: no provider, host staging, clipboard or socket access.
    fn hermes_fixture(
        cx: &mut gpui::TestAppContext,
        width: f32,
    ) -> (
        gpui::WindowHandle<gpui_kit::base::Root>,
        gpui::Entity<ChatView>,
    ) {
        cx.update(|cx| {
            cx.set_global(crate::settings::Settings {
                dictation_mic: true,
                ..Default::default()
            });
            cx.set_global(theme::Appearance {
                selected: theme::ThemeChoice::Hermes,
                palette: theme::Palette::HERMES,
                terminal: None,
                ghostty: None,
                error: None,
            });
            text_input::init(cx);
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
                                        panic!("Hermes fixture cannot stage or launch a provider")
                                    }),
                                },
                                window,
                                cx,
                            );
                            view.chat_id = Some("hermes-fixture".into());
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
    fn hermes_empty_and_multiline_keep_editor_and_controls_inside_pane(
        cx: &mut gpui::TestAppContext,
    ) {
        for scale in [1.0, 1.5, 2.0] {
            let previous = ui_text::set_for_tests(scale, ui_text::Face::Menlo);
            for width in [100.0, 240.0, 360.0, 720.0, 1440.0] {
                let (handle, view) = hermes_fixture(cx, width);
                let editor = view.read_with(cx, |view, _| view.composer.entity_id());
                for text in ["", "First line 🦀\nSecond line\nThird line\nFourth line"] {
                    cx.update_window(handle.into(), |_, window, cx| {
                        view.update(cx, |view, cx| {
                            view.composer
                                .update(cx, |state, cx| state.set_value(text, window, cx));
                            cx.notify();
                        })
                    })
                    .unwrap();
                    draw_hermes(cx, handle);
                    let mut visual = gpui::VisualTestContext::from_window(handle.into(), cx);
                    let pane = visual.debug_bounds("composer-pane").unwrap();
                    let field = visual.debug_bounds("composer-field").unwrap();
                    for selector in [
                        "composer-field",
                        "composer-attach",
                        "composer-mic",
                        "composer-stop",
                        "composer-send",
                        "composer-model-choices",
                    ] {
                        let bounds = visual.debug_bounds(selector).unwrap();
                        assert!(
                            bounds.left() >= pane.left() - px(0.5),
                            "{selector}, width {width}, scale {scale}"
                        );
                        assert!(
                            bounds.right() <= pane.right() + px(0.5),
                            "{selector}, width {width}, scale {scale}"
                        );
                        assert!(bounds.size.width > px(0.0));
                    }
                    if !text.is_empty() {
                        assert!(
                            visual.debug_bounds("composer-send").unwrap().top() >= field.bottom()
                        );
                    } else if composer::hermes_compact(width, scale, true) {
                        assert!(
                            visual.debug_bounds("composer-send").unwrap().top() < field.bottom()
                        );
                    }
                    let header = visual.debug_bounds("chat-header").unwrap();
                    assert!(
                        visual.debug_bounds("composer-model-choices").unwrap().top()
                            >= header.bottom()
                    );
                    view.read_with(cx, |view, cx| {
                        assert_eq!(view.composer.entity_id(), editor);
                        assert_eq!(view.composer.read(cx).value(), text);
                    });
                }
            }
            ui_text::set_for_tests(previous.0, previous.1);
        }
    }

    #[gpui::test]
    fn hermes_attachment_menu_escape_preserves_draft_and_has_only_backed_actions(
        cx: &mut gpui::TestAppContext,
    ) {
        let previous = ui_text::set_for_tests(1.0, ui_text::Face::Menlo);
        let (handle, view) = hermes_fixture(cx, 360.0);
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                view.composer.update(cx, |state, cx| {
                    state.set_value("Keep my draft 🦀", window, cx)
                });
                cx.notify();
            })
        })
        .unwrap();
        draw_hermes(cx, handle);
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("chat-attach", cx)
        })
        .unwrap();
        draw_hermes(cx, handle);
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("chat-attach", cx)
        })
        .unwrap();
        draw_hermes(cx, handle);
        assert!(!view.read_with(cx, |view, _| view.open.contains(ATTACHMENT_MENU_KEY)));
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("chat-attach", cx)
        })
        .unwrap();
        draw_hermes(cx, handle);
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(window.find("chat-attach").expanded(), Some(true));
            assert_eq!(
                window.find("chat-attach-files").role(),
                Some(gpui::Role::MenuItem)
            );
            assert_eq!(
                window.find("chat-attach-paste").label(),
                Some("Paste image")
            );
            window.press("escape", cx);
        })
        .unwrap();
        draw_hermes(cx, handle);
        cx.update_window(handle.into(), |_, window, cx| {
            view.read(cx)
                .composer
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        })
        .map(|focused| assert!(focused))
        .unwrap();
        view.read_with(cx, |view, cx| {
            assert!(!view.open.contains(ATTACHMENT_MENU_KEY));
            assert_eq!(view.composer.read(cx).value(), "Keep my draft 🦀");
            assert_eq!(view.chat_id.as_deref(), Some("hermes-fixture"));
        });
        ui_text::set_for_tests(previous.0, previous.1);
    }

    #[gpui::test]
    fn hermes_model_search_refresh_retains_input_and_menu_fits_narrow_pane(
        cx: &mut gpui::TestAppContext,
    ) {
        let previous = ui_text::set_for_tests(1.0, ui_text::Face::Menlo);
        let (handle, view) = hermes_fixture(cx, 240.0);
        draw_hermes(cx, handle);
        let input = view.read_with(cx, |view, _| view.model_input.entity_id());
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("chat-model", cx)
        })
        .unwrap();
        draw_hermes(cx, handle);
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("chat-model-search", cx);
            window.input("fixture", cx);
        })
        .unwrap();
        draw_hermes(cx, handle);
        let mut visual = gpui::VisualTestContext::from_window(handle.into(), cx);
        let menu = visual.debug_bounds("composer-choices-menu").unwrap();
        assert!(menu.left() >= px(0.0) && menu.right() <= px(240.0));
        assert!(menu.top() >= px(0.0) && menu.bottom() <= px(900.0));
        cx.update_window(handle.into(), |_, _, cx| {
            view.update(cx, |view, cx| {
                view.model.transcript.models[0].name = "Refreshed fixture label".into();
                cx.notify();
            })
        })
        .unwrap();
        draw_hermes(cx, handle);
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(
                window.find("model-fixture-model").label(),
                Some("Refreshed fixture label (default)")
            );
        })
        .unwrap();
        view.read_with(cx, |view, cx| {
            assert_eq!(view.model_input.entity_id(), input);
            assert_eq!(view.model_input.read(cx).value(), "fixture");
            assert_eq!(view.chat_id.as_deref(), Some("hermes-fixture"));
        });
        ui_text::set_for_tests(previous.0, previous.1);
    }

    #[test]
    fn composer_corners_tessellate_within_resized_bounds() {
        for (width, height, radius) in [
            (500.0, 30.0, 16.0),
            (180.0, 180.0, 16.0),
            (240.0, 270.0, 24.0),
            (12.0, 30.0, 24.0),
        ] {
            let bounds =
                gpui::Bounds::new(point(px(40.0), px(60.0)), gpui::size(px(width), px(height)));
            for mut builder in [PathBuilder::fill(), PathBuilder::stroke(px(1.0))] {
                composer_path(&mut builder, bounds.inset(px(0.5)), px(radius));
                let path = builder
                    .build()
                    .expect("continuous composer path tessellates");
                assert!(!path.vertices.is_empty());
                for vertex in path.vertices {
                    let x = f32::from(vertex.xy_position.x);
                    let y = f32::from(vertex.xy_position.y);
                    assert!(x.is_finite() && y.is_finite());
                    // Allow only tessellator floating-point error at the outer stroke edge.
                    assert!((39.99..=40.01 + width).contains(&x));
                    assert!((59.99..=60.01 + height).contains(&y));
                }
            }
        }
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
