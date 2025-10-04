use crate::config::PrintSimulationOptions;
use image::{Rgba, RgbaImage};

/// Applies a simple print-simulation relighting model inspired by
/// "3D Simulation of Prints for Improved Soft Proofing".
///
/// The effect adds directional shading and a mild paper sheen to emulate a
/// physical print under lighting. All computations are performed in-place on
/// the provided image buffer.
pub fn apply_print_simulation(image: &mut RgbaImage, options: &PrintSimulationOptions) {
    if image.width() == 0 || image.height() == 0 {
        return;
    }

    let width = image.width() as i32;
    let height = image.height() as i32;
    let mut luminance = vec![0.0f32; (width * height) as usize];
    let affect_limit = if options.debug {
        (width + 1) / 2
    } else {
        width
    };

    for (idx, pixel) in image.pixels().enumerate() {
        luminance[idx] = luminance_of(pixel);
    }

    let light_angle = options.light_angle_degrees.to_radians();
    let light_dir = (light_angle.cos(), light_angle.sin());
    let relief_strength = options.relief_strength.max(0.0);
    let sheen_strength = options.sheen_strength.max(0.0).min(1.0);
    let ink_spread = options.ink_spread.max(0.0);
    let paper = [
        f32::from(options.paper_color[0]) / 255.0,
        f32::from(options.paper_color[1]) / 255.0,
        f32::from(options.paper_color[2]) / 255.0,
    ];

    for y in 0..height {
        for x in 0..width {
            if options.debug && x >= affect_limit {
                continue;
            }
            let idx = (y * width + x) as usize;
            let center_luma = luminance[idx];

            let left = luminance[(y * width + (x - 1).clamp(0, width - 1)) as usize];
            let right = luminance[(y * width + (x + 1).clamp(0, width - 1)) as usize];
            let up = luminance[(((y - 1).clamp(0, height - 1)) * width + x) as usize];
            let down = luminance[(((y + 1).clamp(0, height - 1)) * width + x) as usize];

            let grad_x = right - left;
            let grad_y = down - up;
            let relief = (grad_x * light_dir.0 + grad_y * light_dir.1) * relief_strength;
            let shade = (1.0 + relief).clamp(0.0, 2.0);
            let sheen = ((relief.abs() + (1.0 - center_luma) * 0.5).min(1.0)) * sheen_strength;

            let pixel = image.get_pixel_mut(x as u32, y as u32);
            apply_channel_model(pixel, shade, sheen, &paper, ink_spread);
        }
    }
}

fn apply_channel_model(
    pixel: &mut Rgba<u8>,
    shade: f32,
    sheen: f32,
    paper: &[f32; 3],
    ink_spread: f32,
) {
    for (channel, paper_component) in pixel.0.iter_mut().take(3).zip(paper.iter()) {
        let mut value = f32::from(*channel) / 255.0;
        value = value.powf(1.0 + ink_spread * 0.8);
        value = (value * shade).clamp(0.0, 1.2);
        let coated = value * (1.0 - sheen) + *paper_component * sheen;
        *channel = (coated.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
}

fn luminance_of(pixel: &Rgba<u8>) -> f32 {
    let r = f32::from(pixel[0]) / 255.0;
    let g = f32::from(pixel[1]) / 255.0;
    let b = f32::from(pixel[2]) / 255.0;
    (0.2126 * r + 0.7152 * g + 0.0722 * b).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_image_when_strength_zero() {
        let mut image = RgbaImage::from_pixel(2, 2, Rgba([100, 150, 200, 255]));
        let options = PrintSimulationOptions {
            relief_strength: 0.0,
            ink_spread: 0.0,
            sheen_strength: 0.0,
            ..PrintSimulationOptions::default()
        };

        apply_print_simulation(&mut image, &options);
        assert_eq!(image.get_pixel(0, 0).0, [100, 150, 200, 255]);
    }

    #[test]
    fn modifies_pixels_with_strength() {
        let mut image = RgbaImage::from_fn(3, 1, |x, _| Rgba([(x * 60) as u8, 120, 180, 255]));
        let options = PrintSimulationOptions {
            relief_strength: 1.0,
            ink_spread: 0.5,
            sheen_strength: 0.4,
            ..PrintSimulationOptions::default()
        };

        let before: Vec<u8> = image.clone().into_raw();
        apply_print_simulation(&mut image, &options);
        assert_ne!(image.into_raw(), before);
    }

    #[test]
    fn debug_mode_only_touches_left_half() {
        let mut image = RgbaImage::from_fn(4, 1, |x, _| Rgba([(x * 40) as u8, 100, 150, 255]));
        let mut options = PrintSimulationOptions {
            relief_strength: 1.0,
            ink_spread: 0.4,
            sheen_strength: 0.3,
            ..PrintSimulationOptions::default()
        };
        options.debug = true;

        let before = image.clone();
        apply_print_simulation(&mut image, &options);

        assert_ne!(image.get_pixel(0, 0).0, before.get_pixel(0, 0).0);
        assert_eq!(image.get_pixel(2, 0).0, before.get_pixel(2, 0).0);
        assert_eq!(image.get_pixel(3, 0).0, before.get_pixel(3, 0).0);
    }
}
