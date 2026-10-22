//! A focused reader for provider-reported patches, never a workspace/net diff.
//! The transcript's list is left intact; a separate list owns review scrolling.
use super::{
    ChatView, ChatViewEvent, cards, diff,
    selection_document::SourceLeaf,
    widgets::{self, Look},
};
use crate::{
    chat::model::{ChangeKind, FileChange, Item, ItemBody},
    ui_text,
};
use gpui::{
    AnyElement, Context, FocusHandle, ListAlignment, ListOffset, ListState, SharedString, Window,
    div, list, prelude::*, px, rgb,
};
use gpui_kit::base::TestSupportExt as _;
use std::collections::{BTreeSet, HashSet};

const BROWSER_PAGE: usize = 12;

#[derive(Clone, Debug, PartialEq, Eq)]
struct File {
    item: String,
    change: usize,
    path: String,
}
impl File {
    fn key(&self) -> String {
        format!("{}#{}", self.item, self.change)
    }
    fn folder(&self) -> &str {
        self.path.rsplit_once('/').map_or("", |(folder, _)| folder)
    }
}

pub(super) struct Review {
    id: String,
    jump: u64,
    files: Vec<File>,
    selected: usize,
    folders: BTreeSet<String>,
    page: usize,
    hunk: usize,
    list: ListState,
    return_focus: Option<FocusHandle>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum BrowserRow {
    Folder(String),
    File(usize),
}

impl Review {
    fn browser(&self) -> Vec<BrowserRow> {
        let mut rows = Vec::new();
        let mut last = None;
        for (at, file) in self.files.iter().enumerate() {
            let folder = file.folder();
            if last != Some(folder) {
                rows.push(BrowserRow::Folder(folder.to_owned()));
                last = Some(folder);
            }
            if self.folders.contains(folder) {
                rows.push(BrowserRow::File(at));
            }
        }
        rows
    }

    fn reveal_selected(&mut self) {
        if let Some(file) = self.files.get(self.selected) {
            self.folders.insert(file.folder().to_owned());
        }
        self.page = self
            .browser()
            .iter()
            .position(|row| *row == BrowserRow::File(self.selected))
            .unwrap_or(0)
            / BROWSER_PAGE;
    }
}

fn files(entries: impl IntoIterator<Item = (String, usize, String)>) -> Vec<File> {
    let mut files: Vec<_> = entries
        .into_iter()
        .map(|(item, change, path)| File { item, change, path })
        .collect();
    files.sort_by(|a, b| {
        (a.folder(), &a.path, &a.item, a.change).cmp(&(b.folder(), &b.path, &b.item, b.change))
    });
    files
}

fn hunks(change: &FileChange) -> Vec<std::ops::Range<usize>> {
    let Some(text) = change.diff.as_deref() else {
        return Vec::new();
    };
    let (lines, _) = diff::display(text, change.kind);
    if lines.is_empty() {
        return Vec::new();
    }
    let starts: Vec<_> = lines
        .iter()
        .enumerate()
        .filter_map(|(at, line)| (line.kind == diff::LineKind::Hunk).then_some(at))
        .collect();
    if starts.is_empty() {
        return vec![0..lines.len()];
    }
    starts
        .iter()
        .enumerate()
        .map(|(at, start)| *start..starts.get(at + 1).copied().unwrap_or(lines.len()))
        .collect()
}

/// A rename's old/new names are available only when the provider supplied them.
fn description(change: &FileChange) -> String {
    if change.kind == ChangeKind::Rename {
        let patch = change.diff.as_deref().unwrap_or_default();
        let from = patch
            .lines()
            .find_map(|line| line.strip_prefix("rename from "));
        let to = patch
            .lines()
            .find_map(|line| line.strip_prefix("rename to "));
        return match (from, to) {
            (Some(from), Some(to)) => format!("Reported rename: {from} → {to}"),
            _ => "Reported rename; original path was not supplied".into(),
        };
    }
    format!("Reported {}", cards::change_badge(change.kind).1)
}

impl ChatView {
    fn reviewed_file(&self) -> Option<(&File, &Item, &FileChange)> {
        let review = self.file_review.as_ref()?;
        let file = review.files.get(review.selected)?;
        let item = self
            .model
            .transcript
            .items
            .iter()
            .find(|item| item.id == file.item)?;
        let ItemBody::FileChange { changes } = &item.body else {
            return None;
        };
        let change = changes
            .get(file.change)
            .filter(|change| change.path == file.path)?;
        Some((file, item, change))
    }

    pub(super) fn review_source(&self) -> Option<Vec<SourceLeaf>> {
        self.file_review.as_ref()?;
        let mut source = Vec::new();
        if let Some((file, _, change)) = self.reviewed_file() {
            let key = file.key();
            source.push(SourceLeaf {
                key: format!("path:{key}"),
                text: change.path.clone(),
                row: 0,
            });
            if let Some(text) = &change.diff {
                for (at, line) in diff::display(text, change.kind).0.iter().enumerate() {
                    let text = line.text.replace('\t', "    ");
                    source.push(SourceLeaf {
                        key: format!("diff:{key}:{at}"),
                        text: if text.is_empty() { " ".into() } else { text },
                        row: 0,
                    });
                }
            }
        }
        Some(source)
    }

    pub(super) fn find_list(&self) -> &ListState {
        self.file_review
            .as_ref()
            .map_or(&self.list, |review| &review.list)
    }

    fn begin_file_review(
        &mut self,
        files: Vec<File>,
        selected: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if files.is_empty() {
            return false;
        }
        let return_focus = self
            .file_review
            .take()
            .and_then(|review| review.return_focus)
            .or_else(|| window.focused(cx));
        let folders = files.iter().map(|file| file.folder().to_owned()).collect();
        let mut review = Review {
            id: uuid::Uuid::new_v4().to_string(),
            jump: 0,
            selected: selected.min(files.len() - 1),
            files,
            folders,
            page: 0,
            hunk: 0,
            list: ListState::new(1, ListAlignment::Top, px(500.)),
            return_focus,
        };
        review.reveal_selected();
        self.find.cancel_reveal();
        self.transcript_selection.retire(self.window_handle, cx);
        self.file_review = Some(review);
        self.focus.focus(window, cx);
        cx.notify();
        true
    }

    pub(super) fn open_answer_file_review(
        &mut self,
        answer: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(item) = self
            .model
            .transcript
            .items
            .iter()
            .find(|item| item.id == answer)
        else {
            return;
        };
        let changes: HashSet<usize> = self
            .answer_files(item)
            .into_iter()
            .map(|change| change as *const FileChange as usize)
            .collect();
        let entries = self
            .model
            .transcript
            .items
            .iter()
            .flat_map(|item| {
                let ItemBody::FileChange { changes: all } = &item.body else {
                    return Vec::new();
                };
                all.iter()
                    .enumerate()
                    .filter(|(_, candidate)| {
                        changes.contains(&(*candidate as *const FileChange as usize))
                    })
                    .map(|(at, change)| (item.id.clone(), at, change.path.clone()))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        self.begin_file_review(files(entries), 0, window, cx);
    }

    pub(super) fn open_file_review_at(
        &mut self,
        item_id: &str,
        change_at: usize,
        line: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(item) = self
            .model
            .transcript
            .items
            .iter()
            .find(|item| item.id == item_id)
        else {
            return false;
        };
        let ItemBody::FileChange { changes } = &item.body else {
            return false;
        };
        if change_at >= changes.len() {
            return false;
        }
        let existing = self.file_review.as_ref().and_then(|review| {
            review.files.iter().position(|file| {
                file.item == item_id
                    && file.change == change_at
                    && file.path == changes[change_at].path
            })
        });
        let files = files(
            changes
                .iter()
                .enumerate()
                .map(|(at, change)| (item.id.clone(), at, change.path.clone())),
        );
        let selected = files
            .iter()
            .position(|file| file.change == change_at)
            .unwrap_or(0);
        if let Some(at) = existing {
            self.select_review_file(at, window, cx);
        } else {
            self.begin_file_review(files, selected, window, cx);
        }
        if let Some(line) = line {
            let hunk = self
                .reviewed_file()
                .map(|(_, _, change)| {
                    hunks(change)
                        .iter()
                        .position(|range| range.contains(&line))
                        .unwrap_or(0)
                })
                .unwrap_or(0);
            if let Some(review) = self.file_review.as_mut() {
                review.hunk = hunk;
            }
            self.reveal_review_line(line, window, cx);
        }
        true
    }

    pub(super) fn close_file_review(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(review) = self.file_review.take() else {
            return false;
        };
        self.find.cancel_reveal();
        self.transcript_selection.retire(self.window_handle, cx);
        review
            .return_focus
            .unwrap_or_else(|| self.focus.clone())
            .focus(window, cx);
        cx.notify();
        true
    }

    fn select_review_file(&mut self, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(review) = self.file_review.as_mut() else {
            return;
        };
        if at >= review.files.len() || at == review.selected {
            return;
        }
        review.selected = at;
        review.hunk = 0;
        review.reveal_selected();
        review.list.scroll_to(ListOffset {
            item_ix: 0,
            offset_in_item: px(0.),
        });
        review.list.remeasure();
        self.find.cancel_reveal();
        self.transcript_selection.retire(self.window_handle, cx);
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn reveal_review_line(&mut self, line: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((file, _, _)) = self.reviewed_file() else {
            return;
        };
        let key = format!("diff:{}:{line}", file.key());
        let review = self
            .file_review
            .as_mut()
            .expect("a reviewed file has a review");
        review.jump = review.jump.wrapping_add(1);
        let owner = (review.id.clone(), review.jump, review.selected);
        self.find.cancel_reveal();
        self.schedule_review_jump(key, owner, 0, window, cx);
    }

    fn schedule_review_jump(
        &mut self,
        key: String,
        owner: (String, u64, usize),
        tries: u8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.on_next_frame(window, move |view, window, cx| {
            if view.file_review.as_ref().is_none_or(|review| {
                (review.id.as_str(), review.jump, review.selected)
                    != (owner.0.as_str(), owner.1, owner.2)
            }) {
                return;
            }
            let rect = view.transcript_selection.with_source(|source| {
                let at = source.iter().position(|leaf| leaf.key == key)?;
                let end = source[at].text.chars().next().map_or(0, char::len_utf8);
                view.transcript_selection.painted_rect(at, 0..end)
            });
            if let Some(rect) = rect {
                let mut moved = view.scroll_box_to(&key, rect);
                if !moved {
                    let viewport = view.find_list().viewport_bounds();
                    if rect.top() < viewport.top() || rect.bottom() > viewport.bottom() {
                        view.find_list()
                            .scroll_by(rect.top() - (viewport.top() + viewport.size.height * 0.3));
                        moved = true;
                    }
                }
                if moved && tries < 6 {
                    view.schedule_review_jump(key, owner, tries + 1, window, cx);
                }
            }
            cx.notify();
        });
        cx.notify();
    }

    pub(super) fn cancel_review_jump(&mut self) {
        if let Some(review) = self.file_review.as_mut() {
            review.jump = review.jump.wrapping_add(1);
        }
    }

    fn move_review_hunk(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let ranges = self
            .reviewed_file()
            .map(|(_, _, change)| hunks(change))
            .unwrap_or_default();
        let Some(review) = self.file_review.as_mut() else {
            return;
        };
        if ranges.is_empty() {
            return;
        }
        let current = review.hunk.min(ranges.len() - 1);
        review.hunk = if forward {
            (current + 1).min(ranges.len() - 1)
        } else {
            current.saturating_sub(1)
        };
        let line = ranges[review.hunk].start;
        self.reveal_review_line(line, window, cx);
    }

    fn comment_review_hunk(
        &mut self,
        item: &str,
        change: usize,
        range: std::ops::Range<usize>,
        revision: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let current = !range.is_empty()
            && self.reviewed_file().is_some_and(|(file, _, patch)| {
                file.item == item
                    && file.change == change
                    && crate::chat_drafts::references::revision(
                        patch.diff.as_deref().unwrap_or_default(),
                    ) == revision
            });
        if current {
            self.add_review_reference(item, change, range.start, range.end - 1, window, cx)
        } else {
            self.notices.set(
                super::notices::LocalKey::Send,
                crate::chat::model::NoticeLevel::Warning,
                "The reported patch changed. Review the current hunk before adding its comment.",
            );
            cx.notify();
            false
        }
    }

    pub(super) fn render_file_review(
        &mut self,
        look: Look,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.file_review.as_ref()?;
        let ranges = self
            .reviewed_file()
            .map(|(_, _, change)| hunks(change))
            .unwrap_or_default();
        // Keep the stored position consistent with the controls after a live
        // patch replacement. A disabled Previous button cannot repair an old
        // index, and a pending jump must not target a hunk that disappeared.
        if let Some(review) = self.file_review.as_mut() {
            let hunk = review.hunk.min(ranges.len().saturating_sub(1));
            if review.hunk != hunk {
                review.hunk = hunk;
                review.jump = review.jump.wrapping_add(1);
            }
        }
        let review = self.file_review.as_ref()?;
        let (selected, count, page) = (review.selected, review.files.len(), review.page);
        let browser = review.browser();
        let pages = browser.len().div_ceil(BROWSER_PAGE).max(1);
        let page = page.min(pages - 1);
        let rows: Vec<_> = browser
            .into_iter()
            .skip(page * BROWSER_PAGE)
            .take(BROWSER_PAGE)
            .map(|row| match row {
                BrowserRow::Folder(folder) => {
                    let expanded = review.folders.contains(&folder);
                    let label = if folder.is_empty() {
                        "Project root"
                    } else {
                        &folder
                    };
                    widgets::button(
                        SharedString::from(format!("review-folder:{folder}")),
                        format!("{} {label}", if expanded { "▾" } else { "▸" }),
                        None,
                        look,
                    )
                    .aria_expanded(expanded)
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if let Some(review) = view.file_review.as_mut() {
                            if !review.folders.remove(&folder) {
                                review.folders.insert(folder.clone());
                            }
                            review.page = 0;
                        }
                        cx.notify();
                    }))
                    .into_any_element()
                }
                BrowserRow::File(at) => {
                    let file = &review.files[at];
                    let name = file.path.rsplit('/').next().unwrap_or(&file.path);
                    widgets::button(
                        SharedString::from(format!("review-file:{at}")),
                        format!("{} {name}", if selected == at { "●" } else { " " }),
                        (selected == at).then_some(look.colors.cyan),
                        look,
                    )
                    .aria_label(format!("Review {}", file.path))
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.select_review_file(at, window, cx)
                    }))
                    .into_any_element()
                }
            })
            .collect();
        let hunk = review.hunk;
        let comment = self.reviewed_file().and_then(|(file, _, change)| {
            ranges.get(hunk).map(|range| {
                (
                    file.item.clone(),
                    file.change,
                    range.clone(),
                    crate::chat_drafts::references::revision(
                        change.diff.as_deref().unwrap_or_default(),
                    ),
                )
            })
        });
        let can_comment = self
            .reviewed_file()
            .and_then(|(_, _, change)| {
                change.diff.as_deref().map(|patch| {
                    let (lines, _) = diff::display(patch, change.kind);
                    ranges.get(hunk).is_some_and(|range| {
                        range.len() <= super::feedback::MAX_DIFF_LINES
                            && lines[range.clone()]
                                .iter()
                                .map(|line| line.text.len())
                                .sum::<usize>()
                                + range.len().saturating_sub(1)
                                <= super::feedback::MAX_QUOTE_BYTES
                    })
                })
            })
            .unwrap_or(false);
        let messages = list(
            review.list.clone(),
            cx.processor(|view, _, _, cx| view.render_review_patch(Look::of(cx), cx)),
        )
        .size_full();
        let messages = self
            .transcript_selection
            .viewport(messages, review.list.clone(), look);
        Some(div().id("chat-file-review").test_support().size_full().flex().flex_col()
            .child(div().flex().flex_wrap().items_center().gap(ui_text::space(6.)).px(ui_text::space(10.)).py(ui_text::space(7.))
                .child(widgets::button("file-review-back", "Back to chat", None, look).on_click(cx.listener(|view, _, window, cx| { view.close_file_review(window, cx); })))
                .child(div().flex_1().child(format!("Reported edits · File {} of {count}", selected + 1)))
                .child(widgets::button("file-review-previous", "Previous file", None, look).disabled(selected == 0).on_click(cx.listener(move |view, _, window, cx| view.select_review_file(selected.saturating_sub(1), window, cx))))
                .child(widgets::button("file-review-next", "Next file", None, look).disabled(selected + 1 >= count).on_click(cx.listener(move |view, _, window, cx| view.select_review_file(selected + 1, window, cx)))))
            .child(div().px(ui_text::space(10.)).text_size(ui_text::text(10.)).text_color(rgb(look.colors.muted)).child("Reported patches in this review; not the net workspace diff. Display and Find retain the first 400 patch lines. Copy patch keeps the complete report."))
            .children(self.find_bar(look, window, cx))
            .child(div().flex().flex_wrap().gap(ui_text::space(5.)).px(ui_text::space(10.)).py(ui_text::space(5.))
                .child(widgets::button("file-review-expand-all", "Expand folders", None, look).on_click(cx.listener(|view, _, _, cx| { if let Some(review) = view.file_review.as_mut() { review.folders = review.files.iter().map(|file| file.folder().to_owned()).collect(); review.page = 0; } cx.notify(); })))
                .child(widgets::button("file-review-collapse-all", "Collapse folders", None, look).on_click(cx.listener(|view, _, _, cx| { if let Some(review) = view.file_review.as_mut() { review.folders.clear(); review.page = 0; } cx.notify(); })))
                .child(widgets::button("file-review-page-previous", "Earlier files", None, look).disabled(page == 0).on_click(cx.listener(|view, _, _, cx| { if let Some(review) = view.file_review.as_mut() { review.page = review.page.saturating_sub(1); } cx.notify(); })))
                .child(div().text_size(ui_text::text(10.)).child(format!("{} / {pages}", page + 1)))
                .child(widgets::button("file-review-page-next", "More files", None, look).disabled(page + 1 >= pages).on_click(cx.listener(move |view, _, _, cx| { if let Some(review) = view.file_review.as_mut() { review.page = (page + 1).min(pages - 1); } cx.notify(); }))))
            .child(div().id("file-review-browser").max_h(ui_text::space(140.)).overflow_y_scroll().px(ui_text::space(10.)).flex().flex_col().items_start().children(rows))
            .child(div().flex().flex_wrap().gap(ui_text::space(6.)).px(ui_text::space(10.)).py(ui_text::space(5.))
                .child(widgets::button("file-review-hunk-previous", "Previous hunk", None, look).disabled(hunk == 0).on_click(cx.listener(|view, _, window, cx| view.move_review_hunk(false, window, cx))))
                .child(div().text_size(ui_text::text(11.)).child(if ranges.is_empty() { "No reported patch".into() } else { format!("Hunk {} of {}", hunk + 1, ranges.len()) }))
                .child(widgets::button("file-review-hunk-next", "Next hunk", None, look).disabled(hunk + 1 >= ranges.len()).on_click(cx.listener(|view, _, window, cx| view.move_review_hunk(true, window, cx))))
                .children(comment.map(|(item, change, range, revision)| widgets::button("file-review-comment", if can_comment { "Comment on hunk" } else { "Select fewer lines to comment" }, None, look).disabled(!can_comment).on_click(cx.listener(move |view, _, window, cx| {
                    view.comment_review_hunk(&item, change, range.clone(), &revision, window, cx);
                })))))
            .child(div().flex_1().min_h_0().child(messages)).into_any_element())
    }

    fn render_review_patch(&self, look: Look, cx: &mut Context<Self>) -> AnyElement {
        let Some((file, _, change)) = self.reviewed_file() else {
            return div().p(ui_text::space(12.)).child("This reported edit changed or is no longer available. Return to chat and reopen its review.").into_any_element();
        };
        let key = file.key();
        let path = change.path.clone();
        div()
            .w_full()
            .min_w_0()
            .p(ui_text::space(8.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui_text::space(6.))
                    .child(div().flex_1().min_w_0().child(self.selectable(
                        &format!("path:{key}"),
                        path.clone(),
                        Vec::new(),
                        Vec::new(),
                        look,
                        cx,
                    )))
                    .when(change.kind != ChangeKind::Delete, |row| {
                        row.child(
                            widgets::button("file-review-open", "Open file", None, look).on_click(
                                cx.listener(move |_, _, _, cx| {
                                    cx.emit(ChatViewEvent::OpenFile {
                                        target: path.clone(),
                                    })
                                }),
                            ),
                        )
                    }),
            )
            .child(
                div()
                    .text_size(ui_text::text(10.))
                    .text_color(rgb(look.colors.muted))
                    .child(description(change)),
            )
            .children(
                change
                    .diff
                    .as_ref()
                    .filter(|text| !text.trim().is_empty())
                    .map(|text| self.diff_lines(&key, text, change.kind, look, cx)),
            )
            .when(
                change
                    .diff
                    .as_ref()
                    .is_none_or(|text| text.trim().is_empty()),
                |row| row.child("The provider reported this change without a patch."),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests;
