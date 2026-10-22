//! Chat path recognition does no I/O; deliberate preview clicks validate inferred links.
use std::{
    ops::Range,
    path::{Path, PathBuf},
};

pub fn local_target(target: &str) -> Result<(PathBuf, Option<u32>), String> {
    let target = target.trim().trim_start_matches('<').trim_end_matches('>');
    if target.is_empty() || target.len() > 4096 || target.contains(['\0', '\n', '\r']) {
        return Err("Invalid file path".into());
    }
    let target = if let Some(path) = target.strip_prefix("file://") {
        if !path.starts_with('/') {
            return Err("Only local file URLs are supported".into());
        }
        path
    } else {
        target
    };
    if target.contains("://") || target.starts_with("data:") || target.starts_with("mailto:") {
        return Err("Unsupported file link".into());
    }
    let decoded = percent_decode(target)?;
    let (path, line, _) = crate::terminal_links::split_position(&decoded);
    if decoded.contains('#') && line.is_none() {
        return Err("File anchors are not preview targets".into());
    }
    if path.contains(':') && !Path::new(path).is_absolute() {
        return Err("Unsupported file link".into());
    }
    Ok((PathBuf::from(path), line))
}
pub(super) fn percent_decode(text: &str) -> Result<String, String> {
    let mut bytes = Vec::new();
    let source = text.as_bytes();
    let mut i = 0;
    while i < source.len() {
        if source[i] == b'%' && i + 2 < source.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if let (Some(a), Some(b)) = (hex(source[i + 1]), hex(source[i + 2])) {
                bytes.push((a * 16 + b) as u8);
                i += 3;
                continue;
            }
        }
        bytes.push(source[i]);
        i += 1;
    }
    let decoded = String::from_utf8(bytes).map_err(|_| "Invalid UTF-8 file URL")?;
    if decoded.contains('\0') {
        return Err("Invalid file path".into());
    }
    Ok(decoded)
}
pub fn resolve(root: &Path, cwd: &Path, target: &str) -> Result<(PathBuf, Option<u32>), String> {
    let (path, line) = local_target(target)?;
    let path = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return Err("This file is outside the selected worktree.".into());
                }
            }
            std::path::Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    let path = normalized;
    crate::file_preview::validated_live_editor_path(root, &path).map(|path| (path, line))
}
pub(super) fn shaped(text: &str) -> bool {
    if crate::terminal_links::is_openable_url(text) || text.contains("://") {
        return false;
    }
    let Ok((path, _)) = local_target(text) else {
        return false;
    };
    let text = path.to_string_lossy();
    text.contains('/')
        || path.extension().is_some_and(|ext| {
            !ext.is_empty() && ext.to_string_lossy().chars().all(char::is_alphanumeric)
        })
}
/// Quoted paths may contain spaces. An inline-code path is handled as a whole.
pub fn detect(text: &str, whole: bool) -> Vec<(Range<usize>, String)> {
    if text.len() > 64 * 1024 {
        return Vec::new();
    }
    if whole && shaped(text) {
        return vec![(0..text.len(), text.to_owned())];
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() && out.len() < 64 {
        let c = text[i..].chars().next().unwrap();
        if c.is_whitespace() {
            i += c.len_utf8();
            continue;
        }
        let (start, end, next) = if matches!(c, '"' | '\'' | '`') {
            let start = i + c.len_utf8();
            if let Some(end) = text[start..].find(c) {
                (start, start + end, start + end + c.len_utf8())
            } else {
                i += c.len_utf8();
                continue;
            }
        } else {
            let end = text[i..]
                .find(char::is_whitespace)
                .map_or(text.len(), |end| i + end);
            let token = text[i..end]
                .trim_start_matches(['(', '['])
                .trim_end_matches([',', '.', ';', ')', ']']);
            let start = i + text[i..end].find(token).unwrap_or(0);
            (start, start + token.len(), end)
        };
        if shaped(&text[start..end]) {
            out.push((start..end, text[start..end].into()));
        }
        i = next;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preview_rejects_missing_files_traversal_and_symlinks() {
        let root = std::env::temp_dir().join(format!("riwork-chat-path-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("space ü.txt"), "hi").unwrap();
        assert_eq!(
            resolve(&root, &root, "space ü.txt:2").unwrap(),
            (root.join("space ü.txt").canonicalize().unwrap(), Some(2))
        );
        assert!(resolve(&root, &root, "missing.txt").is_err());
        assert!(resolve(&root, &root, "../escape.txt").is_err());
        std::os::unix::fs::symlink("/etc/hosts", root.join("link.txt")).unwrap();
        assert!(resolve(&root, &root, "link.txt").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn large_turn_preview_reuses_validation_and_indexes_unique_paths() {
        use crate::chat::model::{
            ChatImage, ImageSource, Item, ItemBody, ItemStatus, Presentation, Semantics, ToolKind,
        };
        let root =
            std::env::temp_dir().join(format!("riwork-preview-perf-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("report.md"), "inline link target").unwrap();
        std::fs::write(root.join("screenshot.png"), "fixture image reference").unwrap();
        let image_path = root.join("screenshot.png").to_string_lossy().into_owned();
        let mut output = "report.md:12: inline output reference\n".repeat(2000);
        output.truncate(64 * 1024);
        let items: Vec<_> = (0..1200)
            .map(|n| Item {
                id: format!("read-{n}"),
                turn_id: Some("dense-turn".into()),
                status: ItemStatus::Completed,
                started_at: None,
                ended_at: None,
                presentation: Presentation {
                    semantics: Some(Semantics {
                        kind: ToolKind::Read,
                        target: Some(format!("src/removed-{n}.rs")),
                        ..Semantics::default()
                    }),
                    images: if n == 0 {
                        vec![ChatImage {
                            label: "screenshot.png".into(),
                            source: ImageSource::Local {
                                path: image_path.clone(),
                            },
                        }]
                    } else {
                        Vec::new()
                    },
                    ..Presentation::default()
                },
                body: ItemBody::ToolCall {
                    server: None,
                    tool: "Read".into(),
                    input: serde_json::Value::Null,
                    output: Some(output.clone()),
                },
            })
            .collect();
        // Every call creates fresh discovery caches; repeat to avoid a single
        // filesystem/scheduler stall deciding the regression result.
        let mut elapsed = [std::time::Duration::ZERO; 3];
        for duration in &mut elapsed {
            let started = std::time::Instant::now();
            let sequence = turn_sequence(&items, &image_path, Some("read-0"), Some(&root));
            *duration = started.elapsed();
            assert_eq!(sequence.entries.len(), 1202);
            assert_eq!(sequence.current().unwrap().target, image_path);
            assert!(
                sequence
                    .entries
                    .iter()
                    .any(|entry| entry.target == "report.md:12")
            );
        }
        elapsed.sort();
        let median = elapsed[1];
        eprintln!(
            "preview perf: 1200 reads/64KiB outputs, cold median={:.3}ms samples={elapsed:?}",
            median.as_secs_f64() * 1000.
        );
        assert!(
            median < std::time::Duration::from_millis(500),
            "median cold preview discovery regressed to {:.3}ms",
            median.as_secs_f64() * 1000.
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// Collect the actual rendered Markdown links, including image sources and code paths.
fn message_references(text: &str) -> Vec<(String, Option<crate::chat::model::ChatImage>)> {
    use super::markdown::{self, Block, Span};
    let mut refs = Vec::new();
    fn spans(spans: &[Span], refs: &mut Vec<(String, Option<crate::chat::model::ChatImage>)>) {
        for span in spans {
            if let Some(target) = &span.link {
                if span.image {
                    refs.push((
                        target.clone(),
                        Some(crate::chat::media::reference(&span.text, target)),
                    ));
                } else if local_target(target).is_ok() {
                    refs.push((target.clone(), None));
                }
            } else if span.style.code {
                refs.extend(
                    detect(&span.text, true)
                        .into_iter()
                        .map(|(_, path)| (path, None)),
                );
            }
        }
    }
    fn walk(blocks: &[Block], refs: &mut Vec<(String, Option<crate::chat::model::ChatImage>)>) {
        for block in blocks {
            match block {
                Block::Heading { spans: parts, .. }
                | Block::Paragraph(parts)
                | Block::Task { spans: parts, .. } => spans(parts, refs),
                Block::Code { .. } => {}
                Block::List { items, .. } => {
                    for item in items {
                        walk(item, refs)
                    }
                }
                Block::Quote(items) | Block::Callout { blocks: items, .. } => walk(items, refs),
                Block::Table { header, rows, .. } => {
                    for cell in header {
                        spans(cell, refs);
                    }
                    for row in rows {
                        for cell in row {
                            spans(cell, refs);
                        }
                    }
                }
                Block::Rule => {}
                Block::Files(files) => refs.extend(files.iter().map(|file| {
                    let image = markdown::is_local_image(&file.path)
                        .then(|| crate::chat::media::reference(file.name(), &file.path));
                    (file.path.clone(), image)
                })),
            }
        }
    }
    walk(&markdown::parse(text), &mut refs);
    refs
}

/// Retained image payloads often arrive labeled only "Image". Prefer their real
/// file source or the owning call's file target, without exposing the retention key.
fn image_preview_title(
    image: &crate::chat::model::ChatImage,
    item: &crate::chat::model::Item,
) -> String {
    use crate::chat::model::{ImageSource, ItemBody};
    let filename = |target: &str| {
        local_target(target).ok().and_then(|(path, _)| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
    };
    match &image.source {
        ImageSource::Local { path } => {
            if let Some(name) = filename(path) {
                return name;
            }
        }
        ImageSource::Url { url } => {
            let path = url.split(['?', '#']).next().unwrap_or(url);
            if let Some(name) = path.rsplit('/').next().filter(|name| !name.is_empty()) {
                return percent_decode(name).unwrap_or_else(|_| name.to_owned());
            }
        }
        _ => {}
    }
    let call_target = item
        .presentation
        .semantics
        .as_ref()
        .and_then(|semantics| semantics.target.as_deref())
        .filter(|target| shaped(target))
        .or_else(|| match &item.body {
            ItemBody::ToolCall { input, .. } => ["path", "file_path", "filePath", "target"]
                .into_iter()
                .find_map(|key| input[key].as_str().filter(|target| shaped(target))),
            _ => None,
        });
    if item.presentation.images.len() == 1
        && let Some(name) = call_target.and_then(filename)
    {
        return name;
    }
    let label = image.label.trim();
    if !label.is_empty()
        && !label.eq_ignore_ascii_case("image")
        && label != "[image]"
        && !label.starts_with("image:")
        && !label.contains("toolu_")
    {
        return label.to_owned();
    }
    if matches!(
        item.body,
        ItemBody::ToolCall { .. } | ItemBody::Command { .. }
    ) {
        "Tool result image".into()
    } else {
        "Image".into()
    }
}

/// Build once for every entry point; the sequence is independent of disclosure and mode.
fn turn_sequence(
    items: &[crate::chat::model::Item],
    target: &str,
    owner: Option<&str>,
    cwd: Option<&Path>,
) -> crate::file_preview::PreviewSequence {
    use crate::{
        chat::{
            attachments::AttachmentKind,
            model::{ImageSource, ItemBody, ToolKind},
        },
        file_preview::{PreviewEntry, PreviewSequence},
    };
    use std::{
        cell::RefCell,
        collections::{HashMap, HashSet},
        rc::Rc,
    };
    // Long turns repeat the same paths in thousands of output lines. Validate
    // and normalize each spelling once per discovery, rather than issuing a
    // filesystem walk for every occurrence on the UI thread.
    let normalized_paths = RefCell::new(HashMap::<String, Option<Rc<PathBuf>>>::new());
    let normalize = |target: &str| -> Option<Rc<PathBuf>> {
        if let Some(known) = normalized_paths.borrow().get(target) {
            return known.clone();
        }
        let normalized = local_target(target).ok().map(|(path, _)| {
            let path = if path.is_absolute() {
                path
            } else {
                cwd.map_or(path.clone(), |cwd| cwd.join(&path))
            };
            let mut normalized = PathBuf::new();
            for part in path.components() {
                match part {
                    std::path::Component::CurDir => {}
                    std::path::Component::ParentDir => {
                        normalized.pop();
                    }
                    part => normalized.push(part.as_os_str()),
                }
            }
            Rc::new(normalized)
        });
        normalized_paths
            .borrow_mut()
            .insert(target.to_owned(), normalized.clone());
        normalized
    };
    let same = |a: &str, b: &str| matches!((normalize(a),normalize(b)), (Some(a),Some(b)) if a==b);
    let existing_paths = RefCell::new(HashMap::<String, bool>::new());
    let existing = |target: &str| {
        if let Some(known) = existing_paths.borrow().get(target) {
            return *known;
        }
        let exists = cwd.is_some_and(|cwd| resolve(cwd, cwd, target).is_ok());
        existing_paths
            .borrow_mut()
            .insert(target.to_owned(), exists);
        exists
    };
    #[derive(Hash, PartialEq, Eq)]
    enum TargetKey {
        Local(Rc<PathBuf>),
        Other(String),
    }
    let target_key = |target: &str| {
        normalize(target)
            .map(TargetKey::Local)
            .unwrap_or_else(|| TargetKey::Other(target.to_owned()))
    };
    let canonical_target = |target: &str| -> Option<String> {
        let (path, line) = local_target(target).ok()?;
        Some(line.map_or_else(
            || path.to_string_lossy().into_owned(),
            |line| format!("{}:{line}", path.to_string_lossy()),
        ))
    };
    let owner_at = owner.and_then(|id| items.iter().position(|item| item.id == id));
    let matching = |item: &crate::chat::model::Item| {
        item.presentation
            .attachments
            .iter()
            .any(|a| same(&a.path.to_string_lossy(), target))
            || item
                .presentation
                .images
                .iter()
                .enumerate()
                .any(|(n, image)| match &image.source {
                    ImageSource::Local { path } => same(path, target),
                    ImageSource::Url { url } => url == target,
                    _ => format!("image:{}:{n}", item.id) == target,
                })
            || item.presentation.semantics.as_ref().is_some_and(|s| {
                s.target.as_deref().is_some_and(|t| same(t, target))
                    || s.produced.iter().any(|p| same(&p.path, target))
            })
            || match &item.body {
                ItemBody::FileChange { changes } => changes.iter().any(|c| same(&c.path, target)),
                ItemBody::AgentMessage { text } | ItemBody::UserMessage { text } => {
                    message_references(text)
                        .iter()
                        .any(|(t, _)| same(t, target) || t == target)
                }
                ItemBody::Command {
                    command, output, ..
                } => detect(command, false)
                    .into_iter()
                    .chain(detect(output, false))
                    .any(|(_, path)| existing(&path) && same(&path, target)),
                ItemBody::ToolCall { output, .. } => output.as_ref().is_some_and(|output| {
                    detect(output, false)
                        .iter()
                        .any(|(_, path)| existing(path) && same(path, target))
                }),
                _ => false,
            }
    };
    let anchor = owner_at.or_else(|| items.iter().rposition(matching));
    let turn = anchor.and_then(|at| items[at].turn_id.as_deref());
    let selected: Vec<_> = items
        .iter()
        .filter(|item| match turn {
            Some(turn) => item.turn_id.as_deref() == Some(turn),
            None => anchor.is_some_and(|at| item.id == items[at].id),
        })
        .collect();
    let mut entries: Vec<PreviewEntry> = Vec::new();
    let mut entry_indices = HashMap::<TargetKey, usize>::new();
    let mut image_indices = Vec::<usize>::new();
    let mut output_paths = HashMap::<&str, Vec<String>>::new();
    let mut owner_index = None;
    let source = |item: &crate::chat::model::Item| -> String {
        if matches!(item.body, ItemBody::UserMessage { .. }) {
            return "Your message".into();
        }
        if let Some(s) = &item.presentation.semantics {
            let verb = match s.kind {
                ToolKind::Read => "Read",
                ToolKind::Search => "Searched",
                ToolKind::List => "Listed",
                ToolKind::Shell => "Ran",
                ToolKind::WebSearch => "Searched the web",
                ToolKind::WebFetch => "Fetched",
                ToolKind::Mcp => "Called",
                ToolKind::Subagent => "Agent",
                ToolKind::Other => "Tool",
            };
            let target = s
                .target
                .as_deref()
                .or_else(|| match &item.body {
                    ItemBody::Command { command, .. } => Some(command.as_str()),
                    ItemBody::ToolCall { tool, .. } => Some(tool.as_str()),
                    _ => None,
                })
                .unwrap_or_default();
            return format!("{verb} {target}").trim().to_owned();
        }
        match &item.body {
            ItemBody::FileChange { changes } => format!(
                "Edited {}",
                changes.first().map(|c| c.path.as_str()).unwrap_or_default()
            ),
            ItemBody::Command { command, .. } => format!("Ran {command}"),
            ItemBody::ToolCall { tool, .. } => tool.clone(),
            _ => "Answer".into(),
        }
    };
    for user in [true, false] {
        for item in &selected {
            if matches!(item.body, ItemBody::UserMessage { .. }) != user {
                continue;
            }
            let origin = source(item);
            let mut own = Vec::new();
            for attachment in &item.presentation.attachments {
                own.push(PreviewEntry {
                    target: attachment.path.to_string_lossy().into_owned(),
                    title: attachment.name.clone(),
                    source: origin.clone(),
                    image: matches!(attachment.kind, AttachmentKind::Image { .. }).then(|| {
                        crate::chat::model::ChatImage {
                            label: attachment.name.clone(),
                            source: ImageSource::Local {
                                path: attachment.path.to_string_lossy().into_owned(),
                            },
                        }
                    }),
                    attachment: true,
                });
            }
            let mut add = |path: String| {
                let Some(path) = canonical_target(&path) else {
                    return;
                };
                if own.iter().any(|e| same(&e.target, &path)) {
                    return;
                }
                let title = local_target(&path)
                    .ok()
                    .and_then(|(p, _)| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .unwrap_or_else(|| path.clone());
                own.push(PreviewEntry {
                    target: path,
                    title,
                    source: origin.clone(),
                    image: None,
                    attachment: false,
                });
            };
            if let Some(s) = &item.presentation.semantics {
                if matches!(s.kind, ToolKind::Read)
                    && let Some(t) = &s.target
                {
                    add(s
                        .range
                        .as_ref()
                        .map_or_else(|| t.clone(), |range| format!("{t}:{}", range.start)));
                }
                for path in &s.produced {
                    add(path
                        .line
                        .map_or_else(|| path.path.clone(), |line| format!("{}:{line}", path.path)));
                }
            }
            if let ItemBody::FileChange { changes } = &item.body {
                for c in changes {
                    add(c.path.clone());
                }
            }
            // The same parser drives inline links in opened output and command headers.
            let output = match &item.body {
                ItemBody::ToolCall { output, .. } => output.as_deref(),
                ItemBody::Command {
                    command, output, ..
                } => {
                    for (_, path) in detect(command, false) {
                        if existing(&path) {
                            add(path);
                        }
                    }
                    Some(output.as_str())
                }
                _ => None,
            };
            if let Some(output) = output {
                let visible = if output.len() <= 64 * 1024 {
                    output
                } else {
                    super::cards::tail(output, 200, 32 * 1024).0
                };
                // Commands and reads often repeat an identical output preview.
                // Borrow its contents as the cache key; retain only discovered
                // paths and never another copy of the large source payload.
                let paths = output_paths.entry(visible).or_insert_with(|| {
                    let mut seen = HashSet::new();
                    detect(visible, false)
                        .into_iter()
                        .map(|(_, path)| path)
                        .filter(|path| seen.insert(path.clone()))
                        .collect()
                });
                for path in paths {
                    if existing(path) {
                        add(path.clone());
                    }
                }
            }

            let mut message_images = Vec::new();
            if let ItemBody::AgentMessage { text } | ItemBody::UserMessage { text } = &item.body {
                for (path, image) in message_references(text) {
                    if let Some(image) = image {
                        message_images.push(image);
                        continue;
                    }
                    // Answer links select the matching call reference instead of duplicating it.
                    if normalize(&path)
                        .is_some_and(|path| entry_indices.contains_key(&TargetKey::Local(path)))
                    {
                        continue;
                    }
                    if existing(&path) {
                        add(path);
                    }
                }
            }
            for (image_index, image) in item
                .presentation
                .images
                .iter()
                .chain(message_images.iter())
                .enumerate()
            {
                let path = match &image.source {
                    ImageSource::Local { path } => path.clone(),
                    ImageSource::Url { url } => url.clone(),
                    _ => format!("image:{}:{image_index}", item.id),
                };
                if let Some(entry) = own.iter_mut().find(|e| {
                    same(&e.target, &path)
                        || e.target == path
                        || e.image
                            .as_ref()
                            .is_some_and(|known| known.source == image.source)
                }) {
                    entry.image = Some(image.clone());
                    if !entry.attachment {
                        entry.title = image_preview_title(image, item);
                    }
                    continue;
                }
                own.push(PreviewEntry {
                    target: path,
                    title: image_preview_title(image, item),
                    source: origin.clone(),
                    image: Some(image.clone()),
                    attachment: user,
                });
            }
            for entry in own {
                let key = target_key(&entry.target);
                let matching_path = entry_indices.get(&key).copied();
                let matching_image = entry.image.as_ref().and_then(|image| {
                    image_indices.iter().copied().find(|&at| {
                        entries[at]
                            .image
                            .as_ref()
                            .is_some_and(|known| known.source == image.source)
                    })
                });
                let index = matching_path
                    .into_iter()
                    .chain(matching_image)
                    .min()
                    .unwrap_or_else(|| {
                        entries.push(entry.clone());
                        let index = entries.len() - 1;
                        entry_indices.insert(key, index);
                        if entry.image.is_some() {
                            image_indices.push(index);
                        }
                        index
                    });
                if owner == Some(item.id.as_str())
                    && (same(&entry.target, target) || entry.target == target)
                {
                    owner_index = Some(index);
                }
                if entries[index].image.is_none() && entry.image.is_some() {
                    entries[index].image = entry.image;
                    image_indices.push(index);
                }
            }
        }
    }
    let index = owner_index
        .or_else(|| {
            entries.iter().position(|e| {
                e.source != "Your message"
                    && same(&e.target, target)
                    && local_target(&e.target).ok().and_then(|(_, line)| line)
                        == local_target(target).ok().and_then(|(_, line)| line)
            })
        })
        .or_else(|| {
            entries.iter().position(|e| {
                e.source != "Your message" && (same(&e.target, target) || e.target == target)
            })
        })
        .or_else(|| {
            entries
                .iter()
                .position(|e| same(&e.target, target) || e.target == target)
        });
    let index = if let Some(index) = index {
        index
    } else if existing(target) {
        entries.push(PreviewEntry {
            target: canonical_target(target).unwrap_or_else(|| target.into()),
            title: local_target(target)
                .ok()
                .and_then(|(p, _)| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_else(|| target.into()),
            source: "Answer".into(),
            image: None,
            attachment: false,
        });
        entries.len() - 1
    } else {
        return PreviewSequence::default();
    };
    // An answer reference reuses its call's sequence position, but keeps the
    // exact line the reader clicked rather than the call's earlier read range.
    if let Ok((_, Some(line))) = local_target(target)
        && let Some(entry) = entries.get_mut(index).filter(|entry| entry.image.is_none())
        && let Ok((path, _)) = local_target(&entry.target)
    {
        entry.target = format!("{}:{line}", path.to_string_lossy());
    }
    PreviewSequence { entries, index }
}

impl super::ChatView {
    pub fn file_preview_sequence(&self, target: &str) -> crate::file_preview::PreviewSequence {
        turn_sequence(
            &self.model.transcript.items,
            target,
            None,
            self.info().map(|info| info.cwd.as_path()),
        )
    }
    pub(super) fn preview_image_reference(
        &self,
        image: &crate::chat::model::ChatImage,
        key: &str,
        owner: Option<&str>,
        cx: &mut gpui::Context<Self>,
    ) {
        use crate::chat::model::ImageSource;
        let owner = owner.or_else(|| {
            self.model
                .transcript
                .items
                .iter()
                .filter(|item| key.starts_with(&item.id))
                .max_by_key(|item| item.id.len())
                .map(|item| item.id.as_str())
        });
        let target = match &image.source {
            ImageSource::Local { path } => path.clone(),
            ImageSource::Url { url } => url.clone(),
            _ => format!(
                "image:{}:{}",
                owner.unwrap_or(key),
                key.rsplit(":image:").next().unwrap_or("0")
            ),
        };
        let mut sequence = turn_sequence(
            &self.model.transcript.items,
            &target,
            owner,
            self.info().map(|info| info.cwd.as_path()),
        );
        if sequence
            .current()
            .is_some_and(|entry| entry.image.is_none())
        {
            if let Some(index) = sequence.entries.iter().position(|entry| {
                entry
                    .image
                    .as_ref()
                    .is_some_and(|known| known.source == image.source)
            }) {
                sequence.index = index;
            }
        }
        if let Some(entry) = sequence.entries.get_mut(sequence.index) {
            if entry.image.is_none() {
                entry.image = Some(image.clone());
            }
        }
        cx.emit(super::ChatViewEvent::OpenPreview { sequence });
    }

    pub(super) fn preview_reference(
        &self,
        target: &str,
        owner: Option<&str>,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.info().is_some_and(|info| {
            matches!(
                info.orchestrator,
                Some(crate::chat::model::OrchestratorScope::Global)
            )
        }) {
            cx.emit(super::ChatViewEvent::OpenFile {
                target: target.into(),
            });
        } else {
            cx.emit(super::ChatViewEvent::OpenPreview {
                sequence: turn_sequence(
                    &self.model.transcript.items,
                    target,
                    owner,
                    self.info().map(|info| info.cwd.as_path()),
                ),
            });
        }
    }
}
