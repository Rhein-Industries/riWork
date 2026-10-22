//! Render the same bounded semantic result rows that selection/search export.
use super::{
    ChatView,
    widgets::{self, Look},
};
use crate::{
    chat::{
        model::{Item, ItemBody},
        tool_output::{self, ToolOutput},
    },
    ui_text,
};
use gpui::{AnyElement, Context, ElementId, SharedString, div, prelude::*, rgb};
use std::{
    hash::{Hash, Hasher},
    sync::Arc,
};

pub(super) struct ResultText {
    pub key: String,
    pub text: String,
    pub link: Option<(std::ops::Range<usize>, String)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::{ItemStatus, Presentation};
    use gpui::TestAppContext;
    use gpui_kit::test::TestWindowExt;

    #[gpui::test]
    fn native_tool_results_preserve_source_links_and_raw_output(cx: &mut TestAppContext) {
        let (handle, view, recording) = super::super::editor_tests::mount_selection(cx);
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                view.model.link = super::super::Link::Live;
                view.display_mode = super::super::DisplayMode::Verbose;
                view.model.transcript.items = vec![Item {
                    started_at: None,
                    ended_at: None,
                    id: "search".into(),
                    turn_id: Some("turn".into()),
                    status: ItemStatus::Completed,
                    presentation: Presentation::default(),
                    body: ItemBody::ToolCall {
                        server: None,
                        tool: "Grep".into(),
                        input: serde_json::json!({"pattern":"hello"}),
                        output: Some("src/日本語.rs:42:hello".into()),
                    },
                }];
                // The call row opens through its projected disclosure.
                view.disclosure.overrides.insert("search".into(), true);
                view.refresh_projection(cx);
                view.list.remeasure();
                cx.notify();
            });
            window.render_frame(cx);
            assert_eq!(
                window.find("transcript:tool-view:search:0").label(),
                Some("src/日本語.rs:42\nhello")
            );
            window.click("copy-path:search:0", cx);
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "src/日本語.rs:42"
            );
            window.click("tool-raw:search", cx);
            window.render_frame(cx);
            // The raw output is the call's output container, one leaf per run of lines.
            assert_eq!(
                window.find("transcript:tool-output:search:run:0").label(),
                Some("src/日本語.rs:42:hello")
            );
            let leaves = super::super::selection_document::leaves(view.read(cx), Look::of(cx));
            assert!(leaves.iter().any(|leaf| leaf.key == "tool-view:search:0"));
            assert!(
                leaves
                    .iter()
                    .any(|leaf| leaf.key == "tool-output:search:run:0")
            );
        })
        .unwrap();
        assert!(recording.try_recv().is_err());
    }
}

pub(super) fn texts(item: &str, output: &ToolOutput) -> Vec<ResultText> {
    let row = |at: usize, text: String, link| ResultText {
        key: format!("tool-view:{item}:{at}"),
        text,
        link,
    };
    match output {
        ToolOutput::FileSearch { hits, .. } => hits
            .iter()
            .enumerate()
            .map(|(at, hit)| {
                let target = hit.target();
                let text = if hit.preview.is_empty() {
                    target.clone()
                } else {
                    format!("{target}\n{}", hit.preview)
                };
                row(at, text, Some((0..target.len(), target)))
            })
            .collect(),
        ToolOutput::WebSearch { hits, .. } => hits
            .iter()
            .enumerate()
            .map(|(at, hit)| {
                let label = if hit.title.is_empty() {
                    &hit.url
                } else {
                    &hit.title
                };
                let text = if hit.snippet.is_empty() {
                    format!("{label}\n{}", hit.url)
                } else {
                    format!("{label}\n{}\n{}", hit.url, hit.snippet)
                };
                row(at, text, Some((0..label.len(), hit.url.clone())))
            })
            .collect(),
        ToolOutput::FileRead { path, text, .. } => vec![
            row(0, path.clone(), Some((0..path.len(), path.clone()))),
            row(1, text.replace('\t', "    "), None),
        ],
        ToolOutput::Json { text, .. } => vec![row(0, text.clone(), None)],
    }
}

pub(super) fn raw_key(item: &Item) -> String {
    format!("tool-raw:{}", item.id)
}

impl ChatView {
    pub(super) fn tool_output(&self, item: &Item) -> Option<Arc<ToolOutput>> {
        if let Some(output) = &item.presentation.tool {
            return Some(Arc::new(output.clone()));
        }
        let ItemBody::ToolCall {
            tool,
            input,
            output: Some(output),
            ..
        } = &item.body
        else {
            return None;
        };
        let mut hash = std::hash::DefaultHasher::new();
        tool.hash(&mut hash);
        input.to_string().hash(&mut hash);
        output.hash(&mut hash);
        let hash = hash.finish();
        let mut cache = self.caches.borrow_mut();
        if let Some((known, result)) = cache.tool_outputs.get(&item.id)
            && *known == hash
        {
            return result.clone();
        }
        let result = tool_output::parse(tool, input, output).map(Arc::new);
        if cache.tool_outputs.len() >= 64 {
            cache.tool_outputs.clear();
        }
        cache
            .tool_outputs
            .insert(item.id.clone(), (hash, result.clone()));
        result
    }

    pub(super) fn rich_tool_body(
        &self,
        item: &Item,
        output: &ToolOutput,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (title, omitted) = match output {
            ToolOutput::FileSearch {
                query,
                hits,
                omitted,
            } => (
                format!(
                    "{} match{}{}",
                    hits.len(),
                    if hits.len() == 1 { "" } else { "es" },
                    if query.is_empty() {
                        String::new()
                    } else {
                        format!(" · {query}")
                    }
                ),
                *omitted,
            ),
            ToolOutput::WebSearch {
                query,
                hits,
                omitted,
                ..
            } => (
                format!(
                    "{} source{}{}",
                    hits.len(),
                    if hits.len() == 1 { "" } else { "s" },
                    if query.is_empty() {
                        String::new()
                    } else {
                        format!(" · {query}")
                    }
                ),
                *omitted,
            ),
            ToolOutput::FileRead { truncated, .. } => (
                (if *truncated {
                    "File preview · truncated"
                } else {
                    "File preview"
                })
                .into(),
                0,
            ),
            ToolOutput::Json { truncated, .. } => (
                (if *truncated {
                    "Structured result · truncated"
                } else {
                    "Structured result"
                })
                .into(),
                0,
            ),
        };
        let content = texts(&item.id, output);
        let code = matches!(
            output,
            ToolOutput::FileRead { .. } | ToolOutput::Json { .. }
        );
        let rows = content.into_iter().enumerate().map(|(at, row)| {
            let copy = row.link.as_ref().map(|(_, target)| target.clone());
            let mut block = div()
                .w_full()
                .min_w_0()
                .px(ui_text::space(10.))
                .py(ui_text::space(7.))
                .when(at > 0, |row| {
                    row.border_t_1().border_color(rgb(look.colors.divider))
                });
            let highlights = match output {
                ToolOutput::Json { .. } => {
                    super::code_highlight::highlight(Some("json"), None, &row.text, look.colors.bg)
                }
                ToolOutput::FileRead { path, .. } if at == 1 => {
                    super::code_highlight::highlight(None, Some(path), &row.text, look.colors.bg)
                }
                _ => Vec::new(),
            };
            block = block.child(
                div()
                    .min_w_0()
                    .text_size(ui_text::text(12.))
                    .when(code, |row| row.font_family(ui_text::code_family()))
                    .child(self.selectable(
                        &row.key,
                        row.text,
                        highlights,
                        row.link.into_iter().collect(),
                        look,
                        cx,
                    )),
            );
            if let Some(target) = copy {
                let copy_key = format!("copy-path:{}:{at}", item.id);
                block = block.child(
                    div().flex().justify_end().child(
                        widgets::copy_button(
                            SharedString::from(copy_key.clone()),
                            self.copied.as_deref() == Some(copy_key.as_str()),
                            "Copy link",
                            "Copy file path or source URL",
                            look,
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.copy(copy_key.clone(), target.clone(), cx)
                        })),
                    ),
                );
            }
            block
        });
        div()
            .w_full()
            .min_w_0()
            .child(
                div()
                    .px(ui_text::space(10.))
                    .pt(ui_text::space(7.))
                    .text_size(ui_text::text(10.))
                    .text_color(rgb(look.colors.muted))
                    .child(title),
            )
            .child(
                self.scroller(&format!("tool-rich:{}", item.id)).attach(
                    div()
                        .id(ElementId::Name(format!("tool-rich:{}", item.id).into()))
                        .max_h(ui_text::space(360.))
                        .overflow_y_scroll()
                        .children(rows),
                ),
            )
            .when(omitted > 0, |body| {
                body.child(
                    div()
                        .px(ui_text::space(10.))
                        .py(ui_text::space(6.))
                        .text_size(ui_text::text(10.))
                        .text_color(rgb(look.colors.muted))
                        .child(format!(
                            "{omitted} more results · show raw input/output for details"
                        )),
                )
            })
            .into_any_element()
    }
}
