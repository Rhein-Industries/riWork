//! Compact Markdown plan documents and explicit native export. Saved bytes are the
//! complete source captured when Save was invoked, never the preview or rendered text.
use std::{
    fs::{self, File, Metadata, OpenOptions},
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

use gpui::{
    AnyElement, Context, PromptButton, PromptLevel, SharedString, Window, div, prelude::*, rgb,
};
use gpui_kit::base::TestSupportExt as _;

use super::{
    ChatView, markdown,
    notices::LocalKey,
    widgets::{self, Look},
};
use crate::{
    chat::model::{Item, ItemBody, ItemStatus, NoticeLevel},
    ui_text,
};

const LONG_CHARS: usize = 1200;
const LONG_LINES: usize = 20;
const PREVIEW_CHARS: usize = 800;
const PREVIEW_LINES: usize = 10;

pub(super) fn expansion_key(id: &str) -> String {
    format!("plan-document:{id}")
}

pub(super) fn is_long(source: &str) -> bool {
    source.chars().take(LONG_CHARS + 1).count() > LONG_CHARS
        || source.lines().take(LONG_LINES + 1).count() > LONG_LINES
}

/// A real source prefix, without synthetic ellipses or changed newlines. The same
/// prefix drives visible selection; Find indexes the full source and expands first.
pub(super) fn shown_source(source: &str, expanded: bool) -> &str {
    if expanded || !is_long(source) {
        return source;
    }
    let mut lines = 0;
    for (characters, (at, character)) in source.char_indices().enumerate() {
        if characters >= PREVIEW_CHARS {
            return &source[..at];
        }
        if character == '\n' {
            lines += 1;
            if lines >= PREVIEW_LINES {
                return &source[..at + 1];
            }
        }
    }
    source
}

fn filename(source: &str) -> String {
    let title = source
        .lines()
        .find_map(|line| {
            let line = line.trim_start();
            let count = line.chars().take_while(|c| *c == '#').count();
            (count > 0
                && count <= 6
                && line
                    .as_bytes()
                    .get(count)
                    .is_some_and(u8::is_ascii_whitespace))
            .then(|| {
                markdown::plain_text(&markdown::inline(
                    line[count..].trim().trim_end_matches('#').trim(),
                ))
            })
        })
        .unwrap_or_else(|| "plan".into());
    let mut stem = String::new();
    for character in title.chars().flat_map(char::to_lowercase).take(64) {
        if character.is_alphanumeric() {
            stem.push(character);
        } else if !stem.is_empty() && !stem.ends_with('-') {
            stem.push('-');
        }
    }
    let stem = stem.trim_end_matches('-');
    format!("{}.md", if stem.is_empty() { "plan" } else { stem })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
    length: u64,
    modified: (i64, i64),
}
impl Identity {
    fn of(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
        }
    }
}

#[derive(Debug)]
pub(super) struct Destination {
    path: PathBuf,
    existing: Option<Identity>,
}

impl Destination {
    pub(super) fn replaces_file(&self) -> bool {
        self.existing.is_some()
    }
}

pub(super) fn export_destination(path: PathBuf) -> Result<Destination, String> {
    destination(path, None)
}

fn destination(path: PathBuf, workspace: Option<&Path>) -> Result<Destination, String> {
    if !path.is_absolute() {
        return Err("Choose an absolute file destination".into());
    }
    let name = path.file_name().ok_or("Choose a Markdown filename")?;
    let parent = path
        .parent()
        .ok_or("Choose a destination folder")?
        .canonicalize()
        .map_err(|e| format!("Cannot open the destination folder: {e}"))?;
    if let Some(workspace) = workspace {
        let root = workspace
            .canonicalize()
            .map_err(|e| format!("Cannot open the workspace: {e}"))?;
        if !parent.starts_with(root) {
            return Err("Choose a file inside this chat's workspace, or use Save Markdown for another folder".into());
        }
    }
    let path = parent.join(name);
    let existing = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() => Some(Identity::of(&metadata)),
        Ok(_) => {
            return Err("The destination is not a regular file; choose another filename".into());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("Cannot inspect the destination: {error}")),
    };
    Ok(Destination { path, existing })
}

/// A new destination uses create-only publication. Replacements need explicit UI
/// confirmation and must still name the file inspected before that confirmation.
fn write(destination: &Destination, source: &str, replace: bool) -> Result<(), String> {
    write_bytes(destination, source.as_bytes(), replace)
}

pub(super) fn write_bytes(
    destination: &Destination,
    source: &[u8],
    replace: bool,
) -> Result<(), String> {
    let parent = destination
        .path
        .parent()
        .ok_or("Missing destination folder")?;
    if destination.existing.is_some() && !replace {
        return Err("Replacing an existing plan requires confirmation".into());
    }
    let temporary = parent.join(format!(".riwork-plan-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|e| format!("Cannot create the saved plan: {e}"))?;
        file.write_all(source)
            .and_then(|_| file.sync_all())
            .map_err(|e| format!("Cannot write the saved plan: {e}"))?;
        if let Some(expected) = &destination.existing {
            let current = fs::symlink_metadata(&destination.path)
                .map_err(|e| format!("The destination changed; choose it again: {e}"))?;
            if !current.is_file() || &Identity::of(&current) != expected {
                return Err(
                    "The destination changed while the dialog was open; choose it again".into(),
                );
            }
            fs::set_permissions(&temporary, current.permissions()).map_err(|e| e.to_string())?;
            fs::rename(&temporary, &destination.path)
                .map_err(|e| format!("Cannot replace the saved plan: {e}"))?;
        } else {
            fs::hard_link(&temporary, &destination.path).map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    "A file now exists at this destination; choose it again to confirm replacement"
                        .into()
                } else {
                    format!("Cannot save the plan: {e}")
                }
            })?;
        }
        File::open(parent)
            .and_then(|file| file.sync_all())
            .map_err(|e| format!("Could not confirm the saved plan on disk: {e}"))
    })();
    let _ = fs::remove_file(&temporary);
    result
}

impl ChatView {
    pub(super) fn plan_document(
        &self,
        item: &Item,
        source: &str,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let expanded_key = expansion_key(&item.id);
        let expanded = self.open.contains(&expanded_key);
        let collapsible = is_long(source);
        let key = format!("plan:{}:document", item.id);
        let copy_key = format!("copy-plan:{}", item.id);
        let markdown = source.to_owned();
        let save_id = item.id.clone();
        let workspace_id = item.id.clone();
        let streaming = item.status == ItemStatus::InProgress;
        let workspace_available = self.info().is_some();
        let shown = shown_source(source, expanded);
        let blocks = self.parsed_variant(item, shown, if expanded { "full" } else { "preview" });
        div()
            .id(format!("plan-card:{}", item.id))
            .w_full()
            .min_w_0()
            .p(ui_text::space(10.))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(ui_text::space(6.))
                    .mb(ui_text::space(8.))
                    .children(collapsible.then(|| {
                        widgets::button(
                            SharedString::from(expanded_key.clone()),
                            if expanded {
                                "Collapse plan"
                            } else {
                                "Expand plan"
                            },
                            None,
                            look,
                        )
                        .aria_expanded(expanded)
                        .on_click(cx.listener(move |view, _, _, cx| {
                            // Expanding is an explicit reading action. Keep the header in
                            // place instead of following the newly lengthened plan's tail.
                            let offset = view.list.logical_scroll_top();
                            view.list.pause_following_tail();
                            view.toggle_output_layout(&expanded_key, cx);
                            view.list.scroll_to(offset);
                        }))
                    }))
                    .child(
                        widgets::copy_button(
                            SharedString::from(copy_key.clone()),
                            self.copied.as_deref() == Some(copy_key.as_str()),
                            "Copy plan",
                            "Copy plan Markdown",
                            look,
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.copy(copy_key.clone(), markdown.clone(), cx)
                        })),
                    )
                    .child(
                        widgets::button(
                            format!("save-plan:{}", item.id),
                            "Save Markdown…",
                            None,
                            look,
                        )
                        .disabled(streaming)
                        .on_click(cx.listener(
                            move |view, _, window, cx| view.save_plan(&save_id, false, window, cx),
                        )),
                    )
                    .child(
                        widgets::button(
                            format!("save-plan-workspace:{}", item.id),
                            "Save to workspace…",
                            None,
                            look,
                        )
                        .disabled(streaming || !workspace_available)
                        .on_click(cx.listener(
                            move |view, _, window, cx| {
                                view.save_plan(&workspace_id, true, window, cx)
                            },
                        )),
                    ),
            )
            .children((collapsible && !expanded || streaming).then(|| {
                div()
                    .text_size(ui_text::text(10.))
                    .text_color(rgb(look.colors.muted))
                    .mb(ui_text::space(6.))
                    .child(if streaming {
                        "Plan is still being written; saving is available when it finishes."
                    } else {
                        "Plan preview · expand to read the complete document"
                    })
            }))
            .child(self.prose(&blocks, &key, look, cx))
            .test_support()
            .into_any_element()
    }

    fn save_plan(
        &mut self,
        item_id: &str,
        workspace_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(item) = self
            .model
            .transcript
            .items
            .iter()
            .find(|item| item.id == item_id)
        else {
            return;
        };
        if item.status == ItemStatus::InProgress {
            return;
        }
        let ItemBody::Plan {
            explanation: Some(source),
            steps,
        } = &item.body
        else {
            return;
        };
        if !steps.is_empty() || source.trim().is_empty() {
            return;
        }
        let source = source.clone();
        let workspace = self.info().map(|info| info.cwd.clone());
        if workspace_only && workspace.is_none() {
            return;
        }
        let directory = workspace.clone().unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir)
        });
        let selected = cx.prompt_for_new_path(&directory, Some(&filename(&source)));
        let executor = cx.background_executor().clone();
        self.attachment_tasks
            .push(cx.spawn_in(window, async move |this, cx| {
                let path = match selected.await {
                    Ok(Ok(Some(path))) => path,
                    Ok(Ok(None)) => return,
                    other => {
                        let _ = this.update(cx, |view, cx| {
                            view.plan_save_result(Err(format!("Save dialog: {other:?}")), cx);
                        });
                        return;
                    }
                };
                let prepared = executor
                    .spawn(async move {
                        destination(
                            path,
                            workspace_only.then_some(workspace).flatten().as_deref(),
                        )
                    })
                    .await;
                let destination = match prepared {
                    Ok(destination) => destination,
                    Err(error) => {
                        let _ = this.update(cx, |view, cx| view.plan_save_result(Err(error), cx));
                        return;
                    }
                };
                let replace = destination.existing.is_some();
                if replace {
                    let confirmation = this.update_in(cx, |_, window, cx| {
                        window.prompt(
                            PromptLevel::Warning,
                            "Replace the existing file with this plan?",
                            Some(&destination.path.display().to_string()),
                            &[PromptButton::cancel("Cancel"), PromptButton::ok("Replace")],
                            cx,
                        )
                    });
                    let Ok(confirmation) = confirmation else {
                        return;
                    };
                    if confirmation.await != Ok(1) {
                        return;
                    }
                }
                if this.update(cx, |_, _| ()).is_err() {
                    return;
                }
                let result = executor
                    .spawn(async move {
                        write(&destination, &source, replace).map(|()| destination.path)
                    })
                    .await;
                let _ = this.update(cx, |view, cx| view.plan_save_result(result, cx));
            }));
    }

    fn plan_save_result(&mut self, result: Result<PathBuf, String>, cx: &mut Context<Self>) {
        match result {
            Ok(path) => self.notices.set(
                LocalKey::Plan,
                NoticeLevel::Info,
                format!("Plan saved to {}", path.display()),
            ),
            Err(error) => self.notices.set(
                LocalKey::Plan,
                NoticeLevel::Error,
                format!("Could not save plan: {error}"),
            ),
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests;
