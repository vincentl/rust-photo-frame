use rand::SeedableRng;
use rust_photo_frame::matting::{compose, MatMode, MattingConfig};

#[test]
fn random_mode_yields_different_styles() {
    let cfg = MattingConfig {
        mode: MatMode::Random,
        ..Default::default()
    };
    let img = image::RgbaImage::from_pixel(10, 10, image::Rgba([128, 64, 32, 255]));
    let mut rng1 = rand::rngs::StdRng::seed_from_u64(7);
    let mut rng2 = rand::rngs::StdRng::seed_from_u64(8);
    let first = compose(&img, 50, 50, &cfg, &mut rng1);
    let second = compose(&img, 50, 50, &cfg, &mut rng2);
    assert_ne!(first.as_raw(), second.as_raw());
}
