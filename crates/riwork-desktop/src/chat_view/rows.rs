//! The rows of the message list: one per transcript item, and a footer after them.

use std::{
    cell::Cell,
    collections::HashMap,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    Animation, AnimationExt, AnyElement, ClickEvent, Context, ElementId, MouseButton, SharedString,
    StyledText, Window, canvas, div, prelude::*, px, rgb,
};
#[cfg(test)]
use gpui_kit::base::ElementExt as _;
use gpui_kit::base::TestSupportExt as _;

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

/// Every design's transcript geometry, in design points: a row's inset from the pane's sides
/// and from the rows around it, and the widest the reading column gets, centered in the pane.
const ROW_INSET_X: f32 = 20.0;
const ROW_INSET_Y: f32 = 1.0;
const COLUMN_WIDTH: f32 = 1040.0;

/// A native worker/subagent result is constructed only when disclosed.
type BodyMaker<'a> = dyn Fn(&ChatView, &mut Context<ChatView>) -> AnyElement + 'a;

const HEADER_HOVER_DURATION: Duration = Duration::from_millis(100);

#[derive(Clone)]
struct HeaderHover {
    entered: bool,
    from: f32,
    since: Instant,
    generation: u64,
}

impl HeaderHover {
    fn level(&self) -> f32 {
        let progress =
            (self.since.elapsed().as_secs_f32() / HEADER_HOVER_DURATION.as_secs_f32()).min(1.0);
        self.from + ((if self.entered { 1.0 } else { 0.0 }) - self.from) * progress
    }
}

#[derive(Default)]
pub(super) struct HeaderHoverState {
    hover: HashMap<String, HeaderHover>,
    generation: u64,
}

/// Standard headers are 24 pt, with 2 pt gaps and 12 pt disclosed indentation
/// at each theme's default text size. Larger accessibility text grows these
/// dimensions relative to that default; SF Pro's normal 13 pt face is not zoom.
fn row_space(base: f32) -> gpui::Pixels {
    let default = ui_text::DEFAULT_POINTS + ui_text::face().offset(false);
    px(base * (ui_text::scale() * ui_text::REFERENCE_SIZE / default).max(1.0))
}

/// The content column starts exactly below the call title, after its border,
/// padding, chevron, icon and the three header gaps.
pub(super) fn text_indent() -> gpui::Pixels {
    px(1.0)
        + ui_text::space(8.0) * 3.0
        + ui_text::space(if ui_text::is_native() { 12.0 } else { 10.0 })
        + ui_text::space(18.0)
}

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
        self.ensure_selection_row(ix, look, cx);
        if let Some(entry) = self.list_rows.get(ix).filter(|entry| entry.row.open) {
            if let Some((owner, images)) = self
                .projected_item(&entry.row.id)
                .filter(|(_, item)| !item.presentation.images.is_empty())
                .map(|(_, item)| (item.id.clone(), item.presentation.images.clone()))
            {
                self.prepare_opened_images(&owner, &images, cx);
            }
        }
        let content = match self.list_rows.get(ix) {
            Some(entry) => div()
                .pl(row_space(12.0 * entry.depth as f32))
                .child(self.render_projected_row(&entry.row, ix, entry.depth, look, cx))
                .into_any_element(),
            None => self.footer(look),
        };
        let geometry = self.header_bounds.clone();
        div()
            .id(id(format!("transcript-row:{ix}")))
            .relative()
            .child(
                canvas(
                    move |bounds, _, _| {
                        geometry.borrow_mut().insert(format!("top:{ix}"), bounds);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .w_full()
            .min_w_0()
            .px(ui_text::space(ROW_INSET_X))
            .py(row_space(ROW_INSET_Y))
            .child(
                div()
                    .id(id(format!("transcript-content:{ix}")))
                    .w_full()
                    .min_w_0()
                    .max_w(ui_text::space(COLUMN_WIDTH))
                    .mx_auto()
                    .child(content)
                    .test_support(),
            )
            .test_support()
            .into_any_element()
    }

    /// Engine rows retain all children while closed. Rendering follows that projection;
    /// no provider output is inspected to decide grouping or disclosure.
    pub(super) fn render_projected_row(
        &self,
        row: &super::display::Row,
        ix: usize,
        depth: usize,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use riwork_engine::chat::projection::RowKind;
        #[cfg(test)]
        self.rendered_rows.set(self.rendered_rows.get() + 1);
        let item = self.projected_item(&row.id);
        let artifacts: Vec<_> = row
            .artifacts
            .iter()
            .filter(|artifact| {
                matches!(
                    row.kind,
                    RowKind::User
                        | RowKind::Final
                        | RowKind::Commentary
                        | RowKind::Compaction
                        | RowKind::Notice
                ) || item.is_none_or(|(_, item)| {
                    !super::media::artifact_shown_inline(artifact, item, row.open)
                })
            })
            .cloned()
            .collect();
        let content = match row.kind {
            RowKind::Activity | RowKind::Group => div()
                .w_full()
                .min_w_0()
                .child(self.projected_header(row, None, look, cx))
                .when(
                    row.kind == RowKind::Activity && !row.artifacts.is_empty(),
                    |body| {
                        body.child(div().pl(row_space(12.0)).child(self.render_artifact_refs(
                            &row.id,
                            &row.artifacts,
                            look,
                            cx,
                        )))
                    },
                )
                .when(row.open, |body| {
                    body.child(
                        div()
                            .pl(row_space(12.0))
                            .flex()
                            .flex_col()
                            .gap(row_space(2.0))
                            // Flattened transcript entries have no children here;
                            // their descendants are separate virtual list entries.
                            .children(row.children.iter().map(|child| {
                                self.render_projected_row(child, ix, depth + 1, look, cx)
                            })),
                    )
                })
                .children(
                    row.comments
                        .iter()
                        .map(|comment| self.render_projected_row(comment, ix, depth + 1, look, cx)),
                )
                .into_any_element(),
            RowKind::Request => div()
                .h(row_space(24.0))
                .overflow_hidden()
                .w_full()
                .px(ui_text::space(8.0))
                .flex()
                .items_center()
                .gap(ui_text::space(8.0))
                .text_size(ui_text::text(12.0))
                .text_color(rgb(look.colors.muted))
                .child(icons::symbol(
                    "checkmark.shield",
                    12.0,
                    Some(look.colors.muted),
                ))
                .child(cards::first_line(&row.title))
                .into_any_element(),
            RowKind::Commentary => div()
                .w_full()
                .px(ui_text::space(12.0))
                .text_size(ui_text::text(12.0))
                .text_color(rgb(look.colors.muted))
                .children(item.and_then(|(_, item)| match &item.body {
                    ItemBody::AgentMessage { text } => Some(self.selectable(
                        &item.id,
                        text.clone(),
                        Vec::new(),
                        Vec::new(),
                        look,
                        cx,
                    )),
                    _ => None,
                }))
                .into_any_element(),
            RowKind::Worker | RowKind::Subagent
                if item.is_some_and(|(_, item)| {
                    crate::chat::workers::of(item).is_some()
                        || crate::chat::subagents::in_transcript(
                            item,
                            &self.model.transcript,
                            self.info().map(|info| info.provider),
                        )
                        .is_some()
                }) =>
            {
                item.map(|(at, item)| self.item(at, item, look, cx))
                    .unwrap_or_else(|| self.projected_header(row, None, look, cx))
            }
            RowKind::User | RowKind::Final | RowKind::Compaction | RowKind::Notice => div()
                .w_full()
                .when(matches!(row.kind, RowKind::User | RowKind::Final), |body| {
                    body.py(ui_text::space(7.0))
                })
                .children(item.map(|(at, item)| self.item(at, item, look, cx)))
                .into_any_element(),
            _ => match item {
                Some((at, item)) => {
                    let stats = match &item.body {
                        ItemBody::FileChange { changes } => Some(self.change_stats(item, changes)),
                        _ => None,
                    };
                    let head = cards::head(item, stats.as_ref());
                    div()
                        .w_full()
                        .min_w_0()
                        .child(self.projected_header(row, head.as_ref(), look, cx))
                        .when(row.open, |body| {
                            body.child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(row_space(2.0))
                                    .child(self.projected_body(at, item, look, cx))
                                    .when(!row.children.is_empty(), |body| {
                                        body.child(
                                            div()
                                                .pl(row_space(12.0))
                                                .flex()
                                                .flex_col()
                                                .gap(row_space(2.0))
                                                .children(row.children.iter().map(|child| {
                                                    self.render_projected_row(
                                                        child,
                                                        ix,
                                                        depth + 1,
                                                        look,
                                                        cx,
                                                    )
                                                })),
                                        )
                                    })
                                    .when(!item.presentation.images.is_empty(), |body| {
                                        body.child(
                                            div()
                                                .pl(text_indent())
                                                .flex()
                                                .flex_col()
                                                .items_start()
                                                .gap(row_space(4.0))
                                                .children(
                                                    item.presentation
                                                        .images
                                                        .iter()
                                                        .enumerate()
                                                        .map(|(n, image)| {
                                                            self.image_card(
                                                                image,
                                                                &format!("{}:image:{n}", item.id),
                                                                Some(item),
                                                                None,
                                                                look,
                                                                cx,
                                                            )
                                                        }),
                                                ),
                                        )
                                    }),
                            )
                        })
                        .into_any_element()
                }
                None => self.projected_header(row, None, look, cx),
            },
        };
        div()
            .id(id(format!("projected:{}", row.id)))
            .group(SharedString::from(format!("tc:{}", row.id)))
            .w_full()
            .min_w_0()
            .child(content)
            .when(
                !matches!(row.kind, RowKind::User | RowKind::Activity) && !artifacts.is_empty(),
                |body| {
                    body.child(
                        div()
                            .pl(row_space(12.0))
                            .child(self.render_artifact_refs(&row.id, &artifacts, look, cx)),
                    )
                },
            )
            .into_any_element()
    }

    fn projected_header(
        &self,
        row: &super::display::Row,
        head: Option<&Head>,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use riwork_engine::chat::projection::RowKind;
        let colors = look.colors;
        let activity = row.kind == RowKind::Activity;
        let group = row.kind == RowKind::Group;
        let key = row.id.clone();
        let hover_key = key.clone();
        let mut meta = self
            .projected_item(&row.id)
            .map(|(_, item)| cards::metadata(item, head))
            .unwrap_or_default();
        if !activity && let Some(duration) = row.duration_ms {
            meta.push(cards::duration(duration));
        }
        if group
            && row.running
            && let Some(label) = &row.current_label
        {
            meta.push(cards::first_line(label));
        }
        if row.provisional {
            meta.push("Partial turn".into());
        }
        let badge = head.and_then(|head| head.badge.clone()).or_else(|| {
            if row.running {
                Some(cards::Badge {
                    label: "Running".into(),
                    tone: cards::Tone::Accent,
                    live: true,
                })
            } else if row.failed_count > 0 && !activity {
                Some(cards::Badge {
                    label: format!("{} failed", row.failed_count),
                    tone: cards::Tone::Error,
                    live: false,
                })
            } else {
                None
            }
        });
        let item = self.projected_item(&row.id).map(|(_, item)| item);
        let thumbnail = item.and_then(|item| self.image_header_thumbnail(item, look, cx));
        let height = row_space(24.0);
        let geometry = self.header_bounds.clone();
        let geometry_key = row.id.clone();
        let header = div()
            .relative()
            .child(
                canvas(
                    move |bounds, _, _| {
                        geometry.borrow_mut().insert(geometry_key, bounds);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .h_full()
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(ui_text::space(8.0))
            .px(ui_text::space(8.0))
            .text_size(ui_text::text(12.0))
            .line_height(gpui::relative(1.0))
            .text_color(rgb(if activity { colors.muted } else { colors.text }))
            .child(chevron(row.open, true, colors.muted))
            .when(!activity, |header| {
                header.child(
                    div()
                        .flex_none()
                        .size(ui_text::space(18.0))
                        .rounded(px(5.0))
                        .bg(rgb(colors.panel_active))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(if row.running {
                            widgets::spinner(id(format!("row-spinner:{}", row.id)), colors.working)
                        } else {
                            icons::symbol(kind_symbol(row.kind), 11.0, Some(colors.muted))
                        }),
                )
            })
            .when(activity && row.running, |header| {
                header.child(widgets::spinner(
                    id(format!("activity-spinner:{}", row.id)),
                    colors.working,
                ))
            })
            .child({
                let title = div()
                    .min_w_0()
                    .truncate()
                    .when(group, |title| title.font_weight(gpui::FontWeight::MEDIUM));
                #[cfg(test)]
                let title = {
                    let geometry = self.header_bounds.clone();
                    let key = format!("label:{}", row.id);
                    title.on_prepaint(move |bounds, _, _| {
                        geometry.borrow_mut().insert(key, bounds);
                    })
                };
                title.child(self.header_label(row))
            })
            .when(!meta.is_empty() && !activity, |header| {
                header.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(rgb(colors.muted))
                        .child(format!("· {}", meta.join(" · "))),
                )
            })
            .child(div().flex_1())
            .when(activity && row.failed_count > 0, |header| {
                header.child(
                    div()
                        .size(px(5.0))
                        .rounded(px(3.0))
                        .bg(rgb(look.diff.removed)),
                )
            })
            .when(group && row.running && row.failed_count > 0, |header| {
                header.child(widgets::badge(
                    id(format!("failure-count:{}", row.id)),
                    &cards::Badge {
                        label: format!("{} failed", row.failed_count),
                        tone: cards::Tone::Error,
                        live: false,
                    },
                    look,
                ))
            })
            .when(!row.open, |header| header.children(thumbnail))
            .when(!activity, |header| {
                header.children(
                    badge
                        .as_ref()
                        .map(|badge| widgets::badge(id(format!("badge:{}", row.id)), badge, look)),
                )
            });
        let control = widgets::content_button(
            id(format!("card:{}", row.id)),
            format!(
                "{}{}{}",
                row.title,
                row.target
                    .as_ref()
                    .map(|target| format!(" {target}"))
                    .unwrap_or_default(),
                if row.failed_count > 0 {
                    format!(" · {} failed", row.failed_count)
                } else {
                    String::new()
                }
            ),
            self.header_hover_background(&row.id, look, cx),
            look,
        )
        .relative()
        .child(header)
        .h(height)
        .overflow_hidden()
        .rounded(px(4.0))
        .cursor_pointer()
        .when(group, |header| header.bg(rgb(colors.panel)))
        .border_1()
        .border_color(if self.row_is_focused(&row.id) {
            gpui::rgba((colors.muted << 8) | 0x66)
        } else {
            gpui::rgba(0)
        })
        .focus_visible(move |style| {
            style
                .bg(gpui::transparent_black())
                .border_color(gpui::rgba((colors.muted << 8) | 0x66))
        })
        .aria_expanded(row.open)
        .on_hover(cx.listener(move |view, entered: &bool, _, cx| {
            let state = &mut view.header_hover;
            if state
                .hover
                .get(&hover_key)
                .is_some_and(|hover| hover.entered == *entered)
                || !*entered && !state.hover.contains_key(&hover_key)
            {
                return;
            }
            let from = state.hover.get(&hover_key).map_or(0.0, HeaderHover::level);
            state.hover.retain(|key, hover| {
                key == &hover_key || hover.since.elapsed() < HEADER_HOVER_DURATION
            });
            state.generation += 1;
            let generation = state.generation;
            state.hover.insert(
                hover_key.clone(),
                HeaderHover {
                    entered: *entered,
                    from,
                    since: Instant::now(),
                    generation,
                },
            );
            cx.notify();
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|view, _, _, cx| {
                gpui_kit::base::GlobalState::suppress_text_selection(cx);
                view.row_keyboard_focus = false;
                cx.notify();
            }),
        )
        .on_click(cx.listener(move |view, event: &ClickEvent, window, cx| {
            window.focus(&view.focus, cx);
            view.row_keyboard_focus = matches!(event, ClickEvent::Keyboard(_));
            view.toggle_projected(&key, cx);
        }));
        control.into_any_element()
    }

    /// Header content is decoration. Only the enclosing row owns hit testing,
    /// pointer styling, disclosure and keyboard focus.
    fn header_label(&self, row: &super::display::Row) -> StyledText {
        let item = self.projected_item(&row.id).map(|(_, item)| item);
        let text = super::display::header_text(row, item);
        let target = super::display::header_target(row, item);
        let range = target.and_then(|target| text.rfind(&target).map(|at| at..at + target.len()));
        let styled = StyledText::new(text);
        if let Some(range) = range {
            // Split the native text run before applying the command/path font.
            styled
                .with_highlights([(range.clone(), gpui::HighlightStyle::default())])
                .with_font_family_overrides([(range, ui_text::code_family())])
        } else {
            styled
        }
    }

    /// Animate decoration only, preserving the Base button's focus identity.
    fn header_hover_background(&self, row: &str, look: Look, cx: &Context<Self>) -> AnyElement {
        let Some(fade) = self.header_hover.hover.get(row).cloned() else {
            return div().absolute().inset_0().into_any_element();
        };
        let entered = fade.entered;
        let generation = fade.generation;
        let owner = cx.weak_entity();
        let key = row.to_owned();
        let level = Rc::new(Cell::new(fade.from));
        let painted_level = level.clone();
        canvas(
            move |bounds, window, cx| {
                // The canvas is inside the row's reserved focus border. Hover
                // belongs to the complete 24px container, including that edge.
                let inside = bounds
                    .dilate(px(1.0))
                    .intersect(&window.content_mask().bounds)
                    .contains(&window.mouse_position());
                if entered && !inside {
                    let owner = owner.clone();
                    let key = key.clone();
                    window.defer(cx, move |_, cx| {
                        let _ = owner.update(cx, |view, cx| {
                            if view.header_hover.hover.get(&key).is_some_and(|hover| {
                                hover.entered && hover.generation == generation
                            }) {
                                view.header_hover.hover.remove(&key);
                                cx.notify();
                            }
                        });
                    });
                }
                inside
            },
            move |bounds, pointer_inside, window, _| {
                // A virtualized row can disappear before receiving mouse-exit.
                // Its cached enter state must never tint it after remounting.
                if !entered || pointer_inside {
                    let fill = gpui::rgba(
                        (look.colors.divider << 8) | (painted_level.get() * 255.0).round() as u32,
                    );
                    window.paint_quad(gpui::fill(bounds, fill).corner_radii(px(4.0)));
                }
            },
        )
        .absolute()
        .inset_0()
        .with_animation(
            id(format!("header-hover:{row}:{}", fade.generation)),
            Animation::new(HEADER_HOVER_DURATION),
            move |background, progress| {
                let to = if fade.entered { 1.0 } else { 0.0 };
                level.set(fade.from + (to - fade.from) * progress);
                background
            },
        )
        .into_any_element()
    }

    fn projected_body(
        &self,
        ix: usize,
        item: &Item,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let content = match &item.body {
            ItemBody::Reasoning { text } => self.reasoning_body(item, text, look, cx),
            ItemBody::Command {
                command, output, ..
            } => self.command_body(item, command, output, look, cx),
            ItemBody::ToolCall { input, output, .. } => {
                self.tool_body(item, input, output.as_deref(), look, cx)
            }
            ItemBody::FileChange { changes } => self.changes_body(
                ix,
                item,
                changes,
                &self.change_stats(item, changes),
                look,
                cx,
            ),
            ItemBody::Plan { explanation, steps } => {
                checklist(self, &item.id, steps, explanation.as_deref(), look, cx)
            }
            ItemBody::Todo { items } => checklist(self, &item.id, items, None, look, cx),
            ItemBody::WebSearch { query } => div()
                .text_color(rgb(look.colors.muted))
                .child(query.clone())
                .into_any_element(),
            _ => div().into_any_element(),
        };
        if matches!(item.body, ItemBody::ToolCall { .. }) {
            content
        } else {
            div()
                .pl(if matches!(item.body, ItemBody::Command { .. }) {
                    text_indent()
                } else {
                    row_space(12.0)
                })
                .child(content)
                .into_any_element()
        }
    }

    fn item(&self, ix: usize, item: &Item, look: Look, cx: &mut Context<Self>) -> AnyElement {
        let colors = look.colors;
        if let Some(worker) = crate::chat::workers::of(item) {
            return self.worker_card(ix, item, worker, look, cx);
        }
        if let Some(run) = crate::chat::subagents::in_transcript(
            item,
            &self.model.transcript,
            self.info().map(|info| info.provider),
        ) {
            use crate::chat::subagents::RunStatus;
            let head = Head {
                title: format!("Subagent · {}", run.name),
                subtitle: run.purpose.clone(),
                badge: Some(cards::Badge {
                    label: run.status.label().into(),
                    tone: match run.status {
                        RunStatus::Running => cards::Tone::Accent,
                        RunStatus::Failed => cards::Tone::Error,
                        RunStatus::Declined => cards::Tone::Warning,
                        _ => cards::Tone::Muted,
                    },
                    live: run.status == RunStatus::Running,
                }),
                expandable: run.summary.is_some() || !run.children.is_empty(),
            };
            let summary = run.result_text();
            let body = |view: &Self, cx: &mut Context<Self>| {
                div()
                    .id(id(format!("subagent-result:{}", item.id)))
                    .px(ui_text::space(10.0))
                    .py(ui_text::space(6.0))
                    .child(view.selectable(
                        &format!("subagent:{}", item.id),
                        summary.clone(),
                        Vec::new(),
                        Vec::new(),
                        look,
                        cx,
                    ))
                    .test_support()
                    .into_any_element()
            };
            return self.card(ix, item, &head, Some(&body), look, cx);
        }
        match &item.body {
            // What you sent is a block across the reading column, in every design; Native's
            // is plain grey with its rounded corners, Hermes's a bordered panel, the colorful
            // themes' tinted in their accent.
            ItemBody::UserMessage { text } => {
                let sent = self.sent_attachments(item, look, cx);
                let text = super::attachment_ui::shown_text(item, text);
                div()
                    .w_full()
                    .flex()
                    .child(
                        div()
                            .id(id(format!("user-message:{}", item.id)))
                            .w_full()
                            .min_w_0()
                            .px(ui_text::space(12.0))
                            .py(ui_text::space(8.0))
                            .when(look.native, |block| {
                                block
                                    .rounded(controls::radius(BUBBLE_RADIUS))
                                    .bg(rgb(colors.panel_active))
                            })
                            .when(look.hermes(), |block| {
                                block
                                    .rounded(px(4.0))
                                    .border_1()
                                    .border_color(rgb(colors.divider))
                                    .bg(rgb(colors.panel))
                            })
                            .when(!look.native && !look.hermes(), |block| {
                                block
                                    .rounded(px(6.0))
                                    .border_1()
                                    .border_color(rgb(look.tint(colors.cyan, 0.45)))
                                    .bg(rgb(look.tint(colors.cyan, 0.10)))
                            })
                            .when(sent.is_some(), |block| {
                                block.flex().flex_col().gap(ui_text::space(8.0))
                            })
                            .children(sent)
                            .children(text.map(|text| {
                                self.prose(
                                    &self.parsed(item, text),
                                    &format!("user:{}", item.id),
                                    look,
                                    cx,
                                )
                            }))
                            .children(
                                item.presentation
                                    .images
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, image)| {
                                        !super::attachment_ui::covered(item, image)
                                    })
                                    .map(|(n, image)| {
                                        self.image_card(
                                            image,
                                            &format!("{}:image:{n}", item.id),
                                            Some(item),
                                            None,
                                            look,
                                            cx,
                                        )
                                    }),
                            )
                            .test_support(),
                    )
                    .into_any_element()
            }
            ItemBody::AgentMessage { text } => self.agent_message(ix, item, text, look, cx),
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
            _ => div().into_any_element(),
        }
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
            .id(id(format!("agent-message:{}", item.id)))
            .group(group.clone())
            .relative()
            .w_full()
            .min_w_0()
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
            .test_support()
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

    fn reasoning_body(
        &self,
        item: &Item,
        text: &str,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
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
        self.output_block(item, output, "output", Some(command), look, cx)
    }

    fn tool_overlay_button(
        &self,
        item: &Item,
        key: String,
        symbol: &'static str,
        label: &'static str,
        enabled: bool,
        copied: bool,
        look: Look,
    ) -> crate::behavior_controls::Button {
        widgets::tool_icon_action(id(key), symbol, label, enabled, look)
            .bg(rgb(look.colors.panel))
            .opacity(if copied || self.row_is_focused(&item.id) {
                1.0
            } else {
                0.0
            })
            .group_hover(SharedString::from(format!("tc:{}", item.id)), |style| {
                style.opacity(1.0)
            })
    }

    fn container_label(&self, text: &str, meta_width: f32) -> String {
        let pane = self.composer_width.get();
        let pane = if pane > 0.0 { pane } else { 700.0 };
        let available = pane.min(f32::from(ui_text::space(COLUMN_WIDTH)))
            - f32::from(
                ui_text::space(ROW_INSET_X * 2.0) + text_indent() + ui_text::space(16.0 + 48.0),
            )
            - meta_width;
        let characters = (available / f32::from(ui_text::text(11.0) * 0.62))
            .floor()
            .max(8.0) as usize;
        crate::file_preview::middle_elide(text, characters)
    }

    pub(super) fn command_container_label(&self, command: &str) -> String {
        let first = command.trim().lines().next().unwrap_or_default();
        self.container_label(
            &format!(
                "$ {first}{}",
                if command.trim().lines().count() > 1 {
                    " …"
                } else {
                    ""
                }
            ),
            f32::from(ui_text::space(160.0)),
        )
    }

    pub(super) fn file_container_label(&self, path: &str) -> String {
        self.container_label(&short_output_path(path), f32::from(ui_text::space(140.0)))
    }

    fn output_header(
        &self,
        item: &Item,
        command: &str,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let running = item.status == crate::chat::model::ItemStatus::InProgress;
        let (exit, cwd) = match &item.body {
            ItemBody::Command { exit_code, cwd, .. } => (*exit_code, cwd.as_deref()),
            _ => (None, None),
        };
        let duration = super::display::find(&self.visible, &item.id)
            .and_then(|row| row.duration_ms)
            .or_else(|| {
                item.started_at
                    .zip(item.ended_at)
                    .map(|(start, end)| end.saturating_sub(start))
            });
        let command_label = self.command_container_label(command);
        let menu_key = format!("command-copy-menu:{}", item.id);
        let right_key = menu_key.clone();
        let close_key = menu_key.clone();
        let copy_key = menu_key.clone();
        let full_command = command.to_owned();
        let command_id = item.id.clone();
        div()
            .id(id(format!("output-header:{}", item.id)))
            .relative()
            .h(ui_text::space(18.0))
            .mb(ui_text::space(4.0))
            .pr(ui_text::space(48.0))
            .flex()
            .items_center()
            .gap(ui_text::space(4.0))
            .font_family(ui_text::code_family())
            .text_size(ui_text::text(11.0))
            .text_color(rgb(look.colors.muted))
            .child(div().min_w_0().truncate().child(self.selectable(
                &format!("command:{}", item.id),
                command_label,
                Vec::new(),
                Vec::new(),
                look,
                cx,
            )))
            .children(exit.filter(|exit| *exit != 0).map(|exit| {
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(ui_text::space(4.0))
                    .child("·")
                    .child(
                        div()
                            .text_color(rgb(look.error()))
                            .child(format!("exit {exit}")),
                    )
            }))
            .when(running, |header| {
                let seconds = duration.unwrap_or(0) / 1000;
                header
                    .child("·")
                    .child(div().size(ui_text::space(10.0)).overflow_hidden().child(
                        widgets::spinner(
                            id(format!("output-spinner:{}", item.id)),
                            look.colors.working,
                        ),
                    ))
                    .child(format!("{}:{:02}", seconds / 60, seconds % 60))
            })
            .children((!running).then_some(duration).flatten().map(|duration| {
                div()
                    .flex_none()
                    .child(format!("· {}", precise_duration(duration)))
            }))
            .children(cwd.filter(|cwd| !cwd.is_empty()).map(|cwd| {
                div()
                    .min_w_0()
                    .truncate()
                    .child(format!("· {}", short_output_path(cwd)))
            }))
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(move |view, _, _, cx| {
                    cx.stop_propagation();
                    view.toggle(&right_key, None, cx);
                }),
            )
            .children(self.open.contains(&menu_key).then(|| {
                div()
                    .absolute()
                    .top(ui_text::space(18.0))
                    .left_0()
                    .p(ui_text::space(4.0))
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(rgb(look.colors.divider))
                    .bg(rgb(look.colors.panel))
                    .shadow_sm()
                    .on_mouse_down_out(cx.listener(move |view, _, _, cx| {
                        view.open.remove(&close_key);
                        cx.notify();
                    }))
                    .child(
                        widgets::button(
                            id(format!("copy-command:{}", item.id)),
                            "Copy command",
                            None,
                            look,
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            cx.stop_propagation();
                            view.open.remove(&copy_key);
                            view.copy(
                                format!("command-copy:{command_id}"),
                                full_command.clone(),
                                cx,
                            );
                        })),
                    )
            }))
            .test_support()
            .into_any_element()
    }

    fn output_lines(
        &self,
        name: &str,
        preview: &cards::OutputPreview,
        empty_message: bool,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if empty_message {
            return div()
                .id(id(format!("output-placeholder:{name}")))
                .w_full()
                .text_color(rgb(look.colors.muted))
                .role(gpui::Role::Label)
                .aria_label("Waiting for output…")
                .child("Waiting for output…")
                .test_support()
                .into_any_element();
        }
        div()
            .w_full()
            .flex()
            .flex_col()
            .children(cards::output_runs(preview).into_iter().map(|run| {
                let omitted = run.kind == cards::OutputRunKind::Omission;
                let error = run.kind == cards::OutputRunKind::Error;
                let key = run.selection_key(name);
                let text = run.text;
                div()
                    .id(id(format!("output-line:{name}:{}", run.first_line)))
                    .w_full()
                    .min_h(ui_text::text(11.5) * 1.45)
                    .when(error, |line| line.bg(rgb(look.tint(look.error(), 0.12))))
                    .when(error, |line| line.text_color(rgb(look.diff.removed)))
                    .when(omitted, |line| line.text_color(rgb(look.colors.muted)))
                    .when(omitted, |line| line.italic())
                    .when(omitted, |line| {
                        line.role(gpui::Role::Label)
                            .aria_label(text.clone())
                            .child(text.clone())
                    })
                    .when_some(key, |element, key| {
                        let len = text.len();
                        element.child(self.selectable(
                            &key,
                            text,
                            if error {
                                vec![(
                                    0..len,
                                    gpui::HighlightStyle {
                                        color: Some(rgb(look.diff.removed).into()),
                                        ..Default::default()
                                    },
                                )]
                            } else {
                                Vec::new()
                            },
                            Vec::new(),
                            look,
                            cx,
                        ))
                    })
                    .test_support()
            }))
            .into_any_element()
    }

    fn output_footer(
        &self,
        item: &Item,
        omitted: usize,
        show_all: bool,
        file: bool,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let all_key = format!("output-all:{}", item.id);
        let item_ix = self.projected_item(&item.id).map(|(at, _)| at);
        div()
            .id(id(format!("output-footer:{}", item.id)))
            .h(ui_text::space(20.0))
            .w_full()
            .flex()
            .items_center()
            .gap(ui_text::space(4.0))
            .font_family(ui_text::ui_family())
            .text_size(ui_text::text(11.0))
            .text_color(rgb(look.colors.muted))
            .when(omitted > 0, |footer| {
                footer.child(format!("{} ·", cards::omission_label(omitted, file)))
            })
            .child(
                crate::behavior_controls::button_content(
                    id(format!("show-all:{}", item.id)),
                    if show_all { "Show less" } else { "Show all" },
                    if show_all { "Show less" } else { "Show all" },
                )
                .text_color(rgb(if look.native {
                    look.colors.focus
                } else {
                    look.colors.cyan
                }))
                .underline()
                .cursor_pointer()
                .on_click(cx.listener(move |view, _, _, cx| {
                    cx.stop_propagation();
                    view.toggle(&all_key, item_ix, cx);
                })),
            )
            .when(show_all && omitted > 0, |footer| {
                footer.child("· Full output copied by ⧉")
            })
            .test_support()
            .into_any_element()
    }

    fn output_block(
        &self,
        item: &Item,
        output: &str,
        prefix: &str,
        command: Option<&str>,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = look.colors;
        let running = item.status == crate::chat::model::ItemStatus::InProgress;
        let show_all = self.open.contains(&format!("output-all:{}", item.id));
        let preview = cards::output_preview(output, running, show_all);
        let compact_omits =
            show_all && !running && cards::output_preview(output, false, false).omitted > 0;
        let has_body = !preview.text.is_empty() || running;
        let waiting = running && preview.text.is_empty();
        let name = format!("{prefix}:{}", item.id);
        let copy_key = format!("copy:{}", item.id);
        let copied = self.copied.as_deref() == Some(copy_key.as_str());
        let command_id = item.id.clone();
        div()
            .id(id(format!("output-block:{}", item.id)))
            .relative()
            .cursor_text()
            .w_full()
            .min_w_0()
            .rounded(px(4.0))
            .overflow_hidden()
            .border_1()
            .border_color(rgb(colors.divider))
            .bg(rgb(colors.panel))
            .py(ui_text::space(6.0))
            .px(ui_text::space(8.0))
            .text_size(ui_text::text(11.5))
            .line_height(gpui::relative(1.45))
            .font_family(ui_text::code_family())
            .text_color(rgb(colors.text))
            .children(command.map(|command| self.output_header(item, command, look, cx)))
            .when(has_body, |container| {
                container.child(
                    self.scroller(&name).attach(
                        div()
                            .id(id(name.clone()))
                            .w_full()
                            .max_h(ui_text::space(320.0))
                            .overflow_y_scroll()
                            .child(self.output_lines(&name, &preview, waiting, look, cx))
                            .test_support(),
                    ),
                )
            })
            .when(
                !running && (preview.omitted > 0 || show_all && compact_omits),
                |container| {
                    container.child(self.output_footer(
                        item,
                        preview.omitted,
                        show_all,
                        false,
                        look,
                        cx,
                    ))
                },
            )
            .child(
                div()
                    .absolute()
                    .top(ui_text::space(4.0))
                    .right(ui_text::space(4.0))
                    .child(
                        self.tool_overlay_button(
                            item,
                            copy_key,
                            if copied { "checkmark" } else { "doc.on.doc" },
                            "Copy output",
                            true,
                            copied,
                            look,
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            cx.stop_propagation();
                            view.copy_item(&command_id, cx);
                        })),
                    ),
            )
            .test_support()
            .into_any_element()
    }

    /// Rendering only consults this cache. Filesystem validation runs off-window,
    /// and a stale completion cannot replace a newer target or scope's result.
    fn excerpt_open_available(&self, item: &Item, target: &str, cx: &mut Context<Self>) -> bool {
        let mut caches = self.caches.borrow_mut();
        if let Some(known) = caches.excerpt_open.get(&item.id)
            && known.target == target
            && known.status == item.status
        {
            return known.available.unwrap_or(false);
        }
        caches.excerpt_open_request += 1;
        let request = caches.excerpt_open_request;
        caches.excerpt_open.insert(
            item.id.clone(),
            super::ExcerptOpenAvailability {
                target: target.to_owned(),
                status: item.status,
                request,
                available: None,
            },
        );
        drop(caches);
        let owner = item.id.clone();
        let target = target.to_owned();
        let scope = self.image_scope.clone();
        let work = cx.background_executor().spawn(async move {
            scope.resolve(&target).is_ok_and(|resolved| {
                crate::file_preview::validated_live_editor_path(&resolved.root, &resolved.path)
                    .is_ok()
            })
        });
        cx.spawn(async move |this, cx| {
            let available = work.await;
            let _ = this.update(cx, |view, cx| {
                let mut caches = view.caches.borrow_mut();
                if let Some(known) = caches.excerpt_open.get_mut(&owner)
                    && known.request == request
                {
                    known.available = Some(available);
                    drop(caches);
                    cx.notify();
                }
            });
        })
        .detach();
        false
    }

    fn file_excerpt(
        &self,
        item: &Item,
        output: &str,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let semantic = item.presentation.semantics.as_ref();
        let target = semantic
            .and_then(|s| s.target.as_deref())
            .unwrap_or_default();
        let start = semantic
            .and_then(|s| s.range.as_ref())
            .map(|range| range.start as usize);
        let lines = cards::numbered_excerpt(output, start);
        let show_all = self.open.contains(&format!("output-all:{}", item.id));
        let preview = cards::file_output_preview(output, show_all);
        let running = item.status == crate::chat::model::ItemStatus::InProgress;
        let compact_omits =
            show_all && !running && cards::file_output_preview(output, false).omitted > 0;
        let failed = item.status == crate::chat::model::ItemStatus::Failed;
        let first_line = lines.first().and_then(|line| line.number).or(start);
        let open_target = super::links::local_target(target).ok().map(|(path, line)| {
            first_line.map(|n| n as u32).or(line).map_or_else(
                || path.to_string_lossy().into_owned(),
                |line| format!("{}:{line}", path.to_string_lossy()),
            )
        });
        let resolvable = open_target
            .as_deref()
            .is_some_and(|target| self.excerpt_open_available(item, target, cx));
        let copy_key = format!("copy:{}", item.id);
        let copied = self.copied.as_deref() == Some(copy_key.as_str());
        let raw = output.to_owned();
        let copy_id = item.id.clone();
        let owner = item.id.clone();
        let displayed_count = preview.text.lines().count();
        let number_width = lines
            .iter()
            .filter_map(|line| line.number)
            .max()
            .map_or(3, |n| n.to_string().len() + 1)
            .max(3) as f32;
        let has_numbers = !failed
            && lines
                .iter()
                .take(displayed_count)
                .any(|line| line.number.is_some());
        let name = format!("tool-output:{}", item.id);
        let range_label = semantic.and_then(|s| s.range.as_ref()).map(|range| {
            let end = range.end.map(|end| {
                if lines.is_empty() {
                    end
                } else {
                    end.min(range.start.saturating_add(lines.len() as u64 - 1))
                }
            });
            match end {
                Some(end) if end > range.start => format!("L{}–{end}", range.start),
                _ => format!("L{}", range.start),
            }
        });
        div()
            .id(id(format!("output-block:{}", item.id)))
            .relative()
            .cursor_text()
            .w_full()
            .min_w_0()
            .rounded(px(4.0))
            .overflow_hidden()
            .border_1()
            .border_color(rgb(look.colors.divider))
            .bg(rgb(look.colors.panel))
            .py(ui_text::space(6.0))
            .px(ui_text::space(8.0))
            .font_family(ui_text::code_family())
            .text_size(ui_text::text(11.5))
            .line_height(gpui::relative(1.45))
            .text_color(rgb(look.colors.text))
            .child(
                div()
                    .id(id(format!("excerpt-header:{}", item.id)))
                    .h(ui_text::space(18.0))
                    .mb(ui_text::space(4.0))
                    .pr(ui_text::space(48.0))
                    .flex()
                    .items_center()
                    .gap(ui_text::space(4.0))
                    .text_size(ui_text::text(11.0))
                    .text_color(rgb(look.colors.muted))
                    .child(div().min_w_0().truncate().child(self.selectable(
                        &format!("file-header:{}", item.id),
                        self.file_container_label(target),
                        Vec::new(),
                        vec![(
                            0..self.file_container_label(target).len(),
                            target.to_owned(),
                        )],
                        look,
                        cx,
                    )))
                    .children(
                        range_label.map(|label| div().flex_none().child(format!("· {label}"))),
                    )
                    .when(!lines.is_empty(), |header| {
                        header.child(format!("· {} lines", lines.len()))
                    })
                    .test_support(),
            )
            .when(running, |container| {
                container.child(div().text_color(rgb(look.colors.muted)).child("Reading…"))
            })
            .when(!running && !preview.text.is_empty(), |container| {
                container.child(
                    self.scroller(&name).attach(
                        div()
                            .id(id(name.clone()))
                            .w_full()
                            .max_h(ui_text::space(320.0))
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .children(preview.text.lines().enumerate().map(|(index, text)| {
                                div()
                                    .id(id(format!("excerpt-line:{}:{index}", item.id)))
                                    .w_full()
                                    .flex()
                                    .items_start()
                                    .gap(ui_text::space(8.0))
                                    .min_h(ui_text::text(11.5) * 1.45)
                                    .when(failed, |line| line.text_color(rgb(look.error())))
                                    .when(has_numbers, |line| {
                                        line.child(
                                            div()
                                                .id(id(format!(
                                                    "excerpt-gutter:{}:{index}",
                                                    item.id
                                                )))
                                                .flex_none()
                                                .w(ui_text::text(11.0) * 0.62 * number_width)
                                                .text_size(ui_text::text(11.0))
                                                .text_align(gpui::TextAlign::Right)
                                                .text_color(rgb(look.colors.muted))
                                                .child(
                                                    lines
                                                        .get(index)
                                                        .and_then(|line| line.number)
                                                        .map(|n| n.to_string())
                                                        .unwrap_or_default(),
                                                )
                                                .test_support(),
                                        )
                                    })
                                    .child(div().flex_1().min_w_0().child(self.selectable(
                                        &format!("{name}:{index}"),
                                        text.to_owned(),
                                        if failed {
                                            vec![(
                                                0..text.len(),
                                                gpui::HighlightStyle {
                                                    color: Some(rgb(look.error()).into()),
                                                    ..Default::default()
                                                },
                                            )]
                                        } else {
                                            Vec::new()
                                        },
                                        Vec::new(),
                                        look,
                                        cx,
                                    )))
                                    .test_support()
                            }))
                            .test_support(),
                    ),
                )
            })
            .when(
                !running && (preview.omitted > 0 || show_all && compact_omits),
                |container| {
                    container.child(self.output_footer(
                        item,
                        preview.omitted,
                        show_all,
                        true,
                        look,
                        cx,
                    ))
                },
            )
            .child(
                div()
                    .absolute()
                    .top(ui_text::space(4.0))
                    .right(ui_text::space(4.0))
                    .flex()
                    .gap(ui_text::space(2.0))
                    .child(
                        self.tool_overlay_button(
                            item,
                            copy_key,
                            if copied { "checkmark" } else { "doc.on.doc" },
                            "Copy file excerpt",
                            true,
                            copied,
                            look,
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            cx.stop_propagation();
                            view.copy(format!("copy:{copy_id}"), cards::excerpt_copy(&raw), cx);
                        })),
                    )
                    .child(
                        self.tool_overlay_button(
                            item,
                            format!("open-excerpt:{}", item.id),
                            "arrow.up.right.square",
                            "Open in viewer",
                            resolvable,
                            copied,
                            look,
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            cx.stop_propagation();
                            if resolvable && let Some(target) = &open_target {
                                view.preview_reference(target, Some(&owner), cx);
                            }
                        })),
                    ),
            )
            .test_support()
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
                    .h(row_space(24.0))
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
        use crate::chat::model::ToolKind;
        let semantic = item.presentation.semantics.as_ref();
        let show_input = semantic
            .is_none_or(|s| matches!(s.kind, ToolKind::Mcp | ToolKind::Other | ToolKind::Subagent));
        let pretty = cards::input_pretty(input);
        let output = output
            .filter(|text| !text.is_empty())
            .or_else(|| semantic.and_then(|s| s.excerpt.as_deref()))
            .map(|text| cards::inline_image_output(text, !item.presentation.images.is_empty()))
            .filter(|text| !text.trim().is_empty());
        let waiting = item.status == crate::chat::model::ItemStatus::InProgress
            && item.presentation.images.is_empty();
        div()
            .w_full()
            .flex()
            .flex_col()
            .when(show_input && !pretty.is_empty(), |body| {
                body.child(
                    div()
                        .w_full()
                        .pl(row_space(12.0))
                        .text_size(ui_text::text(11.5))
                        .font_family(ui_text::code_family())
                        .text_color(rgb(look.colors.muted))
                        .child(self.selectable(
                            &format!("tool-input:{}", item.id),
                            cards::clean_output(cards::tail(&pretty, OUTPUT_LINES, OUTPUT_BYTES).0),
                            Vec::new(),
                            Vec::new(),
                            look,
                            cx,
                        )),
                )
            })
            .when(
                is_file_excerpt(item) && (output.is_some() || item.presentation.images.is_empty()),
                |body| {
                    body.child(div().pl(text_indent()).child(self.file_excerpt(
                        item,
                        output.as_deref().unwrap_or_default(),
                        look,
                        cx,
                    )))
                },
            )
            .when(!is_file_excerpt(item), |body| {
                body.when(output.is_some() || waiting, |body| {
                    body.child(div().pl(text_indent()).child(self.output_block(
                        item,
                        output.as_deref().unwrap_or_default(),
                        "tool-output",
                        None,
                        look,
                        cx,
                    )))
                })
            })
            .into_any_element()
    }

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

/// A user message block's corner radius under Native.
const BUBBLE_RADIUS: f32 = 12.0;

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
        let numbers = diff::line_numbers(text, kind);
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
                                .w_full()
                                .min_w_0()
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
                                            .w_full()
                                            .min_w_0()
                                            .flex()
                                            .items_start()
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
                                            .children({
                                                let (old, new) =
                                                    numbers.get(at).copied().unwrap_or_default();
                                                [old, new].into_iter().map(|number| {
                                                    div()
                                                        .flex_none()
                                                        .w(ui_text::space(30.0))
                                                        .pr(ui_text::space(6.0))
                                                        .text_right()
                                                        .text_color(rgb(colors.muted))
                                                        .bg(rgb(colors.panel))
                                                        .child(
                                                            number
                                                                .map(|n| n.to_string())
                                                                .unwrap_or_default(),
                                                        )
                                                })
                                            })
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .px(ui_text::space(6.0))
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
                                                    )),
                                            )
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

fn precise_duration(milliseconds: u64) -> String {
    if milliseconds < 60_000 && milliseconds % 1_000 != 0 {
        format!("{:.1}s", milliseconds as f64 / 1_000.0)
    } else {
        cards::duration(milliseconds)
    }
}

fn short_output_path(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .take(2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("/")
}

pub(super) fn is_file_excerpt(item: &Item) -> bool {
    use crate::chat::model::ToolKind;
    item.presentation
        .semantics
        .as_ref()
        .is_some_and(|semantic| {
            semantic.kind == ToolKind::Read
                || (!matches!(semantic.kind, ToolKind::Search | ToolKind::List)
                    && semantic.target.as_deref().is_some_and(|target| {
                        !target.ends_with('/') && super::links::shaped(target)
                    }))
        })
}

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

/// The native symbol remains stable across providers; semantic metadata selects it.
fn kind_symbol(kind: riwork_engine::chat::projection::RowKind) -> &'static str {
    use riwork_engine::chat::projection::RowKind::*;
    match kind {
        Shell => "terminal",
        Read => "doc.text",
        Search | WebSearch => "magnifyingglass",
        List => "folder",
        WebFetch => "globe",
        Edit => "pencil",
        Mcp => "network",
        Subagent => "person.2",
        Reasoning => "brain",
        Plan | Todo => "checklist",
        Group => "square.stack.3d.up",
        _ => "circle.grid.2x2",
    }
}
