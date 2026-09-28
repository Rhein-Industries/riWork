//! Bounded, read-only previews of regular files in a selected worktree.

use std::{
    ffi::{CString, OsStr},
    fs::{self, File, Metadata},
    io::{Cursor, Read},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path},
    sync::Arc,
};

use gpui::RenderImage;
use image::{DynamicImage, Frame, ImageDecoder, metadata::Orientation};

const TEXT_LIMIT: u64 = 1024 * 1024;
const MEDIA_LIMIT: u64 = 24 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 6000;
const MAX_IMAGE_PIXELS: u64 = 24_000_000;
/// Longest edge kept for a decoded preview; smaller images are never scaled up.
const PREVIEW_EDGE: u32 = 1800;

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

#[derive(Clone)]
pub enum PreviewContent {
    Text {
        lines: Arc<Vec<String>>,
        truncated: bool,
        markdown: bool,
    },
    Image {
        image: Arc<RenderImage>,
        description: String,
    },
    Pdf {
        bytes: Arc<Vec<u8>>,
        image: Arc<RenderImage>,
        page: usize,
        pages: usize,
    },
    Message(String),
}

impl PreviewContent {
    /// The decoded bitmap, which the owner must release with `drop_image` once
    /// the preview goes away: GPUI keeps its atlas tiles until told otherwise.
    pub fn render_image(&self) -> Option<&Arc<RenderImage>> {
        match self {
            Self::Image { image, .. } | Self::Pdf { image, .. } => Some(image),
            Self::Text { .. } | Self::Message(_) => None,
        }
    }
}

/// PDFs are parsed by CoreGraphics inside the app, so callers only load one
/// after the user explicitly asks for it.
pub fn is_pdf(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
}

/// Wrap RGBA pixels as a GPUI image, which expects BGRA.
fn to_render_image(rgba: image::RgbaImage) -> Arc<RenderImage> {
    let mut rgba = rgba;
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Arc::new(RenderImage::new([Frame::new(rgba)]))
}

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
    if media && before.len() > limit {
        return Ok(PreviewContent::Message(format!(
            "This file exceeds the {} MiB preview limit. Use Open to view it externally.",
            limit / 1024 / 1024
        )));
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
        return Ok(PreviewContent::Message(format!(
            "This file exceeds the {} MiB preview limit. Use Open to view it externally.",
            limit / 1024 / 1024
        )));
    }
    if extension == "pdf" {
        if !bytes.starts_with(b"%PDF-") {
            return Ok(PreviewContent::Message(
                "This file is not a valid PDF.".into(),
            ));
        }
        let bytes = Arc::new(bytes);
        let (image, pages) = render_pdf(&bytes, 1)?;
        return Ok(PreviewContent::Pdf {
            bytes,
            image,
            page: 1,
            pages,
        });
    }
    if is_image(&extension) {
        return decode_image(&bytes);
    }
    let truncated = bytes.len() as u64 > TEXT_LIMIT;
    if truncated {
        bytes.truncate(TEXT_LIMIT as usize);
    }
    // Strict UTF-8 and control checks keep binary data out of the text view.
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text,
        Err(error) if truncated && error.error_len().is_none() => {
            std::str::from_utf8(&bytes[..error.valid_up_to()]).unwrap()
        }
        Err(_) => return Ok(binary_message()),
    };
    if text
        .chars()
        .any(|ch| ch == '\0' || (ch.is_control() && !matches!(ch, '\n' | '\r' | '\t')))
    {
        return Ok(binary_message());
    }
    let mut lines = Vec::new();
    for line in text.lines() {
        if lines.len() >= 10_000 {
            break;
        }
        let mut shortened = line.chars().take(4_000).collect::<String>();
        if line.chars().count() > 4_000 {
            shortened.push_str(" … [line clipped]");
        }
        lines.push(shortened);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    let truncated = truncated || text.lines().count() > lines.len();
    Ok(PreviewContent::Text {
        lines: Arc::new(lines),
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

fn is_image(extension: &str) -> bool {
    matches!(
        extension,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "ico"
    )
}

fn decode_image(bytes: &[u8]) -> Result<PreviewContent, String> {
    let format =
        image::guess_format(bytes).map_err(|_| "This image format is not supported.".to_owned())?;
    if !matches!(
        format,
        image::ImageFormat::Png
            | image::ImageFormat::Jpeg
            | image::ImageFormat::Gif
            | image::ImageFormat::WebP
            | image::ImageFormat::Bmp
            | image::ImageFormat::Tiff
            | image::ImageFormat::Ico
    ) {
        return Ok(PreviewContent::Message(
            "This image format is not supported.".into(),
        ));
    }
    let reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let (width, height) = reader
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
    // `ImageReader::decode` would discard the EXIF orientation, so drive the
    // decoder directly and charge the output buffer to `max_alloc` as it does.
    let mut decoder = reader
        .into_decoder()
        .map_err(|error| format!("Cannot decode this image: {error}"))?;
    limits
        .reserve(decoder.total_bytes())
        .and_then(|()| decoder.set_limits(limits))
        .map_err(|error| format!("Cannot decode this image: {error}"))?;
    // A damaged EXIF block should not stop the pixels from showing.
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut decoded = DynamicImage::from_decoder(decoder)
        .map_err(|error| format!("Cannot decode this image: {error}"))?;
    // Shrinking first keeps the rotation copy small.
    if decoded.width() > PREVIEW_EDGE || decoded.height() > PREVIEW_EDGE {
        decoded = decoded.thumbnail(PREVIEW_EDGE, PREVIEW_EDGE);
    }
    decoded.apply_orientation(orientation);
    let (width, height) = if matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    ) {
        (height, width)
    } else {
        (width, height)
    };
    Ok(PreviewContent::Image {
        image: to_render_image(decoded.into_rgba8()),
        description: format!("{width} × {height} pixels"),
    })
}

fn open_regular_in(root: &Path, path: &Path) -> Result<(File, std::path::PathBuf), String> {
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

#[cfg(target_os = "macos")]
pub fn render_pdf(bytes: &[u8], page: usize) -> Result<(Arc<RenderImage>, usize), String> {
    pdf::render(bytes, page)
}

#[cfg(not(target_os = "macos"))]
pub fn render_pdf(_: &[u8], _: usize) -> Result<(Arc<RenderImage>, usize), String> {
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

    pub fn render(bytes: &[u8], number: usize) -> Result<(Arc<RenderImage>, usize), String> {
        if bytes.len() as u64 > MEDIA_LIMIT {
            return Err("PDF exceeds the preview limit.".into());
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

    unsafe fn render_document(
        document: *mut c_void,
        number: usize,
    ) -> Result<(Arc<RenderImage>, usize), String> {
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
        Ok((to_render_image(rgba), pages))
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
        fn load(&self, name: &str) -> Result<PreviewContent, String> {
            let path = self.0.join(name);
            load(
                &self.0,
                &path,
                Some(FileIdentity::of(&fs::symlink_metadata(&path).unwrap())),
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn text_markdown_and_binary_are_read_only_and_bounded() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("guide.md"), "# Hello\nUTF-8: café\n").unwrap();
        match fixture.load("guide.md").unwrap() {
            PreviewContent::Text {
                lines,
                truncated,
                markdown,
            } => {
                assert!(markdown);
                assert!(!truncated);
                assert_eq!(&**lines, &["# Hello", "UTF-8: café"]);
            }
            _ => panic!("expected Markdown source"),
        }
        fs::write(fixture.0.join("opaque.bin"), [0, 1, 2, 255]).unwrap();
        assert!(
            matches!(fixture.load("opaque.bin").unwrap(), PreviewContent::Message(message) if message.contains("Binary"))
        );
        let mut long = vec![b'a'; TEXT_LIMIT as usize - 1];
        long.extend_from_slice("é".as_bytes());
        long.push(b'!');
        fs::write(fixture.0.join("long.txt"), long).unwrap();
        match fixture.load("long.txt").unwrap() {
            PreviewContent::Text {
                lines, truncated, ..
            } => {
                assert!(truncated);
                assert_eq!(lines[0].chars().count(), 4_017); // clipped line plus marker
                assert!(!lines[0].contains('�'));
            }
            _ => panic!("expected truncated text"),
        }
    }

    #[test]
    fn path_escape_symlink_and_replacement_are_rejected() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let outside = Fixture::new();
        fs::write(outside.0.join("secret.txt"), "secret").unwrap();
        fs::create_dir(fixture.0.join("sub")).unwrap();
        symlink(&outside.0, fixture.0.join("sub/link")).unwrap();
        symlink(outside.0.join("secret.txt"), fixture.0.join("link.txt")).unwrap();
        assert!(
            load(&fixture.0, &outside.0.join("secret.txt"), None)
                .err()
                .unwrap()
                .contains("outside")
        );
        assert!(load(&fixture.0, &fixture.0.join("sub/../../secret.txt"), None).is_err());
        assert!(load(&fixture.0, &fixture.0.join("sub/link/secret.txt"), None).is_err());
        assert!(load(&fixture.0, &fixture.0.join("link.txt"), None).is_err());
        let linked_root = outside.0.join("linked-root");
        symlink(&fixture.0, &linked_root).unwrap();
        assert!(
            load(&linked_root, &linked_root.join("sub"), None)
                .err()
                .unwrap()
                .contains("roots")
        );
        fs::write(fixture.0.join("replace.txt"), "first").unwrap();
        let old = FileIdentity::of(&fs::metadata(fixture.0.join("replace.txt")).unwrap());
        fs::rename(fixture.0.join("replace.txt"), fixture.0.join("old.txt")).unwrap();
        fs::write(fixture.0.join("replace.txt"), "second").unwrap();
        assert!(
            load(&fixture.0, &fixture.0.join("replace.txt"), Some(old))
                .err()
                .unwrap()
                .contains("replaced")
        );
        fs::remove_file(fixture.0.join("replace.txt")).unwrap();
        assert!(load(&fixture.0, &fixture.0.join("replace.txt"), Some(old)).is_err());
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

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(width, height)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    fn pixel_size(image: &RenderImage) -> (i32, i32) {
        let size = image.size(0);
        (size.width.0, size.height.0)
    }

    #[test]
    fn image_preview_is_decoded_and_dimension_limited() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("small.png"), png(3, 2)).unwrap();
        assert!(
            matches!(fixture.load("small.png").unwrap(), PreviewContent::Image { description, .. } if description.contains("3 × 2"))
        );
        fs::write(fixture.0.join("wide.png"), png(MAX_IMAGE_DIMENSION + 1, 1)).unwrap();
        assert!(
            matches!(fixture.load("wide.png").unwrap(), PreviewContent::Message(message) if message.contains("too large"))
        );
    }

    #[test]
    fn small_images_are_not_upscaled_and_large_ones_are_capped() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("icon.png"), png(16, 16)).unwrap();
        let PreviewContent::Image { image, .. } = fixture.load("icon.png").unwrap() else {
            panic!("expected an image")
        };
        assert_eq!(pixel_size(&image), (16, 16));
        fs::write(fixture.0.join("banner.png"), png(2400, 60)).unwrap();
        let PreviewContent::Image { image, description } = fixture.load("banner.png").unwrap()
        else {
            panic!("expected an image")
        };
        assert_eq!(pixel_size(&image), (1800, 45));
        assert!(description.contains("2400 × 60"));
    }

    #[test]
    fn preview_images_are_bgra() {
        let fixture = Fixture::new();
        let mut red = image::RgbaImage::new(1, 1);
        red.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(red)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        fs::write(fixture.0.join("red.png"), bytes.into_inner()).unwrap();
        let PreviewContent::Image { image, .. } = fixture.load("red.png").unwrap() else {
            panic!("expected an image")
        };
        assert_eq!(image.as_bytes(0).unwrap(), [0, 0, 255, 255]);
        assert_eq!(image.frame_count(), 1);
    }

    #[test]
    fn exif_orientation_is_applied() {
        use image::ImageEncoder;
        // Little-endian TIFF header with one IFD entry: Orientation = 6
        // (rotate 90 degrees clockwise).
        let exif = vec![
            0x49, 0x49, 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00, 0x01, 0x00, 0x12, 0x01, 0x03, 0x00,
            0x01, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let mut jpeg = Vec::new();
        let mut encoder = image::codecs::jpeg::JpegEncoder::new(&mut jpeg);
        encoder.set_exif_metadata(exif).unwrap();
        encoder
            .write_image(&[128; 3 * 2 * 3], 3, 2, image::ExtendedColorType::Rgb8)
            .unwrap();
        let fixture = Fixture::new();
        fs::write(fixture.0.join("phone.jpg"), jpeg).unwrap();
        let PreviewContent::Image { image, description } = fixture.load("phone.jpg").unwrap()
        else {
            panic!("expected an image")
        };
        assert_eq!(pixel_size(&image), (2, 3));
        assert!(description.contains("2 × 3"));
    }

    #[test]
    fn pdf_extension_check_is_case_insensitive() {
        assert!(is_pdf(Path::new("a/Report.PDF")));
        assert!(is_pdf(Path::new("plain.pdf")));
        assert!(!is_pdf(Path::new("pdf")));
        assert!(!is_pdf(Path::new("notes.pdf.txt")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn pdf_renders_each_page_from_bounded_bytes() {
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
        fs::write(fixture.0.join("two.pdf"), pdf_bytes()).unwrap();
        let PreviewContent::Pdf {
            bytes,
            image,
            page,
            pages,
        } = fixture.load("two.pdf").unwrap()
        else {
            panic!("expected PDF")
        };
        assert_eq!((page, pages), (1, 2));
        assert_eq!(pixel_size(&image), (600, 400)); // 300 × 200 points at 2x
        let (second, count) = render_pdf(&bytes, 2).unwrap();
        assert_eq!(count, 2);
        assert_ne!(image.as_bytes(0), second.as_bytes(0));
        assert!(render_pdf(&bytes, 3).is_err());
    }
}
