//! Tests for DCT-scaled JPEG decode in the loader.
//!
//! Planning tests use true 12/24/48 MP dimensions on a 3840x2160 canvas (no
//! pixels involved). Decode tests keep the same source-to-canvas ratios at 1/8
//! of the linear size so that they run quickly in a debug build.

use super::decode_scale::{self, CoverNeed, DecodeTarget};
use super::*;
use image::{ImageEncoder, RgbImage};

/// Smooth, colourful, non-trivial test pattern (low frequencies only, so a
/// DCT-domain downscale and a resample agree closely).
pub(crate) fn smooth_rgb(w: u32, h: u32) -> RgbImage {
    RgbImage::from_fn(w, h, |x, y| {
        let u = x as f32 / w as f32;
        let v = y as f32 / h as f32;
        let r = 0.5 + 0.35 * (u * 5.0).sin() + 0.15 * (v * 3.0).cos();
        let g = 0.5 + 0.35 * (v * 4.0 + u * 2.0).sin() + 0.1 * (u * 7.0).cos();
        let b = 0.2 + 0.6 * u * v + 0.2 * ((u + v) * 3.0).sin();
        image::Rgb([r, g, b].map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8))
    })
}

/// Inserts a minimal EXIF APP1 segment carrying `orientation` after the SOI.
fn with_exif_orientation(jpeg: &[u8], orientation: u16) -> Vec<u8> {
    assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"MM\0\x2A\0\0\0\x08"); // big-endian, IFD0 at 8
    tiff.extend_from_slice(&[0, 1]); // one entry
    tiff.extend_from_slice(&[0x01, 0x12, 0, 3, 0, 0, 0, 1]); // Orientation, SHORT, 1
    tiff.extend_from_slice(&orientation.to_be_bytes());
    tiff.extend_from_slice(&[0, 0]); // value padding
    tiff.extend_from_slice(&[0, 0, 0, 0]); // no next IFD
    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend_from_slice(&tiff);
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(&app1);
    out.extend_from_slice(&jpeg[2..]);
    out
}

/// A smooth `w` x `h` baseline JPEG stored with EXIF `orientation`.
pub(crate) fn synthetic_jpeg(w: u32, h: u32, orientation: u16) -> Vec<u8> {
    let rgb = smooth_rgb(w, h);
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 95)
        .write_image(rgb.as_raw(), w, h, image::ExtendedColorType::Rgb8)
        .unwrap();
    with_exif_orientation(&bytes, orientation)
}

fn write_temp(bytes: &[u8], name: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    std::fs::write(&path, bytes).unwrap();
    (dir, path)
}

fn target_4k() -> DecodeTarget {
    DecodeTarget {
        canvas_w: 3840,
        canvas_h: 2160,
        cover: CoverNeed::Never,
        bg_sample_scale: None,
    }
}

/// Source dimensions for `megapixels` at aspect `a:b` (landscape).
fn dims_for(megapixels: f64, a: f64, b: f64) -> (u16, u16) {
    let h = (megapixels * 1e6 * b / a).sqrt();
    ((h * a / b).round() as u16, h.round() as u16)
}

const MEGAPIXELS: [f64; 3] = [12.0, 24.0, 48.0];
const ASPECTS: [(f64, f64); 2] = [(3.0, 2.0), (4.0, 3.0)];
const ORIENTATIONS: [u16; 3] = [1, 6, 8];
const ALL_ORIENTATIONS: [u16; 8] = [1, 2, 3, 4, 5, 6, 7, 8];

#[test]
fn chosen_scale_never_undershoots_the_photo_rect() {
    let targets = [
        target_4k(),
        DecodeTarget {
            cover: CoverNeed::Always,
            ..target_4k()
        },
        DecodeTarget {
            bg_sample_scale: Some(0.125),
            ..target_4k()
        },
        DecodeTarget {
            bg_sample_scale: Some(1.0),
            cover: CoverNeed::Always,
            ..target_4k()
        },
    ];
    for mp in MEGAPIXELS {
        for (a, b) in ASPECTS {
            for orientation in ALL_ORIENTATIONS {
                for target in &targets {
                    let (fw, fh) = dims_for(mp, a, b);
                    let (idct, need_w, need_h) = decode_scale::plan(fw, fh, orientation, target);
                    let ctx = format!("{mp} MP {a}:{b} orientation {orientation} {target:?}");

                    // The chosen scale covers the requirement on both axes.
                    assert!(decode_scale::scaled_len(fw, idct) >= need_w, "{ctx}");
                    assert!(decode_scale::scaled_len(fh, idct) >= need_h, "{ctx}");

                    // It is the smallest that does.
                    if idct > 1 {
                        let smaller = idct / 2;
                        assert!(
                            decode_scale::scaled_len(fw, smaller) < need_w
                                || decode_scale::scaled_len(fh, smaller) < need_h,
                            "{ctx}: scale {smaller}/8 would also have covered"
                        );
                    }

                    // And, measured independently in the displayed orientation,
                    // covers the contained fit of the photo on the canvas.
                    let (dw, dh) = if decode_scale::orientation_swaps_axes(orientation) {
                        (fh, fw)
                    } else {
                        (fw, fh)
                    };
                    // Independent of the production helper: nearest pixel of len*n/8.
                    let out = |len: u16| ((len as f64 * idct as f64 / 8.0) + 0.5).floor();
                    let (out_dw, out_dh) = if decode_scale::orientation_swaps_axes(orientation) {
                        (out(fh), out(fw))
                    } else {
                        (out(fw), out(fh))
                    };
                    let fit = (3840.0 / dw as f64).min(2160.0 / dh as f64).min(1.0);
                    assert!(out_dw + 1e-6 >= dw as f64 * fit, "{ctx}");
                    assert!(out_dh + 1e-6 >= dh as f64 * fit, "{ctx}");
                }
            }
        }
    }
}

#[test]
fn twelve_megapixel_4_3_on_4k_decodes_at_full_scale() {
    // Half scale would be 2016x1512, under the 2880x2160 contained fit.
    let (fw, fh) = dims_for(12.0, 4.0, 3.0);
    for orientation in ORIENTATIONS {
        let (idct, ..) = decode_scale::plan(fw, fh, orientation, &target_4k());
        assert_eq!(idct, 8, "orientation {orientation}");
    }
}

#[test]
fn forty_eight_megapixel_decodes_at_half_scale() {
    for (a, b) in ASPECTS {
        let (fw, fh) = dims_for(48.0, a, b);
        for orientation in ORIENTATIONS {
            let (idct, ..) = decode_scale::plan(fw, fh, orientation, &target_4k());
            assert_eq!(idct, 4, "{a}:{b} orientation {orientation}");
        }
    }
}

#[test]
fn fill_when_fits_and_sharp_backgrounds_ask_for_more() {
    // A 48 MP portrait contained on a landscape canvas is happy at 1/2, but the
    // cover-crop `fill-when-fits` may draw is as wide as the canvas.
    let (fh, fw) = dims_for(48.0, 4.0, 3.0); // portrait file
    let contain = decode_scale::plan(fw, fh, 1, &target_4k()).0;
    let cover = decode_scale::plan(
        fw,
        fh,
        1,
        &DecodeTarget {
            cover: CoverNeed::Always,
            ..target_4k()
        },
    )
    .0;
    assert!(contain < 8);
    assert!(cover >= contain);
    let (need_w, _) = decode_scale::required_size(fw as u32, fh as u32, (3840, 2160), true, None);
    assert!(need_w >= 3840.min(fw as u32));
}

fn fill_cfg(max_crop: f32, prob: f32) -> crate::config::FillWhenFits {
    serde_yaml::from_str(&format!(
        "maximum-crop-percentage: {max_crop}\nskip-matting-probability: {prob}\n"
    ))
    .unwrap()
}

fn fillable_4k(max_crop: f32) -> DecodeTarget {
    DecodeTarget {
        cover: CoverNeed::WhenFillable(fill_cfg(max_crop, 1.0)),
        ..target_4k()
    }
}

#[test]
fn cover_is_sized_only_for_photos_that_could_fill() {
    // 24 MP 3:2 portrait (3266x4899) on a 3840x2160 canvas: a cover-crop would
    // trim 62% of it, so with the shipped 5% maximum it can never fill and is
    // sized for its contained fit (1440x2160), which 1/2 covers.
    let (fw, fh) = (3266u16, 4899u16);
    for (w, h, orientation) in [(fw, fh, 1u16), (fh, fw, 6), (fh, fw, 8)] {
        let (idct, ..) = decode_scale::plan(w, h, orientation, &fillable_4k(5.0));
        assert_eq!(idct, 4, "default max-crop, orientation {orientation}");
        // ... exactly as if fill-when-fits were not configured.
        assert_eq!(idct, decode_scale::plan(w, h, orientation, &target_4k()).0);

        // A maximum that admits it (62.5% crop) means it may fill, so it is
        // decoded for the cover rect (3840 wide: more than the source has).
        let target = fillable_4k(80.0);
        let (idct, ..) = decode_scale::plan(w, h, orientation, &target);
        assert_eq!(idct, 8, "permissive max-crop, orientation {orientation}");
        // The cover rect (3840 px wide) is more than the source has, so the
        // requirement is the whole source.
        let (_, need_w, need_h) = decode_scale::plan(w, h, orientation, &target);
        assert_eq!(
            (need_w, need_h),
            (w as u32, h as u32),
            "orientation {orientation}"
        );
        assert_eq!(
            idct,
            decode_scale::plan(
                w,
                h,
                orientation,
                &DecodeTarget {
                    cover: CoverNeed::Always,
                    ..target_4k()
                }
            )
            .0
        );
    }

    // The boundary follows the same rule as `should_fill`: a 16:9 48 MP photo
    // is fillable at 5% and sized for the canvas, which 1/2 covers.
    let (idct, ..) = decode_scale::plan(9243, 5199, 1, &fillable_4k(5.0));
    assert_eq!(idct, 4);
    // And one just beyond the maximum (crop 6% > 5%) is not.
    let (w, h) = (8000u16, 5000u16); // 16:10
    let crop = crate::config::FillWhenFits::crop_fraction(w as u32, h as u32, 3840, 2160);
    assert!(crop > 0.05 && crop < 0.15);
    assert_eq!(
        decode_scale::plan(w, h, 1, &fillable_4k(5.0)).0,
        decode_scale::plan(w, h, 1, &target_4k()).0
    );
    // A zero skip probability can never fill.
    let never = DecodeTarget {
        cover: CoverNeed::WhenFillable(fill_cfg(100.0, 0.0)),
        ..target_4k()
    };
    assert_eq!(
        decode_scale::plan(fw, fh, 1, &never).0,
        decode_scale::plan(fw, fh, 1, &target_4k()).0
    );
}

#[test]
fn pair_members_are_sized_for_contain_even_when_they_could_fill() {
    // The same portrait with a permissive maximum: solo needs full, but a pair
    // member (pairs never fill) is sized for its contained fit.
    let target = fillable_4k(80.0);
    assert_eq!(decode_scale::plan(3266, 4899, 1, &target).0, 8);
    assert_eq!(
        decode_scale::plan(3266, 4899, 1, &target.for_pair_member()).0,
        4
    );
}

#[test]
fn small_sources_are_never_scaled_or_upscaled_for() {
    let (idct, need_w, need_h) = decode_scale::plan(1600, 1200, 1, &target_4k());
    assert_eq!(idct, 8);
    assert_eq!((need_w, need_h), (1600, 1200));
}

#[test]
fn unknown_canvas_or_disabled_scaling_yields_no_target() {
    assert!(DecodeScaling::disabled().target().is_none());
    let scaling = DecodeScaling::disabled();
    scaling.publish_canvas(3840, 2160);
    assert!(scaling.target().is_none(), "disabled stays disabled");
}

/// Decode-level fixtures: the planning ratios above at 1/8 linear size, on a
/// 480x270 canvas.
fn small_target() -> DecodeTarget {
    DecodeTarget {
        canvas_w: 480,
        canvas_h: 270,
        cover: CoverNeed::Never,
        bg_sample_scale: None,
    }
}

#[test]
fn decode_picks_scale_per_size_aspect_and_orientation() {
    // (width, height, expected DCT scale) at 1/8 linear size.
    let cases: [(u32, u32, u8); 6] = [
        (530, 354, 8),  // 12 MP 3:2
        (504, 378, 8),  // 12 MP 4:3
        (750, 500, 8),  // 24 MP 3:2
        (707, 530, 8),  // 24 MP 4:3
        (1061, 707, 4), // 48 MP 3:2
        (1000, 750, 4), // 48 MP 4:3
    ];
    for (w, h, expected_landscape) in cases {
        for orientation in ORIENTATIONS {
            let bytes = synthetic_jpeg(w, h, orientation);
            let (_dir, path) = write_temp(&bytes, "photo.jpg");
            let target = small_target();
            let swapped = decode_scale::orientation_swaps_axes(orientation);
            let (dw, dh) = if swapped { (h, w) } else { (w, h) };

            // Expected scale worked out directly in the displayed axes: the
            // smallest n/8 whose output covers the contained fit. A rotated
            // (portrait) photo has a smaller fit on a landscape canvas, so it
            // may legitimately take a smaller scale than its landscape twin.
            let fit = (480.0 / dw as f64).min(270.0 / dh as f64).min(1.0);
            let expected = [1u8, 2, 4, 8]
                .into_iter()
                .find(|&n| {
                    let ow = (dw as f64 * n as f64 / 8.0 + 0.5).floor();
                    let oh = (dh as f64 * n as f64 / 8.0 + 0.5).floor();
                    ow + 1e-6 >= dw as f64 * fit && oh + 1e-6 >= dh as f64 * fit
                })
                .unwrap();
            if orientation == 1 {
                assert_eq!(expected, expected_landscape, "{w}x{h}");
            }

            let scaled = decode_jpeg_scaled(std::io::Cursor::new(&bytes), orientation, &target);
            match (&scaled, expected) {
                (None, 8) => {}
                (Some(jpeg), n) if n < 8 => assert_eq!(jpeg.idct, n, "{w}x{h} o{orientation}"),
                other => panic!("{w}x{h} o{orientation}: unexpected {:?}", other.0.is_some()),
            }

            // End to end: orientation applied, covers the contained fit.
            let photo = decode_rgba8_apply_exif(&path, Some(target)).unwrap();
            let img = photo.image;
            let full = decode_rgba8_apply_exif(&path, None).unwrap().image;
            assert_eq!(full.dimensions(), (dw, dh));
            assert_eq!(photo.source_size, (dw, dh), "source size is the original's");
            assert!(img.width() as f64 + 1e-6 >= dw as f64 * fit);
            assert!(img.height() as f64 + 1e-6 >= dh as f64 * fit);
            if expected == 8 {
                assert_eq!(img.dimensions(), full.dimensions());
                assert_eq!(img.as_raw(), full.as_raw(), "full scale is bit-identical");
            } else {
                let k = 8.0 / expected as f64;
                // Exact scaled extent, rounded: the padded block remainder is resampled away.
                assert_eq!(img.width(), (dw as f64 / k + 0.5).floor() as u32);
                assert_eq!(img.height(), (dh as f64 / k + 0.5).floor() as u32);
            }
        }
    }
}

#[test]
fn scaled_decode_matches_full_decode_after_resample() {
    // Resample both decodes to the contained photo rect and compare.
    let bytes = synthetic_jpeg(1000, 750, 1);
    let (_dir, path) = write_temp(&bytes, "photo.jpg");
    let scaled = decode_rgba8_apply_exif(&path, Some(small_target()))
        .unwrap()
        .image;
    let full = decode_rgba8_apply_exif(&path, None).unwrap().image;
    assert_eq!(scaled.dimensions(), (500, 375));
    assert_eq!(full.dimensions(), (1000, 750));
    let a = image::imageops::resize(&scaled, 360, 270, image::imageops::FilterType::Triangle);
    let b = image::imageops::resize(&full, 360, 270, image::imageops::FilterType::Triangle);
    let psnr = psnr_rgb(&a, &b);
    assert!(psnr > 40.0, "PSNR {psnr:.1} dB");
}

pub(crate) fn psnr_rgb(a: &image::RgbaImage, b: &image::RgbaImage) -> f64 {
    assert_eq!(a.dimensions(), b.dimensions());
    let mut se = 0f64;
    let mut n = 0f64;
    for (pa, pb) in a.pixels().zip(b.pixels()) {
        for c in 0..3 {
            let d = pa[c] as f64 - pb[c] as f64;
            se += d * d;
            n += 1.0;
        }
    }
    if se == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (255.0f64 * 255.0 / (se / n)).log10()
}

#[test]
fn png_decodes_through_the_image_crate() {
    let rgb = smooth_rgb(1000, 750);
    let (_dir, path) = {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.png");
        rgb.save(&path).unwrap();
        (dir, path)
    };
    let photo = decode_rgba8_apply_exif(&path, Some(small_target())).unwrap();
    assert_eq!(photo.image.dimensions(), (1000, 750), "PNG is never scaled");
    assert_eq!(photo.source_size, (1000, 750));
}

/// A 64x64, four-component (CMYK-style) baseline JPEG of flat mid-grey blocks.
/// jpeg-decoder decodes four components as CMYK, which the scaled path does
/// not take, so this exercises the fallback to the `image` crate.
fn cmyk_jpeg() -> Vec<u8> {
    // Use the encoder's own tables from an 8x8 greyscale JPEG.
    let mut gray = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut gray, 90)
        .write_image(&[128u8; 64], 8, 8, image::ExtendedColorType::L8)
        .unwrap();
    let mut out = vec![0xFF, 0xD8];
    let mut i = 2;
    while i + 4 <= gray.len() {
        assert_eq!(gray[i], 0xFF);
        let marker = gray[i + 1];
        let len = u16::from_be_bytes([gray[i + 2], gray[i + 3]]) as usize;
        let seg = &gray[i..i + 2 + len];
        match marker {
            0xDB | 0xC4 => out.extend_from_slice(seg), // quantisation / Huffman tables
            0xC0 => {
                // SOF0: 64x64, four components, 1x1 sampling, table 0.
                out.extend_from_slice(&[0xFF, 0xC0, 0, 8 + 3 * 4, 8, 0, 64, 0, 64, 4]);
                for id in 1..=4u8 {
                    out.extend_from_slice(&[id, 0x11, 0]);
                }
            }
            0xDA => {
                out.extend_from_slice(&[0xFF, 0xDA, 0, 6 + 2 * 4, 4]);
                for id in 1..=4u8 {
                    out.extend_from_slice(&[id, 0x00]);
                }
                out.extend_from_slice(&[0, 63, 0]);
                // 64 MCUs x 4 blocks of (DC diff 0 = "00", EOB = "1010") = 24 bits.
                for _ in 0..64 {
                    out.extend_from_slice(&[0x28, 0xA2, 0x8A]);
                }
                out.extend_from_slice(&[0xFF, 0xD9]);
                return out;
            }
            _ => {}
        }
        i += 2 + len;
    }
    panic!("no SOS in encoder output");
}

#[test]
fn jpeg_rejected_by_scaled_path_falls_back_to_image_crate() {
    let bytes = cmyk_jpeg();
    let target = DecodeTarget {
        canvas_w: 8,
        canvas_h: 8,
        cover: CoverNeed::Never,
        bg_sample_scale: None,
    };
    // The scaled path declines it ...
    let declined = decode_jpeg_scaled(std::io::Cursor::new(&bytes), 1, &target);
    assert!(declined.is_none());
    // ... and the image crate path is what decodes it, identically with or
    // without a target.
    let (_dir, path) = write_temp(&bytes, "cmyk.jpg");
    let with = decode_rgba8_apply_exif(&path, Some(target));
    let without = decode_rgba8_apply_exif(&path, None);
    let (with, without) = (with.unwrap().image, without.unwrap().image);
    assert_eq!(with.dimensions(), (64, 64));
    assert_eq!(with.as_raw(), without.as_raw());
}

#[test]
fn garbage_after_a_valid_header_falls_back_without_panicking() {
    let mut bytes = synthetic_jpeg(1000, 750, 1);
    bytes.truncate(bytes.len() / 2);
    let (_dir, path) = write_temp(&bytes, "truncated.jpg");
    let scaled = decode_rgba8_apply_exif(&path, Some(small_target()));
    let full = decode_rgba8_apply_exif(&path, None);
    // Whatever the image crate makes of a truncated file, the scaled request
    // must not change the outcome class.
    assert_eq!(scaled.is_ok(), full.is_ok());
}

// ---------------------------------------------------------------------------
// Textured, non-multiple-of-8 fixtures in several JPEG flavours.
// ---------------------------------------------------------------------------

/// Photo-like content: smooth gradients plus fine texture and hard edges, so a
/// resample of a mis-sized or stretched buffer visibly disagrees.
pub(crate) fn textured_rgb(w: u32, h: u32) -> RgbImage {
    let mut rgb = smooth_rgb(w, h);
    for (x, y, px) in rgb.enumerate_pixels_mut() {
        let hash = x
            .wrapping_mul(2654435761)
            .wrapping_add(y.wrapping_mul(40503))
            .rotate_left(13)
            .wrapping_mul(2246822519);
        let noise = ((hash >> 24) as i32 - 128) / 16; // +-8 levels of grain
        let stripe = (((x as f32) * 0.9).sin() * ((y as f32) * 0.7).cos() * 14.0) as i32;
        let edge = if (x / 37 + y / 29) % 5 == 0 { 28 } else { 0 };
        for c in px.0.iter_mut() {
            *c = (*c as i32 + noise + stripe + edge).clamp(0, 255) as u8;
        }
    }
    rgb
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Flavor {
    Baseline444,
    Baseline422,
    Baseline420,
    Progressive420,
    Gray,
}

pub(crate) const FLAVORS: [Flavor; 5] = [
    Flavor::Baseline444,
    Flavor::Baseline422,
    Flavor::Baseline420,
    Flavor::Progressive420,
    Flavor::Gray,
];

/// A textured `w` x `h` JPEG of the given flavour stored with `orientation`.
pub(crate) fn photo_jpeg(w: u32, h: u32, orientation: u16, flavor: Flavor) -> Vec<u8> {
    use jpeg_encoder::{ColorType, Encoder, SamplingFactor};
    let rgb = textured_rgb(w, h);
    let mut out = Vec::new();
    let mut enc = Encoder::new(&mut out, 92);
    let (data, color) = match flavor {
        Flavor::Gray => (
            image::DynamicImage::ImageRgb8(rgb).to_luma8().into_raw(),
            ColorType::Luma,
        ),
        _ => (rgb.into_raw(), ColorType::Rgb),
    };
    match flavor {
        Flavor::Baseline444 => enc.set_sampling_factor(SamplingFactor::F_1_1),
        Flavor::Baseline422 => enc.set_sampling_factor(SamplingFactor::F_2_1),
        Flavor::Baseline420 | Flavor::Progressive420 => {
            enc.set_sampling_factor(SamplingFactor::F_2_2)
        }
        Flavor::Gray => {}
    }
    enc.set_progressive(matches!(flavor, Flavor::Progressive420));
    enc.encode(&data, w as u16, h as u16, color).unwrap();
    with_exif_orientation(&out, orientation)
}

/// Triangle-resample both to the contained fit of `fit_w` x `fit_h` and score.
fn resampled_psnr(a: &image::RgbaImage, b: &image::RgbaImage, fit: (u32, u32)) -> f64 {
    let f = image::imageops::FilterType::Triangle;
    psnr_rgb(
        &image::imageops::resize(a, fit.0, fit.1, f),
        &image::imageops::resize(b, fit.0, fit.1, f),
    )
}

/// The contained fit, in pixels, of `dims` on `canvas`.
fn contain(dims: (u32, u32), canvas: (u32, u32)) -> (u32, u32) {
    let s = (canvas.0 as f64 / dims.0 as f64)
        .min(canvas.1 as f64 / dims.1 as f64)
        .min(1.0);
    (
        (dims.0 as f64 * s).round().max(1.0) as u32,
        (dims.1 as f64 * s).round().max(1.0) as u32,
    )
}

fn small_canvas_target(canvas: (u32, u32)) -> DecodeTarget {
    DecodeTarget {
        canvas_w: canvas.0,
        canvas_h: canvas.1,
        cover: CoverNeed::Never,
        bg_sample_scale: None,
    }
}

/// jpeg-decoder's own padded output (ceil(len*n/8), the pad replicated edge
/// data), as an RGBA image: what the loader handed on before cropping.
fn uncropped_scaled(bytes: &[u8], idct_target: (u16, u16)) -> image::RgbaImage {
    let mut d = jpeg_decoder::Decoder::new(std::io::Cursor::new(bytes));
    d.scale(idct_target.0, idct_target.1).unwrap();
    let px = d.decode().unwrap();
    let info = d.info().unwrap();
    let mut rgba = Vec::new();
    for c in px.chunks_exact(3) {
        rgba.extend_from_slice(&[c[0], c[1], c[2], 255]);
    }
    image::RgbaImage::from_raw(info.width as u32, info.height as u32, rgba).unwrap()
}

#[test]
fn reduced_decode_spans_exactly_the_photo() {
    // 997x749 at 1/4: jpeg-decoder returns 250x188 (ceil of 249.25 x 187.25);
    // the loader must hand on 249x187. Canvas chosen so 1/4 is the plan.
    let (w, h) = (997u32, 749u32);
    let canvas = (240u32, 135u32);
    let bytes = photo_jpeg(w, h, 1, Flavor::Baseline420);
    let target = small_canvas_target(canvas);

    let jpeg = decode_jpeg_scaled(std::io::Cursor::new(&bytes), 1, &target).expect("reduced");
    assert_eq!(jpeg.idct, 2);
    assert_eq!(jpeg.img.dimensions(), (249, 187));

    // The matting resample of the cropped buffer tracks the full decode ...
    let (_dir, path) = write_temp(&bytes, "photo.jpg");
    let full = decode_rgba8_apply_exif(&path, None).unwrap().image;
    let fit = contain((w, h), canvas);
    let cropped = resampled_psnr(&jpeg.img, &full, fit);
    // ... and the padded buffer it replaces stretches the frame measurably.
    let padded = uncropped_scaled(&bytes, (250, 188));
    assert_eq!(padded.dimensions(), (250, 188));
    let stretched = resampled_psnr(&padded, &full, fit);
    assert!(
        cropped > stretched + 4.0,
        "cropped {cropped:.1} dB should beat the padded buffer's {stretched:.1} dB"
    );
    assert!(cropped > 44.0, "cropped PSNR {cropped:.1} dB");
}

#[test]
fn reduced_decode_matches_full_decode_for_every_flavour_and_orientation() {
    // Deliberately not multiples of 8, on a canvas about half the source:
    // planned at 1/2.
    let (w, h) = (499u32, 375u32);
    let canvas = (240u32, 135u32);
    for flavor in FLAVORS {
        for orientation in ALL_ORIENTATIONS {
            let ctx = format!("{flavor:?} orientation {orientation}");
            let bytes = photo_jpeg(w, h, orientation, flavor);
            let (_dir, path) = write_temp(&bytes, "photo.jpg");
            let swapped = decode_scale::orientation_swaps_axes(orientation);
            let (dw, dh) = if swapped { (h, w) } else { (w, h) };

            let photo = decode_rgba8_apply_exif(&path, Some(small_canvas_target(canvas))).unwrap();
            let full = decode_rgba8_apply_exif(&path, None).unwrap().image;
            assert_eq!(full.dimensions(), (dw, dh), "{ctx}");
            assert_eq!(photo.source_size, (dw, dh), "{ctx}");
            assert!(photo.image.width() < dw, "{ctx}: expected a reduced decode");

            let fit = contain((dw, dh), canvas);
            assert!(
                photo.image.width() >= fit.0 && photo.image.height() >= fit.1,
                "{ctx}"
            );
            let psnr = resampled_psnr(&photo.image, &full, fit);
            assert!(psnr > 46.0, "{ctx}: PSNR {psnr:.1} dB");
        }
    }
}

#[test]
fn guard_does_not_depend_on_scale_or_reject() {
    // RGB and gray accounting mirrors the image crate's w*h*components.
    assert!(within_scaled_decode_bound(12000, 12000, 3, false)); // 432 MB
    assert!(within_scaled_decode_bound(20000, 20000, 1, false)); // gray 400 MB
    assert!(!within_scaled_decode_bound(16000, 12000, 3, false)); // 576 MB
    // Progressive keeps full-size coefficients: twice the footprint.
    assert!(!within_scaled_decode_bound(12000, 12000, 3, true));
    assert!(within_scaled_decode_bound(8000, 6000, 3, true));
}

/// Manual measurement, not a regression test. Run once to write the fixture,
/// then once per mode under `/usr/bin/time -l` for peak RSS:
/// `PF_BENCH_FILE=/path/48mp.jpg PF_BENCH=full|scaled cargo test --release -p photoframe bench_48mp -- --ignored --nocapture`
#[test]
#[ignore]
fn bench_48mp() {
    let path = PathBuf::from(std::env::var("PF_BENCH_FILE").expect("PF_BENCH_FILE"));
    if !path.exists() {
        std::fs::write(&path, synthetic_jpeg(8000, 6000, 6)).unwrap();
        println!("wrote fixture {}", path.display());
        return;
    }
    let mode = std::env::var("PF_BENCH").unwrap_or_else(|_| "full".into());
    let target = (mode == "scaled").then(target_4k);
    let start = std::time::Instant::now();
    let img = decode_rgba8_apply_exif(&path, target).unwrap().image;
    println!(
        "bench_48mp mode={mode} decoded {}x{} ({} MB RGBA) in {:?}",
        img.width(),
        img.height(),
        img.as_raw().len() / (1024 * 1024),
        start.elapsed()
    );
}
