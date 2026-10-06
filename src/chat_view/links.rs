//! Chat path recognition does no I/O. Resolve only after a deliberate click/expand.
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
    if path.contains(':') && !Path::new(path).is_absolute() {
        return Err("Unsupported file link".into());
    }
    let path = if line.is_none() && decoded.contains('#') {
        &decoded
    } else {
        path
    };
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
fn shaped(text: &str) -> bool {
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
    fn unicode_spaces_positions_and_http_are_recognized_without_io() {
        assert_eq!(
            local_target("<docs/Grüße file.rs:12:3>").unwrap(),
            (PathBuf::from("docs/Grüße file.rs"), Some(12))
        );
        assert_eq!(
            local_target("file:///tmp/a%20b.txt#L2").unwrap(),
            (PathBuf::from("/tmp/a b.txt"), Some(2))
        );
        assert_eq!(local_target("a#b.txt").unwrap().0, PathBuf::from("a#b.txt"));
        assert_eq!(local_target("%☀.png").unwrap().0, PathBuf::from("%☀.png"));
        let text = "Read `docs/Grüße file.rs:12` and src/main.rs:3, https://example.com/a.rs";
        let links = detect(text, false);
        assert_eq!(links.len(), 2);
        assert_eq!(&text[links[0].0.clone()], "docs/Grüße file.rs:12");
    }
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
}
