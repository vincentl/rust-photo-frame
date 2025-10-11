use image::{RgbaImage, imageops};

use crate::config::BlurBackend;

pub fn apply_blur(image: &RgbaImage, sigma: f32, backend: BlurBackend) -> RgbaImage {
    if sigma <= 0.0 {
        return image.clone();
    }

    match backend {
        BlurBackend::Cpu => blur_cpu(image, sigma),
        BlurBackend::Neon => neon_blur(image, sigma).unwrap_or_else(|| blur_cpu(image, sigma)),
    }
}

fn blur_cpu(image: &RgbaImage, sigma: f32) -> RgbaImage {
    imageops::blur(image, sigma)
}

#[cfg(target_arch = "aarch64")]
fn gaussian_kernel(sigma: f32) -> (Vec<f32>, u32) {
    let sigma = sigma.max(0.01);
    let radius = (sigma * 3.0).ceil() as i32;
    if radius <= 0 {
        return (vec![1.0], 0);
    }

    let mut weights = Vec::with_capacity((radius * 2 + 1) as usize);
    let denom = 2.0 * sigma * sigma;
    let mut sum = 0.0;
    for i in -radius..=radius {
        let x = i as f32;
        let w = (-x * x / denom).exp();
        weights.push(w);
        sum += w;
    }

    if sum > 0.0 {
        for w in &mut weights {
            *w /= sum;
        }
    }

    (weights, radius as u32)
}

#[cfg(target_arch = "aarch64")]
fn rgba_to_f32(image: &RgbaImage) -> Vec<f32> {
    image
        .pixels()
        .flat_map(|p| p.0.iter().map(|&c| (c as f32) / 255.0))
        .collect()
}

#[cfg(target_arch = "aarch64")]
fn f32_to_rgba(width: u32, height: u32, data: &[f32]) -> RgbaImage {
    let mut out = RgbaImage::new(width, height);
    for (i, pixel) in out.pixels_mut().enumerate() {
        let base = i * 4;
        let rgba = [
            (data.get(base).copied().unwrap_or(0.0) * 255.0).clamp(0.0, 255.0) as u8,
            (data.get(base + 1).copied().unwrap_or(0.0) * 255.0).clamp(0.0, 255.0) as u8,
            (data.get(base + 2).copied().unwrap_or(0.0) * 255.0).clamp(0.0, 255.0) as u8,
            (data.get(base + 3).copied().unwrap_or(1.0) * 255.0).clamp(0.0, 255.0) as u8,
        ];
        pixel.0 = rgba;
    }
    out
}

#[cfg(target_arch = "aarch64")]
fn neon_blur(image: &RgbaImage, sigma: f32) -> Option<RgbaImage> {
    if !std::arch::is_aarch64_feature_detected!("neon") {
        return None;
    }

    let (weights, radius) = gaussian_kernel(sigma);
    if radius == 0 {
        return Some(image.clone());
    }

    let width = image.width() as usize;
    let height = image.height() as usize;
    let mut src = rgba_to_f32(image);
    let mut tmp = vec![0.0f32; src.len()];

    unsafe {
        neon::blur_pass(
            &src,
            &mut tmp,
            width,
            height,
            radius as usize,
            &weights,
            true,
        );
        neon::blur_pass(
            &tmp,
            &mut src,
            width,
            height,
            radius as usize,
            &weights,
            false,
        );
    }

    Some(f32_to_rgba(image.width(), image.height(), &src))
}

#[cfg(not(target_arch = "aarch64"))]
fn neon_blur(_image: &RgbaImage, _sigma: f32) -> Option<RgbaImage> {
    None
}

#[cfg(target_arch = "aarch64")]
mod neon {
    use std::arch::aarch64::*;

    #[target_feature(enable = "neon")]
    pub unsafe fn blur_pass(
        src: &[f32],
        dst: &mut [f32],
        width: usize,
        height: usize,
        radius: usize,
        weights: &[f32],
        horizontal: bool,
    ) {
        let src_ptr = src.as_ptr();
        let dst_ptr = dst.as_mut_ptr();
        let kernel = &weights[..(2 * radius + 1)];
        for y in 0..height {
            for x in 0..width {
                let mut acc = vdupq_n_f32(0.0);
                for (idx, &weight) in kernel.iter().enumerate() {
                    let offset = idx as isize - radius as isize;
                    let sample_index = if horizontal {
                        let sx = clamp_i((x as isize) + offset, width as isize);
                        ((y * width) + sx) * 4
                    } else {
                        let sy = clamp_i((y as isize) + offset, height as isize);
                        ((sy * width) + x) * 4
                    };
                    let pix = unsafe { vld1q_f32(src_ptr.add(sample_index)) };
                    let weight_vec = vdupq_n_f32(weight);
                    acc = vmlaq_f32(acc, pix, weight_vec);
                }
                let out_index = (y * width + x) * 4;
                unsafe {
                    vst1q_f32(dst_ptr.add(out_index), acc);
                }
            }
        }
    }

    #[inline(always)]
    fn clamp_i(value: isize, max: isize) -> usize {
        value.clamp(0, max.saturating_sub(1)) as usize
    }
}
