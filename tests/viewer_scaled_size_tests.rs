use rust_photo_frame::tasks::viewer::compute_scaled_size;

fn assert_aspect_preserved(w0: u32, h0: u32, w1: u32, h1: u32) {
    // Compare ratios within a small epsilon
    let r0 = (w0 as f32) / (h0 as f32);
    let r1 = (w1 as f32) / (h1 as f32);
    assert!((r0 - r1).abs() < 0.01, "aspect changed: {} vs {}", r0, r1);
}

#[test]
fn landscape_large_on_1080p_with_max2048() {
    let (out_w, out_h) = compute_scaled_size(4032, 3024, 1920, 1080, 1.0, 2048);
    assert_eq!((out_w, out_h), (1440, 1080));
    assert!(out_w <= 2048 && out_h <= 2048);
    assert_aspect_preserved(4032, 3024, out_w, out_h);
}

#[test]
fn portrait_large_on_1080x1920_with_max2048() {
    let (out_w, out_h) = compute_scaled_size(3024, 4032, 1080, 1920, 1.0, 2048);
    assert_eq!((out_w, out_h), (1080, 1440));
    assert!(out_w <= 2048 && out_h <= 2048);
    assert_aspect_preserved(3024, 4032, out_w, out_h);
}

#[test]
fn already_small_no_upscale() {
    let (out_w, out_h) = compute_scaled_size(800, 600, 1920, 1080, 1.0, 4096);
    assert_eq!((out_w, out_h), (800, 600));
    assert_aspect_preserved(800, 600, out_w, out_h);
}

