//! Actions capture the displayed pixels. Local originals are checked again before
//! being opened; saving or copying never fetches a URL or rereads a changed file.
use super::*;
use crate::chat::model::NoticeLevel;
use crate::chat_view::notices::LocalKey;
use gpui::{ClipboardItem, Window};
use image::ImageEncoder;
use std::path::PathBuf;

#[derive(Clone, Copy)]
enum Action {
    Copy,
    Save,
    Original,
}

fn png_snapshot(image: &RenderImage) -> Result<Vec<u8>, String> {
    let size = image.size(0);
    let width = u32::try_from(size.width.0).map_err(|_| "Invalid image width.")?;
    let height = u32::try_from(size.height.0).map_err(|_| "Invalid image height.")?;
    let bytes = image
        .as_bytes(0)
        .ok_or("This image has no decoded pixels.")?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("This image exceeds the export limit.".into());
    }
    let mut rgba = bytes.to_vec();
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&rgba, width, height, image::ExtendedColorType::Rgba8)
        .map_err(|error| format!("Cannot encode image: {error}"))?;
    Ok(png)
}

/// Decoding through the same bounded no-follow reader also checks extension,
/// dimensions and orientation. A changed preview must be explicitly reopened.
fn local_original(scope: &ImageScope, path: &str, shown: &RenderImage) -> Result<PathBuf, String> {
    let resolved = scope.resolve(path)?;
    let path = file_preview::validated_live_editor_path(&resolved.root, &resolved.path)?;
    let (current, _) = load(
        &ChatImage {
            label: String::new(),
            source: ImageSource::Local {
                path: path.to_string_lossy().into_owned(),
            },
        },
        scope,
    );
    let PreviewContent::Image { image, .. } = current? else {
        return Err("This file is no longer a supported image.".into());
    };
    if image.size(0) != shown.size(0) || image.as_bytes(0) != shown.as_bytes(0) {
        return Err(
            "This image changed since its preview. Fold and reopen it before opening the original."
                .into(),
        );
    }
    if let Some(reason) = crate::terminal_links::open_refusal(&path) {
        return Err(reason.into());
    }
    Ok(path)
}

fn export_name(source: &ImageSource) -> String {
    let name = match source {
        ImageSource::Local { path } => filename(path),
        ImageSource::Url { url } => filename(url.split(['?', '#']).next().unwrap_or(url)),
        _ => None,
    }
    .unwrap_or_else(|| "image".into());
    let stem = std::path::Path::new(&name)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("image");
    let stem: String = stem
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '/' | '\\' | ':'))
        .take(100)
        .collect();
    format!(
        "{}-preview.png",
        if stem.is_empty() { "image" } else { &stem }
    )
}

impl ChatView {
    pub(super) fn image_actions(
        &self,
        key: &str,
        location: &str,
        look: Look,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let slot = self.media.slots.get(key)?;
        if !matches!(&slot.content, Some(Ok(PreviewContent::Image { .. }))) {
            return None;
        }
        let has_original = matches!(&slot.source, ImageSource::Local { .. })
            || matches!(&slot.source, ImageSource::Url { url } if url.starts_with("https://") || url.starts_with("http://"));
        let generation = slot.generation;
        let path_actions = if let ImageSource::Local { path } = &slot.source {
            self.file_actions_button(&format!("{location}:{key}"), vec![path.clone()], look, cx)
        } else {
            None
        };
        Some(
            div()
                .id(SharedString::from(format!(
                    "image-actions:{location}:{key}"
                )))
                .flex()
                .flex_wrap()
                .gap(ui_text::space(5.))
                .children(
                    [
                        (Action::Save, "save", "Save displayed image…"),
                        (Action::Copy, "copy", "Copy image"),
                        (Action::Original, "original", "Open original"),
                    ]
                    .into_iter()
                    .filter(|(action, _, _)| !matches!(action, Action::Original) || has_original)
                    .map(|(action, id, label)| {
                        let key = key.to_owned();
                        button(
                            SharedString::from(format!("image-{id}:{location}:{key}")),
                            label,
                            None,
                            look,
                        )
                        .disabled(self.media.action_pending)
                        .on_click(cx.listener(
                            move |view, _, window, cx| {
                                view.image_action(&key, generation, action, window, cx)
                            },
                        ))
                    }),
                )
                .children(path_actions)
                .test_support()
                .into_any_element(),
        )
    }

    fn image_action(
        &mut self,
        key: &str,
        generation: u64,
        action: Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.media.action_pending {
            return;
        }
        let Some(slot) = self
            .media
            .slots
            .get(key)
            .filter(|slot| slot.generation == generation)
        else {
            return;
        };
        let Some(Ok(PreviewContent::Image { image, .. })) = &slot.content else {
            return;
        };
        let source = slot.source.clone();
        let image = image.clone();
        if matches!(action, Action::Original)
            && let ImageSource::Url { url } = &source
        {
            if url.starts_with("https://") || url.starts_with("http://") {
                cx.open_url(url);
            }
            return;
        }
        let scope = self.image_scope.clone();
        let expected_scope = scope.clone();
        let work_source = source.clone();
        let key = key.to_owned();
        self.media.action_pending = true;
        self.notices.clear(LocalKey::Export);
        cx.notify();
        let work = cx.background_executor().spawn(async move {
            match action {
                Action::Copy | Action::Save => {
                    png_snapshot(&image).map(|bytes| (Some(bytes), None))
                }
                Action::Original => {
                    if let ImageSource::Local { path } = work_source {
                        local_original(&scope, &path, &image).map(|path| (None, Some(path)))
                    } else {
                        Err("This image has no external original.".into())
                    }
                }
            }
        });
        self.attachment_tasks
            .push(cx.spawn_in(window, async move |this, cx| {
                let result = work.await;
                // Moving a chat can close the original window while its entity lives.
                // Release the pending guard even when the window-bound action cannot run.
                if this
                    .update(cx, |view, cx| {
                        view.media.action_pending = false;
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
                let _ = this.update_in(cx, |view, window, cx| {
                    cx.notify();
                    if view.image_scope != expected_scope
                        || !view.media.slots.get(&key).is_some_and(|slot| {
                            slot.generation == generation && slot.source == source
                        })
                    {
                        return;
                    }
                    match result {
                        Ok((Some(bytes), _)) => match action {
                            Action::Copy => {
                                let image = gpui::Image::from_bytes(gpui::ImageFormat::Png, bytes);
                                cx.write_to_clipboard(ClipboardItem::new_image(&image));
                                view.notices.set(
                                    LocalKey::Export,
                                    NoticeLevel::Info,
                                    "Image copied.",
                                );
                            }
                            Action::Save => {
                                view.export_bytes(bytes, export_name(&source), window, cx)
                            }
                            Action::Original => {}
                        },
                        Ok((_, Some(path))) => cx.open_with_system(&path),
                        Ok(_) => {}
                        Err(error) => view
                            .notices
                            .set(LocalKey::Export, NoticeLevel::Error, error),
                    }
                });
            }));
    }
}

#[cfg(test)]
mod tests;
