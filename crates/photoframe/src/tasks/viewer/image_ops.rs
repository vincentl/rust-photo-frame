//! Pure image-processing and math utilities used by the viewer pipeline.
//!
//! These functions operate only on CPU-side image data and plain numeric
//! types, so they are easy to unit-test without a GPU or display.

use image::{Rgba, RgbaImage, imageops};

// ── Stride / sizing ──────────────────────────────────────────────────────────

pub(super) fn compute_padded_stride(bytes_per_row: u32) -> u32 {
    const ALIGN: u32 = 256;
    if bytes_per_row == 0 {
        return 0;
    }
    bytes_per_row.div_ceil(ALIGN) * ALIGN
}

pub(super) fn compute_canvas_size(
    screen_w: u32,
    screen_h: u32,
    oversample: f32,
    max_dim: u32,
) -> (u32, u32) {
    let safe_max_dim = max_dim.max(1) as f32;
    let safe_oversample = if oversample.is_finite() && oversample > 0.0 {
        oversample
    } else {
        1.0
    };

    let mut sw = (screen_w.max(1) as f32 * safe_oversample).round().max(1.0);
    let mut sh = (screen_h.max(1) as f32 * safe_oversample).round().max(1.0);

    if sw > safe_max_dim || sh > safe_max_dim {
        let scale = safe_max_dim / sw.max(sh).max(1.0);
        sw = (sw * scale).round().clamp(1.0, safe_max_dim);
        sh = (sh * scale).round().clamp(1.0, safe_max_dim);
    } else {
        sw = sw.min(safe_max_dim);
        sh = sh.min(safe_max_dim);
    }

    (sw as u32, sh as u32)
}

// ── Image scaling / compositing ───────────────────────────────────────────────

pub(super) fn scale_image_to_cover_canvas(
    src: &RgbaImage,
    canvas_w: u32,
    canvas_h: u32,
    max_dim: u32,
) -> RgbaImage {
    let (src_w, src_h) = src.dimensions();
    let safe_canvas_w = canvas_w.max(1);
    let safe_canvas_h = canvas_h.max(1);
    let safe_max_dim = max_dim.max(1);

    if src_w == 0 || src_h == 0 {
        return RgbaImage::from_pixel(safe_canvas_w, safe_canvas_h, Rgba([0, 0, 0, 255]));
    }

    let src_w_f = src_w as f64;
    let src_h_f = src_h as f64;
    let canvas_w_f = safe_canvas_w as f64;
    let canvas_h_f = safe_canvas_h as f64;

    let aspect_src = src_w_f / src_h_f;
    let aspect_canvas = canvas_w_f / canvas_h_f;

    let (crop_x, crop_y, crop_w, crop_h) = if (aspect_src - aspect_canvas).abs() < f64::EPSILON {
        (0, 0, src_w, src_h)
    } else if aspect_src < aspect_canvas {
        // Source is taller relative to the canvas; trim vertical excess.
        let desired_h = (src_w_f / aspect_canvas).round().clamp(1.0, src_h_f) as u32;
        let crop_y = ((src_h - desired_h) / 2).min(src_h.saturating_sub(desired_h));
        (0, crop_y, src_w, desired_h.max(1))
    } else {
        // Source is wider relative to the canvas; trim horizontal excess.
        let desired_w = (src_h_f * aspect_canvas).round().clamp(1.0, src_w_f) as u32;
        let crop_x = ((src_w - desired_w) / 2).min(src_w.saturating_sub(desired_w));
        (crop_x, 0, desired_w.max(1), src_h)
    };

    let crop = imageops::crop_imm(src, crop_x, crop_y, crop_w, crop_h).to_image();

    let scale_cap_w = safe_max_dim as f64 / safe_canvas_w as f64;
    let scale_cap_h = safe_max_dim as f64 / safe_canvas_h as f64;
    let needs_downscale = safe_canvas_w > safe_max_dim || safe_canvas_h > safe_max_dim;
    let uniform_scale = if needs_downscale {
        scale_cap_w.min(scale_cap_h)
    } else {
        1.0
    };

    let target_w = ((safe_canvas_w as f64) * uniform_scale)
        .round()
        .clamp(1.0, safe_max_dim as f64) as u32;
    let target_h = ((safe_canvas_h as f64) * uniform_scale)
        .round()
        .clamp(1.0, safe_max_dim as f64) as u32;
    let scaled = imageops::resize(&crop, target_w, target_h, imageops::FilterType::Triangle);

    center_crop_or_pad(scaled, canvas_w, canvas_h)
}

pub(super) fn center_crop_or_pad(mut img: RgbaImage, target_w: u32, target_h: u32) -> RgbaImage {
    if img.width() > target_w {
        let crop_x = (img.width() - target_w) / 2;
        img = imageops::crop_imm(&img, crop_x, 0, target_w, img.height()).to_image();
    }

    if img.height() > target_h {
        let crop_y = (img.height() - target_h) / 2;
        let crop_w = img.width();
        img = imageops::crop_imm(&img, 0, crop_y, crop_w, target_h).to_image();
    }

    if img.width() < target_w || img.height() < target_h {
        let mut canvas = RgbaImage::from_pixel(target_w, target_h, Rgba([0, 0, 0, 255]));
        let x = (target_w.saturating_sub(img.width())) / 2;
        let y = (target_h.saturating_sub(img.height())) / 2;
        imageops::overlay(&mut canvas, &img, x as i64, y as i64);
        return canvas;
    }

    img
}

pub(super) fn resize_to_fit_with_margin(
    canvas_w: u32,
    canvas_h: u32,
    src_w: u32,
    src_h: u32,
    margin_frac: f32,
    max_upscale: f32,
) -> (u32, u32) {
    let iw = src_w.max(1) as f32;
    let ih = src_h.max(1) as f32;
    let cw = canvas_w.max(1) as f32;
    let ch = canvas_h.max(1) as f32;
    let margin_frac = margin_frac.clamp(0.0, 0.45);
    let avail_w = (cw * (1.0 - 2.0 * margin_frac)).max(1.0);
    let avail_h = (ch * (1.0 - 2.0 * margin_frac)).max(1.0);
    let max_upscale = max_upscale.max(1.0);
    let scale = (avail_w / iw).min(avail_h / ih).min(max_upscale);
    let w = (iw * scale).round().clamp(1.0, cw);
    let h = (ih * scale).round().clamp(1.0, ch);
    (w as u32, h as u32)
}

// ── Studio mat rendering ─────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub(super) fn render_studio_mat(
    canvas_w: u32,
    canvas_h: u32,
    photo_x: u32,
    photo_y: u32,
    photo_w: u32,
    photo_h: u32,
    photo: &RgbaImage,
    mat_color: [f32; 3],
    bevel_width_px: f32,
    bevel_color: [u8; 3],
    texture_strength: f32,
    warp_period_px: f32,
    weft_period_px: f32,
) -> RgbaImage {
    let mut bevel_px = bevel_width_px.max(0.0);
    let max_border = photo_x
        .min(photo_y)
        .min(canvas_w.saturating_sub(photo_x.saturating_add(photo_w)))
        .min(canvas_h.saturating_sub(photo_y.saturating_add(photo_h))) as f32;
    if bevel_px > 0.0 {
        bevel_px = bevel_px.min(max_border.max(0.0));
    } else {
        bevel_px = 0.0;
    }

    let window_x = photo_x as f32;
    let window_y = photo_y as f32;
    let window_max_x = window_x + photo_w.max(1) as f32;
    let window_max_y = window_y + photo_h.max(1) as f32;

    let bevel_rgb_f32 = [
        bevel_color[0] as f32 / 255.0,
        bevel_color[1] as f32 / 255.0,
        bevel_color[2] as f32 / 255.0,
    ];
    let light_dir = normalize3([-0.55, -0.65, 0.52]);
    let ambient = 0.88;
    let diffuse = 0.18;
    let texture_strength = texture_strength.clamp(0.0, 2.0);
    let warp_period = warp_period_px.max(0.5);
    let weft_period = weft_period_px.max(0.5);

    let mut mat = RgbaImage::new(canvas_w, canvas_h);
    for (x, y, pixel) in mat.enumerate_pixels_mut() {
        let px = x as f32 + 0.5;
        let py = y as f32 + 0.5;

        let inside_window =
            px >= window_x && px < window_max_x && py >= window_y && py < window_max_y;

        if inside_window {
            let u = if photo_w == 0 {
                0.0
            } else {
                ((px - window_x) / photo_w as f32).clamp(0.0, 1.0)
            };
            let v = if photo_h == 0 {
                0.0
            } else {
                ((py - window_y) / photo_h as f32).clamp(0.0, 1.0)
            };
            let sample_x = (u * (photo_w.max(1) as f32 - 1.0)).clamp(0.0, photo_w as f32 - 1.0);
            let sample_y = (v * (photo_h.max(1) as f32 - 1.0)).clamp(0.0, photo_h as f32 - 1.0);
            let sample = sample_bilinear(photo, sample_x, sample_y);

            for c in 0..3 {
                pixel[c] = srgb_u8(sample[c]);
            }
            pixel[3] = 255;
            continue;
        }

        if bevel_px > 0.0 {
            let dx = if px < window_x {
                window_x - px
            } else if px >= window_max_x {
                px - window_max_x
            } else {
                0.0
            };
            let dy = if py < window_y {
                window_y - py
            } else if py >= window_max_y {
                py - window_max_y
            } else {
                0.0
            };

            if dx < bevel_px && dy < bevel_px {
                let max_offset = dx.max(dy).clamp(0.0, bevel_px);
                let depth = if bevel_px <= f32::EPSILON {
                    0.0
                } else {
                    (1.0 - max_offset / bevel_px).clamp(0.0, 1.0)
                };

                let nearest_x = px.clamp(window_x, window_max_x);
                let nearest_y = py.clamp(window_y, window_max_y);
                let mut dir = [nearest_x - px, nearest_y - py];
                let dir_len_sq = dir[0] * dir[0] + dir[1] * dir[1];
                if dir_len_sq > 1e-6 {
                    let inv_len = dir_len_sq.sqrt().recip();
                    dir[0] *= inv_len;
                    dir[1] *= inv_len;
                } else if dx > dy {
                    dir = [if px < window_x { 1.0 } else { -1.0 }, 0.0];
                } else {
                    dir = [0.0, if py < window_y { 1.0 } else { -1.0 }];
                }

                let mut normal = [dir[0], dir[1], 1.0];
                normal = normalize3(normal);
                let mut shade = ambient + diffuse * dot3(normal, light_dir).max(0.0);
                shade += 0.1 * depth.powf(2.0);
                shade = shade.clamp(0.82, 1.08);

                let mat_mix = (1.0 - depth).powf(3.0) * 0.35;
                let mat_mix = mat_mix.clamp(0.0, 1.0);

                let mut color = [0u8; 3];
                for c in 0..3 {
                    let base = lerp(bevel_rgb_f32[c], mat_color[c], mat_mix);
                    let shaded = (base * shade).clamp(0.0, 1.0);
                    color[c] = srgb_u8(shaded);
                }

                pixel[0] = color[0];
                pixel[1] = color[1];
                pixel[2] = color[2];
                pixel[3] = 255;
                continue;
            }
        }

        let warp_noise = (weave_grain(x, y) - 0.5) * 0.65;
        let weft_noise = (weave_grain(x.wrapping_add(17), y.wrapping_add(113)) - 0.5) * 0.65;
        let warp_phase = ((px + warp_noise) / warp_period).fract();
        let weft_phase = ((py + weft_noise) / weft_period).fract();
        let warp_profile = weave_thread_profile(warp_phase);
        let weft_profile = weave_thread_profile(weft_phase);
        let warp_centered = warp_profile - 0.5;
        let weft_centered = weft_profile - 0.5;
        let cross_highlight = warp_profile * weft_profile - 0.25;
        let thread_mix = (warp_centered * 0.08 - weft_centered * 0.06 + cross_highlight * 0.12)
            * texture_strength;
        let grain_strength = texture_strength.min(1.0);
        let grain =
            (weave_grain(x.wrapping_add(137), y.wrapping_add(197)) - 0.5) * 0.025 * grain_strength;
        let envelope = 0.1 * texture_strength.min(1.2);
        let shade = (1.0 + thread_mix + grain).clamp(1.0 - envelope, 1.0 + envelope);

        for c in 0..3 {
            let tinted = (mat_color[c] * shade).clamp(0.0, 1.0);
            pixel[c] = srgb_u8(tinted);
        }
        pixel[3] = 255;
    }

    mat
}

// ── Math utilities ────────────────────────────────────────────────────────────

pub(super) fn srgb_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

pub(super) fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

pub(super) fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub(super) fn normalize3(mut v: [f32; 3]) -> [f32; 3] {
    let len_sq = dot3(v, v);
    if len_sq > 1e-6 {
        let inv_len = len_sq.sqrt().recip();
        v[0] *= inv_len;
        v[1] *= inv_len;
        v[2] *= inv_len;
        v
    } else {
        [0.0, 0.0, 1.0]
    }
}

pub(super) fn weave_thread_profile(phase: f32) -> f32 {
    let dist = (phase - 0.5).abs() * 2.0;
    let base = (1.0 - dist).clamp(0.0, 1.0);
    base * base * (3.0 - 2.0 * base)
}

pub(super) fn weave_grain(x: u32, y: u32) -> f32 {
    let mut hash = x.wrapping_mul(0x045d_9f3b) ^ y.wrapping_mul(0x27d4_eb2d);
    hash ^= hash.rotate_left(13);
    hash = hash.wrapping_mul(0x1656_67b1);
    ((hash >> 8) & 0xffff) as f32 / 65535.0
}

pub(super) fn sample_bilinear(img: &RgbaImage, x: f32, y: f32) -> [f32; 3] {
    let w = img.width();
    let h = img.height();
    if w == 0 || h == 0 {
        return [0.0, 0.0, 0.0];
    }
    let max_x = (w - 1) as f32;
    let max_y = (h - 1) as f32;
    let xf = x.clamp(0.0, max_x);
    let yf = y.clamp(0.0, max_y);
    let x0 = xf.floor() as u32;
    let y0 = yf.floor() as u32;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let tx = xf - x0 as f32;
    let ty = yf - y0 as f32;

    let p00 = img.get_pixel(x0, y0);
    let p10 = img.get_pixel(x1, y0);
    let p01 = img.get_pixel(x0, y1);
    let p11 = img.get_pixel(x1, y1);

    let mut result = [0.0f32; 3];
    for c in 0..3 {
        let c00 = p00[c] as f32 / 255.0;
        let c10 = p10[c] as f32 / 255.0;
        let c01 = p01[c] as f32 / 255.0;
        let c11 = p11[c] as f32 / 255.0;
        let c0 = lerp(c00, c10, tx);
        let c1 = lerp(c01, c11, tx);
        result[c] = lerp(c0, c1, ty);
    }
    result
}

// ── GPU rectangle helpers ─────────────────────────────────────────────────────

pub(super) fn compute_cover_rect(
    img_w: u32,
    img_h: u32,
    screen_w: u32,
    screen_h: u32,
) -> (f32, f32, f32, f32) {
    let iw = img_w.max(1) as f32;
    let ih = img_h.max(1) as f32;
    let sw = screen_w.max(1) as f32;
    let sh = screen_h.max(1) as f32;
    let scale = (sw / iw).max(sh / ih);
    let w = iw * scale;
    let h = ih * scale;
    let x = (sw - w) * 0.5;
    let y = (sh - h) * 0.5;
    (x, y, w, h)
}

pub(super) fn rect_to_uniform(rect: (f32, f32, f32, f32)) -> [f32; 4] {
    [rect.0, rect.1, rect.2, rect.3]
}

pub(super) fn compute_wipe_span(normal: [f32; 2], screen_w: f32, screen_h: f32) -> (f32, f32) {
    let corners = [
        [0.0, 0.0],
        [screen_w, 0.0],
        [0.0, screen_h],
        [screen_w, screen_h],
    ];
    let mut min_proj = f32::MAX;
    let mut max_proj = f32::MIN;
    for corner in corners {
        let proj = normal[0] * corner[0] + normal[1] * corner[1];
        min_proj = min_proj.min(proj);
        max_proj = max_proj.max(proj);
    }
    let span = (max_proj - min_proj).abs().max(1e-3);
    (min_proj, 1.0 / span)
}

/// Convert an angle in degrees to a unit-length 2-D direction vector `[x, y]`.
/// Falls back to `[1.0, 0.0]` on degenerate input.
pub(super) fn angle_to_unit_vec(angle_deg: f32) -> [f32; 2] {
    let (sin, cos) = angle_deg.to_radians().sin_cos();
    let mut v = [cos, sin];
    let len = (v[0] * v[0] + v[1] * v[1]).sqrt();
    if len > f32::EPSILON {
        v[0] /= len;
        v[1] /= len;
        v
    } else {
        [1.0, 0.0]
    }
}
