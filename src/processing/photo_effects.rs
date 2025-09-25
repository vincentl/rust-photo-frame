use image::{imageops, RgbaImage};

use crate::config::{
    AdaptiveSharpeningConfig, LightFalloffConfig, PaperSimulationConfig, PhotoEffectsConfig,
};

pub fn apply_photo_effects(image: &mut RgbaImage, cfg: &PhotoEffectsConfig, oversample: f32) {
    if let Some(paper) = cfg.paper_simulation() {
        apply_paper_simulation(image, paper, oversample);
    }
    if let Some(vignette) = cfg.light_falloff() {
        apply_light_falloff(image, vignette);
    }
    if let Some(softening) = cfg.adaptive_sharpening() {
        apply_adaptive_softening(image, softening);
    }
}

fn apply_paper_simulation(image: &mut RgbaImage, cfg: &PaperSimulationConfig, oversample: f32) {
    let strength = cfg.strength().max(0.0);
    if strength <= f32::EPSILON {
        return;
    }
    let scale = cfg.texture_period_px().max(1.0) * oversample.max(0.01);
    let seed = cfg.seed();
    let mut noise = SmoothNoise::new(scale, seed);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let factor = 1.0 - strength + strength * noise.sample(x, y);
        for channel in &mut pixel.0[..3] {
            let v = (*channel as f32) * factor;
            *channel = v.clamp(0.0, 255.0) as u8;
        }
    }
}

fn apply_light_falloff(image: &mut RgbaImage, cfg: &LightFalloffConfig) {
    let strength = cfg.strength().max(0.0);
    if strength <= f32::EPSILON {
        return;
    }
    let width = image.width() as f32;
    let height = image.height() as f32;
    if width <= 0.0 || height <= 0.0 {
        return;
    }
    let center_x = (width - 1.0) * 0.5;
    let center_y = (height - 1.0) * 0.5;
    let half_min = 0.5 * width.min(height).max(1.0);
    let inner = (cfg.radius() * half_min).max(0.0);
    let falloff = (cfg.softness() * half_min).max(1.0);
    let outer = (inner + falloff).max(inner + 1.0);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let dx = x as f32 - center_x;
        let dy = y as f32 - center_y;
        let dist = f32::sqrt(dx * dx + dy * dy);
        let mut factor = 1.0;
        if dist > inner {
            let t = ((dist - inner) / (outer - inner)).clamp(0.0, 1.0);
            let eased = smoothstep(t);
            factor = 1.0 - strength * eased;
        }
        for channel in &mut pixel.0[..3] {
            let v = (*channel as f32) * factor;
            *channel = v.clamp(0.0, 255.0) as u8;
        }
    }
}

fn apply_adaptive_softening(image: &mut RgbaImage, cfg: &AdaptiveSharpeningConfig) {
    let sigma = cfg.effective_sigma(image.width(), image.height());
    if sigma <= f32::EPSILON {
        return;
    }
    let blurred = imageops::blur(image, sigma);
    *image = blurred;
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

struct SmoothNoise {
    scale: f32,
    seed: u64,
}

impl SmoothNoise {
    fn new(scale: f32, seed: u64) -> Self {
        Self { scale, seed }
    }

    fn sample(&mut self, x: u32, y: u32) -> f32 {
        let fx = (x as f32) / self.scale;
        let fy = (y as f32) / self.scale;
        let x0 = fx.floor() as i32;
        let y0 = fy.floor() as i32;
        let tx = fx - x0 as f32;
        let ty = fy - y0 as f32;
        let tx = smoothstep(tx.clamp(0.0, 1.0));
        let ty = smoothstep(ty.clamp(0.0, 1.0));
        let n00 = hashed_noise(x0, y0, self.seed);
        let n10 = hashed_noise(x0 + 1, y0, self.seed);
        let n01 = hashed_noise(x0, y0 + 1, self.seed);
        let n11 = hashed_noise(x0 + 1, y0 + 1, self.seed);
        let nx0 = lerp(n00, n10, tx);
        let nx1 = lerp(n01, n11, tx);
        lerp(nx0, nx1, ty)
    }
}

fn hashed_noise(x: i32, y: i32, seed: u64) -> f32 {
    let mut v = x as u64;
    v = v.wrapping_mul(0x9E37_79B1_85EB_CA87);
    v ^= (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    v ^= seed.wrapping_mul(0x1656_67B1_9E37_79F9);
    v ^= v >> 33;
    v = v.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    v ^= v >> 33;
    ((v as u32) as f32) / (u32::MAX as f32)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
