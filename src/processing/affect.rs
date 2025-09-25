use image::{Pixel, RgbaImage};

use crate::config::Print3dOptions;

fn light_direction(options: &Print3dOptions) -> [f32; 3] {
    let azimuth = options.light_azimuth_degrees.to_radians();
    let elevation = options.light_elevation_degrees.to_radians();
    let cos_elev = elevation.cos();
    [
        azimuth.cos() * cos_elev,
        azimuth.sin() * cos_elev,
        elevation.sin().max(1e-3),
    ]
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len_sq = v[0] * v[0] + v[1] * v[1] + v[2] * v[2];
    if len_sq <= 0.0 {
        [0.0, 0.0, 1.0]
    } else {
        let inv = len_sq.sqrt().recip();
        [v[0] * inv, v[1] * inv, v[2] * inv]
    }
}

/// Apply a simple relief shading model inspired by
/// "3D Simulation of Prints for Improved Soft Proofing" to mimic paper depth.
pub fn apply_print_relief(image: &mut RgbaImage, options: &Print3dOptions) {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return;
    }

    let light = normalize(light_direction(options));
    let view = [0.0_f32, 0.0, 1.0];
    let half_vector = normalize([light[0] + view[0], light[1] + view[1], light[2] + view[2]]);

    let mut luminance = vec![0.0_f32; (width * height) as usize];
    for (x, y, pixel) in image.enumerate_pixels() {
        let idx = (y as usize) * (width as usize) + (x as usize);
        let rgb = pixel.to_rgb();
        let channels = rgb.0;
        luminance[idx] = 0.2126 * f32::from(channels[0])
            + 0.7152 * f32::from(channels[1])
            + 0.0722 * f32::from(channels[2]);
    }

    let relief_strength = options.relief_strength.max(0.0);
    let ambient = options.ambient.clamp(0.0, 1.0);
    let diffuse_scale = options.diffuse.clamp(0.0, 1.0);
    let specular_strength = options.specular_strength.max(0.0);
    let shininess = options.specular_shininess.max(1.0);
    let shadow_floor = options.shadow_floor.clamp(0.0, 1.0);
    let highlight_gain = options.highlight_gain.max(0.0);

    let width_i = width as usize;

    for y in 0..height as usize {
        for x in 0..width as usize {
            let idx = y * width_i + x;
            let center = luminance[idx];
            let left = if x == 0 { center } else { luminance[idx - 1] };
            let right = if x + 1 >= width_i {
                center
            } else {
                luminance[idx + 1]
            };
            let up = if y == 0 {
                center
            } else {
                luminance[idx - width_i]
            };
            let down = if y + 1 >= height as usize {
                center
            } else {
                luminance[idx + width_i]
            };

            let dx = (right - left) * 0.5 * relief_strength * (1.0 / 255.0);
            let dy = (down - up) * 0.5 * relief_strength * (1.0 / 255.0);

            let normal = normalize([-dx, -dy, 1.0]);
            let diffuse =
                (normal[0] * light[0] + normal[1] * light[1] + normal[2] * light[2]).max(0.0);
            let ndoth = (normal[0] * half_vector[0]
                + normal[1] * half_vector[1]
                + normal[2] * half_vector[2])
                .max(0.0);
            let specular = if specular_strength > 0.0 {
                specular_strength * ndoth.powf(shininess)
            } else {
                0.0
            };

            let mut shading = ambient + diffuse_scale * diffuse;
            shading = shading.max(shadow_floor).min(1.0);
            shading = (shading + specular * (1.0 + highlight_gain)).min(1.0 + highlight_gain);

            let pixel = image.get_pixel_mut(x as u32, y as u32);
            for channel in &mut pixel.0[0..3] {
                let value = f32::from(*channel) / 255.0;
                let mapped = (value * shading).clamp(0.0, 1.0);
                *channel = (mapped * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relief_applies_perceptible_change() {
        let mut img = RgbaImage::from_raw(
            3,
            3,
            vec![
                10, 10, 10, 255, 40, 40, 40, 255, 80, 80, 80, 255, 10, 10, 10, 255, 128, 128, 128,
                255, 200, 200, 200, 255, 10, 10, 10, 255, 220, 220, 220, 255, 250, 250, 250, 255,
            ],
        )
        .expect("image creation");
        let before = img.clone();
        apply_print_relief(&mut img, &Print3dOptions::default());
        assert_ne!(before.into_raw(), img.into_raw());
    }
}
