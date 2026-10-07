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
        let model = info.and_then(|info| info.model.clone());
        let effort = info.and_then(|info| info.effort.clone());
        let fast = info.is_some_and(|info| info.fast);
        let thread = info.and_then(|info| info.provider_thread_id.clone());
        let idle = matches!(self.model.transcript.state, ChatState::Idle);
        // What the driver said it offers; empty for a driver that says nothing.
        let models = &self.model.transcript.models;
        let provider = self.provider().unwrap_or(Provider::Codex);

        // A pop-up button: its choice and an arrow. Native's is a grey capsule with a chevron,
        // a step darker while its menu is open.
        // A picker with nothing chosen shows its name, a sentence in Native.
        let placeholder = |label: String, name: &str| {
            if label == name {
                widgets::sentence(name, look)
            } else {
                label
            }
        };
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
                            view.toggle_menu(menu, window, cx)
                        })),
                )
                .children(open.then(|| self.menu_popover(menu, look, window, cx)))
        };

        div()
            .w_full()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(ui_text::space(6.0))
            .px(ui_text::space(10.0))
            .py(ui_text::space(5.0))
            .border_b_1()
            .border_color(rgb(colors.divider))
            .bg(rgb(colors.panel))
            .child(
                gpui_kit::base::ToggleGroup::new("chat-display-mode")
                    .aria_label("Transcript display")
                    .flex()
                    .flex_none()
                    .gap(ui_text::space(1.0))
                    .p(ui_text::space(2.0))
                    .rounded(ui_text::space(6.0))
                    .border_1()
                    .border_color(rgb(colors.divider))
                    .bg(rgb(colors.panel_active))
                    .children(
                        [super::DisplayMode::Normal, super::DisplayMode::Verbose]
                            .into_iter()
                            .map(|mode| {
                                widgets::toggle_button(
                                    if mode == super::DisplayMode::Normal {
                                        "chat-display-normal"
                                    } else {
                                        "chat-display-verbose"
                                    },
                                    mode.label(),
                                    (self.display_mode == mode).then_some(colors.cyan),
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
            .child(picker(
                "chat-model",
                placeholder(toolbar::model_label(models, model.as_deref()), "model"),
                Menu::Model,
                cx,
            ))
            .children(
                toolbar::effort_available(models, model.as_deref(), provider).then(|| {
                    // An effort is a lowercase word; Native starts it with a capital.
                    picker(
                        "chat-effort",
                        widgets::sentence(
                            &toolbar::effort_label(models, model.as_deref(), effort.as_deref()),
                            look,
                        ),
                        Menu::Effort,
                        cx,
                    )
                }),
            )
            .children(toolbar::fast_available(models, model.as_deref()).then(|| {
                // Native's is a capsule toggle with the bolt symbol: grey while off, and in
                // the working color with the bolt filled while on, as the mic shows it listens.
                let toggle = if look.native {
                    widgets::toggle_capsule("chat-fast", "Fast", fast, look)
                        .pl(ui_text::space(8.0))
                        .child(icons::symbol(
                            if fast { "bolt.fill" } else { "bolt" },
                            10.0,
                            None,
                        ))
                        .flex_row_reverse()
                        .when(fast, |toggle| {
                            toggle
                                .bg(rgb(look.tint(colors.working, 0.18)))
                                .text_color(rgb(colors.working))
                        })
                        .when(!fast, |toggle| toggle.text_color(rgb(colors.muted)))
                        .cursor_pointer()
                        .hover(move |style| {
                            style.bg(rgb(if fast {
                                look.tint(colors.working, 0.28)
                            } else {
                                Button::Secondary.hover(colors)
                            }))
                        })
                } else {
                    widgets::toggle_button(
                        "chat-fast",
                        toolbar::fast_label(fast),
                        fast.then_some(colors.cyan),
                        fast,
                        look,
                    )
                };
                div()
                    .relative()
                    .child(toggle.on_change({
                        let owner = cx.weak_entity();
                        move |_, _, _, cx| {
                            let _ = owner.update(cx, |view, cx| view.toggle_fast(cx));
                        }
                    }))
                    .child(tooltip::anchor(
                        "Fast mode answers sooner and uses more of your limits",
                        TipLook::Control,
                    ))
            }))
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
                toolbar::model_rows(&self.model.transcript.models, current)
                    .into_iter()
                    .map(|line| {
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
                    })
                    .collect()
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
                        .child(text_input::input(
                            "chat-model-input",
                            &self.model_input,
                            window,
                            cx,
                        ))
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
        // The last button of the toolbar opens its menu toward the left, into the window.
        let toward_left = matches!(menu, Menu::More | Menu::ConfirmDelete);
        let popover = div()
            .id("chat-choices-popover")
            .absolute()
            .top(relative(1.0))
            .when(toward_left, |menu| menu.right(px(0.0)))
            .when(!toward_left, |menu| menu.left(px(0.0)))
            .mt(ui_text::space(3.0))
            .min_w(ui_text::space(220.0))
            .max_w(ui_text::space(340.0))
            .py(ui_text::space(4.0))
            .rounded(px(4.0))
            .border_1()
            .border_color(rgb(colors.magenta))
            .bg(rgb(colors.panel_active));
        deferred(
            controls::native(popover, |menu| controls::menu(menu, colors))
                .occlude()
                .role(gpui::Role::Menu)
                .aria_label("Chat choices")
                .on_mouse_down_out(cx.listener(|view, _, _, cx| view.close_menu(cx)))
                .children(content),
        )
        .with_priority(10)
        .into_any_element()
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

    fn composer_box(&self, look: Look, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = look.colors;
        let running = self.running();
        let dictation = self.dictation.phase();
        // The mic, its key and its hints only while Settings shows it in chats.
        let mic = dictate::mic_shown(cx);
        let hint = composer::hint(dictation, running, mic);
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(ui_text::space(4.0))
            .px(ui_text::space(12.0))
            .py(ui_text::space(8.0))
            .border_t_1()
            .border_color(rgb(colors.divider))
            .bg(rgb(colors.panel))
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
            .child(
                // The buttons keep to the bottom, each centered on the box's last line (see
                // `widgets::beside_field`), so they share one center line with the box.
                div()
                    .w_full()
                    .flex()
                    .items_end()
                    .gap(ui_text::space(8.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(self.composer_editor(look, window, cx)),
                    )
                    .children(mic.then(|| widgets::beside_field(self.mic_button(look, cx))))
                    // Native's are round symbol buttons beside the field, as a message field
                    // has them; their keys are in the tooltips and the hint below. Send waits
                    // in grey until there is something to send.
                    .children(running.then(|| {
                        widgets::beside_field(
                            if look.native {
                                widgets::round_button(
                                    "chat-interrupt",
                                    "stop.fill",
                                    "Interrupt · ⌘.",
                                    Button::Secondary,
                                    look,
                                )
                            } else {
                                button(
                                    "chat-interrupt",
                                    "Interrupt  ⌘.",
                                    Some(look.diff.removed),
                                    look,
                                )
                            }
                            .on_click(cx.listener(|view, _, _, cx| view.interrupt(cx))),
                        )
                    }))
                    .child(widgets::beside_field(
                        if look.native {
                            let empty = self.draft_empty(cx) || self.pending_submission.is_some();
                            widgets::round_button(
                                "chat-send",
                                "arrow.up",
                                "Send · ⏎",
                                if empty {
                                    Button::Disabled
                                } else {
                                    Button::Primary
                                },
                                look,
                            )
                        } else {
                            button("chat-send", "Send  ⏎", Some(colors.cyan), look)
                                .disabled(self.draft_empty(cx) || self.pending_submission.is_some())
                        }
                        .on_click(cx.listener(|view, _, window, cx| view.send_message(window, cx))),
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui_text::space(6.))
                    .child(button("chat-attach", "Attach…", None, look).on_click(
                        cx.listener(|view, _, window, cx| view.attach_picker(window, cx)),
                    ))
                    .child(
                        div()
                            .text_size(ui_text::text(9.))
                            .text_color(rgb(colors.muted))
                            .child("UTF-8 text · PNG · JPEG · paste or drop files"),
                    ),
            )
            .child(
                div()
                    .id("chat-submission-reviews")
                    .max_h(ui_text::space(280.))
                    .overflow_y_scroll()
                    .child(self.submission_cards(look, cx)),
            )
            .child(
                div()
                    .text_size(ui_text::text(9.0))
                    .text_color(rgb(if dictation.is_active() {
                        colors.text
                    } else {
                        colors.muted
                    }))
                    .child(hint),
            )
            .into_any_element()
    }

    /// The mic beside Send: click to dictate, click again to stop; ⌃⌥D does the same. Native
    /// draws it as the round buttons beside it: a mic, filled in the working color while it
    /// listens and pulsing while it gets ready or settles. The colorful themes write it out.
    fn mic_button(&self, look: Look, cx: &mut Context<Self>) -> AnyElement {
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
        let mic = if look.native {
            let symbol = match phase {
                Phase::Listening { .. } | Phase::Preparing { .. } | Phase::Finishing { .. } => {
                    "mic.fill"
                }
                Phase::Failed(_) => "mic.slash",
                Phase::Idle => "mic",
            };
            widgets::round_toggle("chat-dictate", symbol, tooltip, phase.is_active(), look)
                .when(phase.is_active(), |mic| mic.text_color(rgb(colors.working)))
        } else {
            let label = match phase {
                Phase::Listening { .. } => format!("● Stop  {key}"),
                Phase::Preparing { .. } | Phase::Finishing { .. } => "Mic …".to_owned(),
                _ => format!("Mic  {key}"),
            };
            widgets::toggle_button(
                "chat-dictate",
                label,
                phase.is_active().then_some(colors.working),
                phase.is_active(),
                look,
            )
            .child(tooltip::anchor(tooltip, TipLook::Control))
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
        let editor = text_input::on_paste(
            text_input::textarea("chat-composer", &self.composer, window, cx),
            &self.composer,
            move |item, window, cx| {
                weak.update(cx, |view, cx| view.paste_attachments(item, window, cx))
                    .unwrap_or(false)
            },
        )
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
        .px(ui_text::space(8.))
        .py(ui_text::space(widgets::FIELD_PAD_Y))
        .text_size(ui_text::text(widgets::FIELD_TEXT))
        .font_family(ui_text::ui_family())
        .capture_action(cx.listener(Self::capture_enter));
        div()
            .id("chat-composer-shell")
            .relative()
            .w_full()
            .border_1()
            .bg(transparent_black())
            .border_color(transparent_black())
            .child(surface)
            .child(editor)
            .on_drop(
                cx.listener(|view, paths: &gpui::ExternalPaths, window, cx| {
                    view.dropped_attachments(paths, window, cx)
                }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::{QuestionOption, QuestionPrompt};

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
