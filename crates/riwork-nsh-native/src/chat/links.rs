//! Chat path recognition does no I/O; deliberate preview clicks validate inferred links.
//!
//! Ported from the pure part of `crates/riwork-desktop/src/chat_view/links.rs` (GPUI
//! parity reference). The preview sequence and message-reference walking stay with the
//! views that need them.
use std::{
    borrow::Cow,
    ops::Range,
    path::{Path, PathBuf},
};

use riwork_engine::terminal_links;

pub fn local_target(target: &str) -> Result<(PathBuf, Option<u32>), String> {
    local_path(target, |path, line| (path.to_path_buf(), line)).map_err(str::to_owned)
}
/// `local_target` without allocating: drawing a message asks `shaped` this of every word,
/// on every frame, and nearly all of them are not paths.
fn local_path<T>(
    target: &str,
    found: impl FnOnce(&Path, Option<u32>) -> T,
) -> Result<T, &'static str> {
    let target = target.trim().trim_start_matches('<').trim_end_matches('>');
    if target.is_empty() || target.len() > 4096 || target.contains(['\0', '\n', '\r']) {
        return Err("Invalid file path");
    }
    let target = if let Some(path) = target.strip_prefix("file://") {
        if !path.starts_with('/') {
            return Err("Only local file URLs are supported");
        }
        path
    } else {
        target
    };
    if target.contains("://") || target.starts_with("data:") || target.starts_with("mailto:") {
        return Err("Unsupported file link");
    }
    let decoded = decode(target)?;
    let (path, line, _) = terminal_links::split_position(&decoded);
    if decoded.contains('#') && line.is_none() {
        return Err("File anchors are not preview targets");
    }
    if path.contains(':') && !Path::new(path).is_absolute() {
        return Err("Unsupported file link");
    }
    Ok(found(Path::new(path), line))
}
pub fn percent_decode(text: &str) -> Result<String, String> {
    decode(text).map(Cow::into_owned).map_err(str::to_owned)
}
/// Text without a `%` decodes to itself, so it is borrowed.
fn decode(text: &str) -> Result<Cow<'_, str>, &'static str> {
    let decoded = if text.contains('%') {
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
        Cow::Owned(String::from_utf8(bytes).map_err(|_| "Invalid UTF-8 file URL")?)
    } else {
        Cow::Borrowed(text)
    };
    if decoded.contains('\0') {
        return Err("Invalid file path");
    }
    Ok(decoded)
}
/// A deliberate preview click: the target inside `root`, read relative to `cwd`. This one
/// does I/O (it checks the live file), unlike everything else here.
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
    riwork_engine::file_preview::validated_live_editor_path(root, &path).map(|path| (path, line))
}
pub fn shaped(text: &str) -> bool {
    if terminal_links::is_openable_url(text) || text.contains("://") {
        return false;
    }
    local_path(text, |path, _| {
        let text = path.to_string_lossy();
        text.contains('/')
            || path.extension().is_some_and(|ext| {
                !ext.is_empty() && ext.to_string_lossy().chars().all(char::is_alphanumeric)
            })
    })
    .unwrap_or(false)
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
    fn words_read_as_paths_only_when_shaped_like_one() {
        for (word, path) in [
            ("src/lib.rs", true),
            ("lib.rs:12", true),
            ("README.md#L3", true),
            ("%2Ftmp", true),
            ("file:///tmp/a%20b.rs", false),
            ("<docs/guide>", true),
            ("hello", false),
            ("issue:", false),
            ("e.g.", false),
            ("a#b", false),
            ("C:thing", false),
            ("https://example.com/a.rs", false),
            ("file://relative/a.rs", false),
            ("data:text/plain", false),
            ("bad%FF.rs", false),
            ("nul%00.rs", false),
            ("v1.2-beta", false),
        ] {
            assert_eq!(shaped(word), path, "{word}");
        }
        assert_eq!(
            local_target("file:///tmp/a%20%C3%BC.rs:12").unwrap(),
            (PathBuf::from("/tmp/a ü.rs"), Some(12))
        );
        assert_eq!(
            local_target("a#b").unwrap_err(),
            "File anchors are not preview targets"
        );
        assert_eq!(percent_decode("100%").unwrap(), "100%");
        assert_eq!(percent_decode("%zz.rs").unwrap(), "%zz.rs");
        assert_eq!(
            percent_decode("bad%FF").unwrap_err(),
            "Invalid UTF-8 file URL"
        );
        assert_eq!(
            detect("see src/lib.rs, (docs/a.md) and `x y.rs` here", false),
            vec![
                (4..14, "src/lib.rs".to_owned()),
                (17..26, "docs/a.md".to_owned()),
                (33..39, "x y.rs".to_owned()),
            ]
        );
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
