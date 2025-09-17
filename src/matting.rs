use rand::Rng;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MatMode {
    FixedColor,
    Studio,
    Blur,
    Random,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct MattingConfig {
    pub mode: MatMode,
    /// RGB color used when `mode` is `fixed-color`.
    pub color: [u8; 3],
    /// Minimum mat border as a percentage of the shorter screen dimension.
    /// Example: `2.0` reserves at least 2% of the smaller screen side for the mat border.
    pub minimum_border_percentage: f32,
}

impl Default for MattingConfig {
    fn default() -> Self {
        Self {
            mode: MatMode::FixedColor,
            color: [0, 0, 0],
            minimum_border_percentage: 0.0,
        }
    }
}

pub fn select_mode(cfg: &MattingConfig, rng: &mut impl Rng) -> MatMode {
    match cfg.mode {
        MatMode::Random => {
            let modes = [MatMode::FixedColor, MatMode::Studio, MatMode::Blur];
            modes[rng.gen_range(0..modes.len())].clone()
        }
        ref m => m.clone(),
    }
}

fn average_color(img: &image::RgbaImage) -> [u8; 3] {
    let mut r: u64 = 0;
    let mut g: u64 = 0;
    let mut b: u64 = 0;
    let mut n: u64 = 0;
    for p in img.pixels() {
        r += p[0] as u64;
        g += p[1] as u64;
        b += p[2] as u64;
        n += 1;
    }
    if n == 0 {
        return [0, 0, 0];
    }
    [(r / n) as u8, (g / n) as u8, (b / n) as u8]
}

fn lighten(px: &mut image::Rgba<u8>, amt: u8) {
    px[0] = px[0].saturating_add(amt);
    px[1] = px[1].saturating_add(amt);
    px[2] = px[2].saturating_add(amt);
}

fn darken(px: &mut image::Rgba<u8>, amt: u8) {
    px[0] = px[0].saturating_sub(amt);
    px[1] = px[1].saturating_sub(amt);
    px[2] = px[2].saturating_sub(amt);
}

fn add_linen_texture(img: &mut image::RgbaImage) {
    let (w, h) = img.dimensions();
    for y in 0..h {
        for x in 0..w {
            if x % 4 == 0 {
                lighten(img.get_pixel_mut(x, y), 3);
            }
            if y % 4 == 0 {
                darken(img.get_pixel_mut(x, y), 3);
            }
        }
    }
}

fn apply_inner_bevel(img: &mut image::RgbaImage, x: u32, y: u32, w: u32, h: u32, base: [u8; 3]) {
    let bev = ((w.min(h) as f32) * 0.03).max(1.0) as u32;
    let mut light = image::Rgba([base[0], base[1], base[2], 255]);
    lighten(&mut light, 20);
    let mut dark = image::Rgba([base[0], base[1], base[2], 255]);
    darken(&mut dark, 20);
    for xx in x..x + w {
        for yy in y..y + bev {
            img.put_pixel(xx, yy, light);
        }
        for yy in y + h - bev..y + h {
            img.put_pixel(xx, yy, dark);
        }
    }
    for yy in y..y + h {
        for xx in x..x + bev {
            img.put_pixel(xx, yy, light);
        }
        for xx in x + w - bev..x + w {
            img.put_pixel(xx, yy, dark);
        }
    }
}

pub fn compose(
    img: &image::RgbaImage,
    screen_w: u32,
    screen_h: u32,
    cfg: &MattingConfig,
    rng: &mut impl Rng,
) -> image::RgbaImage {
    use image::imageops::{blur, crop_imm, overlay, resize, FilterType};
    use image::{Rgba, RgbaImage};
    let mode = select_mode(cfg, rng);
    let mut bg = match mode {
        MatMode::FixedColor => RgbaImage::from_pixel(
            screen_w,
            screen_h,
            Rgba([cfg.color[0], cfg.color[1], cfg.color[2], 255]),
        ),
        MatMode::Blur => {
            // Scale to cover the screen without distorting the aspect ratio
            let scale =
                (screen_w as f32 / img.width() as f32).max(screen_h as f32 / img.height() as f32);
            let bw = (img.width() as f32 * scale).ceil().max(1.0) as u32;
            let bh = (img.height() as f32 * scale).ceil().max(1.0) as u32;
            let resized = resize(img, bw, bh, FilterType::Triangle);
            let x = (bw - screen_w) / 2;
            let y = (bh - screen_h) / 2;
            let cropped = crop_imm(&resized, x, y, screen_w, screen_h).to_image();
            blur(&cropped, 15.0)
        }
        MatMode::Studio => {
            let c = average_color(img);
            let mut base = RgbaImage::from_pixel(screen_w, screen_h, Rgba([c[0], c[1], c[2], 255]));
            add_linen_texture(&mut base);
            base
        }
        MatMode::Random => unreachable!(),
    };
    let min_border = ((cfg.minimum_border_percentage.max(0.0) / 100.0)
        * (screen_w.min(screen_h) as f32))
        .round() as u32;
    let avail_w = screen_w.saturating_sub(min_border * 2).max(1);
    let avail_h = screen_h.saturating_sub(min_border * 2).max(1);
    let sw = (avail_w as f32) / (img.width() as f32);
    let sh = (avail_h as f32) / (img.height() as f32);
    let s = sw.min(sh).min(1.0);
    let dest_w = ((img.width() as f32) * s).floor().max(1.0) as u32;
    let dest_h = ((img.height() as f32) * s).floor().max(1.0) as u32;
    let dx = ((screen_w - dest_w) / 2) as i64;
    let dy = ((screen_h - dest_h) / 2) as i64;
    let scaled = resize(img, dest_w, dest_h, FilterType::Triangle);
    overlay(&mut bg, &scaled, dx, dy);
    if let MatMode::Studio = mode {
        apply_inner_bevel(
            &mut bg,
            dx as u32,
            dy as u32,
            dest_w,
            dest_h,
            average_color(img),
        );
    }
    bg
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    #[test]
    fn average_color_basic() {
        let img = image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255]));
        assert_eq!(average_color(&img), [10, 20, 30]);
    }

    #[test]
    fn select_mode_random_with_seed() {
        let cfg = MattingConfig {
            mode: MatMode::Random,
            ..Default::default()
        };
        let mut rng_studio = rand::rngs::StdRng::seed_from_u64(42);
        let first = select_mode(&cfg, &mut rng_studio);
        assert!(matches!(first, MatMode::Studio));

        let mut rng_fixed = rand::rngs::StdRng::seed_from_u64(10);
        let second = select_mode(&cfg, &mut rng_fixed);
        assert!(matches!(second, MatMode::FixedColor));

        let mut rng_blur = rand::rngs::StdRng::seed_from_u64(1);
        let third = select_mode(&cfg, &mut rng_blur);
        assert!(matches!(third, MatMode::Blur));
    }

    #[test]
    fn compose_fixed_color_places_image() {
        let cfg = MattingConfig {
            mode: MatMode::FixedColor,
            color: [1, 2, 3],
            minimum_border_percentage: 2.0,
        };
        let mut rng = rand::rngs::StdRng::seed_from_u64(2);
        let img = image::RgbaImage::from_pixel(100, 50, image::Rgba([10, 10, 10, 255]));
        let res = compose(&img, 200, 200, &cfg, &mut rng);
        assert_eq!(&res.get_pixel(0, 0).0[0..3], &[1, 2, 3]);
    }

    #[test]
    fn studio_bevel_overlays_photo_edges() {
        let cfg = MattingConfig {
            mode: MatMode::Studio,
            minimum_border_percentage: 0.0,
            ..Default::default()
        };
        let mut rng = rand::rngs::StdRng::seed_from_u64(3);
        let img = image::RgbaImage::from_pixel(50, 50, image::Rgba([100, 100, 100, 255]));
        let res = compose(&img, 100, 100, &cfg, &mut rng);
        // Bevel should cover top-left corner with lighter mat color
        let top_left = res.get_pixel(25, 25);
        assert!(top_left[0] > 100);
    }
}
