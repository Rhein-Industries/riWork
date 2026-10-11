//! No-follow editor validation and bounded text-file policy.

use std::{
    ffi::{CString, OsStr},
    fs::{self, File, Metadata},
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path},
};

const TEXT_LIMIT: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileIdentity {
    device: u64,
    inode: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

impl FileIdentity {
    pub fn of(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
        }
    }
}

/// Check a selected file for a new editor session using the same no-follow
/// walk as previews. A changed selection must be refreshed before Vim opens
/// it, so the user cannot unknowingly edit a replacement file.
pub fn validated_editor_path(
    root: &Path,
    path: &Path,
    expected: FileIdentity,
) -> Result<std::path::PathBuf, String> {
    let (file, resolved_path) = open_regular_in(root, path)?;
    let actual = FileIdentity::of(
        &file
            .metadata()
            .map_err(|error| format!("Cannot inspect this file: {error}"))?,
    );
    if actual != expected {
        return Err("This file changed or was replaced. Refresh Files and try again.".into());
    }
    Ok(resolved_path)
}

/// Resolve a file for returning to an already-live editor. Vim may have
/// changed the file's size, timestamp, or inode while saving, so the cached
/// tree identity is not used here. The no-follow regular-file check still
/// rejects links, missing files, and paths outside the selected worktree.
pub fn validated_live_editor_path(root: &Path, path: &Path) -> Result<std::path::PathBuf, String> {
    let (_file, resolved_path) = open_regular_in(root, path)?;
    Ok(resolved_path)
}

/// Text with control characters other than newline, carriage return and tab
/// is treated as binary, by the preview and by Copy Contents alike.
fn has_binary_control(text: &str) -> bool {
    text.chars()
        .any(|ch| ch == '\0' || (ch.is_control() && !matches!(ch, '\n' | '\r' | '\t')))
}

fn is_image(extension: &str) -> bool {
    matches!(
        extension,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "ico"
    )
}

/// Why a file's contents cannot be copied, judged from its listing alone so
/// the button can say so up front. Binary content is only known after reading,
/// which `read_text` checks again together with the size.
pub fn copy_block(path: &Path, identity: FileIdentity) -> Option<&'static str> {
    let extension = path
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    if is_image(&extension) || extension == "pdf" {
        Some("Images and PDFs cannot be copied as text")
    } else if identity.length > TEXT_LIMIT {
        Some(TOO_LARGE_TO_COPY)
    } else {
        None
    }
}

const TOO_LARGE_TO_COPY: &str = "Too large to copy (over 1 MiB)";

/// The exact text of a file for the clipboard: no clipping, no line cap, line
/// endings untouched. It opens through the same no-follow walk as previews and
/// refuses a file that is not the one selected, so a replacement is never
/// copied. A file over `TEXT_LIMIT` is refused outright rather than copied in
/// part.
pub fn read_text(root: &Path, path: &Path, expected: FileIdentity) -> Result<String, String> {
    let (mut file, _) = open_regular_in(root, path)?;
    let before = file
        .metadata()
        .map_err(|error| format!("Cannot inspect this file: {error}"))?;
    if FileIdentity::of(&before) != expected {
        return Err("This file changed or was replaced. Refresh Files and try again.".into());
    }
    if before.len() > TEXT_LIMIT {
        return Err(TOO_LARGE_TO_COPY.into());
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(TEXT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Cannot read this file: {error}"))?;
    let after = file
        .metadata()
        .map_err(|error| format!("Cannot inspect this file: {error}"))?;
    if FileIdentity::of(&before) != FileIdentity::of(&after) {
        return Err("This file changed while it was being read. Refresh and try again.".into());
    }
    if bytes.len() as u64 > TEXT_LIMIT {
        return Err(TOO_LARGE_TO_COPY.into());
    }
    let text = String::from_utf8(bytes)
        .ok()
        .filter(|text| !has_binary_control(text))
        .ok_or("Binary or non-UTF-8 content cannot be copied as text.")?;
    Ok(text)
}

pub fn open_regular_in(root: &Path, path: &Path) -> Result<(File, std::path::PathBuf), String> {
    if !root.is_absolute() {
        return Err("The selected worktree path is not absolute.".into());
    }
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "This file is outside the selected worktree.".to_owned())?;
    if relative.as_os_str().is_empty() {
        return Err("Select a file to preview.".into());
    }
    if fs::symlink_metadata(root)
        .map_err(|error| format!("Cannot inspect the worktree: {error}"))?
        .file_type()
        .is_symlink()
    {
        return Err("Symbolic-link worktree roots cannot be previewed.".into());
    }
    // macOS's /var and some user-selected parent folders are symlinks. Resolve
    // ancestors of the trusted worktree root once, then walk every descendant
    // with no-follow directory descriptors.
    let resolved_root =
        fs::canonicalize(root).map_err(|error| format!("Cannot locate the worktree: {error}"))?;
    let base = CString::new("/").unwrap();
    let fd = unsafe {
        libc::open(
            base.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(format!(
            "Cannot open the worktree: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut directory = unsafe { File::from_raw_fd(fd) };
    for component in resolved_root
        .components()
        .chain(relative.parent().unwrap_or(Path::new("")).components())
    {
        let Component::Normal(name) = component else {
            if matches!(component, Component::RootDir) {
                continue;
            }
            return Err("This file is outside the selected worktree.".into());
        };
        let c_name = CString::new(name.as_bytes()).map_err(|_| "Invalid file name.".to_owned())?;
        let next = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                c_name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if next < 0 {
            return Err(format!(
                "Cannot open a folder in this worktree: {}",
                std::io::Error::last_os_error()
            ));
        }
        directory = unsafe { File::from_raw_fd(next) };
    }
    let name = relative.file_name().ok_or("Select a file to preview.")?;
    let c_name = CString::new(name.as_bytes()).map_err(|_| "Invalid file name.".to_owned())?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c_name.as_ptr(),
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(format!(
            "Cannot open this file: {}",
            std::io::Error::last_os_error()
        ));
    }
    let file = unsafe { File::from_raw_fd(fd) };
    if !file
        .metadata()
        .map_err(|error| format!("Cannot inspect this file: {error}"))?
        .is_file()
    {
        return Err("Only regular files can be previewed.".into());
    }
    Ok((file, resolved_root.join(relative)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };

    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "riwork-preview-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn editor_path_requires_the_selected_regular_file_in_the_worktree() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let outside = Fixture::new();
        let file = fixture.0.join("café ' ;.txt");
        fs::write(&file, "original\n").unwrap();
        let identity = FileIdentity::of(&fs::symlink_metadata(&file).unwrap());
        assert_eq!(
            validated_editor_path(&fixture.0, &file, identity).unwrap(),
            fixture.0.canonicalize().unwrap().join("café ' ;.txt")
        );
        assert!(validated_editor_path(&fixture.0, &outside.0.join("x"), identity).is_err());
        symlink(&file, fixture.0.join("link.txt")).unwrap();
        assert!(validated_editor_path(&fixture.0, &fixture.0.join("link.txt"), identity).is_err());
        fs::rename(&file, fixture.0.join("old.txt")).unwrap();
        fs::write(&file, "replacement\n").unwrap();
        assert!(validated_editor_path(&fixture.0, &file, identity).is_err());
    }

    fn identity_of(path: &Path) -> FileIdentity {
        FileIdentity::of(&fs::symlink_metadata(path).unwrap())
    }

    fn read_copy(fixture: &Fixture, name: &str) -> Result<String, String> {
        let path = fixture.0.join(name);
        read_text(&fixture.0, &path, identity_of(&path))
    }

    #[test]
    fn copy_returns_the_exact_text_the_preview_would_alter() {
        let fixture = Fixture::new();
        // CRLF, a lone CR, tabs, non-ASCII, and no trailing newline.
        let crlf = "first\r\n\tsecond é ✓\r\nlone\rcr\r\nlast";
        fs::write(fixture.0.join("crlf.txt"), crlf).unwrap();
        assert_eq!(read_copy(&fixture, "crlf.txt").unwrap(), crlf);
        fs::write(fixture.0.join("newline.txt"), "one\ntwo\n\n").unwrap();
        assert_eq!(read_copy(&fixture, "newline.txt").unwrap(), "one\ntwo\n\n");
        fs::write(fixture.0.join("empty.txt"), "").unwrap();
        assert_eq!(read_copy(&fixture, "empty.txt").unwrap(), "");

        // The preview clips this line at 4,000 characters; the copy must not.
        let long = format!("{}\nshort", "é".repeat(5_000));
        fs::write(fixture.0.join("long.txt"), &long).unwrap();
        let copied = read_copy(&fixture, "long.txt").unwrap();
        assert_eq!(copied, long);
        assert!(!copied.contains("[line clipped]"));

        // The preview stops at 10,000 lines; the copy must keep all of them.
        let many = "line\n".repeat(12_000);
        fs::write(fixture.0.join("many.txt"), &many).unwrap();
        assert!((many.len() as u64) < TEXT_LIMIT);
        let copied = read_copy(&fixture, "many.txt").unwrap();
        assert_eq!(copied.lines().count(), 12_000);
        assert_eq!(copied, many);
    }

    #[test]
    fn copy_refuses_files_over_the_text_limit_without_a_partial_copy() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("exact.txt"), vec![b'a'; TEXT_LIMIT as usize]).unwrap();
        assert_eq!(
            read_copy(&fixture, "exact.txt").unwrap().len() as u64,
            TEXT_LIMIT
        );
        fs::write(
            fixture.0.join("big.txt"),
            vec![b'a'; TEXT_LIMIT as usize + 1],
        )
        .unwrap();
        let error = read_copy(&fixture, "big.txt").unwrap_err();
        assert!(error.contains("Too large to copy") && error.contains("1 MiB"));
    }

    #[test]
    fn copy_refuses_binary_and_non_utf8_content() {
        let fixture = Fixture::new();
        for (name, bytes) in [
            ("nul.bin", b"a\0b".to_vec()),
            ("latin1.txt", b"caf\xe9\n".to_vec()),
            ("escape.txt", b"\x1b[31mred\x1b[0m\n".to_vec()),
            ("opaque.bin", vec![0, 1, 2, 255]),
        ] {
            fs::write(fixture.0.join(name), bytes).unwrap();
            let error = read_copy(&fixture, name).unwrap_err();
            assert!(error.contains("Binary"), "{name}: {error}");
        }
    }

    #[test]
    fn copy_refuses_symlinks_and_paths_outside_the_worktree() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let outside = Fixture::new();
        fs::write(outside.0.join("secret.txt"), "secret").unwrap();
        fs::write(fixture.0.join("real.txt"), "inside").unwrap();
        // The link's target carries a matching identity, so only the no-follow
        // open stands between the link and the copy.
        symlink(outside.0.join("secret.txt"), fixture.0.join("link.txt")).unwrap();
        let secret = identity_of(&outside.0.join("secret.txt"));
        assert!(read_text(&fixture.0, &fixture.0.join("link.txt"), secret).is_err());
        symlink(fixture.0.join("real.txt"), fixture.0.join("inner.txt")).unwrap();
        let real = identity_of(&fixture.0.join("real.txt"));
        assert!(read_text(&fixture.0, &fixture.0.join("inner.txt"), real).is_err());
        symlink(&outside.0, fixture.0.join("dir-link")).unwrap();
        assert!(read_text(&fixture.0, &fixture.0.join("dir-link/secret.txt"), secret).is_err());
        assert!(
            read_text(&fixture.0, &outside.0.join("secret.txt"), secret)
                .unwrap_err()
                .contains("outside")
        );
        assert!(read_text(&fixture.0, &fixture.0.join("sub/../../secret.txt"), secret).is_err());
        assert_eq!(read_copy(&fixture, "real.txt").unwrap(), "inside");
    }

    #[test]
    fn copy_refuses_a_file_that_changed_since_it_was_selected() {
        let fixture = Fixture::new();
        let path = fixture.0.join("note.txt");
        fs::write(&path, "first").unwrap();
        let selected = identity_of(&path);
        // A replacement at the same path is a different file.
        fs::rename(&path, fixture.0.join("old.txt")).unwrap();
        fs::write(&path, "second").unwrap();
        assert!(
            read_text(&fixture.0, &path, selected)
                .unwrap_err()
                .contains("replaced")
        );
        // So is an in-place edit that changes its size.
        let selected = identity_of(&path);
        fs::write(&path, "second, extended").unwrap();
        assert!(read_text(&fixture.0, &path, selected).is_err());
        fs::remove_file(&path).unwrap();
        assert!(read_text(&fixture.0, &path, selected).is_err());
    }
}
