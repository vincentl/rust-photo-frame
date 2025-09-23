use image::{Rgba, RgbaImage};

pub fn average_color(img: &RgbaImage) -> [f32; 3] {
    let mut accum = [0f64; 3];
    let mut total = 0f64;
    for pixel in img.pixels() {
        let alpha = (pixel[3] as f64) / 255.0;
        if alpha <= 0.0 {
            continue;
        }
        total += alpha;
        for c in 0..3 {
            accum[c] += (pixel[c] as f64) * alpha;
        }
    }
    if total <= f64::EPSILON {
        return [0.1, 0.1, 0.1];
    }
    [
        (accum[0] / (255.0 * total)) as f32,
        (accum[1] / (255.0 * total)) as f32,
        (accum[2] / (255.0 * total)) as f32,
    ]
}

pub fn average_color_rgba(img: &RgbaImage) -> Rgba<u8> {
    let avg = average_color(img);
    Rgba([
        (avg[0] * 255.0).round().clamp(0.0, 255.0) as u8,
        (avg[1] * 255.0).round().clamp(0.0, 255.0) as u8,
        (avg[2] * 255.0).round().clamp(0.0, 255.0) as u8,
        255,
    ])
}
