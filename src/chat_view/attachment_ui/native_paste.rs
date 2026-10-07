//! App-owned macOS composer reader. Never changes GPUI's paste semantics for forms.
//! Only typed representations are copied here; decoding runs on background workers.
use crate::chat::attachments::{FILE_BYTES, RAW_TIFF_BYTES};
use gpui::{ClipboardEntry, ClipboardItem, ExternalPaths, Image, ImageFormat};
use objc2::{class, msg_send, rc::autoreleasepool, runtime::AnyObject};
use std::{
    ffi::{CStr, CString},
    path::PathBuf,
};

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}
#[link(name = "UniformTypeIdentifiers", kind = "framework")]
unsafe extern "C" {}

// No production pasteboard accessor exists in test builds. Native regression
// fixtures pass a uniquely named private board directly to read_board.
#[cfg(not(test))]
pub(super) fn read_general_attachments() -> Result<Option<ClipboardItem>, String> {
    autoreleasepool(|_| unsafe {
        let board: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
        read_board(board)
    })
}

unsafe fn string(value: &str) -> *mut AnyObject {
    let value = CString::new(value).expect("pasteboard type/name has no NUL");
    unsafe { msg_send![class!(NSString), stringWithUTF8String: value.as_ptr()] }
}

unsafe fn rust_string(value: *mut AnyObject) -> Option<String> {
    if value.is_null() {
        return None;
    }
    let ptr: *const std::ffi::c_char = unsafe { msg_send![value, UTF8String] };
    if ptr.is_null() {
        return None;
    }
    Some(
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned(),
    )
}

/// SAFETY: board is a live NSPasteboard in the caller's autorelease pool. All
/// Objective-C results are consumed before that pool ends; only owned Rust data escapes.
unsafe fn read_board(board: *mut AnyObject) -> Result<Option<ClipboardItem>, String> {
    if board.is_null() {
        return Err("native pasteboard is unavailable".into());
    }
    unsafe {
        let before: isize = msg_send![board, changeCount];
        let item = read_representations(board)?;
        let after: isize = msg_send![board, changeCount];
        if before != after {
            return Err("clipboard changed while reading; paste again".into());
        }
        Ok(item)
    }
}

unsafe fn read_representations(board: *mut AnyObject) -> Result<Option<ClipboardItem>, String> {
    unsafe {
        // Modern Finder file URLs, then the legacy file list used by pinned GPUI.
        // Only file URLs qualify; a copied web URL must not outrank an image.
        // NSArray accepts id, while NSURL's class pointer is encoded as Class.
        let url_class = (class!(NSURL) as *const objc2::runtime::AnyClass).cast::<AnyObject>();
        let classes: *mut AnyObject = msg_send![class!(NSArray), arrayWithObject: url_class];
        let nil: *mut AnyObject = std::ptr::null_mut();
        let urls: *mut AnyObject = msg_send![board, readObjectsForClasses: classes, options: nil];
        let mut paths = Vec::new();
        if !urls.is_null() {
            let count: usize = msg_send![urls, count];
            for i in 0..count {
                let url: *mut AnyObject = msg_send![urls, objectAtIndex: i];
                let is_file: bool = msg_send![url, isFileURL];
                if is_file {
                    let path: *mut AnyObject = msg_send![url, path];
                    if let Some(path) = rust_string(path) {
                        paths.push(PathBuf::from(path));
                    }
                }
            }
        }
        if paths.is_empty() {
            let files: *mut AnyObject =
                msg_send![board, propertyListForType: string("NSFilenamesPboardType")];
            if !files.is_null() {
                let is_array: bool = msg_send![files, isKindOfClass: class!(NSArray)];
                if is_array {
                    let count: usize = msg_send![files, count];
                    for i in 0..count {
                        let file: *mut AnyObject = msg_send![files, objectAtIndex: i];
                        let is_string: bool = msg_send![file, isKindOfClass: class!(NSString)];
                        if is_string && let Some(path) = rust_string(file) {
                            paths.push(PathBuf::from(path));
                        }
                    }
                }
            }
        }
        if !paths.is_empty() {
            return Ok(Some(ClipboardItem {
                entries: vec![ClipboardEntry::ExternalPaths(ExternalPaths(paths.into()))],
            }));
        }
        let types: *mut AnyObject = msg_send![board, types];
        if types.is_null() {
            return Ok(None);
        }
        for (kind, format, cap) in [
            ("public.png", ImageFormat::Png, FILE_BYTES),
            ("public.jpeg", ImageFormat::Jpeg, FILE_BYTES),
            ("public.tiff", ImageFormat::Tiff, RAW_TIFF_BYTES),
        ] {
            let kind = string(kind);
            let present: bool = msg_send![types, containsObject: kind];
            if !present {
                continue;
            }
            let data: *mut AnyObject = msg_send![board, dataForType: kind];
            if data.is_null() {
                return Err("image representation could not be read; paste again".into());
            }
            let len: usize = msg_send![data, length];
            if len as u64 > cap {
                return Err(format!(
                    "clipboard image exceeds the {} MiB input limit",
                    cap >> 20
                ));
            }
            let ptr: *const std::ffi::c_void = msg_send![data, bytes];
            if ptr.is_null() || len == 0 {
                return Err("clipboard image is empty".into());
            }
            let bytes = std::slice::from_raw_parts(ptr.cast::<u8>(), len).to_vec();
            return Ok(Some(ClipboardItem {
                entries: vec![ClipboardEntry::Image(Image::from_bytes(format, bytes))],
            }));
        }
        // Refuse other image UTIs explicitly, even when accompanied by text.
        // Falling through here would reproduce the image+URL loss for GIF/HEIC/etc.
        let image_type: *mut AnyObject =
            msg_send![class!(UTType), typeWithIdentifier: string("public.image")];
        let count: usize = msg_send![types, count];
        for i in 0..count {
            let kind: *mut AnyObject = msg_send![types, objectAtIndex: i];
            let ty: *mut AnyObject = msg_send![class!(UTType), typeWithIdentifier: kind];
            if !ty.is_null() {
                let is_image: bool = msg_send![ty, conformsToType: image_type];
                if is_image {
                    return Err(
                        "unsupported clipboard image format; use static PNG, JPEG or TIFF".into(),
                    );
                }
            }
        }
        // Preserve GPUI's normal string+metadata path for ordinary text.
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    unsafe fn fixture() -> *mut AnyObject {
        unsafe {
            let board: *mut AnyObject = msg_send![class!(NSPasteboard), pasteboardWithName: string(&format!("riwork-paste-fixture-{}", Uuid::new_v4()))];
            let _: isize = msg_send![board, clearContents];
            board
        }
    }
    unsafe fn put(board: *mut AnyObject, kind: &str, bytes: &[u8]) {
        unsafe {
            let data: *mut AnyObject = msg_send![class!(NSData), dataWithBytes: bytes.as_ptr().cast::<std::ffi::c_void>(), length: bytes.len()];
            let ok: bool = msg_send![board, setData: data, forType: string(kind)];
            assert!(ok);
        }
    }
    #[test]
    fn private_native_mixed_image_url_prefers_png_then_jpeg_then_tiff() {
        autoreleasepool(|_| unsafe {
            let board = fixture();
            put(
                board,
                "public.utf8-plain-text",
                b"https://fixture.invalid/image",
            );
            put(board, "public.url", b"https://fixture.invalid/image");
            assert!(
                read_board(board).unwrap().is_none(),
                "plain text remains GPUI's responsibility"
            );
            for (kind, format) in [
                ("public.tiff", ImageFormat::Tiff),
                ("public.jpeg", ImageFormat::Jpeg),
                ("public.png", ImageFormat::Png),
            ] {
                put(board, kind, kind.as_bytes());
                let item = read_board(board).unwrap().unwrap();
                let [ClipboardEntry::Image(image)] = &item.entries[..] else {
                    panic!("image lost to URL text")
                };
                assert_eq!(image.format(), format);
                assert_eq!(image.bytes(), kind.as_bytes());
            }
            let _: () = msg_send![board, releaseGlobally];
        });
    }
    #[test]
    fn private_native_files_outrank_image_and_filename_text() {
        autoreleasepool(|_| unsafe {
            let board = fixture();
            put(board, "public.png", b"fixture image representation");
            put(board, "public.utf8-plain-text", b"fixture-file.png");
            let file_path = format!(
                "/tmp/riwork-nonexistent-paste-fixture-{}.png",
                Uuid::new_v4()
            );
            put(
                board,
                "public.file-url",
                format!("file://{file_path}").as_bytes(),
            );
            let item = read_board(board).unwrap().unwrap();
            let [ClipboardEntry::ExternalPaths(paths)] = &item.entries[..] else {
                panic!("file priority lost")
            };
            assert_eq!(paths.paths(), &[PathBuf::from(file_path)]);
            let _: isize = msg_send![board, clearContents];
            put(board, "public.png", b"fixture image representation");
            let legacy_path = format!("/tmp/riwork-legacy-paste-fixture-{}.png", Uuid::new_v4());
            let files: *mut AnyObject =
                msg_send![class!(NSArray), arrayWithObject: string(&legacy_path)];
            let ok: bool =
                msg_send![board, setPropertyList: files, forType: string("NSFilenamesPboardType")];
            assert!(ok);
            let item = read_board(board).unwrap().unwrap();
            assert!(matches!(
                &item.entries[..],
                [ClipboardEntry::ExternalPaths(_)]
            ));
            let _: () = msg_send![board, releaseGlobally];
        });
    }
    #[test]
    fn private_native_unsupported_image_refuses_instead_of_url_text() {
        autoreleasepool(|_| unsafe {
            let board = fixture();
            put(
                board,
                "public.utf8-plain-text",
                b"https://fixture.invalid/animated",
            );
            put(board, "com.compuserve.gif", b"GIF89a");
            let error = read_board(board).unwrap_err();
            let types: *mut AnyObject = msg_send![board, types];
            let description: *mut AnyObject = msg_send![types, description];
            let _: () = msg_send![board, releaseGlobally];
            assert!(
                error.contains("unsupported"),
                "actual refusal: {error}; advertised types: {:?}",
                rust_string(description)
            );
        });
    }
}
