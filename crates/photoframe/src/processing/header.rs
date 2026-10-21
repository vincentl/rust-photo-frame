//! Header-only reads of a photo's displayed size, for portrait pairing.
//!
//! Pairing plans a screen before anything is decoded, so it needs each
//! photo's size in its displayed orientation up front. That comes from the
//! file header alone: the EXIF orientation (the same read the loader does
//! before decoding) and the image dimensions from the JPEG SOF, PNG IHDR or
//! WebP header, via `ImageReader::into_dimensions`, which decodes no pixels.
//! `None` means "not known yet" (unreadable, truncated before the header,
//! still being copied in); such a photo is simply not pairable until the
//! loader's first decode reports its size (`MetaSource::Decoded`).

use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;
use tracing::debug;

use crate::tasks::loader::orientation_swaps_axes;

/// EXIF orientation of the image behind `reader` (1 when absent or
/// unreadable). Leaves the reader at an unspecified position; callers seek
/// back before decoding. Shared by the loader's decode and the header read
/// so the two can never disagree about orientation.
pub fn read_exif_orientation<R: BufRead + Seek>(reader: &mut R, path: &Path) -> u16 {
    (|| -> Option<u16> {
        let exif = exif::Reader::new().read_from_container(reader).ok()?;
        let field = exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)?;
        let val = field.value.get_uint(0)? as u16;
        debug!("exif orientation {} for {}", val, path.display());
        Some(val)
    })()
    .unwrap_or(1)
}

/// A stored `(width, height)` as displayed: axes swapped for the EXIF
/// orientations that transpose the image (5..=8).
pub fn displayed_dimensions(stored: (u32, u32), orientation: u16) -> (u32, u32) {
    if orientation_swaps_axes(orientation) {
        (stored.1, stored.0)
    } else {
        stored
    }
}

/// The photo's pixel size as displayed (EXIF orientation applied), read from
/// its header only. `None` when the header cannot be read.
pub fn read_display_size(path: &Path) -> Option<(u32, u32)> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let orientation = read_exif_orientation(&mut reader, path);
    reader.seek(SeekFrom::Start(0)).ok()?;
    let stored = image::ImageReader::new(reader)
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()?;
    if stored.0 == 0 || stored.1 == 0 {
        return None;
    }
    Some(displayed_dimensions(stored, orientation))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::loader::decode_rgba8_apply_exif;
    use image::{ImageBuffer, ImageFormat, Rgb, RgbaImage};

    /// A JPEG of `w x h` stored pixels carrying EXIF `orientation`, built by
    /// splicing an APP1 segment after SOI.
    fn jpeg_with_orientation(w: u32, h: u32, orientation: u16) -> Vec<u8> {
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(w, h, |x, y| Rgb([(x * 7) as u8, (y * 5) as u8, 90]));
        let mut plain = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut plain), ImageFormat::Jpeg)
            .unwrap();
        // Big-endian TIFF with one IFD entry: Orientation (0x0112), SHORT.
        let mut tiff = b"MM\x00\x2a\x00\x00\x00\x08".to_vec();
        tiff.extend_from_slice(&[0x00, 0x01]);
        tiff.extend_from_slice(&[0x01, 0x12, 0x00, 0x03, 0x00, 0x00, 0x00, 0x01]);
        tiff.extend_from_slice(&orientation.to_be_bytes());
        tiff.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        let mut app1 = b"Exif\x00\x00".to_vec();
        app1.extend_from_slice(&tiff);
        let len = (app1.len() + 2) as u16;
        let mut out = plain[..2].to_vec();
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&app1);
        out.extend_from_slice(&plain[2..]);
        out
    }

    fn write(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn jpeg_orientations_1_6_and_8_agree_with_the_decoder() {
        let dir = tempfile::tempdir().unwrap();
        for (orientation, expected) in [(1, (48, 32)), (6, (32, 48)), (8, (32, 48))] {
            let path = write(
                &dir,
                &format!("o{orientation}.jpg"),
                &jpeg_with_orientation(48, 32, orientation),
            );
            assert_eq!(
                read_display_size(&path),
                Some(expected),
                "orientation {orientation}"
            );
            let decoded = decode_rgba8_apply_exif(&path, None).unwrap();
            assert_eq!(
                decoded.image.dimensions(),
                expected,
                "decoder, {orientation}"
            );
            assert_eq!(decoded.source_size, expected);
        }
    }

    #[test]
    fn png_and_webp_headers_are_read() {
        let dir = tempfile::tempdir().unwrap();
        let img = RgbaImage::from_pixel(30, 70, image::Rgba([1, 2, 3, 255]));
        for (name, format) in [("a.png", ImageFormat::Png), ("a.webp", ImageFormat::WebP)] {
            let mut bytes = Vec::new();
            image::DynamicImage::ImageRgba8(img.clone())
                .write_to(&mut std::io::Cursor::new(&mut bytes), format)
                .unwrap();
            let path = write(&dir, name, &bytes);
            assert_eq!(read_display_size(&path), Some((30, 70)), "{name}");
            let decoded = decode_rgba8_apply_exif(&path, None).unwrap();
            assert_eq!(decoded.image.dimensions(), (30, 70), "{name} decoder");
        }
    }

    #[test]
    fn truncated_or_missing_files_give_none() {
        let dir = tempfile::tempdir().unwrap();
        let full = jpeg_with_orientation(48, 32, 1);
        // Cut inside the APP1 segment, before the SOF.
        let path = write(&dir, "cut.jpg", &full[..20]);
        assert_eq!(read_display_size(&path), None);
        assert_eq!(read_display_size(&dir.path().join("absent.jpg")), None);
        let path = write(&dir, "empty.jpg", b"");
        assert_eq!(read_display_size(&path), None);
    }
}
