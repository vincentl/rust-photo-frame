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
/// Bandwidth (Gaussian sigma, in cells) of the fine density that picks the
/// representative colour: 0.05 Oklab, a little over two just-noticeable steps.
const CLUSTER_FINE_SIGMA_CELLS: f32 = 2.0;
/// Bandwidth of the coarse density that decides which region dominates: 0.15
/// Oklab, wide enough that a shaded sky (a gradient several tenths long) is
/// one region. Also the radius around the winning peak searched for the
/// representative colour.
const CLUSTER_COARSE_SIGMA_CELLS: f32 = 6.0;
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

/// The dominant colour of the photo: the representative colour of the region
/// that holds the most of it.
///
/// Pixels are sampled on a coarse grid, placed in Oklab (distance there
/// approximates perceived difference) and binned into a 3-D histogram. Two
/// questions are answered at two scales:
///
/// 1. *Which region dominates?* A coarse question: each colour is scored by the
///    mass within about 0.15 Oklab of it (a wide Gaussian density), so a shaded
///    sky is one region however far its gradient runs, and it is compared
///    fairly against a small flat region. The densest coarse point wins.
/// 2. *Which colour represents it?* A fine question: inside the winning region
///    (cells within the coarse bandwidth of that peak) take the densest cell of
///    a narrow density (0.05) and refine it by mean-shift to the weighted mean
///    of its immediate neighbourhood. The answer is the centre of a real
///    concentration of pixels in the photo, not a point between distant
///    regions (that is `average_color`).
///
/// Why not "the largest k-means cluster": k-means splits a smooth region over
/// several clusters, so a small flat region can win against a far bigger sky.
/// Why not a single bandwidth: wide enough to keep a sunset gradient whole, it
/// would blend neighbouring hues; narrow enough to keep hues apart, it splits
/// the gradient into weak peaks. Two distinct hues closer than the coarse
/// bandwidth count as one region and report the representative of the denser
/// one (or, on a tie, the lowest histogram cell: darkest, then lowest a, then
/// lowest b, a rule that depends on the colours, never on where they sit in the
/// frame); mean-shift may stop a hair short of its fixed point at the iteration
/// cap, which moves the result by well under one histogram cell. Fully
/// deterministic, no random seeding.
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
    let unindex = |i: usize| {
        let plane = dims[1] * dims[2];
        [i / plane, (i % plane) / dims[2], i % dims[2]]
    };
    let smooth = |hist: &[f32], sigma_cells: f32| -> Vec<f32> {
        let mut out = hist.to_vec();
        for axis in 0..3 {
            out = smooth_axis(&out, dims, axis, sigma_cells);
        }
        out
    };
    // First maximum in index order = lowest (L, a, b) cell on ties.
    let argmax = |values: &[f32], allowed: &dyn Fn(usize) -> bool| -> usize {
        let mut best = usize::MAX;
        for (i, v) in values.iter().enumerate() {
            if allowed(i) && (best == usize::MAX || *v > values[best]) {
                best = i;
            }
        }
        best
    };

    let mut hist = vec![0f32; dims[0] * dims[1] * dims[2]];
    for sample in &samples {
        hist[index(cell(&sample.lab))] += sample.weight;
    }

    // 1. Dominant region: the peak of the coarse density.
    let coarse = smooth(&hist, CLUSTER_COARSE_SIGMA_CELLS);
    let region_peak = unindex(argmax(&coarse, &|_| true));

    // 2. Representative colour: the densest fine cell within the region.
    let fine = smooth(&hist, CLUSTER_FINE_SIGMA_CELLS);
    let in_region = |i: usize| {
        let c = unindex(i);
        let d2: f32 = (0..3)
            .map(|k| {
                let d = c[k] as f32 - region_peak[k] as f32;
                d * d
            })
            .sum();
        d2 <= CLUSTER_COARSE_SIGMA_CELLS * CLUSTER_COARSE_SIGMA_CELLS
    };
    let peak = unindex(argmax(&fine, &in_region));
    let mut centre = [
        (peak[0] as f32 + 0.5) * CLUSTER_CELL,
        CLUSTER_AB_MIN + (peak[1] as f32 + 0.5) * CLUSTER_CELL,
        CLUSTER_AB_MIN + (peak[2] as f32 + 0.5) * CLUSTER_CELL,
    ];

    // Mean-shift from there: weighted mean of the nearby samples.
    let sigma = CLUSTER_FINE_SIGMA_CELLS * CLUSTER_CELL;
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
fn smooth_axis(src: &[f32], dims: [usize; 3], axis: usize, sigma_cells: f32) -> Vec<f32> {
    let radius = (2.0 * sigma_cells).ceil() as i32;
    let kernel: Vec<f32> = (-radius..=radius)
        .map(|d| (-((d * d) as f32) / (2.0 * sigma_cells * sigma_cells)).exp())
        .collect();
    let strides = [dims[1] * dims[2], dims[2], 1];
    let mut out = vec![0f32; src.len()];
    for (i, value) in src.iter().enumerate() {
        if *value == 0.0 {
            continue;
        }
        let pos = (i / strides[axis]) % dims[axis];
        for (k, w) in kernel.iter().enumerate() {
            let target = pos as i32 + k as i32 - radius;
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

    fn oklab(rgb: [f32; 3]) -> [f32; 3] {
        let lab = Oklab::from_color(Srgb::new(rgb[0], rgb[1], rgb[2]));
        [lab.l, lab.a, lab.b]
    }

    fn delta_e(a: [f32; 3], b: [f32; 3]) -> f32 {
        distance_sq(&oklab(a), &oklab(b)).sqrt()
    }

    fn mix(from: [u8; 3], to: [u8; 3], t: f32) -> [u8; 3] {
        let ch = |k: usize| (from[k] as f32 + (to[k] as f32 - from[k] as f32) * t).round() as u8;
        [ch(0), ch(1), ch(2)]
    }

    /// Sky (a left-to-right gradient from `from` to `to`) over a flat dark
    /// ground; `sky_percent` of the rows are sky.
    fn sky_over_ground(from: [u8; 3], to: [u8; 3], ground: [u8; 3], sky_percent: u32) -> RgbaImage {
        let (w, h) = (640u32, 480u32);
        RgbaImage::from_fn(w, h, |x, y| {
            let c = if y < h * sky_percent / 100 {
                mix(from, to, x as f32 / (w - 1) as f32)
            } else {
                ground
            };
            Rgba([c[0], c[1], c[2], 255])
        })
    }

    /// Distance from `got` to the nearest colour on the sky gradient.
    fn distance_to_gradient(got: [f32; 3], from: [u8; 3], to: [u8; 3]) -> f32 {
        let f = |c: [u8; 3]| {
            [
                c[0] as f32 / 255.0,
                c[1] as f32 / 255.0,
                c[2] as f32 / 255.0,
            ]
        };
        (0..=100)
            .map(|i| delta_e(got, f(mix(from, to, i as f32 / 100.0))))
            .fold(f32::INFINITY, f32::min)
    }

    /// Wide gradients (the review's measured cases): the sky must still win,
    /// and the answer must be a colour that lies on the sky gradient.
    #[test]
    fn wide_sky_gradients_beat_a_smaller_dark_ground() {
        let f = |c: [u8; 3]| {
            [
                c[0] as f32 / 255.0,
                c[1] as f32 / 255.0,
                c[2] as f32 / 255.0,
            ]
        };
        let ground = [15, 12, 10];
        let cases: [([u8; 3], [u8; 3], u32, f32); 3] = [
            // zenith-to-horizon sky, delta E ~0.39
            ([10, 30, 110], [150, 190, 235], 65, 0.35),
            // sunset, delta E ~0.54
            ([20, 40, 120], [250, 160, 60], 75, 0.5),
            // dusk glow, delta E ~0.72
            ([10, 10, 60], [255, 200, 120], 85, 0.65),
        ];
        for (from, to, sky_percent, min_span) in cases {
            assert!(
                delta_e(f(from), f(to)) > min_span,
                "scenario no longer spans the stated delta E"
            );
            let got = cluster_color(&sky_over_ground(from, to, ground, sky_percent));
            assert!(
                distance_to_gradient(got, from, to) < 0.05,
                "{from:?}->{to:?} at {sky_percent}%: not a sky colour: {got:?}"
            );
            assert!(delta_e(got, f(ground)) > 0.15, "got the ground: {got:?}");
        }
    }

    /// Two hues closer than the coarse bandwidth are one region; the answer is
    /// the representative of one of them (the lower cell on this exact tie), not
    /// the blend of the two.
    #[test]
    fn near_hues_report_one_of_them_not_the_blend() {
        let red = [220u8, 30, 30];
        let orange = [240u8, 110, 20];
        let img = RgbaImage::from_fn(200, 100, |_, y| {
            let c = if y < 50 { red } else { orange };
            Rgba([c[0], c[1], c[2], 255])
        });
        let got = cluster_color(&img);
        let f = |c: [u8; 3]| {
            [
                c[0] as f32 / 255.0,
                c[1] as f32 / 255.0,
                c[2] as f32 / 255.0,
            ]
        };
        let nearest = delta_e(got, f(red)).min(delta_e(got, f(orange)));
        assert!(nearest < 0.03, "expected one of the two hues, got {got:?}");
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
