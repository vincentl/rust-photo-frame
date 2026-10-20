use std::cell::OnceCell;

use image::{Rgba, RgbaImage};
use palette::{FromColor, Oklab, Srgb};

/// Fallback swatch for images with no visible pixels.
const EMPTY_FALLBACK: [f32; 3] = [0.1, 0.1, 0.1];
/// Long-side sample size for `cluster_color`; the result is a statistic, so
/// a coarse grid is enough.
const CLUSTER_SAMPLE_LONG_SIDE: u32 = 64;
/// Width of one density-histogram cell in Oklab units (a just-noticeable
/// difference is about 0.02).
const CLUSTER_CELL: f32 = 0.025;
/// Gaussian bandwidth of the density estimate, in cells (so 0.075 in Oklab).
const CLUSTER_SIGMA_CELLS: f32 = 3.0;
/// The smoothing kernel is truncated at this many cells either side.
const CLUSTER_KERNEL_RADIUS: i32 = 6;
const CLUSTER_MEAN_SHIFT_ITERATIONS: usize = 16;
/// Largest mean-shift step (Oklab units) that still counts as converged.
const CLUSTER_CONVERGENCE: f32 = 1e-4;
/// Oklab bounding box of the sRGB gamut, padded to whole cells: L in [0, 1],
/// a and b in [-0.4, 0.4).
const CLUSTER_AB_MIN: f32 = -0.4;
const CLUSTER_L_BINS: usize = 41;
const CLUSTER_AB_BINS: usize = 32;

/// Per-photo swatches that mat colour tokens resolve against. Each statistic
/// is computed the first time a mat slot asks for it and then reused, so a
/// slot that uses `photo-average` never pays for `photo-cluster`.
pub struct PhotoSwatches<'a> {
    img: &'a RgbaImage,
    average: OnceCell<[f32; 3]>,
    cluster: OnceCell<[f32; 3]>,
}

impl<'a> PhotoSwatches<'a> {
    pub fn new(img: &'a RgbaImage) -> Self {
        Self {
            img,
            average: OnceCell::new(),
            cluster: OnceCell::new(),
        }
    }

    pub fn average(&self) -> [f32; 3] {
        *self.average.get_or_init(|| average_color(self.img))
    }

    pub fn cluster(&self) -> [f32; 3] {
        *self.cluster.get_or_init(|| cluster_color(self.img))
    }
}

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
        return EMPTY_FALLBACK;
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

/// The dominant colour of the photo: the **mode** of its colour distribution.
///
/// The photo's pixels are sampled on a coarse grid, placed in Oklab (where
/// distance approximates perceived difference) and binned into a 3-D histogram
/// that is smoothed with a Gaussian of 0.075, i.e. a kernel density estimate.
/// The densest point is the colour that the largest amount of the photo is
/// *near*; it is then refined by mean-shift to the weighted mean of its
/// neighbourhood, so the result is the centre of a real concentration of
/// pixels, never a blend of distant regions (that is `average_color`).
///
/// Why a density mode and not "the largest k-means cluster": k-means splits a
/// smooth region (a sky gradient) over several clusters, so a small flat region
/// (a dark foreground) can hold the largest single cluster and win against a
/// far bigger sky. Density is measured with a fixed bandwidth, so a smooth
/// region keeps all its mass in one peak however it is shaded. Ties between
/// equally dense peaks go to the lowest histogram cell (darkest, then lowest
/// a, then lowest b), a rule that depends only on the colours, not on where
/// they sit in the frame. Fully deterministic, no random seeding.
pub fn cluster_color(img: &RgbaImage) -> [f32; 3] {
    let samples = sample_oklab(img);
    if samples.is_empty() {
        return EMPTY_FALLBACK;
    }

    let cell = |lab: &[f32; 3]| -> [usize; 3] {
        let bin = |v: f32, min: f32, bins: usize| -> usize {
            (((v - min) / CLUSTER_CELL).floor().max(0.0) as usize).min(bins - 1)
        };
        [
            bin(lab[0], 0.0, CLUSTER_L_BINS),
            bin(lab[1], CLUSTER_AB_MIN, CLUSTER_AB_BINS),
            bin(lab[2], CLUSTER_AB_MIN, CLUSTER_AB_BINS),
        ]
    };
    let dims = [CLUSTER_L_BINS, CLUSTER_AB_BINS, CLUSTER_AB_BINS];
    let index = |c: [usize; 3]| (c[0] * dims[1] + c[1]) * dims[2] + c[2];

    let mut density = vec![0f32; dims[0] * dims[1] * dims[2]];
    for sample in &samples {
        density[index(cell(&sample.lab))] += sample.weight;
    }
    for axis in 0..3 {
        density = smooth_axis(&density, dims, axis);
    }

    // First maximum in index order = lowest (L, a, b) cell on ties.
    let mut peak = 0;
    for (i, d) in density.iter().enumerate() {
        if *d > density[peak] {
            peak = i;
        }
    }
    let (pl, rest) = (peak / (dims[1] * dims[2]), peak % (dims[1] * dims[2]));
    let (pa, pb) = (rest / dims[2], rest % dims[2]);
    let mut centre = [
        (pl as f32 + 0.5) * CLUSTER_CELL,
        CLUSTER_AB_MIN + (pa as f32 + 0.5) * CLUSTER_CELL,
        CLUSTER_AB_MIN + (pb as f32 + 0.5) * CLUSTER_CELL,
    ];

    // Mean-shift from the peak cell: weighted mean of the nearby samples.
    let sigma = CLUSTER_SIGMA_CELLS * CLUSTER_CELL;
    for _ in 0..CLUSTER_MEAN_SHIFT_ITERATIONS {
        let mut sum = [0f64; 3];
        let mut total = 0f64;
        for sample in &samples {
            let d2 = distance_sq(&sample.lab, &centre);
            let w = (sample.weight * (-d2 / (2.0 * sigma * sigma)).exp()) as f64;
            total += w;
            for (acc, v) in sum.iter_mut().zip(sample.lab) {
                *acc += w * v as f64;
            }
        }
        if total <= f64::MIN_POSITIVE {
            break;
        }
        let next = [
            (sum[0] / total) as f32,
            (sum[1] / total) as f32,
            (sum[2] / total) as f32,
        ];
        let step = distance_sq(&centre, &next).sqrt();
        centre = next;
        if step < CLUSTER_CONVERGENCE {
            break;
        }
    }

    let rgb = Srgb::from_color(Oklab::new(centre[0], centre[1], centre[2]));
    [
        rgb.red.clamp(0.0, 1.0),
        rgb.green.clamp(0.0, 1.0),
        rgb.blue.clamp(0.0, 1.0),
    ]
}

/// One sampled pixel: its Oklab colour and its opacity as a weight, matching
/// how `average_color` weights by alpha.
struct Sample {
    lab: [f32; 3],
    weight: f32,
}

/// Grid sample of the visible pixels. Each grid cell reads one pixel at a
/// position hashed from the cell, so periodic content (stripes, fabric) is not
/// sampled at a constant phase; the values are real pixels, not blends.
fn sample_oklab(img: &RgbaImage) -> Vec<Sample> {
    let (width, height) = img.dimensions();
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let long = width.max(height);
    let scale = (CLUSTER_SAMPLE_LONG_SIDE as f64 / long as f64).min(1.0);
    let sample_w = ((width as f64 * scale).round() as u32).max(1);
    let sample_h = ((height as f64 * scale).round() as u32).max(1);
    let mut samples = Vec::with_capacity((sample_w * sample_h) as usize);
    for sy in 0..sample_h {
        let y0 = (sy as u64 * height as u64 / sample_h as u64) as u32;
        let y1 = (((sy as u64 + 1) * height as u64 / sample_h as u64) as u32).max(y0 + 1);
        for sx in 0..sample_w {
            let x0 = (sx as u64 * width as u64 / sample_w as u64) as u32;
            let x1 = (((sx as u64 + 1) * width as u64 / sample_w as u64) as u32).max(x0 + 1);
            let hash = splitmix64(((sy as u64) << 32) | sx as u64);
            let x = (x0 + (hash as u32) % (x1 - x0)).min(width - 1);
            let y = (y0 + ((hash >> 32) as u32) % (y1 - y0)).min(height - 1);
            let pixel = img.get_pixel(x, y);
            if pixel[3] == 0 {
                continue;
            }
            let lab = Oklab::from_color(Srgb::new(
                pixel[0] as f32 / 255.0,
                pixel[1] as f32 / 255.0,
                pixel[2] as f32 / 255.0,
            ));
            samples.push(Sample {
                lab: [lab.l, lab.a, lab.b],
                weight: pixel[3] as f32 / 255.0,
            });
        }
    }
    samples
}

fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Gaussian blur of the histogram along one axis (separable, so three passes
/// give the 3-D smoothing).
fn smooth_axis(src: &[f32], dims: [usize; 3], axis: usize) -> Vec<f32> {
    let kernel: Vec<f32> = (-CLUSTER_KERNEL_RADIUS..=CLUSTER_KERNEL_RADIUS)
        .map(|d| (-((d * d) as f32) / (2.0 * CLUSTER_SIGMA_CELLS * CLUSTER_SIGMA_CELLS)).exp())
        .collect();
    let strides = [dims[1] * dims[2], dims[2], 1];
    let mut out = vec![0f32; src.len()];
    for (i, value) in src.iter().enumerate() {
        if *value == 0.0 {
            continue;
        }
        let pos = (i / strides[axis]) % dims[axis];
        for (k, w) in kernel.iter().enumerate() {
            let target = pos as i32 + k as i32 - CLUSTER_KERNEL_RADIUS;
            if target < 0 || target >= dims[axis] as i32 {
                continue;
            }
            let j = i as isize + (target as isize - pos as isize) * strides[axis] as isize;
            out[j as usize] += value * w;
        }
    }
    out
}

fn distance_sq(a: &[f32; 3], b: &[f32; 3]) -> f32 {
    let dl = a[0] - b[0];
    let da = a[1] - b[1];
    let db = a[2] - b[2];
    dl * dl + da * da + db * db
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORANGE: [u8; 3] = [255, 140, 0];
    const BLUE: [u8; 3] = [0, 90, 220];

    fn split_image(w: u32, h: u32, first_rows: u32) -> RgbaImage {
        RgbaImage::from_fn(w, h, |_, y| {
            let c = if y < first_rows { ORANGE } else { BLUE };
            Rgba([c[0], c[1], c[2], 255])
        })
    }

    fn close(actual: [f32; 3], expected: [u8; 3]) -> bool {
        (0..3).all(|c| (actual[c] - expected[c] as f32 / 255.0).abs() < 0.01)
    }

    #[test]
    fn two_colour_image_returns_the_majority_colour_not_the_mean() {
        let img = split_image(200, 100, 60); // 60 % orange, 40 % blue
        let cluster = cluster_color(&img);
        assert!(close(cluster, ORANGE), "got {cluster:?}");
        let mean = average_color(&img);
        assert!(!close(mean, ORANGE) && !close(mean, BLUE));
        assert!(cluster.iter().zip(mean).any(|(c, m)| (c - m).abs() > 0.1));
    }

    #[test]
    fn minority_order_does_not_matter() {
        let img = split_image(200, 100, 40); // 40 % orange, 60 % blue
        assert!(close(cluster_color(&img), BLUE));
    }

    #[test]
    fn uniform_image_returns_its_colour() {
        let img = RgbaImage::from_pixel(37, 21, Rgba([12, 200, 99, 255]));
        assert!(close(cluster_color(&img), [12, 200, 99]));
    }

    #[test]
    fn repeated_runs_agree_exactly() {
        let img = RgbaImage::from_fn(150, 90, |x, y| {
            Rgba([
                (x * 7 % 256) as u8,
                (y * 13 % 256) as u8,
                ((x ^ y) % 256) as u8,
                255,
            ])
        });
        assert_eq!(cluster_color(&img), cluster_color(&img));
    }

    #[test]
    fn transparent_pixels_are_ignored() {
        let img = RgbaImage::from_fn(100, 100, |x, _| {
            if x < 90 {
                Rgba([0, 0, 0, 0])
            } else {
                Rgba([BLUE[0], BLUE[1], BLUE[2], 255])
            }
        });
        assert!(close(cluster_color(&img), BLUE));
    }

    #[test]
    fn degenerate_inputs_use_the_average_fallback() {
        assert_eq!(cluster_color(&RgbaImage::new(0, 0)), EMPTY_FALLBACK);
        let clear = RgbaImage::from_pixel(8, 8, Rgba([200, 10, 10, 0]));
        assert_eq!(cluster_color(&clear), EMPTY_FALLBACK);
        assert_eq!(cluster_color(&clear), average_color(&clear));
    }

    /// Review finding: a smooth sky must win against a smaller flat region even
    /// though a clustering that splits the sky would give the ground the largest
    /// single cluster.
    #[test]
    fn gradient_region_beats_a_smaller_flat_region() {
        for sky_percent in [71u32, 62, 54] {
            let (w, h) = (640u32, 480u32);
            let sky_rows = h * sky_percent / 100;
            let img = RgbaImage::from_fn(w, h, |x, y| {
                if y < sky_rows {
                    let t = x as f32 / (w - 1) as f32;
                    Rgba([(60.0 + 56.0 * t) as u8, (120.0 + 42.0 * t) as u8, 220, 255])
                } else {
                    Rgba([30, 25, 20, 255])
                }
            });
            let got = cluster_color(&img);
            // Must be a sky colour (blue channel high), not the near-black ground.
            assert!(
                got[2] > 0.7,
                "{sky_percent}% sky: expected a sky colour, got {got:?}"
            );
            assert!(got[0] > 0.2 && got[0] < 0.5, "got {got:?}");
        }
    }

    #[test]
    fn larger_flat_region_still_beats_a_smaller_gradient() {
        let (w, h) = (640u32, 480u32);
        let img = RgbaImage::from_fn(w, h, |x, y| {
            if y < h * 41 / 100 {
                let t = x as f32 / (w - 1) as f32;
                Rgba([(60.0 + 56.0 * t) as u8, (120.0 + 42.0 * t) as u8, 220, 255])
            } else {
                Rgba([30, 25, 20, 255])
            }
        });
        assert!(close(cluster_color(&img), [30, 25, 20]));
    }

    #[test]
    fn grey_ramp_returns_a_grey_from_the_ramp() {
        let img = RgbaImage::from_fn(64, 256, |_, y| Rgba([y as u8, y as u8, y as u8, 255]));
        let got = cluster_color(&img);
        assert!((got[0] - got[1]).abs() < 0.01 && (got[1] - got[2]).abs() < 0.01);
        assert!(got[0] > 0.1 && got[0] < 0.9, "got {got:?}");
    }

    #[test]
    fn equal_masses_resolve_independent_of_layout() {
        let top = split_image(200, 100, 50);
        let flipped = RgbaImage::from_fn(200, 100, |x, y| *top.get_pixel(x, 99 - y));
        assert_eq!(cluster_color(&top), cluster_color(&flipped));
    }

    #[test]
    fn periodic_stripes_are_not_aliased_to_one_phase() {
        // 5 px red / 5 px black stripes: both colours are half the photo, so the
        // answer must not depend on the sampling grid hitting only one of them
        // (the mean would be dark red, which is neither).
        let img = RgbaImage::from_fn(640, 480, |x, _| {
            if (x / 5) % 2 == 0 {
                Rgba([255, 0, 0, 255])
            } else {
                Rgba([0, 0, 0, 255])
            }
        });
        let got = cluster_color(&img);
        assert!(close(got, [255, 0, 0]) || close(got, [0, 0, 0]));
        // Tie-break is the darker cell; what matters is it is deterministic and
        // that both colours were seen (so a red-dominant variant picks red).
        let red_heavy = RgbaImage::from_fn(640, 480, |x, _| {
            if (x / 5) % 4 != 0 {
                Rgba([255, 0, 0, 255])
            } else {
                Rgba([0, 0, 0, 255])
            }
        });
        assert!(close(cluster_color(&red_heavy), [255, 0, 0]));
    }

    #[test]
    fn nearly_transparent_pixels_carry_little_weight() {
        // Left half: red at alpha 1/255. Right half: opaque blue.
        let img = RgbaImage::from_fn(100, 100, |x, _| {
            if x < 50 {
                Rgba([255, 0, 0, 1])
            } else {
                Rgba([BLUE[0], BLUE[1], BLUE[2], 255])
            }
        });
        assert!(close(cluster_color(&img), BLUE));
    }

    #[test]
    fn swatches_compute_each_statistic_once() {
        let img = split_image(40, 40, 24);
        let swatches = PhotoSwatches::new(&img);
        assert!(swatches.cluster.get().is_none());
        assert_eq!(swatches.cluster(), cluster_color(&img));
        assert_eq!(swatches.average(), average_color(&img));
    }
}
