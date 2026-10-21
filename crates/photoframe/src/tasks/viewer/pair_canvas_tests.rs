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
        bar_ratio: 0.618,
    }
}

fn solo_result(img: &RgbaImage, params: MatParams) -> MatResult {
    let task = MatTask {
        image: prepared(img, "solo.png"),
        mat_kind: Some(params.matting.caption()),
        params,
        priority: false,
        partner: None,
    };
    process_mat_task(task).expect("canvas")
}

fn to_image(result: MatResult) -> RgbaImage {
    let c = result.canvas;
    RgbaImage::from_raw(c.width, c.height, c.pixels).unwrap()
}

fn solo_canvas(img: &RgbaImage, params: MatParams) -> RgbaImage {
    to_image(solo_result(img, params))
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
                got.push((
                    format!("{kind:?} {name} {pct}"),
                    fnv1a(solo_canvas(img, p).as_raw()),
                ));
            }
        }
    }
    let p = params(
        mat_option(MattingKind::FixedColor, 6.0, &fixed),
        (320, 180),
        true,
    );
    got.push((
        "fill landscape".into(),
        fnv1a(solo_canvas(&landscape, p).as_raw()),
    ));
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

// ── Pairs (n = 2) ────────────────────────────────────────────────────────────

/// A textured photo with no pixel near black, so a black mat finds its edges.
fn bright(w: u32, h: u32, seed: u32) -> RgbaImage {
    let mut img = textured(w, h, seed);
    for px in img.pixels_mut() {
        for c in 0..3 {
            px[c] |= 0x20;
        }
    }
    img
}

fn pair_canvas(left: &RgbaImage, right: &RgbaImage, params: MatParams) -> RgbaImage {
    to_image(pair_result(left, right, params))
}

fn pair_result(left: &RgbaImage, right: &RgbaImage, params: MatParams) -> MatResult {
    let task = MatTask {
        image: prepared(left, "left.png"),
        mat_kind: Some(params.matting.caption()),
        params,
        priority: false,
        partner: Some(LoadedPartner {
            prepared: prepared(right, "right.png"),
            priority: true,
        }),
    };
    let result = process_mat_task(task).expect("canvas");
    assert_eq!(result.path, std::path::PathBuf::from("left.png"));
    let partner = result.partner.as_ref().expect("a pair result");
    assert_eq!(partner.path, std::path::PathBuf::from("right.png"));
    assert!(partner.priority && !result.priority && result.is_priority());
    assert_eq!(result.windows.len(), 2);
    result
}

/// Rects of the non-black runs on the canvas's middle row, each extended
/// vertically along its middle column: the windows on a black mat.
fn find_windows(canvas: &RgbaImage) -> Vec<photo_set::Window> {
    let is_mat = |x: u32, y: u32| canvas.get_pixel(x, y).0[..3] == [0, 0, 0];
    let mid = canvas.height() / 2;
    let mut runs = Vec::new();
    let mut x = 0;
    while x < canvas.width() {
        if is_mat(x, mid) {
            x += 1;
            continue;
        }
        let start = x;
        while x < canvas.width() && !is_mat(x, mid) {
            x += 1;
        }
        runs.push((start, x));
    }
    runs.into_iter()
        .map(|(x0, x1)| {
            let cx = (x0 + x1) / 2;
            let y0 = (0..canvas.height()).find(|&y| !is_mat(cx, y)).unwrap();
            let y1 = (0..canvas.height())
                .rev()
                .find(|&y| !is_mat(cx, y))
                .unwrap()
                + 1;
            photo_set::Window {
                x: x0,
                y: y0,
                w: x1 - x0,
                h: y1 - y0,
            }
        })
        .collect()
}

fn crop(canvas: &RgbaImage, w: photo_set::Window) -> RgbaImage {
    imageops::crop_imm(canvas, w.x, w.y, w.w, w.h).to_image()
}

const SCREEN: (u32, u32) = (960, 540);

/// **K2 Layout.** Equal heights; widths in aspect ratio ±1 px; the bar is
/// `round(r·H_c·m)` ±1 px at 2%, 3.5% and 6%; centred; and each window is
/// exactly the member's solo height (C2).
#[test]
fn k2_pair_layout_bar_and_solo_height() {
    let dir = tempfile::tempdir().unwrap();
    let fixed = fixed_background(&dir);
    let left = bright(750, 1000, 3); // 3:4
    let right = bright(600, 1000, 4); // 3:5
    for pct in [2.0f32, 3.5, 6.0] {
        let matting = mat_option(MattingKind::FixedColor, pct, &fixed);
        let canvas = pair_canvas(&left, &right, params(matting.clone(), SCREEN, false));
        let windows = find_windows(&canvas);
        assert_eq!(windows.len(), 2, "{pct}%: {windows:?}");
        let (l, r) = (windows[0], windows[1]);
        assert_eq!(l.h, r.h, "{pct}%: equal heights");
        assert_eq!(l.y, r.y, "{pct}%: aligned");
        let h = l.h as f32;
        assert!((l.w as f32 - h * 0.75).abs() <= 1.0, "{pct}%: left {l:?}");
        assert!((r.w as f32 - h * 0.6).abs() <= 1.0, "{pct}%: right {r:?}");
        let bar = r.x - (l.x + l.w);
        let expected = (0.618 * SCREEN.1 as f32 * pct / 100.0).round();
        assert!(
            (bar as f32 - expected).abs() <= 1.0,
            "{pct}%: bar {bar} vs {expected}"
        );
        let (left_margin, right_margin) = (l.x, SCREEN.0 - (r.x + r.w));
        assert!(left_margin.abs_diff(right_margin) <= 1, "{pct}%: centred");
        let (top, bottom) = (l.y, SCREEN.1 - (l.y + l.h));
        assert!(top.abs_diff(bottom) <= 1, "{pct}%: centred vertically");
        // C2: each member is drawn at exactly its solo height.
        for member in [&left, &right] {
            let solo = solo_canvas(member, params(matting.clone(), SCREEN, false));
            let solo_window = find_windows(&solo);
            assert_eq!(solo_window.len(), 1);
            assert_eq!(
                solo_window[0].h, l.h,
                "{pct}%: window height == solo height"
            );
        }
    }
}

/// The rule's reason (R2): a low-resolution portrait is not pairable, and
/// paired anyway it would shrink its taller partner below its solo height.
#[test]
fn k2_low_resolution_fixture_is_excluded_because_pairing_would_shrink() {
    use crate::tasks::manager::pairing::PairingRules;
    let dir = tempfile::tempdir().unwrap();
    let fixed = fixed_background(&dir);
    let low = bright(300, 400, 5);
    let tall = bright(750, 1000, 6);
    let rules = PairingRules {
        probability: 1.0,
        bar_ratio: 0.618,
        max_upscale: 1.0,
        m_min: 0.02,
        m_max: 0.02,
    };
    assert!(!rules.is_pairable(Some(low.dimensions()), Some(SCREEN)));
    assert!(rules.is_pairable(Some(tall.dimensions()), Some(SCREEN)));
    let matting = mat_option(MattingKind::FixedColor, 2.0, &fixed);
    let paired = find_windows(&pair_canvas(
        &tall,
        &low,
        params(matting.clone(), SCREEN, false),
    ));
    let solo = find_windows(&solo_canvas(&tall, params(matting, SCREEN, false)));
    assert!(paired[0].h < solo[0].h, "{paired:?} vs {solo:?}");
}

/// **K3 Bar (fixed colour).** Every pixel outside the two windows is exactly
/// the mat colour, the bar included.
#[test]
fn k3_fixed_color_bar_is_exactly_the_mat() {
    let dir = tempfile::tempdir().unwrap();
    let fixed = fixed_background(&dir);
    let matting = mat_option(MattingKind::FixedColor, 6.0, &fixed);
    let canvas = pair_canvas(
        &bright(750, 1000, 7),
        &bright(667, 1000, 8),
        params(matting, SCREEN, false),
    );
    let windows = find_windows(&canvas);
    assert_eq!(windows.len(), 2);
    let inside = |x: u32, y: u32| {
        windows
            .iter()
            .any(|w| x >= w.x && x < w.x + w.w && y >= w.y && y < w.y + w.h)
    };
    for (x, y, px) in canvas.enumerate_pixels() {
        if !inside(x, y) {
            assert_eq!(px.0, [0, 0, 0, 255], "({x}, {y})");
        }
    }
}

/// **K3 Bar (studio).** The bevel is clamped to half the bar (R5): any bevel
/// wider than that renders exactly as a bevel of bar/2.
#[test]
fn k3_studio_bevel_is_clamped_to_half_the_bar() {
    let a = bright(50, 60, 9);
    let b = bright(50, 60, 10);
    let left = photo_set::Window {
        x: 40,
        y: 20,
        w: 50,
        h: 60,
    };
    let right = photo_set::Window { x: 100, ..left };
    let windows = [(left, &a), (right, &b)];
    let render = |bevel: f32| {
        render_studio_mat(
            200,
            100,
            &windows,
            [0.5, 0.4, 0.3],
            bevel,
            [255, 255, 255],
            1.0,
            5.6,
            5.2,
        )
    };
    let half_bar = render(5.0);
    assert_eq!(render(30.0), half_bar, "bar = 10 px: the bevel stops at 5");
    assert_ne!(render(3.0), half_bar);
    // The two bevels meet in the middle of the bar and do not overlap: each
    // half is shaded as its own window's bevel at full width.
    let solo = |win: photo_set::Window, img: &RgbaImage| {
        render_studio_mat(
            200,
            100,
            &[(win, img)],
            [0.5, 0.4, 0.3],
            5.0,
            [255, 255, 255],
            1.0,
            5.6,
            5.2,
        )
    };
    let (left_only, right_only) = (solo(left, &a), solo(right, &b));
    for y in 25..75 {
        for x in 90..95 {
            assert_eq!(
                half_bar.get_pixel(x, y),
                left_only.get_pixel(x, y),
                "({x}, {y})"
            );
        }
        for x in 95..100 {
            assert_eq!(
                half_bar.get_pixel(x, y),
                right_only.get_pixel(x, y),
                "({x}, {y})"
            );
        }
    }
}

/// **K3 Bar (drop shadow).** The shadow is the union's: the maximum alpha of
/// the windows' shadows (on white with a black shadow, the darker pixel).
#[test]
fn k3_drop_shadow_is_the_union() {
    let shadow = || DropShadowParams {
        color: [0, 0, 0],
        opacity: 1.0,
        blur_px: 15.0,
        offset_px: [6, 6],
    };
    let white = || RgbaImage::from_pixel(200, 100, Rgba([255, 255, 255, 255]));
    let w1 = photo_set::Window {
        x: 20,
        y: 20,
        w: 50,
        h: 40,
    };
    let w2 = photo_set::Window { x: 80, ..w1 };
    let mut union = white();
    render_drop_shadow(&mut union, &[w1, w2], shadow());
    let mut a = white();
    render_drop_shadow(&mut a, &[w1], shadow());
    let mut b = white();
    render_drop_shadow(&mut b, &[w2], shadow());
    for (x, y, px) in union.enumerate_pixels() {
        let darker = a.get_pixel(x, y).0[0].min(b.get_pixel(x, y).0[0]);
        assert_eq!(px.0[0], darker, "({x}, {y})");
    }
}

/// **K4.** Overlay kinds draw each member resized once, straight to its
/// window; studio windows equal the solo studio render at that rect.
#[test]
fn k4_windows_hold_the_members_resized_once() {
    let dir = tempfile::tempdir().unwrap();
    let fixed = fixed_background(&dir);
    let left = bright(750, 1000, 11);
    let right = bright(600, 1000, 12);
    let fixed_color = mat_option(MattingKind::FixedColor, 6.0, &fixed);
    let windows = find_windows(&pair_canvas(
        &left,
        &right,
        params(fixed_color, SCREEN, false),
    ));
    for kind in [
        MattingKind::FixedColor,
        MattingKind::Gradient,
        MattingKind::Vignette,
        MattingKind::FixedImage,
        MattingKind::DropShadow,
        MattingKind::Blur,
        MattingKind::CinematicBlur,
    ] {
        let canvas = pair_canvas(
            &left,
            &right,
            params(mat_option(kind, 6.0, &fixed), SCREEN, false),
        );
        for (member, window) in [(&left, windows[0]), (&right, windows[1])] {
            let expected =
                imageops::resize(member, window.w, window.h, imageops::FilterType::Triangle);
            assert!(crop(&canvas, window) == expected, "{kind:?}: {window:?}");
        }
    }
    let mut compared = 0;
    for kind in [MattingKind::Studio, MattingKind::PassePartout] {
        let matting = mat_option(kind, 6.0, &fixed);
        let pair = pair_result(&left, &right, params(matting.clone(), SCREEN, false));
        let pair_windows = pair.windows.clone();
        let pair = to_image(pair);
        for (index, member) in [&left, &right].into_iter().enumerate() {
            let solo = solo_result(member, params(matting.clone(), SCREEN, false));
            let solo_window = solo.windows[0];
            let window = pair_windows[index];
            assert_eq!(window.h, solo_window.h, "{kind:?}: solo height");
            assert!(
                window.w.abs_diff(solo_window.w) <= 1,
                "{kind:?}: {window:?} {solo_window:?}"
            );
            if window.w == solo_window.w {
                let solo = to_image(solo);
                assert!(
                    crop(&pair, window) == crop(&solo, solo_window),
                    "{kind:?}: window != solo render"
                );
                compared += 1;
            }
        }
    }
    assert!(
        compared >= 2,
        "only {compared} studio windows had their solo width"
    );
}

/// **K5.** The blurred background samples the members abutted with no gap,
/// so it is continuous across the bar (no stripe, no fringe).
#[test]
fn k5_blur_background_is_continuous_across_the_bar() {
    let dir = tempfile::tempdir().unwrap();
    let fixed = fixed_background(&dir);
    let left = bright(750, 1000, 13);
    let right = bright(600, 1000, 14);
    let windows = find_windows(&pair_canvas(
        &left,
        &right,
        params(
            mat_option(MattingKind::FixedColor, 6.0, &fixed),
            SCREEN,
            false,
        ),
    ));
    let canvas = pair_canvas(
        &left,
        &right,
        params(mat_option(MattingKind::Blur, 6.0, &fixed), SCREEN, false),
    );
    let (bar_x0, bar_x1) = (windows[0].x + windows[0].w, windows[1].x);
    assert!(bar_x1 > bar_x0 + 2);
    for y in (windows[0].y..windows[0].y + windows[0].h).step_by(97) {
        for x in bar_x0..bar_x1 - 1 {
            let (p, q) = (canvas.get_pixel(x, y), canvas.get_pixel(x + 1, y));
            for c in 0..3 {
                assert!(
                    p[c].abs_diff(q[c]) <= 3,
                    "({x}, {y}) channel {c}: {p:?} {q:?}"
                );
            }
        }
    }
}

/// **K6.** A pair matted from DCT-scaled decodes matches the one from full
/// decodes within D3's tolerance (`decoded_suffices` runs per member).
#[test]
fn k6_scaled_decodes_match_full_decodes_for_pairs() {
    use crate::tasks::loader::scaled_decode_tests::{Flavor, photo_jpeg, psnr_rgb};
    use crate::tasks::loader::{DecodeTarget, decode_rgba8_apply_exif};

    let dir = tempfile::tempdir().unwrap();
    let fixed = fixed_background(&dir);
    let screen = (240u32, 135u32);
    let (canvas_w, canvas_h) = compute_canvas_size(screen.0, screen.1, 1.0, 4096);
    let mut paths = Vec::new();
    for (name, w, h, orientation) in [("l.jpg", 499, 375, 6u16), ("r.jpg", 377, 601, 1)] {
        let path = dir.path().join(name);
        std::fs::write(&path, photo_jpeg(w, h, orientation, Flavor::Baseline420)).unwrap();
        paths.push(path);
    }
    let decode = |path: &std::path::Path, scaled: bool| {
        let target = scaled.then_some(DecodeTarget {
            canvas_w,
            canvas_h,
            cover: false,
            bg_sample_scale: Some(0.125),
        });
        let d = decode_rgba8_apply_exif(path, target).unwrap();
        PreparedImageCpu {
            path: path.to_path_buf(),
            width: d.image.width(),
            height: d.image.height(),
            source_size: d.source_size,
            pixels: d.image.into_raw(),
        }
    };
    for kind in [
        MattingKind::Studio,
        MattingKind::Blur,
        MattingKind::DropShadow,
    ] {
        let render = |scaled: bool| {
            let mut p = params(mat_option(kind, 6.0, &fixed), screen, false);
            p.max_upscale_factor = 2.0;
            let left = decode(&paths[0], scaled);
            if scaled {
                assert!(left.width < left.source_size.0, "expected a reduced decode");
            }
            let task = MatTask {
                image: left,
                mat_kind: None,
                params: p,
                priority: false,
                partner: Some(LoadedPartner {
                    prepared: decode(&paths[1], scaled),
                    priority: false,
                }),
            };
            let c = process_mat_task(task).unwrap().canvas;
            RgbaImage::from_raw(c.width, c.height, c.pixels).unwrap()
        };
        let psnr = psnr_rgb(&render(false), &render(true));
        assert!(psnr > 44.0, "{kind:?}: PSNR {psnr:.1} dB");
    }
}
