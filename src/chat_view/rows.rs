//! The rows of the message list: one per transcript item, and a footer after them.

use std::sync::Arc;

use gpui::{AnyElement, Context, ElementId, SharedString, Window, div, prelude::*, px, rgb};

use crate::{
    chat::model::{ChangeKind, FileChange, Item, ItemBody, NoticeLevel, Step, StepStatus},
    controls, icons, theme, ui_text,
};

use super::{
    ChatView, DrawnDiff, Link,
    cards::{self, Head},
    diff::{self, LineKind},
    markdown,
    state::provider_name,
    widgets::{self, Look, chevron},
};

/// Output lines and bytes a command card shows; the rest is counted and can be copied.
const OUTPUT_LINES: usize = 200;
const OUTPUT_BYTES: usize = 32 * 1024;

/// Makes the body of a card, which is made only for a card that is open.
type BodyMaker<'a> = dyn Fn(&ChatView, &mut Context<ChatView>) -> AnyElement + 'a;

fn id(parts: impl Into<String>) -> ElementId {
    ElementId::Name(SharedString::from(parts.into()))
}

impl ChatView {
    pub(super) fn render_row(
        &mut self,
        ix: usize,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let look = Look::of(cx);
        let content = match self.visible.get(ix) {
            Some(row @ (super::display::Row::Item(at) | super::display::Row::Details(at))) => {
                let item = &self.model.transcript.items[*at];
                div()
                    .w_full()
                    .child(self.item(*at, item, look, cx))
                    .when(matches!(row, super::display::Row::Item(_)), |row| {
                        row.children(item.presentation.images.iter().enumerate().map(
                            |(n, image)| {
                                self.image_card(
                                    image,
                                    &format!("{}:image:{n}", item.id),
                                    Some(item),
                                    None,
                                    look,
                                    cx,
                                )
                            },
                        ))
                    })
                    .into_any_element()
            }
            Some(super::display::Row::Artifacts { turn_id, items }) => {
                let key = format!("artifacts:{turn_id}");
                let open = self.open.contains(&key);
                let count: usize = items
                    .iter()
                    .map(|at| self.model.transcript.items[*at].presentation.images.len())
                    .sum();
                let group_items = items.clone();
                let range = super::media::artifact_range(
                    count,
                    self.media.artifact_pages.get(&key).copied().unwrap_or(0),
                );
                let previous_key = key.clone();
                let next_key = key.clone();
                let previous_page = range.start / super::media::ARTIFACT_PAGE;
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(ui_text::space(4.0))
                    .child(
                        widgets::button(
                            SharedString::from(key.clone()),
                            format!(
                                "{} Images from this turn · {count}",
                                if open { "▾" } else { "▸" }
                            ),
                            None,
                            look,
                        )
                        .aria_expanded(open)
                        .on_click(cx.listener(move |view, _, _, cx| {
                            let offset = view.list.logical_scroll_top();
                            view.list.pause_following_tail();
                            view.toggle(&key, None, cx);
                            if !view.open.contains(&key) {
                                let image_keys: Vec<String> = group_items
                                    .iter()
                                    .flat_map(|at| {
                                        let item = &view.model.transcript.items[*at];
                                        (0..item.presentation.images.len())
                                            .map(move |n| format!("{}:image:{n}", item.id))
                                    })
                                    .collect();
                                view.fold_group_images(&image_keys, cx);
                            }
                            view.list.remeasure();
                            view.list.scroll_to(offset);
                        })),
                    )
                    .when(open, |row| {
                        row.children(
                            items
                                .iter()
                                .flat_map(|at| {
                                    let item = &self.model.transcript.items[*at];
                                    item.presentation
                                        .images
                                        .iter()
                                        .enumerate()
                                        .map(move |(n, image)| (item, n, image))
                                })
                                .enumerate()
                                .skip(range.start)
                                .take(range.len())
                                .map(|(ordinal, (item, n, image))| {
                                    self.image_card(
                                        image,
                                        &format!("{}:image:{n}", item.id),
                                        Some(item),
                                        Some((ordinal, count)),
                                        look,
                                        cx,
                                    )
                                }),
                        )
                    })
                    .when(open && count > super::media::ARTIFACT_PAGE, |row| {
                        row.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(ui_text::space(6.0))
                                .children((range.start > 0).then(|| {
                                    widgets::button(
                                        SharedString::from(format!("previous:{previous_key}")),
                                        "Previous images",
                                        None,
                                        look,
                                    )
                                    .on_click(cx.listener(
                                        move |view, _, _, cx| {
                                            let offset = view.list.logical_scroll_top();
                                            view.list.pause_following_tail();
                                            view.media.artifact_pages.insert(
                                                previous_key.clone(),
                                                previous_page.saturating_sub(1),
                                            );
                                            view.list.remeasure();
                                            view.list.scroll_to(offset);
                                            cx.notify();
                                        },
                                    ))
                                }))
                                .child(format!("{}–{} of {count}", range.start + 1, range.end))
                                .children((range.end < count).then(|| {
                                    widgets::button(
                                        SharedString::from(format!("next:{next_key}")),
                                        "Next images",
                                        None,
                                        look,
                                    )
                                    .on_click(cx.listener(
                                        move |view, _, _, cx| {
                                            let offset = view.list.logical_scroll_top();
                                            view.list.pause_following_tail();
                                            view.media
                                                .artifact_pages
                                                .insert(next_key.clone(), previous_page + 1);
                                            view.list.remeasure();
                                            view.list.scroll_to(offset);
                                            cx.notify();
                                        },
                                    ))
                                })),
                        )
                    })
                    .into_any_element()
            }
            Some(super::display::Row::Outcome(turn, outcome)) => div()
                .text_size(ui_text::text(11.0))
                .text_color(rgb(look.colors.muted))
                .child(self.selectable(
                    &format!("outcome:{turn}"),
                    super::display::outcome_text(outcome),
                    Vec::new(),
                    Vec::new(),
                    look,
                    cx,
                ))
                .into_any_element(),
            None => self.footer(look),
        };
        div()
            .w_full()
            .px(ui_text::space(14.0))
            .py(ui_text::space(4.0))
            .child(
                div()
                    .w_full()
                    .max_w(ui_text::space(920.0))
                    .mx_auto()
                    .child(content),
            )
            .into_any_element()
    }

    fn item(&self, ix: usize, item: &Item, look: Look, cx: &mut Context<Self>) -> AnyElement {
        let colors = look.colors;
        match &item.body {
            ItemBody::UserMessage { text } => div()
                .w_full()
                .flex()
                .justify_end()
                .child(
                    div()
                        .max_w(gpui::relative(0.85))
                        .px(ui_text::space(10.0))
                        .py(ui_text::space(6.0))
                        // Native's is a plain grey bubble, as Messages draws one.
                        .when(look.native, |bubble| {
                            bubble
                                .px(ui_text::space(12.0))
                                .rounded(controls::radius(BUBBLE_RADIUS))
                                .bg(rgb(colors.panel_active))
                        })
                        .when(!look.native, |bubble| {
                            bubble
                                .rounded(px(6.0))
                                .border_1()
                                .border_color(rgb(look.tint(colors.cyan, 0.45)))
                                .bg(rgb(look.tint(colors.cyan, 0.10)))
                        })
                        .child(self.prose(
                            &self.parsed(item, text),
                            &format!("user:{}", item.id),
                            look,
                            cx,
                        )),
                )
                .into_any_element(),
            ItemBody::AgentMessage { text } => self.agent_message(ix, item, text, look, cx),
            ItemBody::Reasoning { text } => {
                let head = cards::head(item, None).expect("reasoning has a head");
                let body =
                    |view: &Self, cx: &mut Context<Self>| view.reasoning_body(item, text, look, cx);
                self.card(ix, item, &head, Some(&body), look, cx)
            }
            ItemBody::Command {
                command, output, ..
            } => {
                let head = cards::head(item, None).expect("a command has a head");
                let body = |view: &Self, cx: &mut Context<Self>| {
                    view.command_body(item, command, output, look, cx)
                };
                self.card(ix, item, &head, Some(&body), look, cx)
            }
            ItemBody::FileChange { changes } => {
                let stats = self.change_stats(item, changes);
                let head = cards::head(item, Some(&stats)).expect("a file change has a head");
                let body = |view: &Self, cx: &mut Context<Self>| {
                    view.changes_body(ix, item, changes, &stats, look, cx)
                };
                self.card(ix, item, &head, Some(&body), look, cx)
            }
            ItemBody::ToolCall { input, output, .. } => {
                let head = cards::head(item, None).expect("a tool call has a head");
                let body = |view: &Self, cx: &mut Context<Self>| {
                    view.tool_body(item, input, output.as_deref(), look, cx)
                };
                self.card(ix, item, &head, Some(&body), look, cx)
            }
            ItemBody::Plan { explanation, steps } => {
                let head = cards::head(item, None).expect("a plan has a head");
                let body = checklist(self, &item.id, steps, explanation.as_deref(), look, cx);
                self.always_open(ix, &head, body, look)
            }
            ItemBody::Todo { items } => {
                let head = cards::head(item, None).expect("a todo list has a head");
                self.always_open(
                    ix,
                    &head,
                    checklist(self, &item.id, items, None, look, cx),
                    look,
                )
            }
            ItemBody::WebSearch { .. } => {
                let head = cards::head(item, None).expect("a search has a head");
                self.card(ix, item, &head, None, look, cx)
            }
            ItemBody::Compaction => div()
                .w_full()
                .flex()
                .items_center()
                .gap(ui_text::space(10.0))
                .text_size(ui_text::text(10.0))
                .text_color(rgb(colors.muted))
                .child(div().flex_1().h(px(1.0)).bg(rgb(colors.divider)))
                .child(self.selectable(
                    &format!("compaction:{}", item.id),
                    widgets::sentence("context compacted", look),
                    Vec::new(),
                    Vec::new(),
                    look,
                    cx,
                ))
                .child(div().flex_1().h(px(1.0)).bg(rgb(colors.divider)))
                .into_any_element(),
            ItemBody::Notice { level, text, .. } if look.native => {
                let color = look.tone(cards::notice_tone(*level));
                div()
                    .w_full()
                    .flex()
                    .items_start()
                    .gap(ui_text::space(6.0))
                    .text_size(ui_text::text(11.0))
                    .text_color(rgb(color))
                    .children(notice_symbol(*level).map(|symbol| {
                        controls::on_first_line(
                            icons::symbol(symbol, 11.0, Some(color)),
                            widgets::BODY_LINE,
                        )
                    }))
                    .child(div().flex_1().min_w_0().child(self.selectable(
                        &format!("notice:{}", item.id),
                        text.clone(),
                        Vec::new(),
                        Vec::new(),
                        look,
                        cx,
                    )))
                    .into_any_element()
            }
            ItemBody::Notice { level, text, .. } => div()
                .w_full()
                .text_size(ui_text::text(11.0))
                .text_color(rgb(look.tone(cards::notice_tone(*level))))
                .child(self.selectable(
                    &format!("notice:{}", item.id),
                    format!(
                        "{}{text}",
                        match level {
                            NoticeLevel::Info => "",
                            NoticeLevel::Warning => "! ",
                            NoticeLevel::Error => "✕ ",
                        }
                    ),
                    Vec::new(),
                    Vec::new(),
                    look,
                    cx,
                ))
                .into_any_element(),
        }
    }

    fn agent_message(
        &self,
        ix: usize,
        item: &Item,
        text: &str,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = look.colors;
        let blocks = self.parsed(item, text);
        let copy_key = format!("copy:{}", item.id);
        let copied = self.copied.as_deref() == Some(copy_key.as_str());
        let message = item.id.clone();
        let group = SharedString::from(format!("agent-{ix}"));
        div()
            .group(group.clone())
            .relative()
            .w_full()
            .child(self.prose(&blocks, &item.id, look, cx))
            .child(
                div()
                    .absolute()
                    .top(px(0.0))
                    .right(px(0.0))
                    .opacity(0.0)
                    .group_hover(group, |style| style.opacity(1.0))
                    .child(
                        widgets::copy_button(id(copy_key), copied, "copy", "Copy message", look)
                            .when(!look.native, |button| button.bg(rgb(colors.panel_active)))
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.copy_item(&message, cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    /// The message's blocks, parsed again only when its text has changed.
    pub(super) fn parsed(&self, item: &Item, text: &str) -> Arc<Vec<markdown::Block>> {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        let hash = hasher.finish();
        let mut caches = self.caches.borrow_mut();
        if let Some((known, blocks)) = caches.markdown.get(&item.id)
            && *known == hash
        {
            return blocks.clone();
        }
        let blocks = Arc::new(markdown::parse(text));
        caches
            .markdown
            .insert(item.id.clone(), (hash, blocks.clone()));
        blocks
    }

    /// Counts follow the exact source, including same-length streaming edits.
    fn change_stats(&self, item: &Item, changes: &[FileChange]) -> cards::FileStats {
        let mut caches = self.caches.borrow_mut();
        if let Some((known, stats)) = caches.changes.get(&item.id)
            && known == changes
        {
            return stats.clone();
        }
        let stats: cards::FileStats = changes
            .iter()
            .map(|change| match &change.diff {
                Some(text) => diff::stats(&diff::classify(text, change.kind)),
                None => (0, 0),
            })
            .collect();
        caches
            .changes
            .insert(item.id.clone(), (changes.to_vec(), stats.clone()));
        stats
    }

    /// A card: its head, always shown, and its body, shown while the card is open.
    fn card(
        &self,
        ix: usize,
        item: &Item,
        head: &Head,
        body: Option<&BodyMaker<'_>>,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = look.colors;
        let open = self.open.contains(&item.id) && head.expandable;
        let key = item.id.clone();
        // A command is code; the subtitles are folders, paths, arguments and line counts.
        let code_title = matches!(item.body, ItemBody::Command { .. });
        let header = div()
            .w_full()
            .flex()
            .items_center()
            .gap(ui_text::space(6.0))
            .px(ui_text::space(10.0))
            .py(ui_text::space(6.0))
            .text_size(ui_text::text(11.0))
            .when(head.expandable, |header| {
                header.cursor_pointer().hover(move |style| {
                    controls::hovered(style, controls::row_hover(false, colors), |style| {
                        style.bg(rgb(colors.panel_active))
                    })
                })
            })
            .child(chevron(open, head.expandable, colors.muted))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(rgb(colors.text))
                    .when(code_title, |title| {
                        title.font_family(ui_text::code_family())
                    })
                    .child(head.title.clone()),
            )
            .children(head.subtitle.clone().map(|subtitle| {
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(ui_text::mono_family())
                    .text_color(rgb(colors.muted))
                    .child(subtitle)
            }))
            .when(head.subtitle.is_none(), |header| {
                header.child(div().flex_1())
            })
            .children(
                head.badge
                    .as_ref()
                    .map(|badge| widgets::badge(id(format!("badge:{}", item.id)), badge, look)),
            );
        let header = widgets::content_button(
            id(format!("card:{}", item.id)),
            head.title.clone(),
            header,
            look,
        )
        .disabled(!head.expandable)
        .aria_expanded(open)
        .on_click(cx.listener(move |view, _, _, cx| view.toggle(&key, Some(ix), cx)));
        card_box(look)
            .child(header)
            // The body is made only for a card that is open.
            .children(body.filter(|_| open).map(|body| {
                div()
                    .border_t_1()
                    .border_color(rgb(colors.divider))
                    .child(body(self, cx))
            }))
            .into_any_element()
    }

    /// A card whose body is its content: a plan, a todo list.
    fn always_open(&self, ix: usize, head: &Head, body: AnyElement, look: Look) -> AnyElement {
        let colors = look.colors;
        card_box(look)
            .child(
                div()
                    .id(id(format!("plan:{ix}")))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(ui_text::space(8.0))
                    .px(ui_text::space(10.0))
                    .py(ui_text::space(6.0))
                    .text_size(ui_text::text(11.0))
                    .child(
                        div()
                            .text_color(rgb(colors.cyan))
                            .when(look.native, |title| {
                                title.font_weight(gpui::FontWeight::SEMIBOLD)
                            })
                            .child(head.title.clone()),
                    )
                    .children(head.subtitle.clone().map(|subtitle| {
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(rgb(colors.muted))
                            .child(subtitle)
                    })),
            )
            .child(
                div()
                    .border_t_1()
                    .border_color(rgb(colors.divider))
                    .child(body),
            )
            .into_any_element()
    }

    fn reasoning_body(
        &self,
        item: &Item,
        text: &str,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .px(ui_text::space(10.0))
            .py(ui_text::space(6.0))
            .italic()
            .text_color(rgb(look.colors.muted))
            .child(self.selectable(
                &format!("thought:{}", item.id),
                text.trim().to_owned(),
                Vec::new(),
                Vec::new(),
                look,
                cx,
            ))
            .into_any_element()
    }

    fn command_body(
        &self,
        item: &Item,
        command: &str,
        output: &str,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = look.colors;
        // Only the end of the output is read, however long it is: lines are the same before and
        // after cleaning, and the whole of it is cleaned when it is copied.
        let (end, hidden) = cards::tail(output, OUTPUT_LINES, OUTPUT_BYTES);
        let shown = cards::clean_output(end);
        let copy_key = format!("copy:{}", item.id);
        let copied = self.copied.as_deref() == Some(copy_key.as_str());
        let command_id = item.id.clone();
        let scroller = self.scroller(&format!("output:{}", item.id));
        div()
            .w_full()
            .flex()
            .flex_col()
            .when(command.trim().contains('\n'), |body| {
                body.child(
                    div()
                        .px(ui_text::space(10.0))
                        .py(ui_text::space(6.0))
                        .text_size(ui_text::text(11.0))
                        .font_family(ui_text::code_family())
                        .text_color(rgb(colors.cyan))
                        .border_b_1()
                        .border_color(rgb(colors.divider))
                        .child(self.selectable(
                            &format!("command:{}", item.id),
                            command.trim().to_owned(),
                            Vec::new(),
                            Vec::new(),
                            look,
                            cx,
                        )),
                )
            })
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(ui_text::space(10.0))
                    .pt(ui_text::space(4.0))
                    .text_size(ui_text::text(10.0))
                    .text_color(rgb(colors.muted))
                    .child(if hidden > 0 {
                        format!("{hidden} earlier lines not shown")
                    } else {
                        widgets::sentence("output", look)
                    })
                    .child(
                        widgets::copy_button(
                            id(copy_key),
                            copied,
                            "copy output",
                            "Copy output",
                            look,
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.copy_item(&command_id, cx);
                        })),
                    ),
            )
            .child(
                scroller.attach(
                    div()
                        .id(id(format!("output:{}", item.id)))
                        .w_full()
                        .max_h(ui_text::space(280.0))
                        .flex()
                        // Items keep their own height, so what is taller than the box scrolls.
                        .items_start()
                        .overflow_scroll()
                        .px(ui_text::space(10.0))
                        .py(ui_text::space(6.0))
                        .bg(rgb(colors.bg))
                        .text_size(ui_text::text(11.0))
                        .font_family(ui_text::code_family())
                        .child(div().flex_none().min_w_full().whitespace_nowrap().child(
                            self.selectable(
                                &format!("output:{}", item.id),
                                shown,
                                Vec::new(),
                                Vec::new(),
                                look,
                                cx,
                            ),
                        )),
                ),
            )
            .into_any_element()
    }

    fn changes_body(
        &self,
        ix: usize,
        item: &Item,
        changes: &[FileChange],
        stats: &cards::FileStats,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = look.colors;
        let only_one = changes.len() == 1;
        div()
            .w_full()
            .flex()
            .flex_col()
            .children(changes.iter().enumerate().map(|(at, change)| {
                let key = format!("{}#{at}", item.id);
                let has_diff = change.diff.as_ref().is_some_and(|d| !d.trim().is_empty());
                // A card of one file shows its diff as soon as the card opens.
                let open = has_diff && (only_one || self.open.contains(&key));
                let (letter, word) = cards::change_badge(change.kind);
                let letter_color = match change.kind {
                    ChangeKind::Add => look.diff.added,
                    ChangeKind::Delete => look.diff.removed,
                    // Native keeps its signal color for state: an edit is in its secondary.
                    ChangeKind::Modify | ChangeKind::Rename if look.native => colors.magenta,
                    ChangeKind::Modify | ChangeKind::Rename => colors.gold,
                };
                let (added, removed) = stats.get(at).copied().unwrap_or((0, 0));
                let header = div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(ui_text::space(8.0))
                    .px(ui_text::space(10.0))
                    .py(ui_text::space(4.0))
                    .text_size(ui_text::text(11.0))
                    .when(has_diff && !only_one, |row| {
                        row.cursor_pointer().hover(move |style| {
                            controls::hovered(style, controls::row_hover(false, colors), |style| {
                                style.bg(rgb(colors.panel_active))
                            })
                        })
                    })
                    .child(
                        div()
                            .flex_none()
                            .w(ui_text::space(14.))
                            .text_color(rgb(letter_color))
                            .child(letter),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(ui_text::mono_family())
                            .child(change.path.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(rgb(colors.muted))
                            .child(widgets::sentence(word, look)),
                    )
                    .when(added > 0, |row| {
                        row.child(
                            div()
                                .flex_none()
                                .text_color(rgb(look.diff.added))
                                .child(format!("+{added}")),
                        )
                    })
                    .when(removed > 0, |row| {
                        row.child(
                            div()
                                .flex_none()
                                .text_color(rgb(look.diff.removed))
                                .child(format!("−{removed}")),
                        )
                    })
                    .when(has_diff && !only_one, |row| {
                        row.child(chevron(open, true, colors.muted))
                    });
                let toggle_key = key.clone();
                let header = widgets::content_button(
                    id(format!("file:{key}")),
                    change.path.clone(),
                    header,
                    look,
                )
                .disabled(!has_diff || only_one)
                .aria_expanded(open)
                .on_click(
                    cx.listener(move |view, _, _, cx| view.toggle(&toggle_key, Some(ix), cx)),
                );
                div()
                    .w_full()
                    .when(at > 0, |file| {
                        file.border_t_1().border_color(rgb(colors.divider))
                    })
                    .child(header)
                    .children(
                        change
                            .diff
                            .as_ref()
                            .filter(|_| open)
                            .map(|text| self.diff_lines(&key, text, change.kind, look, cx)),
                    )
            }))
            .into_any_element()
    }

    fn tool_body(
        &self,
        item: &Item,
        input: &serde_json::Value,
        output: Option<&str>,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = look.colors;
        let pretty = cards::input_pretty(input);
        let mut part = |label: &'static str, text: String, at: usize| {
            let name = format!("tool-{label}:{}", item.id);
            div()
                .w_full()
                .px(ui_text::space(10.0))
                .py(ui_text::space(6.0))
                .when(at > 0, |part| {
                    part.border_t_1().border_color(rgb(colors.divider))
                })
                .child(
                    div()
                        .text_size(ui_text::text(10.0))
                        .text_color(rgb(colors.muted))
                        .child(widgets::sentence(label, look)),
                )
                .child(
                    self.scroller(&name).attach(
                        div()
                            .id(id(name.clone()))
                            .max_h(ui_text::space(240.0))
                            .flex()
                            // Items keep their own height, so what is taller than the box scrolls.
                            .items_start()
                            .overflow_scroll()
                            .text_size(ui_text::text(11.0))
                            .font_family(ui_text::code_family())
                            .child(div().flex_none().min_w_full().whitespace_nowrap().child(
                                self.selectable(&name, text, Vec::new(), Vec::new(), look, cx),
                            )),
                    ),
                )
        };
        // The end of what a tool printed is what is read, as for a command.
        let output = output
            .filter(|text| !text.is_empty())
            .map(|text| cards::clean_output(cards::tail(text, OUTPUT_LINES, OUTPUT_BYTES).0));
        let input = (!pretty.is_empty()).then(|| {
            cards::tail(&pretty, OUTPUT_LINES, OUTPUT_BYTES)
                .0
                .to_owned()
        });
        let output_at = usize::from(input.is_some());
        let input = input.map(|text| part("input", text, 0));
        let output = output.map(|text| part("output", text, output_at));
        div()
            .w_full()
            .flex()
            .flex_col()
            .children(input)
            .children(output)
            .into_any_element()
    }

    /// The row after the transcript: what the chat is doing, and how the tab reaches it.
    fn footer(&self, look: Look) -> AnyElement {
        use crate::chat::model::ChatState;
        let colors = look.colors;
        let transcript = &self.model.transcript;
        let status = match (&transcript.state, self.model.link) {
            (_, Link::Reconnecting) => Some(("Reconnecting…", colors.muted, false)),
            (_, Link::Connecting) if transcript.items.is_empty() => {
                Some(("Connecting…", colors.muted, false))
            }
            (ChatState::Starting, _) => Some(("Starting…", colors.muted, true)),
            (ChatState::Running, _) => Some(("Working…", colors.working, true)),
            (ChatState::Waiting, _) => Some(("Waiting for you", colors.gold, true)),
            _ => None,
        };
        let empty = transcript.items.is_empty() && self.model.link == Link::Live;
        let title = match self.provider() {
            Some(provider) => format!("{} chat", provider_name(provider)),
            None => "Chat".to_owned(),
        };
        let cwd = transcript
            .info
            .as_ref()
            .map(|info| info.cwd.display().to_string());
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(ui_text::space(8.0))
            .pb(ui_text::space(10.0))
            // Native's empty state, as its panels have one: a muted symbol over the title.
            .when(empty && look.native, |footer| {
                footer.child(
                    div()
                        .w_full()
                        .py(ui_text::space(48.0))
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(ui_text::space(6.0))
                        .text_size(ui_text::text(11.0))
                        .text_color(rgb(colors.muted))
                        .child(icons::symbol(
                            "bubble.left.and.bubble.right",
                            22.0,
                            Some(theme::mix(colors.muted, colors.bg, 0.25)),
                        ))
                        .child(
                            div()
                                .pt(ui_text::space(4.0))
                                .text_size(ui_text::text(controls::TITLE_TEXT))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(rgb(colors.text))
                                .child(title.clone()),
                        )
                        .children(cwd.clone().map(|cwd| {
                            div()
                                .max_w_full()
                                .truncate()
                                .text_size(ui_text::text(controls::META_TEXT))
                                .font_family(ui_text::mono_family())
                                .child(cwd)
                        }))
                        .child("Type a message below to begin."),
                )
            })
            .when(empty && !look.native, |footer| {
                footer.child(
                    div()
                        .w_full()
                        .py(ui_text::space(40.0))
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(ui_text::space(6.0))
                        .text_color(rgb(colors.muted))
                        .child(
                            div()
                                .text_size(ui_text::text(14.0))
                                .text_color(rgb(colors.text))
                                .child(title),
                        )
                        .children(cwd.map(|cwd| div().child(cwd)))
                        .child("Type a message below to begin."),
                )
            })
            .children(status.map(|(label, color, live)| {
                div()
                    .flex()
                    .items_center()
                    .gap(ui_text::space(8.0))
                    .text_size(ui_text::text(11.0))
                    .text_color(rgb(color))
                    .when(empty, |line| line.justify_center())
                    .when(live, |line| {
                        line.child(widgets::pulse("footer-pulse", color))
                    })
                    .child(label)
            }))
            .into_any_element()
    }
}

/// A user message bubble's corner radius under Native.
const BUBBLE_RADIUS: f32 = 12.0;

/// A card's surface: Native rounds it like its rows and lets the hairline be its edge.
fn card_box(look: Look) -> gpui::Div {
    let colors = look.colors;
    div()
        .w_full()
        .rounded(if look.native {
            controls::radius(controls::ROW_RADIUS)
        } else {
            px(4.0)
        })
        .border_1()
        .border_color(rgb(colors.divider))
        .bg(rgb(colors.panel))
        .overflow_hidden()
}

/// The SF Symbol Native shows before a notice: none for one that only informs.
fn notice_symbol(level: NoticeLevel) -> Option<&'static str> {
    match level {
        NoticeLevel::Info => None,
        NoticeLevel::Warning => Some("exclamationmark.triangle"),
        NoticeLevel::Error => Some("xmark.octagon"),
    }
}

/// The SF Symbol Native shows for a step, in place of `cards::step_mark`.
fn step_symbol(status: StepStatus) -> &'static str {
    match status {
        StepStatus::Pending => "circle",
        StepStatus::InProgress => "circle.lefthalf.filled",
        StepStatus::Completed => "checkmark.circle.fill",
    }
}

/// The steps of a plan or todo list, each with its mark.
fn checklist(
    view: &ChatView,
    key: &str,
    steps: &[Step],
    explanation: Option<&str>,
    look: Look,
    cx: &mut Context<ChatView>,
) -> AnyElement {
    let colors = look.colors;
    div()
        .w_full()
        .px(ui_text::space(10.0))
        .py(ui_text::space(6.0))
        .flex()
        .flex_col()
        .gap(ui_text::space(3.0))
        .children(explanation.map(|text| {
            div()
                .pb(ui_text::space(4.0))
                .text_color(rgb(colors.muted))
                .child(view.selectable(
                    &format!("plan:{key}:explanation"),
                    text.trim().to_owned(),
                    Vec::new(),
                    Vec::new(),
                    look,
                    cx,
                ))
        }))
        .children(steps.iter().enumerate().map(|(at, step)| {
            let (mark_color, text_color) = match step.status {
                StepStatus::Pending => (colors.muted, colors.text),
                // Native says what is under way with its mark alone, in the working color.
                StepStatus::InProgress if look.native => (colors.working, colors.text),
                StepStatus::InProgress => (colors.cyan, colors.cyan),
                StepStatus::Completed => (look.diff.added, colors.muted),
            };
            let mark = if look.native {
                controls::on_first_line(
                    icons::symbol(step_symbol(step.status), 10.0, Some(mark_color)),
                    widgets::BODY_LINE,
                )
                .into_any_element()
            } else {
                div()
                    .text_color(rgb(mark_color))
                    .child(cards::step_mark(step.status))
                    .into_any_element()
            };
            div()
                .w_full()
                .flex()
                .items_start()
                .gap(ui_text::space(8.0))
                .text_size(ui_text::text(11.0))
                .child(
                    div()
                        .flex_none()
                        .w(ui_text::space(if look.native { 14.0 } else { 12.0 }))
                        .child(mark),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(rgb(text_color))
                        .child(view.selectable(
                            &format!("plan:{key}:{at}"),
                            step.text.clone(),
                            Vec::new(),
                            Vec::new(),
                            look,
                            cx,
                        )),
                )
        }))
        .into_any_element()
}

impl ChatView {
    /// The lines of one file's diff, added and removed ones tinted. `key` names the file's
    /// box; text/kind changes invalidate the renderer even at the same byte length.
    fn diff_lines(
        &self,
        key: &str,
        text: &str,
        kind: ChangeKind,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = look.colors;
        let drawn = {
            let mut caches = self.caches.borrow_mut();
            match caches.diffs.get(key) {
                Some((previous, previous_kind, drawn))
                    if previous == text && *previous_kind == kind =>
                {
                    drawn.clone()
                }
                _ => {
                    let (lines, hidden) = diff::display(text, kind);
                    let drawn = Arc::new(DrawnDiff {
                        lines: lines
                            .iter()
                            .map(|line| (line.kind, line.text.replace('\t', "    ")))
                            .collect(),
                        hidden,
                    });
                    caches
                        .diffs
                        .insert(key.to_owned(), (text.to_owned(), kind, drawn.clone()));
                    drawn
                }
            }
        };
        let name = format!("diff:{key}");
        div()
            .border_t_1()
            .border_color(rgb(colors.divider))
            .child(
                self.scroller(&name).attach(
                    div()
                        .id(id(name))
                        .w_full()
                        .max_h(ui_text::space(360.0))
                        .flex()
                        // Items keep their own height, so what is taller than the box scrolls.
                        .items_start()
                        .overflow_scroll()
                        .bg(rgb(colors.bg))
                        .text_size(ui_text::text(11.0))
                        .font_family(ui_text::code_family())
                        .child(
                            div()
                                .flex_none()
                                .min_w_full()
                                .flex()
                                .flex_col()
                                .children(drawn.lines.iter().enumerate().map(
                                    |(at, (kind, shown))| {
                                        let (ink, tint) = match kind {
                                            LineKind::Add => {
                                                (look.diff.added, Some(look.diff.added))
                                            }
                                            LineKind::Remove => {
                                                (look.diff.removed, Some(look.diff.removed))
                                            }
                                            LineKind::Hunk => (colors.cyan, Some(colors.cyan)),
                                            LineKind::Header | LineKind::Note => {
                                                (colors.muted, None)
                                            }
                                            LineKind::Context => (colors.text, None),
                                        };
                                        div()
                                            .min_w_full()
                                            .flex_none()
                                            .px(ui_text::space(10.0))
                                            .text_color(rgb(ink))
                                            .when_some(tint, |row, tint| {
                                                row.bg(rgb(look.tint(
                                                    tint,
                                                    if *kind == LineKind::Hunk {
                                                        0.08
                                                    } else {
                                                        0.16
                                                    },
                                                )))
                                            })
                                            .whitespace_nowrap()
                                            .child(self.selectable(
                                                &format!("diff:{key}:{at}"),
                                                if shown.is_empty() {
                                                    " ".to_owned()
                                                } else {
                                                    shown.clone()
                                                },
                                                Vec::new(),
                                                Vec::new(),
                                                look,
                                                cx,
                                            ))
                                    },
                                ))
                                .children((drawn.hidden > 0).then(|| {
                                    div()
                                        .px(ui_text::space(10.0))
                                        .py(ui_text::space(4.0))
                                        .italic()
                                        .text_color(rgb(colors.muted))
                                        .child(format!("… {} more lines", drawn.hidden))
                                })),
                        ),
                ),
            )
            .into_any_element()
    }
}
