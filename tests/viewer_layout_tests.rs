use rust_photo_frame::events::MatMode;
use rust_photo_frame::tasks::viewer::compute_dest_rect;

fn rect_close(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32), eps: f32) {
    assert!((a.0 - b.0).abs() <= eps, "x mismatch: {:?} vs {:?}", a, b);
    assert!((a.1 - b.1).abs() <= eps, "y mismatch: {:?} vs {:?}", a, b);
    assert!((a.2 - b.2).abs() <= eps, "w mismatch: {:?} vs {:?}", a, b);
    assert!((a.3 - b.3).abs() <= eps, "h mismatch: {:?} vs {:?}", a, b);
}

#[test]
fn letterbox_square_on_16x9() {
    // 1000x1000 image on 1920x1080 screen
    let rect = compute_dest_rect(1000, 1000, 1920, 1080, &MatMode::LetterboxBlack);
    // scale = min(1920/1000=1.92, 1080/1000=1.08) = 1.08
    // w = 1080, h = 1080, x = (1920-1080)/2 = 420, y = 0
    rect_close(rect, (420.0, 0.0, 1080.0, 1080.0), 0.001);
}

#[test]
fn letterbox_wide_on_16x9() {
    // 4000x2000 (2:1) on 1920x1080
    let rect = compute_dest_rect(4000, 2000, 1920, 1080, &MatMode::LetterboxBlack);
    // scale = min(1920/4000=0.48, 1080/2000=0.54) = 0.48
    // w = 1920, h = 960, x = 0, y = (1080-960)/2 = 60
    rect_close(rect, (0.0, 60.0, 1920.0, 960.0), 0.001);
}

#[test]
fn studio_mat_matches_letterbox() {
    let img = (3000, 2000);
    let screen = (1920, 1080);
    let lb = compute_dest_rect(img.0, img.1, screen.0, screen.1, &MatMode::LetterboxBlack);
    let studio = compute_dest_rect(
        img.0,
        img.1,
        screen.0,
        screen.1,
        &MatMode::StudioMat { min_border_px: 48, color_rgb: (32, 32, 32) },
    );
    rect_close(lb, studio, 0.001);
}

#[test]
fn blurred_bg_matches_letterbox() {
    let img = (3000, 2000);
    let screen = (1920, 1080);
    let lb = compute_dest_rect(img.0, img.1, screen.0, screen.1, &MatMode::LetterboxBlack);
    let blur = compute_dest_rect(
        img.0,
        img.1,
        screen.0,
        screen.1,
        &MatMode::BlurredBackground { strength: 4.0, dim: 0.25 },
    );
    rect_close(lb, blur, 0.001);
}

