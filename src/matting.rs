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
    /// Minimum mat size as fraction of the shorter screen dimension.
    pub min_fraction: f32,
}

impl Default for MattingConfig {
    fn default() -> Self {
        Self {
            mode: MatMode::FixedColor,
            color: [0, 0, 0],
            min_fraction: 0.0,
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

fn apply_bevel(img: &mut image::RgbaImage) {
    let w = img.width();
    let h = img.height();
    let bev = ((w.min(h) as f32) * 0.03).max(1.0) as u32;
    for x in 0..w {
        for y in 0..bev {
            lighten(img.get_pixel_mut(x, y), 20);
        }
    }
    for y in 0..h {
        for x in 0..bev {
            lighten(img.get_pixel_mut(x, y), 20);
        }
    }
    for x in 0..w {
        for y in h - bev..h {
            darken(img.get_pixel_mut(x, y), 20);
        }
    }
    for y in 0..h {
        for x in w - bev..w {
            darken(img.get_pixel_mut(x, y), 20);
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
    use image::imageops::{blur, overlay, resize, FilterType};
    use image::{Rgba, RgbaImage};
    let mode = select_mode(cfg, rng);
    let mut bg = match mode {
        MatMode::FixedColor => RgbaImage::from_pixel(
            screen_w,
            screen_h,
            Rgba([cfg.color[0], cfg.color[1], cfg.color[2], 255]),
        ),
        MatMode::Blur => {
            let resized = resize(img, screen_w, screen_h, FilterType::Triangle);
            blur(&resized, 50.0)
        }
        MatMode::Studio => {
            let c = average_color(img);
            let mut base = RgbaImage::from_pixel(screen_w, screen_h, Rgba([c[0], c[1], c[2], 255]));
            apply_bevel(&mut base);
            base
        }
        MatMode::Random => unreachable!(),
    };
    let min_border = (cfg.min_fraction.max(0.0) * (screen_w.min(screen_h) as f32)).round() as u32;
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
    fn select_mode_random_is_deterministic() {
        let cfg = MattingConfig {
            mode: MatMode::Random,
            ..Default::default()
        };
        let mut rng = rand::rngs::StdRng::seed_from_u64(1);
        let first = select_mode(&cfg, &mut rng);
        let second = select_mode(&cfg, &mut rng);
        assert_ne!(
            std::mem::discriminant(&first),
            std::mem::discriminant(&second)
        );
    }

    #[test]
    fn compose_fixed_color_places_image() {
        let cfg = MattingConfig {
            mode: MatMode::FixedColor,
            color: [1, 2, 3],
            min_fraction: 0.1,
        };
        let mut rng = rand::rngs::StdRng::seed_from_u64(2);
        let img = image::RgbaImage::from_pixel(100, 50, image::Rgba([10, 10, 10, 255]));
        let res = compose(&img, 200, 200, &cfg, &mut rng);
        assert_eq!(&res.get_pixel(0, 0).0[0..3], &[1, 2, 3]);
    }
}
