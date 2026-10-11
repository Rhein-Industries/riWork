//! Static clipboard TIFF admission, before the image decoder can allocate metadata.
use super::{FILE_BYTES, RAW_TIFF_BYTES, decoded, static_image_mime};
use image::{ImageEncoder, ImageFormat};
use std::{
    borrow::Cow,
    io::{self, Write},
};

/// Call off the UI thread for both local previews and host staging. PNG/JPEG
/// retain their original bytes. TIFF is normalized once per independent task;
/// the retry source always retains the exact original clipboard representation.
pub fn normalize_clipboard_image(bytes: &[u8]) -> Result<Cow<'_, [u8]>, String> {
    if bytes.len() as u64 > RAW_TIFF_BYTES {
        return Err("raw clipboard image exceeds 64 MiB".into());
    }
    let format = image::guess_format(bytes).map_err(|e| format!("invalid image: {e}"))?;
    if format != ImageFormat::Tiff {
        static_image_mime(bytes)?;
        return Ok(Cow::Borrowed(bytes));
    }
    single_tiff_directory(bytes)?;
    // decoded shares the 8192-axis, 16-MP and 64-MiB allocation admission with
    // staged PNG/JPEG. ImageReader reserves the output buffer before giving
    // the remaining budget to TiffDecoder's internal buffer and intermediates.
    let image = decoded(bytes)?;
    let mut output = BoundedPng(Vec::new());
    image::codecs::png::PngEncoder::new(&mut output)
        .write_image(
            image.as_bytes(),
            image.width(),
            image.height(),
            image.color().into(),
        )
        .map_err(|e| format!("TIFF could not be normalized within the 4 MiB PNG limit: {e}"))?;
    static_image_mime(&output.0)?;
    Ok(Cow::Owned(output.0))
}

struct BoundedPng(Vec<u8>);
impl Write for BoundedPng {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > (FILE_BYTES as usize).saturating_sub(self.0.len()) {
            return Err(io::Error::other("normalized image exceeds 4 MiB"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// image 0.25.10's TiffDecoder only exposes the first image. Inspect classic
/// TIFF's first IFD without allocating tag payloads, refusing additional pages,
/// SubIFDs and BigTIFF explicitly instead of silently choosing a first frame.
/// This also bounds metadata consumed by TiffDecoder::new before set_limits.
fn single_tiff_directory(bytes: &[u8]) -> Result<(), String> {
    let invalid = || "invalid or oversized TIFF directory".to_owned();
    let little = match bytes.get(..2) {
        Some(b"II") => true,
        Some(b"MM") => false,
        _ => return Err(invalid()),
    };
    let u16_at = |offset: usize| -> Result<u16, String> {
        let word: [u8; 2] = bytes
            .get(offset..offset.checked_add(2).ok_or_else(invalid)?)
            .ok_or_else(invalid)?
            .try_into()
            .map_err(|_| invalid())?;
        Ok(if little {
            u16::from_le_bytes(word)
        } else {
            u16::from_be_bytes(word)
        })
    };
    let u32_at = |offset: usize| -> Result<u32, String> {
        let word: [u8; 4] = bytes
            .get(offset..offset.checked_add(4).ok_or_else(invalid)?)
            .ok_or_else(invalid)?
            .try_into()
            .map_err(|_| invalid())?;
        Ok(if little {
            u32::from_le_bytes(word)
        } else {
            u32::from_be_bytes(word)
        })
    };
    if u16_at(2)? != 42 {
        return Err(
            "BigTIFF/unsupported TIFF header; use a static classic TIFF, PNG or JPEG".into(),
        );
    }
    let start = u32_at(4)? as usize;
    if start < 8 {
        return Err(invalid());
    }
    let count = usize::from(u16_at(start)?);
    if count == 0 || count > 4096 {
        return Err(invalid());
    }
    let entries = start.checked_add(2).ok_or_else(invalid)?;
    let end = entries.checked_add(count * 12).ok_or_else(invalid)?;
    if u32_at(end)? != 0 {
        return Err("multipage TIFF is unsupported; use a single static image".into());
    }
    let mut metadata = 0usize;
    let mut tags = std::collections::HashSet::new();
    for i in 0..count {
        let entry = entries + i * 12;
        let tag = u16_at(entry)?;
        if !tags.insert(tag) {
            return Err("duplicate TIFF tags are unsupported".into());
        }
        if tag == 330 {
            return Err(
                "TIFF SubIFDs/additional images are unsupported; use a single static image".into(),
            );
        }
        let field_type = u16_at(entry + 2)?;
        let size = match field_type {
            1 | 2 | 6 | 7 => 1usize,
            3 | 8 => 2,
            4 | 9 | 11 | 13 => 4,
            5 | 10 | 12 => 8,
            _ => return Err("unsupported TIFF field type".into()),
        };
        let len = (u32_at(entry + 4)? as usize)
            .checked_mul(size)
            .ok_or_else(invalid)?;
        metadata = metadata.checked_add(len).ok_or_else(invalid)?;
        if metadata > 1 << 20 {
            return Err("TIFF metadata exceeds 1 MiB".into());
        }
        let value = if len <= 4 {
            entry + 8
        } else {
            u32_at(entry + 8)? as usize
        };
        if bytes
            .get(value..value.checked_add(len).ok_or_else(invalid)?)
            .is_none()
        {
            return Err(invalid());
        }
        if tag == 297
            && (field_type != 3 || len != 4 || u16_at(value)? != 0 || u16_at(value + 2)? != 1)
        {
            return Err("multipage/ambiguous TIFF PageNumber is unsupported".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::attachments::image_thumbnail;
    use std::io::Cursor;

    fn encoded(width: u32, height: u32, format: ImageFormat) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::DynamicImage::new_rgb8(width, height)
            .write_to(&mut Cursor::new(&mut bytes), format)
            .unwrap();
        bytes
    }
    fn first_ifd(bytes: &[u8]) -> (usize, usize, bool) {
        let little = &bytes[..2] == b"II";
        let word = bytes[4..8].try_into().unwrap();
        let start = if little {
            u32::from_le_bytes(word)
        } else {
            u32::from_be_bytes(word)
        } as usize;
        let word = bytes[start..start + 2].try_into().unwrap();
        let count = if little {
            u16::from_le_bytes(word)
        } else {
            u16::from_be_bytes(word)
        } as usize;
        (start + 2, count, little)
    }
    fn put32(bytes: &mut [u8], offset: usize, value: u32, little: bool) {
        bytes[offset..offset + 4].copy_from_slice(&if little {
            value.to_le_bytes()
        } else {
            value.to_be_bytes()
        });
    }
    #[test]
    fn static_tiff_normalizes_for_preview_and_staging_without_changing_png_jpeg() {
        // Uncompressed RGB TIFF >4 MiB is an ordinary screenshot size. Its
        // normalized PNG is admitted under exactly the host's existing cap.
        let raw = encoded(1600, 1000, ImageFormat::Tiff);
        assert!(raw.len() as u64 > FILE_BYTES);
        let png = normalize_clipboard_image(&raw).unwrap();
        assert_eq!(image::guess_format(&png).unwrap(), ImageFormat::Png);
        assert!(png.len() as u64 <= FILE_BYTES);
        let original = image::load_from_memory(&raw).unwrap();
        assert_eq!(image::load_from_memory(&png).unwrap(), original);
        let thumb = image_thumbnail(&png).unwrap();
        let thumb = image::load_from_memory(&thumb).unwrap();
        assert_eq!((thumb.width(), thumb.height()), (256, 160));
        for format in [ImageFormat::Png, ImageFormat::Jpeg] {
            let bytes = encoded(20, 10, format);
            let same = normalize_clipboard_image(&bytes).unwrap();
            assert!(matches!(same, Cow::Borrowed(_)));
            assert_eq!(same.as_ref(), bytes);
        }
    }
    #[test]
    fn tiff_refuses_pages_subifds_corrupt_metadata_and_hostile_dimensions() {
        let raw = encoded(20, 10, ImageFormat::Tiff);
        let (entries, count, little) = first_ifd(&raw);
        // A real second full IFD is reachable; never decode only the first.
        let mut pages = raw.clone();
        let next = pages.len() as u32;
        pages.extend_from_slice(&raw[entries - 2..entries + count * 12 + 4]);
        put32(&mut pages, entries + count * 12, next, little);
        assert!(
            normalize_clipboard_image(&pages)
                .unwrap_err()
                .contains("multipage")
        );
        let mut sub = raw.clone();
        sub[entries..entries + 2].copy_from_slice(&if little {
            330u16.to_le_bytes()
        } else {
            330u16.to_be_bytes()
        });
        assert!(
            normalize_clipboard_image(&sub)
                .unwrap_err()
                .contains("SubIFDs")
        );
        let mut metadata = raw.clone();
        put32(&mut metadata, entries + 4, u32::MAX, little);
        assert!(normalize_clipboard_image(&metadata).is_err());
        let mut outside = raw.clone();
        put32(&mut outside, 4, u32::MAX, little);
        assert!(normalize_clipboard_image(&outside).is_err());
        assert!(normalize_clipboard_image(&raw[..8]).is_err());
        assert!(
            single_tiff_directory(b"II+\0\x08\0\0\0")
                .unwrap_err()
                .contains("BigTIFF")
        );
        for (width, height) in [(8193, 1), (4097, 4097)] {
            // Mutate a genuine TIFF width/height tag without allocating the
            // hostile raster. Header remains valid, decoder must reject bounds.
            let mut huge = raw.clone();
            for i in 0..count {
                let entry = entries + i * 12;
                let word = huge[entry..entry + 2].try_into().unwrap();
                let tag = if little {
                    u16::from_le_bytes(word)
                } else {
                    u16::from_be_bytes(word)
                };
                if tag == 256 || tag == 257 || tag == 278 {
                    put32(
                        &mut huge,
                        entry + 8,
                        if tag == 256 { width } else { height },
                        little,
                    );
                }
            }
            let error = normalize_clipboard_image(&huge).unwrap_err();
            assert!(
                error.contains(if width > 8192 {
                    "oversized"
                } else {
                    "16 megapixels"
                }),
                "{error}"
            );
        }
    }
    #[test]
    fn animated_png_is_refused_before_preview_or_staging_can_choose_a_frame() {
        fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
            let mut chunk = (data.len() as u32).to_be_bytes().to_vec();
            chunk.extend_from_slice(kind);
            chunk.extend_from_slice(data);
            let mut crc = u32::MAX;
            for byte in &chunk[4..] {
                crc ^= u32::from(*byte);
                for _ in 0..8 {
                    crc = (crc >> 1) ^ if crc & 1 != 0 { 0xedb88320 } else { 0 };
                }
            }
            chunk.extend_from_slice(&(!crc).to_be_bytes());
            chunk
        }
        let mut png = encoded(20, 10, ImageFormat::Png);
        // A valid APNG frame-control header around the genuine PNG's IDAT.
        let animation = chunk(b"acTL", &[0, 0, 0, 1, 0, 0, 0, 0]);
        let frame = chunk(
            b"fcTL",
            &[
                0, 0, 0, 0, 0, 0, 0, 20, 0, 0, 0, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 10, 0, 0,
            ],
        );
        png.splice(33..33, animation.into_iter().chain(frame));
        assert!(
            normalize_clipboard_image(&png)
                .unwrap_err()
                .contains("animated PNG")
        );
        assert!(image_thumbnail(&png).unwrap_err().contains("animated PNG"));
    }

    #[test]
    fn raw_output_and_allocation_limits_refuse_explicitly() {
        assert!(
            normalize_clipboard_image(&vec![0; RAW_TIFF_BYTES as usize + 1])
                .unwrap_err()
                .contains("64 MiB")
        );
        // Deterministic noise produces a valid static TIFF whose PNG exceeds
        // the staging limit; verify refusal through the real encoder pipeline.
        let mut noise = image::RgbImage::new(2048, 1024);
        let mut random = 0x12345678u32;
        for byte in noise.as_mut() {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            *byte = random as u8;
        }
        let mut noisy_tiff = Vec::new();
        image::DynamicImage::ImageRgb8(noise)
            .write_to(&mut Cursor::new(&mut noisy_tiff), ImageFormat::Tiff)
            .unwrap();
        assert!(
            normalize_clipboard_image(&noisy_tiff)
                .unwrap_err()
                .contains("4 MiB")
        );
        assert!(
            normalize_clipboard_image(b"GIF89a")
                .unwrap_err()
                .contains("unsupported")
        );
        // A legitimate 16-bit RGBA buffer >64 MiB (within axis/pixel limits)
        // must fail admission before allocating its raster.
        let mut raw = Vec::new();
        image::DynamicImage::new_rgba16(1, 1)
            .write_to(&mut Cursor::new(&mut raw), ImageFormat::Tiff)
            .unwrap();
        let (entries, count, little) = first_ifd(&raw);
        for i in 0..count {
            let entry = entries + i * 12;
            let word = raw[entry..entry + 2].try_into().unwrap();
            let tag = if little {
                u16::from_le_bytes(word)
            } else {
                u16::from_be_bytes(word)
            };
            if tag == 256 || tag == 257 || tag == 278 {
                put32(&mut raw, entry + 8, 4096, little);
            }
        }
        let error = normalize_clipboard_image(&raw).unwrap_err();
        assert!(error.contains("oversized"), "{error}");
    }
}
