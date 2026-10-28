//! No-follow file access and bounded, transport-neutral previews.

use std::{
    ffi::{CString, OsStr},
    fs::{self, File, Metadata},
    io::{Cursor, Read},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path},
};

const TEXT_LIMIT: u64 = 1024 * 1024;
const MEDIA_LIMIT: u64 = 24 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 6000;
const MAX_IMAGE_PIXELS: u64 = 24_000_000;
const PREVIEW_EDGE: u32 = 1800;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

/// A preview contains only bounded data. Media bytes are never file URLs: images
/// are decoded under limits and transported as PNG, and PDFs are read through the
/// same descriptor walk before an explicit page-render request.
#[derive(Clone, Debug, PartialEq)]
pub enum PreviewContent {
    Text {
        text: String,
        lines: Vec<String>,
        truncated: bool,
        markdown: bool,
    },
    Image {
        bytes: Vec<u8>,
        mime: String,
        width: u32,
        height: u32,
    },
    Pdf {
        bytes: Vec<u8>,
    },
    Message(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PdfPage {
    /// A bounded PNG bitmap of this page, rather than a URL or original PDF.
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub page: usize,
    pub pages: usize,
}

/// Load a regular file without following any descendant link. `expected` is the
/// opaque identity from the listing; callers handling a listing must pass it.
pub fn load(
    root: &Path,
    path: &Path,
    expected: Option<FileIdentity>,
) -> Result<PreviewContent, String> {
    let (mut file, _) = open_regular_in(root, path)?;
    let before = file
        .metadata()
        .map_err(|error| format!("Cannot inspect this file: {error}"))?;
    if expected.is_some_and(|identity| identity != FileIdentity::of(&before)) {
        return Err(
            "This file changed or was replaced since the folder was loaded. Refresh to inspect it."
                .into(),
        );
    }
    let extension = path
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let media = is_image(&extension) || extension == "pdf";
    let limit = if media { MEDIA_LIMIT } else { TEXT_LIMIT };
    let too_large = || {
        PreviewContent::Message(format!(
            "This file exceeds the {} MiB preview limit. Use Open to view it externally.",
            MEDIA_LIMIT / 1024 / 1024,
        ))
    };
    if media && before.len() > limit {
        return Ok(too_large());
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Cannot read this file: {error}"))?;
    let after = file
        .metadata()
        .map_err(|error| format!("Cannot inspect this file: {error}"))?;
    if FileIdentity::of(&before) != FileIdentity::of(&after) {
        return Err("This file changed while it was being read. Refresh and try again.".into());
    }
    if media && bytes.len() as u64 > limit {
        return Ok(too_large());
    }
    if extension == "pdf" {
        return Ok(if bytes.starts_with(b"%PDF-") {
            PreviewContent::Pdf { bytes }
        } else {
            PreviewContent::Message("This file is not a valid PDF.".into())
        });
    }
    if is_image(&extension) {
        return decode_image(&bytes);
    }
    let byte_truncated = bytes.len() as u64 > TEXT_LIMIT;
    bytes.truncate(TEXT_LIMIT as usize);
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text,
        Err(error) if byte_truncated && error.error_len().is_none() => {
            std::str::from_utf8(&bytes[..error.valid_up_to()]).unwrap()
        }
        Err(_) => return Ok(binary_message()),
    };
    if has_binary_control(text) {
        return Ok(binary_message());
    }
    let mut lines = Vec::new();
    let mut clipped_line = false;
    for line in text.lines().take(10_000) {
        let mut chars = line.chars();
        let mut shortened: String = chars.by_ref().take(4_000).collect();
        if chars.next().is_some() {
            shortened.push_str(" … [line clipped]");
            clipped_line = true;
        }
        lines.push(shortened);
    }
    let truncated = byte_truncated || clipped_line || text.lines().count() > lines.len();
    if lines.is_empty() {
        lines.push(String::new());
    }
    Ok(PreviewContent::Text {
        text: lines.join("\n"),
        lines,
        truncated,
        markdown: matches!(extension.as_str(), "md" | "markdown" | "mdown"),
    })
}

fn binary_message() -> PreviewContent {
    PreviewContent::Message(
        "Binary or non-UTF-8 content cannot be previewed safely. Use Open to view it externally."
            .into(),
    )
}

fn png_bytes(rgba: image::RgbaImage) -> Result<Vec<u8>, String> {
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(rgba)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .map_err(|error| format!("Cannot prepare this preview: {error}"))?;
    Ok(bytes.into_inner())
}

fn decode_image(bytes: &[u8]) -> Result<PreviewContent, String> {
    use image::{DynamicImage, ImageDecoder, metadata::Orientation};
    let format = match image::guess_format(bytes) {
        Ok(format)
            if matches!(
                format,
                image::ImageFormat::Png
                    | image::ImageFormat::Jpeg
                    | image::ImageFormat::Gif
                    | image::ImageFormat::WebP
                    | image::ImageFormat::Bmp
                    | image::ImageFormat::Tiff
                    | image::ImageFormat::Ico
            ) =>
        {
            format
        }
        _ => {
            return Ok(PreviewContent::Message(
                "This image format is not supported.".into(),
            ));
        }
    };
    let (width, height) = image::ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|error| format!("Cannot inspect this image: {error}"))?;
    if width == 0
        || height == 0
        || width > MAX_IMAGE_DIMENSION
        || height > MAX_IMAGE_DIMENSION
        || u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS
    {
        return Ok(PreviewContent::Message(
            "This image is too large to preview safely. Use Open to view it externally.".into(),
        ));
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(160 * 1024 * 1024);
    reader.limits(limits.clone());
    let mut decoder = reader
        .into_decoder()
        .map_err(|error| format!("Cannot decode this image: {error}"))?;
    limits
        .reserve(decoder.total_bytes())
        .and_then(|()| decoder.set_limits(limits))
        .map_err(|error| format!("Cannot decode this image: {error}"))?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut decoded = DynamicImage::from_decoder(decoder)
        .map_err(|error| format!("Cannot decode this image: {error}"))?;
    if decoded.width() > PREVIEW_EDGE || decoded.height() > PREVIEW_EDGE {
        decoded = decoded.thumbnail(PREVIEW_EDGE, PREVIEW_EDGE);
    }
    decoded.apply_orientation(orientation);
    let (width, height) = (decoded.width(), decoded.height());
    Ok(PreviewContent::Image {
        bytes: png_bytes(decoded.into_rgba8())?,
        mime: "image/png".into(),
        width,
        height,
    })
}

/// Open a directory under the selected root by descriptor. Callers must list
/// through this descriptor (for example `fdopendir`/`fstatat`), not reopen the
/// returned display path, which could have been replaced after validation.
pub fn open_directory_in(root: &Path, path: &Path) -> Result<(File, std::path::PathBuf), String> {
    if !root.is_absolute() {
        return Err("The selected worktree path is not absolute.".into());
    }
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "This file is outside the selected worktree.".to_owned())?;
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("This file is outside the selected worktree.".into());
    }
    let root_metadata = fs::symlink_metadata(root)
        .map_err(|error| format!("Cannot inspect the worktree: {error}"))?;
    if root_metadata.file_type().is_symlink() {
        return Err("Symbolic-link worktree roots cannot be previewed.".into());
    }
    if !root_metadata.is_dir() {
        return Err("The selected worktree is not a folder.".into());
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
    for component in resolved_root.components() {
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
    // Canonicalizing permits trusted ancestor aliases such as macOS /var, but
    // a root/ancestor swapped during that operation must never select a new
    // tree. Compare stable directory identity before walking descendants.
    let opened_root = directory
        .metadata()
        .map_err(|error| format!("Cannot inspect the worktree: {error}"))?;
    if opened_root.dev() != root_metadata.dev() || opened_root.ino() != root_metadata.ino() {
        return Err("The selected worktree changed. Select the file again.".into());
    }
    for component in relative.components() {
        let Component::Normal(name) = component else {
            unreachable!()
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
    Ok((directory, resolved_root.join(relative)))
}

pub fn open_regular_in(root: &Path, path: &Path) -> Result<(File, std::path::PathBuf), String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "This file is outside the selected worktree.".to_owned())?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("Select a regular file inside the selected worktree.".into());
    }
    let parent = path.parent().ok_or("Select a file to preview.")?;
    let (directory, resolved_parent) = open_directory_in(root, parent)?;
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
    Ok((file, resolved_parent.join(name)))
}

/// Render only on an explicit user request; no filesystem access occurs here.
#[cfg(target_os = "macos")]
pub fn render_pdf(bytes: &[u8], page: usize) -> Result<PdfPage, String> {
    pdf::render(bytes, page)
}

#[cfg(not(target_os = "macos"))]
pub fn render_pdf(_: &[u8], _: usize) -> Result<PdfPage, String> {
    Err("PDF preview is available on macOS.".into())
}

#[cfg(target_os = "macos")]
mod pdf {
    use super::*;
    use std::{ffi::c_void, ptr};

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Point {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Size {
        width: f64,
        height: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        origin: Point,
        size: Size,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Transform {
        a: f64,
        b: f64,
        c: f64,
        d: f64,
        tx: f64,
        ty: f64,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGDataProviderCreateWithData(
            info: *const c_void,
            data: *const c_void,
            size: usize,
            release: *const c_void,
        ) -> *mut c_void;
        fn CGDataProviderRelease(provider: *mut c_void);
        fn CGPDFDocumentCreateWithProvider(provider: *mut c_void) -> *mut c_void;
        fn CGPDFDocumentRelease(document: *mut c_void);
        fn CGPDFDocumentGetNumberOfPages(document: *mut c_void) -> usize;
        fn CGPDFDocumentGetPage(document: *mut c_void, page: usize) -> *mut c_void;
        fn CGPDFPageGetBoxRect(page: *mut c_void, box_type: i32) -> Rect;
        fn CGPDFPageGetDrawingTransform(
            page: *mut c_void,
            box_type: i32,
            rect: Rect,
            rotation: i32,
            preserve_aspect: bool,
        ) -> Transform;
        fn CGColorSpaceCreateDeviceRGB() -> *mut c_void;
        fn CGColorSpaceRelease(space: *mut c_void);
        fn CGBitmapContextCreate(
            data: *mut c_void,
            width: usize,
            height: usize,
            bits_per_component: usize,
            bytes_per_row: usize,
            space: *mut c_void,
            bitmap_info: u32,
        ) -> *mut c_void;
        fn CGContextRelease(context: *mut c_void);
        fn CGContextSetRGBFillColor(context: *mut c_void, r: f64, g: f64, b: f64, a: f64);
        fn CGContextFillRect(context: *mut c_void, rect: Rect);
        fn CGContextConcatCTM(context: *mut c_void, transform: Transform);
        fn CGContextDrawPDFPage(context: *mut c_void, page: *mut c_void);
    }

    pub fn render(bytes: &[u8], number: usize) -> Result<PdfPage, String> {
        if bytes.len() as u64 > MEDIA_LIMIT {
            return Err("PDF exceeds the preview limit.".into());
        }
        if !bytes.starts_with(b"%PDF-") {
            return Err("This file is not a valid PDF.".into());
        }
        unsafe {
            let provider = CGDataProviderCreateWithData(
                ptr::null(),
                bytes.as_ptr().cast(),
                bytes.len(),
                ptr::null(),
            );
            if provider.is_null() {
                return Err("Cannot read this PDF.".into());
            }
            let document = CGPDFDocumentCreateWithProvider(provider);
            CGDataProviderRelease(provider);
            if document.is_null() {
                return Err("Cannot read this PDF.".into());
            }
            let result = render_document(document, number);
            CGPDFDocumentRelease(document);
            result
        }
    }

    unsafe fn render_document(document: *mut c_void, number: usize) -> Result<PdfPage, String> {
        let pages = unsafe { CGPDFDocumentGetNumberOfPages(document) };
        if number == 0 || number > pages {
            return Err("This PDF page is unavailable.".into());
        }
        let page = unsafe { CGPDFDocumentGetPage(document, number) };
        if page.is_null() {
            return Err("Cannot load this PDF page.".into());
        }
        let box_rect = unsafe { CGPDFPageGetBoxRect(page, 1) };
        let (w, h) = (box_rect.size.width, box_rect.size.height);
        if !w.is_finite() || !h.is_finite() || w <= 0.0 || h <= 0.0 {
            return Err("This PDF page has invalid dimensions.".into());
        }
        let scale = (1600.0 / w.max(h)).min(2.0);
        let width = (w * scale).ceil().clamp(1.0, 1600.0) as usize;
        let height = (h * scale).ceil().clamp(1.0, 1600.0) as usize;
        let stride = width * 4;
        let mut pixels = vec![255u8; stride * height];
        let color_space = unsafe { CGColorSpaceCreateDeviceRGB() };
        if color_space.is_null() {
            return Err("Cannot render this PDF.".into());
        }
        let context = unsafe {
            CGBitmapContextCreate(
                pixels.as_mut_ptr().cast(),
                width,
                height,
                8,
                stride,
                color_space,
                1 | (4 << 12),
            )
        };
        unsafe {
            CGColorSpaceRelease(color_space);
        }
        if context.is_null() {
            return Err("Cannot render this PDF.".into());
        }
        let target = Rect {
            origin: Point { x: 0.0, y: 0.0 },
            size: Size {
                width: width as f64,
                height: height as f64,
            },
        };
        unsafe {
            CGContextSetRGBFillColor(context, 1.0, 1.0, 1.0, 1.0);
            CGContextFillRect(context, target);
            let transform = CGPDFPageGetDrawingTransform(page, 1, target, 0, true);
            CGContextConcatCTM(context, transform);
            CGContextDrawPDFPage(context, page);
            CGContextRelease(context);
        }
        let rgba = image::RgbaImage::from_raw(width as u32, height as u32, pixels)
            .ok_or("Cannot prepare this PDF page.")?;
        Ok(PdfPage {
            bytes: png_bytes(rgba)?,
            width: width as u32,
            height: height as u32,
            page: number,
            pages,
        })
    }
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

    fn preview(fixture: &Fixture, name: &str) -> PreviewContent {
        let path = fixture.0.join(name);
        load(&fixture.0, &path, Some(identity_of(&path))).unwrap()
    }

    #[test]
    fn preview_nested_spaced_unicode_text_and_binary_types() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.0.join("nested folder/日本語")).unwrap();
        let name = "nested folder/日本語/café.MD";
        fs::write(fixture.0.join(name), "# Hello\r\n\t世界 ✓\n").unwrap();
        let PreviewContent::Text {
            text,
            lines,
            markdown,
            truncated,
        } = preview(&fixture, name)
        else {
            panic!("expected Markdown text")
        };
        assert_eq!(text, "# Hello\n\t世界 ✓");
        assert_eq!(lines, ["# Hello", "\t世界 ✓"]);
        assert!(markdown);
        assert!(!truncated);
        for (name, bytes) in [
            ("nul.bin", b"text\0secret".to_vec()),
            ("latin1.rs", b"caf\xe9".to_vec()),
            ("terminal.txt", b"\x1b[31mred".to_vec()),
            ("unicode-control.txt", "a\u{0085}b".as_bytes().to_vec()),
        ] {
            fs::write(fixture.0.join(name), bytes).unwrap();
            assert!(
                matches!(preview(&fixture, name), PreviewContent::Message(message) if message.contains("Binary")),
                "{name}"
            );
        }
        fs::write(fixture.0.join("empty"), []).unwrap();
        assert!(
            matches!(preview(&fixture, "empty"), PreviewContent::Text { text, lines, .. } if text.is_empty() && lines == [""])
        );
    }

    #[test]
    fn preview_limits_bytes_lines_and_characters_without_replacement_characters() {
        let fixture = Fixture::new();
        let mut bytes = vec![b'a'; TEXT_LIMIT as usize - 1];
        bytes.extend_from_slice("é!".as_bytes());
        fs::write(fixture.0.join("large.txt"), bytes).unwrap();
        let PreviewContent::Text {
            text,
            lines,
            truncated,
            ..
        } = preview(&fixture, "large.txt")
        else {
            panic!("expected bounded text")
        };
        assert!(truncated);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].ends_with(" … [line clipped]"));
        assert_eq!(lines[0].chars().count(), 4_017);
        assert!(!text.contains('�'));
        fs::write(fixture.0.join("many.txt"), "line\n".repeat(10_001)).unwrap();
        assert!(
            matches!(preview(&fixture, "many.txt"), PreviewContent::Text { lines, truncated:true, .. } if lines.len() == 10_000)
        );
        fs::write(fixture.0.join("only-long-line.txt"), "é".repeat(4_001)).unwrap();
        assert!(matches!(
            preview(&fixture, "only-long-line.txt"),
            PreviewContent::Text {
                truncated: true,
                ..
            }
        ));
        fs::write(fixture.0.join("exact.txt"), "\n".repeat(10_000)).unwrap();
        assert!(
            matches!(preview(&fixture, "exact.txt"), PreviewContent::Text { lines, truncated:false, .. } if lines.len() == 10_000)
        );
    }

    #[test]
    fn preview_media_uses_validated_bytes_and_bounded_png_transport() {
        let fixture = Fixture::new();
        // The extension selects the media policy; its actual type comes from
        // bytes. A PNG under a JPEG name must still be transported as PNG.
        let rgba = image::RgbaImage::from_pixel(3, 2, image::Rgba([9, 21, 33, 255]));
        let bytes = png_bytes(rgba.clone()).unwrap();
        fs::write(fixture.0.join("misnamed.JPEG"), &bytes).unwrap();
        let PreviewContent::Image {
            bytes: output,
            mime,
            width,
            height,
        } = preview(&fixture, "misnamed.JPEG")
        else {
            panic!("expected verified image")
        };
        assert_eq!((width, height), (3, 2));
        assert_eq!(mime, "image/png");
        assert_eq!(
            image::guess_format(&output).unwrap(),
            image::ImageFormat::Png
        );
        assert_eq!(image::load_from_memory(&output).unwrap().into_rgba8(), rgba);
        fs::write(fixture.0.join("fake.png"), "plain text").unwrap();
        assert!(
            matches!(preview(&fixture, "fake.png"), PreviewContent::Message(message) if message.contains("not supported"))
        );
        let rgba = image::RgbaImage::new(2_000, 100);
        fs::write(fixture.0.join("wide.png"), png_bytes(rgba).unwrap()).unwrap();
        assert!(matches!(
            preview(&fixture, "wide.png"),
            PreviewContent::Image {
                width: 1800,
                height: 90,
                ..
            }
        ));
        let rgba = image::RgbaImage::new(6_001, 1);
        fs::write(fixture.0.join("huge.png"), png_bytes(rgba).unwrap()).unwrap();
        assert!(
            matches!(preview(&fixture, "huge.png"), PreviewContent::Message(message) if message.contains("too large"))
        );
        let large_path = fixture.0.join("oversized.pdf");
        File::create(&large_path)
            .unwrap()
            .set_len(MEDIA_LIMIT + 1)
            .unwrap();
        assert!(
            matches!(preview(&fixture, "oversized.pdf"), PreviewContent::Message(message) if message.contains("24 MiB"))
        );
        fs::write(fixture.0.join("fake.pdf"), &bytes).unwrap();
        assert!(
            matches!(preview(&fixture, "fake.pdf"), PreviewContent::Message(message) if message.contains("not a valid PDF"))
        );
        // Loading verifies the header and preserves exact bytes. Parsing only
        // happens on render_pdf, after an explicit request from the UI.
        let pdf = b"%PDF-1.4\nnot yet parsed\n";
        fs::write(fixture.0.join("raw.pdf"), pdf).unwrap();
        assert_eq!(
            preview(&fixture, "raw.pdf"),
            PreviewContent::Pdf {
                bytes: pdf.to_vec()
            }
        );
    }

    #[test]
    fn image_transport_applies_exif_orientation_before_encoding() {
        use image::ImageEncoder;
        let exif = vec![
            0x49, 0x49, 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00, 0x01, 0x00, 0x12, 0x01, 0x03, 0x00,
            0x01, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let mut jpeg = Vec::new();
        let mut encoder = image::codecs::jpeg::JpegEncoder::new(&mut jpeg);
        encoder.set_exif_metadata(exif).unwrap();
        encoder
            .write_image(&[128; 18], 3, 2, image::ExtendedColorType::Rgb8)
            .unwrap();
        let fixture = Fixture::new();
        fs::write(fixture.0.join("phone.jpg"), jpeg).unwrap();
        let PreviewContent::Image {
            bytes,
            width,
            height,
            ..
        } = preview(&fixture, "phone.jpg")
        else {
            panic!("expected image")
        };
        assert_eq!((width, height), (2, 3));
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (2, 3));
    }

    #[test]
    fn all_preview_containment_attacks_fail_closed() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let outside = Fixture::new();
        fs::create_dir_all(fixture.0.join("safe/sub")).unwrap();
        fs::write(fixture.0.join("safe/sub/inside.txt"), "inside").unwrap();
        let secret = outside.0.join("secret.txt");
        fs::write(&secret, "outside-secret").unwrap();
        let expected = identity_of(&secret);
        symlink(&secret, fixture.0.join("file-link")).unwrap();
        symlink(&outside.0, fixture.0.join("dir-link")).unwrap();
        symlink(fixture.0.join("safe"), fixture.0.join("inside-link")).unwrap();
        symlink("cycle", fixture.0.join("cycle")).unwrap();
        for path in [
            secret,
            fixture.0.join("../secret.txt"),
            fixture.0.join("safe/../../secret.txt"),
            fixture.0.join("file-link"),
            fixture.0.join("dir-link/secret.txt"),
            fixture.0.join("inside-link/sub/inside.txt"),
            fixture.0.join("cycle"),
            fixture.0.join("safe"),
            fixture.0.join("nul\0.txt"),
        ] {
            assert!(
                load(&fixture.0, &path, Some(expected)).is_err(),
                "opened {}",
                path.display()
            );
        }
        let root_link = outside.0.join("linked-root");
        symlink(&fixture.0, &root_link).unwrap();
        assert!(load(&root_link, &root_link.join("safe/sub/inside.txt"), None).is_err());
        assert!(load(Path::new("relative"), Path::new("relative/file"), None).is_err());
        assert!(open_directory_in(&fixture.0, &fixture.0.join("dir-link")).is_err());
        assert!(open_directory_in(&fixture.0, &fixture.0.join("safe/../safe")).is_err());
        // A formerly cached descendant cannot escape after an ancestor turns
        // into an outside link, even when the target identity is supplied.
        fs::rename(fixture.0.join("safe"), fixture.0.join("old-safe")).unwrap();
        symlink(&outside.0, fixture.0.join("safe")).unwrap();
        assert!(
            load(
                &fixture.0,
                &fixture.0.join("safe/secret.txt"),
                Some(expected)
            )
            .is_err()
        );
    }

    #[test]
    fn preview_replacement_identity_and_special_files_fail_closed() {
        let fixture = Fixture::new();
        let path = fixture.0.join("selected.txt");
        fs::write(&path, "original").unwrap();
        let expected = identity_of(&path);
        fs::rename(&path, fixture.0.join("old.txt")).unwrap();
        fs::write(&path, "replacement").unwrap();
        assert!(
            load(&fixture.0, &path, Some(expected))
                .unwrap_err()
                .contains("replaced")
        );
        let fifo = fixture.0.join("pipe");
        let c_path = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        assert!(
            load(&fixture.0, &fifo, None)
                .unwrap_err()
                .contains("regular")
        );
        let socket_path = fixture.0.join("socket");
        let _socket = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
        assert!(load(&fixture.0, &socket_path, None).is_err());
        // A prefix-sharing sibling is not contained by string-prefix rules.
        let sibling = PathBuf::from(format!("{}-sibling", fixture.0.display()));
        assert!(open_directory_in(&fixture.0, &sibling).is_err());
    }

    #[test]
    fn root_symlink_replacement_race_never_reads_an_outside_file() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let outside = Fixture::new();
        let root = fixture.0.join("root");
        let held_root = fixture.0.join("held-root");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("same.txt"), "inside").unwrap();
        fs::write(outside.0.join("same.txt"), "outside-secret").unwrap();
        std::thread::scope(|scope| {
            let root = &root;
            let held_root = &held_root;
            let outside = &outside.0;
            scope.spawn(move || {
                for _ in 0..300 {
                    fs::rename(root, held_root).unwrap();
                    symlink(outside, root).unwrap();
                    std::thread::yield_now();
                    fs::remove_file(root).unwrap();
                    fs::rename(held_root, root).unwrap();
                }
            });
            for _ in 0..1_000 {
                if let Ok(content) = load(root, &root.join("same.txt"), None) {
                    assert!(
                        matches!(content, PreviewContent::Text { text, .. } if text == "inside")
                    );
                }
                std::thread::yield_now();
            }
        });
        assert!(
            matches!(load(&root, &root.join("same.txt"), None).unwrap(), PreviewContent::Text { text, .. } if text == "inside")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn pdf_pages_render_to_verified_png_with_page_bounds() {
        fn pdf_bytes() -> Vec<u8> {
            let red = "1 0 0 rg 10 10 90 90 re f\n";
            let blue = "0 0 1 rg 10 10 90 90 re f\n";
            let objects = [
                "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
                "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_owned(),
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 5 0 R >>"
                    .to_owned(),
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] /Contents 6 0 R >>"
                    .to_owned(),
                format!("<< /Length {} >>\nstream\n{}endstream", red.len(), red),
                format!("<< /Length {} >>\nstream\n{}endstream", blue.len(), blue),
            ];
            let mut bytes = b"%PDF-1.4\n".to_vec();
            let mut offsets = Vec::new();
            for (index, object) in objects.iter().enumerate() {
                offsets.push(bytes.len());
                bytes.extend_from_slice(
                    format!("{} 0 obj\n{}\nendobj\n", index + 1, object).as_bytes(),
                );
            }
            let xref = bytes.len();
            bytes.extend_from_slice(
                format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
            );
            for offset in offsets {
                bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
            }
            bytes.extend_from_slice(
                format!(
                    "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                    objects.len() + 1
                )
                .as_bytes(),
            );
            bytes
        }
        let fixture = Fixture::new();
        let bytes = pdf_bytes();
        fs::write(fixture.0.join("two pages.pdf"), &bytes).unwrap();
        assert_eq!(
            preview(&fixture, "two pages.pdf"),
            PreviewContent::Pdf {
                bytes: bytes.clone()
            }
        );
        let first = render_pdf(&bytes, 1).unwrap();
        assert_eq!(
            (first.width, first.height, first.page, first.pages),
            (600, 400, 1, 2)
        );
        assert_eq!(
            image::guess_format(&first.bytes).unwrap(),
            image::ImageFormat::Png
        );
        let pixels = image::load_from_memory(&first.bytes).unwrap().into_rgba8();
        assert!(
            pixels
                .pixels()
                .any(|pixel| pixel[0] > 200 && pixel[1] < 10 && pixel[2] < 10)
        );
        let second = render_pdf(&bytes, 2).unwrap();
        assert_eq!(
            (second.width, second.height, second.page, second.pages),
            (400, 600, 2, 2)
        );
        let pixels = image::load_from_memory(&second.bytes).unwrap().into_rgba8();
        assert!(
            pixels
                .pixels()
                .any(|pixel| pixel[2] > 200 && pixel[0] < 10 && pixel[1] < 10)
        );
        assert_ne!(first.bytes, second.bytes);
        assert!(render_pdf(&bytes, 0).is_err());
        assert!(render_pdf(&bytes, 3).is_err());
        assert!(render_pdf(b"not a PDF", 1).is_err());
        assert!(render_pdf(&vec![0; MEDIA_LIMIT as usize + 1], 1).is_err());
    }
}
