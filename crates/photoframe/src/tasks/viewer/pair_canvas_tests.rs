//! Matting a photo as a set of windows (design §5, tests K1-K6).

use super::*;
use crate::config::{MattingKind, MattingOptions};

const KINDS: [MattingKind; 9] = [
    MattingKind::FixedColor,
    MattingKind::Blur,
    MattingKind::Studio,
    MattingKind::FixedImage,
    MattingKind::Gradient,
    MattingKind::Vignette,
    MattingKind::CinematicBlur,
    MattingKind::PassePartout,
    MattingKind::DropShadow,
];

/// A deterministic textured photo: gradients plus a hash grain, so a shifted,
/// stretched or resampled buffer changes many pixels.
fn textured(w: u32, h: u32, seed: u32) -> RgbaImage {
    RgbaImage::from_fn(w, h, |x, y| {
        let grain = (x.wrapping_mul(0x045d_9f3b) ^ y.wrapping_mul(0x27d4_eb2d) ^ seed) >> 27;
        Rgba([
            ((x * 255 / w.max(1)) as u8).wrapping_add(grain as u8),
            ((y * 255 / h.max(1)) as u8).wrapping_add(seed as u8),
            (((x + y) * 3) as u8) ^ (grain as u8),
            255,
        ])
    })
}

fn prepared(img: &RgbaImage, name: &str) -> PreparedImageCpu {
    PreparedImageCpu {
        path: std::path::PathBuf::from(name),
        width: img.width(),
        height: img.height(),
        source_size: img.dimensions(),
        pixels: img.as_raw().clone(),
    }
}

fn mat_option(kind: MattingKind, pct: f32, fixed: &std::path::Path) -> MattingOptions {
    let mut option = MattingOptions::default_for_showcase(kind, pct, Some(fixed.to_path_buf()))
        .expect("mat option");
    option.prepare_runtime().expect("runtime");
    option
}

fn params(matting: MattingOptions, screen: (u32, u32), fill_screen: bool) -> MatParams {
    MatParams {
        screen_w: screen.0,
        screen_h: screen.1,
        oversample: 1.0,
        max_dim: 4096,
        max_upscale_factor: 1.0,
        matting,
        fill_screen,
    }
}

fn solo_canvas(img: &RgbaImage, params: MatParams) -> RgbaImage {
    let task = MatTask {
        image: prepared(img, "solo.png"),
        mat_kind: Some(params.matting.caption()),
        params,
        priority: false,
    };
    let c = process_mat_task(task).expect("canvas").canvas;
    RgbaImage::from_raw(c.width, c.height, c.pixels).unwrap()
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn fixed_background(dir: &tempfile::TempDir) -> std::path::PathBuf {
    let path = dir.path().join("bg.png");
    textured(64, 48, 99).save(&path).unwrap();
    path
}

/// **K1.** A solo photo (n = 1) renders bit-identically to the code before
/// pairing: checksums recorded on the pre-refactor matting worker, for every
/// mat kind, a portrait and a landscape, two margins, and full-bleed.
#[test]
fn k1_solo_canvases_match_the_pre_pairing_goldens() {
    let dir = tempfile::tempdir().unwrap();
    let fixed = fixed_background(&dir);
    let portrait = textured(150, 200, 1);
    let landscape = textured(421, 297, 2);
    let mut got = Vec::new();
    for kind in KINDS {
        for (name, img) in [("portrait", &portrait), ("landscape", &landscape)] {
            for pct in [0.0, 6.0] {
                let p = params(mat_option(kind, pct, &fixed), (320, 180), false);
                got.push((format!("{kind:?} {name} {pct}"), fnv1a(solo_canvas(img, p).as_raw())));
            }
        }
    }
    let p = params(mat_option(MattingKind::FixedColor, 6.0, &fixed), (320, 180), true);
    got.push(("fill landscape".into(), fnv1a(solo_canvas(&landscape, p).as_raw())));
    if std::env::var_os("PRINT_K1_GOLDENS").is_some() {
        for (name, hash) in &got {
            println!("    ({name:?}, 0x{hash:016x}),");
        }
    }
    let expected: &[(&str, u64)] = &K1_GOLDENS;
    assert_eq!(got.len(), expected.len());
    for ((name, hash), (want_name, want)) in got.iter().zip(expected) {
        assert_eq!(name, want_name);
        assert_eq!(*hash, *want, "{name}: solo canvas changed");
    }
}

/// Recorded on the matting worker before pairing (main at a1c89f3 plus the
/// publish fix); regenerate only for an intended change to solo rendering
/// (PRINT_K1_GOLDENS=1).
const K1_GOLDENS: [(&str, u64); 37] = [
    ("FixedColor portrait 0", 0x0adcb2392768dd83),
    ("FixedColor portrait 6", 0xd2b06f6575cc0399),
    ("FixedColor landscape 0", 0x64f51d2785c0e7a8),
    ("FixedColor landscape 6", 0xb89451cc5469594c),
    ("Blur portrait 0", 0x9177f53eaee0dbd7),
    ("Blur portrait 6", 0x2ff4ed7bcfdbdd16),
    ("Blur landscape 0", 0x121f9b86b44fb019),
    ("Blur landscape 6", 0x67b5e970bb0702fb),
    ("Studio portrait 0", 0xa7207e44d94e57de),
    ("Studio portrait 6", 0x355f1925479935a7),
    ("Studio landscape 0", 0x3e82db83161abd59),
    ("Studio landscape 6", 0x06689b08dc844628),
    ("FixedImage portrait 0", 0xaa6ccd184c72481b),
    ("FixedImage portrait 6", 0xab58846e571d8391),
    ("FixedImage landscape 0", 0x8f875fc6adfa36a8),
    ("FixedImage landscape 6", 0x72addf0e800b70b9),
    ("Gradient portrait 0", 0x2dee6a240895f582),
    ("Gradient portrait 6", 0xc1701b752858bd98),
    ("Gradient landscape 0", 0x1b969bab7928abbd),
    ("Gradient landscape 6", 0x47074bba8059cff9),
    ("Vignette portrait 0", 0xd0173800f5343187),
    ("Vignette portrait 6", 0xf2d1a9b114ec0c39),
    ("Vignette landscape 0", 0x6cec151733f7e094),
    ("Vignette landscape 6", 0x17ff06ba4dd621a8),
    ("CinematicBlur portrait 0", 0x4210c3831e8e24dc),
    ("CinematicBlur portrait 6", 0xbe158036d101465b),
    ("CinematicBlur landscape 0", 0x6a6f2e4c17cb9c5e),
    ("CinematicBlur landscape 6", 0x6bb3d723fdf4067b),
    ("PassePartout portrait 0", 0x98434b367e95d1d3),
    ("PassePartout portrait 6", 0xbfb002d4a9039658),
    ("PassePartout landscape 0", 0x6c087df1f026233c),
    ("PassePartout landscape 6", 0xd92aa801aed4627e),
    ("DropShadow portrait 0", 0x29971e1df297001b),
    ("DropShadow portrait 6", 0x05eac8a7c1e8cf25),
    ("DropShadow landscape 0", 0x94f6653c8fc59810),
    ("DropShadow landscape 6", 0x539321a0a505fed4),
    ("fill landscape", 0xe4afcfa797bb0a46),
];
