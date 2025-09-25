use crate::config::{
    ClassicSepiaOptions, CleanBoostOptions, CrossProcessSimOptions, FilterMode, FilterOptions,
    HighKeyBlackWhiteOptions, KodachromePunchOptions, LomoPopOptions, MattePortraitOptions,
    PolaroidFadeOptions,
};
use image::{imageops, Rgba, RgbaImage};
use palette::{FromColor, Hsl, Hsv, Lab, LinSrgb, RgbHue, Srgb};

pub fn apply_filter(image: &mut RgbaImage, options: &FilterOptions, seed: u64) {
    if image.width() == 0 || image.height() == 0 {
        return;
    }
    if options.amount() <= f32::EPSILON {
        return;
    }
    let original = image.clone();
    match options.mode() {
        FilterMode::ClassicSepia(cfg) => classic_sepia(image, &original, cfg),
        FilterMode::PolaroidFade(cfg) => polaroid_fade(image, &original, cfg),
        FilterMode::KodachromePunch(cfg) => kodachrome_punch(image, &original, cfg),
        FilterMode::HighKeyBlackWhite(cfg) => high_key_bw(image, &original, cfg, seed),
        FilterMode::MattePortrait(cfg) => matte_portrait(image, &original, cfg),
        FilterMode::LomoPop(cfg) => lomo_pop(image, &original, cfg, seed),
        FilterMode::CrossProcessSim(cfg) => cross_process(image, &original, cfg),
        FilterMode::CleanBoost(cfg) => clean_boost(image, &original, cfg),
    }
}

fn classic_sepia(dest: &mut RgbaImage, original: &RgbaImage, cfg: &ClassicSepiaOptions) {
    let amount = cfg.amount.clamp(0.0, 1.0);
    if amount <= f32::EPSILON {
        return;
    }
    let tone_strength = cfg.tone_strength.clamp(0.0, 1.0);
    let vignette_strength = cfg.vignette_strength.clamp(0.0, 1.0);
    let width = dest.width().max(1) as f32;
    let height = dest.height().max(1) as f32;
    for (x, y, pixel) in dest.enumerate_pixels_mut() {
        let source = original.get_pixel(x, y);
        let (orig_rgb, alpha) = pixel_to_rgb_alpha(source);
        let lin = srgb_to_linear(orig_rgb);
        let mut sepia = [
            lin[0] * 0.393 + lin[1] * 0.769 + lin[2] * 0.189,
            lin[0] * 0.349 + lin[1] * 0.686 + lin[2] * 0.168,
            lin[0] * 0.272 + lin[1] * 0.534 + lin[2] * 0.131,
        ];
        for value in &mut sepia {
            *value = tone_curve((*value).clamp(0.0, 1.0), tone_strength);
        }
        let sepia_rgb = linear_to_srgb(sepia);
        let radius = vignette_radius(x, y, width, height);
        let vignette_factor = (1.0 - vignette_strength * radius.powf(1.35)).clamp(0.0, 1.0);
        let sepia_rgb = sepia_rgb.map(|c| (c * vignette_factor).clamp(0.0, 1.0));
        let mixed = mix_rgb(orig_rgb, sepia_rgb, amount);
        *pixel = rgb_alpha_to_pixel(mixed, alpha);
    }
}

fn polaroid_fade(dest: &mut RgbaImage, original: &RgbaImage, cfg: &PolaroidFadeOptions) {
    let amount = cfg.amount.clamp(0.0, 1.0);
    if amount <= f32::EPSILON {
        return;
    }
    let vignette_strength = cfg.vignette_strength.clamp(0.0, 1.0);
    let width = dest.width().max(1) as f32;
    let height = dest.height().max(1) as f32;
    for (x, y, pixel) in dest.enumerate_pixels_mut() {
        let source = original.get_pixel(x, y);
        let (orig_rgb, alpha) = pixel_to_rgb_alpha(source);
        let srgb = Srgb::new(orig_rgb[0], orig_rgb[1], orig_rgb[2]);
        let lin: LinSrgb<f32> = srgb.into_linear();
        let mut lab: Lab = Lab::from_color(lin);
        let l_norm = (lab.l / 100.0).clamp(0.0, 1.0);
        let shadow_weight = (1.0 - l_norm).powf(1.3);
        let highlight_weight = l_norm.powf(1.3);
        lab.b -= cfg.shadow_cool * shadow_weight;
        lab.b += cfg.highlight_warm * highlight_weight;
        let matte = matte_curve(l_norm, cfg.matte_strength);
        lab.l = (matte * 100.0).clamp(0.0, 100.0);
        let lin: LinSrgb<f32> = LinSrgb::from_color(lab);
        let mut rgb = linear_to_srgb([lin.red, lin.green, lin.blue]);
        let radius = vignette_radius(x, y, width, height);
        let vignette_factor = (1.0 - vignette_strength * radius.powf(1.6)).clamp(0.0, 1.0);
        rgb = rgb.map(|c| (c * vignette_factor).clamp(0.0, 1.0));
        let mixed = mix_rgb(orig_rgb, rgb, amount);
        *pixel = rgb_alpha_to_pixel(mixed, alpha);
    }
}

fn kodachrome_punch(dest: &mut RgbaImage, original: &RgbaImage, cfg: &KodachromePunchOptions) {
    let amount = cfg.amount.clamp(0.0, 1.0);
    if amount <= f32::EPSILON {
        return;
    }
    let width = dest.width();
    let height = dest.height();
    let mut adjusted = vec![[0.0f32; 3]; (width as usize) * (height as usize)];
    for (x, y, _) in dest.enumerate_pixels() {
        let idx = (y as usize) * (width as usize) + (x as usize);
        let source = original.get_pixel(x, y);
        let (orig_rgb, _) = pixel_to_rgb_alpha(source);
        let mut lin = srgb_to_linear(orig_rgb);
        for channel in &mut lin {
            *channel = filmic_curve(*channel, cfg.contrast);
        }
        let mut rgb = linear_to_srgb(lin);
        let mut hsv = Hsv::from_color(Srgb::new(rgb[0], rgb[1], rgb[2]));
        hsv.saturation = (hsv.saturation * (1.0 + cfg.saturation_boost)).clamp(0.0, 1.0);
        let srgb: Srgb = Srgb::from_color(hsv);
        rgb = [srgb.red, srgb.green, srgb.blue];
        adjusted[idx] = rgb;
    }
    let mut intermediate = RgbaImage::new(width, height);
    for (x, y, pixel) in intermediate.enumerate_pixels_mut() {
        let idx = (y as usize) * (width as usize) + (x as usize);
        *pixel = rgb_alpha_to_pixel(adjusted[idx], 1.0);
    }
    let sigma = cfg.sharpen_sigma.clamp(0.2, 4.0);
    let blurred = imageops::blur(&intermediate, sigma);
    for (x, y, pixel) in dest.enumerate_pixels_mut() {
        let idx = (y as usize) * (width as usize) + (x as usize);
        let source = original.get_pixel(x, y);
        let (orig_rgb, alpha) = pixel_to_rgb_alpha(source);
        let mut rgb = adjusted[idx];
        if cfg.sharpen_amount > f32::EPSILON {
            let blur_rgb = pixel_to_rgb_alpha(blurred.get_pixel(x, y)).0;
            for channel in 0..3 {
                let detail = rgb[channel] - blur_rgb[channel];
                rgb[channel] = (rgb[channel] + detail * cfg.sharpen_amount).clamp(0.0, 1.0);
            }
        }
        let mixed = mix_rgb(orig_rgb, rgb, amount);
        *pixel = rgb_alpha_to_pixel(mixed, alpha);
    }
}

fn high_key_bw(
    dest: &mut RgbaImage,
    original: &RgbaImage,
    cfg: &HighKeyBlackWhiteOptions,
    seed: u64,
) {
    let amount = cfg.amount.clamp(0.0, 1.0);
    if amount <= f32::EPSILON {
        return;
    }
    for (x, y, pixel) in dest.enumerate_pixels_mut() {
        let source = original.get_pixel(x, y);
        let (orig_rgb, alpha) = pixel_to_rgb_alpha(source);
        let lin = srgb_to_linear(orig_rgb);
        let mut lumin = 0.2126 * lin[0] + 0.7152 * lin[1] + 0.0722 * lin[2];
        lumin = high_key_curve(lumin, cfg.contrast);
        if cfg.grain_strength > f32::EPSILON {
            let noise = hash_noise(seed, x, y) - 0.5;
            lumin = (lumin + noise * cfg.grain_strength).clamp(0.0, 1.0);
        }
        let mapped = linear_to_srgb([lumin, lumin, lumin]);
        let mixed = mix_rgb(orig_rgb, mapped, amount);
        *pixel = rgb_alpha_to_pixel(mixed, alpha);
    }
}

fn matte_portrait(dest: &mut RgbaImage, original: &RgbaImage, cfg: &MattePortraitOptions) {
    let amount = cfg.amount.clamp(0.0, 1.0);
    if amount <= f32::EPSILON {
        return;
    }
    for (x, y, pixel) in dest.enumerate_pixels_mut() {
        let source = original.get_pixel(x, y);
        let (orig_rgb, alpha) = pixel_to_rgb_alpha(source);
        let (mut y_val, cb, cr) = rgb_to_ycbcr(orig_rgb);
        y_val = (y_val + cfg.lift).clamp(0.0, 1.0);
        if cfg.gamma > f32::EPSILON {
            y_val = y_val.powf(cfg.gamma);
        }
        y_val = (y_val * cfg.gain).clamp(0.0, 1.0);
        let mut rgb = ycbcr_to_rgb(y_val, cb, cr);
        if is_skin_tone(cb, cr) {
            rgb = mix_rgb(rgb, orig_rgb, cfg.skin_preserve);
        }
        let mixed = mix_rgb(orig_rgb, rgb, amount);
        *pixel = rgb_alpha_to_pixel(mixed, alpha);
    }
}

fn lomo_pop(dest: &mut RgbaImage, original: &RgbaImage, cfg: &LomoPopOptions, _seed: u64) {
    let amount = cfg.amount.clamp(0.0, 1.0);
    if amount <= f32::EPSILON {
        return;
    }
    let width = dest.width().max(1) as f32;
    let height = dest.height().max(1) as f32;
    for (x, y, pixel) in dest.enumerate_pixels_mut() {
        let source = original.get_pixel(x, y);
        let (orig_rgb, alpha) = pixel_to_rgb_alpha(source);
        let mut hsl = Hsl::from_color(Srgb::new(orig_rgb[0], orig_rgb[1], orig_rgb[2]));
        hsl.saturation = (hsl.saturation * (1.0 + cfg.saturation_boost)).clamp(0.0, 1.0);
        let mid_weight = (1.0 - (2.0 * (hsl.lightness - 0.5)).abs()).clamp(0.0, 1.0);
        let mut hue = hsl.hue.into_degrees();
        hue += cfg.hue_shift_deg * mid_weight;
        hsl.hue = RgbHue::from_degrees(hue);
        let mut rgb = Srgb::from_color(hsl);
        let radius = vignette_radius(x, y, width, height);
        let vignette_factor = (1.0 - cfg.vignette_strength * radius.powf(2.4)).clamp(0.0, 1.0);
        rgb.red = (rgb.red * vignette_factor).clamp(0.0, 1.0);
        rgb.green = (rgb.green * vignette_factor).clamp(0.0, 1.0);
        let edge = ((radius - 0.75).max(0.0) / 0.25).clamp(0.0, 1.0);
        rgb.blue = (rgb.blue * vignette_factor + cfg.blue_lift * edge).clamp(0.0, 1.0);
        let rgb = [rgb.red, rgb.green, rgb.blue];
        let mixed = mix_rgb(orig_rgb, rgb, amount);
        *pixel = rgb_alpha_to_pixel(mixed, alpha);
    }
}

fn cross_process(dest: &mut RgbaImage, original: &RgbaImage, cfg: &CrossProcessSimOptions) {
    let amount = cfg.amount.clamp(0.0, 1.0);
    if amount <= f32::EPSILON {
        return;
    }
    for (x, y, pixel) in dest.enumerate_pixels_mut() {
        let source = original.get_pixel(x, y);
        let (orig_rgb, alpha) = pixel_to_rgb_alpha(source);
        let lin: LinSrgb<f32> = Srgb::new(orig_rgb[0], orig_rgb[1], orig_rgb[2]).into_linear();
        let mut lab: Lab = Lab::from_color(lin);
        let l_norm = (lab.l / 100.0).clamp(0.0, 1.0);
        lab.l = cross_process_curve(l_norm, cfg.shadow_crush, cfg.highlight_boost) * 100.0;
        let shadow_weight = (1.0 - l_norm).powf(1.2);
        let highlight_weight = l_norm.powf(1.2);
        lab.a += cfg.a_shadow_shift * shadow_weight + cfg.a_highlight_shift * highlight_weight;
        lab.b += cfg.b_shift;
        let lin: LinSrgb<f32> = LinSrgb::from_color(lab);
        let rgb = linear_to_srgb([lin.red, lin.green, lin.blue]);
        let mixed = mix_rgb(orig_rgb, rgb, amount);
        *pixel = rgb_alpha_to_pixel(mixed, alpha);
    }
}

fn clean_boost(dest: &mut RgbaImage, original: &RgbaImage, cfg: &CleanBoostOptions) {
    let amount = cfg.amount.clamp(0.0, 1.0);
    if amount <= f32::EPSILON {
        return;
    }
    let width = dest.width();
    let height = dest.height();
    let mut sum = [0.0f32; 3];
    let mut count = 0f32;
    for pixel in original.pixels() {
        let (rgb, _) = pixel_to_rgb_alpha(pixel);
        sum[0] += rgb[0];
        sum[1] += rgb[1];
        sum[2] += rgb[2];
        count += 1.0;
    }
    if count <= 0.0 {
        return;
    }
    let avg = [sum[0] / count, sum[1] / count, sum[2] / count];
    let grey = (avg[0] + avg[1] + avg[2]) / 3.0;
    let mut gains = [1.0f32; 3];
    for channel in 0..3 {
        if avg[channel] > f32::EPSILON {
            let target = grey / avg[channel];
            gains[channel] = lerp(1.0, target, cfg.white_balance_strength.clamp(0.0, 1.0));
        }
    }
    let mut balanced = vec![[0.0f32; 3]; (width as usize) * (height as usize)];
    for (x, y, _) in dest.enumerate_pixels() {
        let idx = (y as usize) * (width as usize) + (x as usize);
        let source = original.get_pixel(x, y);
        let (rgb, _) = pixel_to_rgb_alpha(source);
        let bal = [
            (rgb[0] * gains[0]).clamp(0.0, 1.0),
            (rgb[1] * gains[1]).clamp(0.0, 1.0),
            (rgb[2] * gains[2]).clamp(0.0, 1.0),
        ];
        balanced[idx] = bal;
    }
    let mut temp = RgbaImage::new(width, height);
    for (x, y, pixel) in temp.enumerate_pixels_mut() {
        let idx = (y as usize) * (width as usize) + (x as usize);
        *pixel = rgb_alpha_to_pixel(balanced[idx], 1.0);
    }
    let blurred = imageops::blur(&temp, 2.0);
    for (x, y, pixel) in dest.enumerate_pixels_mut() {
        let idx = (y as usize) * (width as usize) + (x as usize);
        let source = original.get_pixel(x, y);
        let (orig_rgb, alpha) = pixel_to_rgb_alpha(source);
        let mut rgb = balanced[idx];
        if cfg.contrast_strength > f32::EPSILON {
            let blur_rgb = pixel_to_rgb_alpha(blurred.get_pixel(x, y)).0;
            for channel in 0..3 {
                let detail = rgb[channel] - blur_rgb[channel];
                rgb[channel] = (rgb[channel] + detail * cfg.contrast_strength).clamp(0.0, 1.0);
            }
        }
        if cfg.saturation_boost > f32::EPSILON {
            let mut hsv = Hsv::from_color(Srgb::new(rgb[0], rgb[1], rgb[2]));
            hsv.saturation = (hsv.saturation * (1.0 + cfg.saturation_boost)).clamp(0.0, 1.0);
            let srgb: Srgb = Srgb::from_color(hsv);
            rgb = [srgb.red, srgb.green, srgb.blue];
        }
        let mixed = mix_rgb(orig_rgb, rgb, amount);
        *pixel = rgb_alpha_to_pixel(mixed, alpha);
    }
}

fn pixel_to_rgb_alpha(pixel: &Rgba<u8>) -> ([f32; 3], f32) {
    let r = (pixel[0] as f32) / 255.0;
    let g = (pixel[1] as f32) / 255.0;
    let b = (pixel[2] as f32) / 255.0;
    let a = (pixel[3] as f32) / 255.0;
    ([r, g, b], a)
}

fn rgb_alpha_to_pixel(rgb: [f32; 3], alpha: f32) -> Rgba<u8> {
    let mut out = [0u8; 4];
    for (i, channel) in rgb.iter().enumerate() {
        out[i] = (channel.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    }
    out[3] = (alpha.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    Rgba(out)
}

fn srgb_to_linear(rgb: [f32; 3]) -> [f32; 3] {
    rgb.map(|c| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    })
}

fn linear_to_srgb(rgb: [f32; 3]) -> [f32; 3] {
    rgb.map(|c| {
        if c <= 0.0031308 {
            (c * 12.92).clamp(0.0, 1.0)
        } else {
            (1.055 * c.powf(1.0 / 2.4) - 0.055).clamp(0.0, 1.0)
        }
    })
}

fn mix_rgb(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        lerp(a[0], b[0], t),
        lerp(a[1], b[1], t),
        lerp(a[2], b[2], t),
    ]
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn smoothstep01(x: f32) -> f32 {
    let t = x.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn tone_curve(value: f32, strength: f32) -> f32 {
    if strength <= f32::EPSILON {
        return value.clamp(0.0, 1.0);
    }
    let mid = smoothstep01(value);
    let lifted = lerp(value, mid, 0.35 * strength);
    let low = 0.08 * strength;
    let high = 1.0 - 0.06 * strength;
    (lifted * (high - low) + low).clamp(0.0, 1.0)
}

fn vignette_radius(x: u32, y: u32, width: f32, height: f32) -> f32 {
    let nx = (x as f32 + 0.5) / width - 0.5;
    let ny = (y as f32 + 0.5) / height - 0.5;
    let radius = (nx * nx + ny * ny).sqrt();
    (radius / 0.70710677).clamp(0.0, 1.2)
}

fn matte_curve(l_norm: f32, strength: f32) -> f32 {
    if strength <= f32::EPSILON {
        return l_norm.clamp(0.0, 1.0);
    }
    let lift = 0.1 * strength;
    let shoulder = 0.92 - 0.08 * strength;
    let scaled = (l_norm * (shoulder - lift) + lift).clamp(0.0, 1.0);
    lerp(scaled, smoothstep01(scaled), 0.4 * strength)
}

fn filmic_curve(value: f32, contrast: f32) -> f32 {
    let contrast = contrast.clamp(0.5, 2.5);
    let pivot = 0.5;
    let mut x = ((value - pivot) * contrast + pivot).clamp(0.0, 1.0);
    let shoulder = smoothstep01(x);
    x = lerp(x, shoulder, 0.2);
    x
}

fn high_key_curve(value: f32, contrast: f32) -> f32 {
    let lift = 0.15 * contrast;
    let mut v = value + lift;
    v = v.clamp(0.0, 1.0);
    let highlight = 1.0 - (1.0 - v).powf(1.2 + 0.6 * contrast);
    lerp(v, highlight, 0.5)
}

fn rgb_to_ycbcr(rgb: [f32; 3]) -> (f32, f32, f32) {
    let y = 0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2];
    let cb = 0.564 * (rgb[2] - y);
    let cr = 0.713 * (rgb[0] - y);
    (y, cb, cr)
}

fn ycbcr_to_rgb(y: f32, cb: f32, cr: f32) -> [f32; 3] {
    let r = (y + 1.403 * cr).clamp(0.0, 1.0);
    let g = (y - 0.344 * cb - 0.714 * cr).clamp(0.0, 1.0);
    let b = (y + 1.773 * cb).clamp(0.0, 1.0);
    [r, g, b]
}

fn is_skin_tone(cb: f32, cr: f32) -> bool {
    let cb = cb + 0.5;
    let cr = cr + 0.5;
    (0.25..=0.65).contains(&cb) && (0.35..=0.75).contains(&cr)
}

fn cross_process_curve(l_norm: f32, shadow_crush: f32, highlight_boost: f32) -> f32 {
    let mut v = l_norm.clamp(0.0, 1.0);
    if shadow_crush > f32::EPSILON {
        v = v.powf(1.0 + shadow_crush);
    }
    if highlight_boost > f32::EPSILON {
        let highlight = 1.0 - (1.0 - v).powf(1.0 - 0.5 * highlight_boost);
        v = lerp(v, highlight, highlight_boost);
    }
    v.clamp(0.0, 1.0)
}

fn hash_noise(seed: u64, x: u32, y: u32) -> f32 {
    let mut value = seed ^ ((x as u64) << 32) ^ (y as u64);
    value = value.wrapping_mul(0x9E3779B97F4A7C15);
    value ^= value >> 33;
    value = value.wrapping_mul(0xC2B2AE3D27D4EB4F);
    let bits = (value >> 32) as u32;
    (bits as f32) / (u32::MAX as f32)
}
