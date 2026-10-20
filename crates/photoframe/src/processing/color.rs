use std::cell::OnceCell;

use image::{Rgba, RgbaImage};
use palette::{FromColor, Oklab, Srgb};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

/// Fallback swatch for images with no visible pixels.
const EMPTY_FALLBACK: [f32; 3] = [0.1, 0.1, 0.1];
/// Long-side sample size for `cluster_color`; the result is a statistic, so
/// a coarse nearest-neighbour grid is enough.
const CLUSTER_SAMPLE_LONG_SIDE: u32 = 64;
const CLUSTER_K: usize = 4;
const CLUSTER_MAX_ITERATIONS: usize = 16;
/// Largest centroid movement (Oklab units, L spans 0..1) that still counts as
/// converged.
const CLUSTER_CONVERGENCE: f32 = 1e-4;
const CLUSTER_SEED: u64 = 0x70_68_6f_74_6f_63_6c_75; // "photoclu"

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

/// The dominant colour of the photo: k-means (k = 4) over a coarse sample in
/// Oklab, returning the centroid of the cluster holding the most pixels. Unlike
/// `average_color`, the result is a colour the photo actually contains rather
/// than the blend of its regions. Deterministic: the seeding is fixed.
pub fn cluster_color(img: &RgbaImage) -> [f32; 3] {
    let points = sample_oklab(img);
    if points.is_empty() {
        return EMPTY_FALLBACK;
    }
    let mut centroids = seed_centroids(&points);
    let mut assignment = vec![0usize; points.len()];
    let mut counts = vec![0usize; centroids.len()];

    for _ in 0..CLUSTER_MAX_ITERATIONS {
        counts.iter_mut().for_each(|c| *c = 0);
        let mut sums = vec![[0f64; 3]; centroids.len()];
        for (point, slot) in points.iter().zip(assignment.iter_mut()) {
            let nearest = nearest_centroid(&centroids, point);
            *slot = nearest;
            counts[nearest] += 1;
            for (sum, value) in sums[nearest].iter_mut().zip(point) {
                *sum += *value as f64;
            }
        }
        let mut shift = 0f32;
        for (index, centroid) in centroids.iter_mut().enumerate() {
            if counts[index] == 0 {
                continue;
            }
            let n = counts[index] as f64;
            let updated = [
                (sums[index][0] / n) as f32,
                (sums[index][1] / n) as f32,
                (sums[index][2] / n) as f32,
            ];
            shift = shift.max(distance_sq(centroid, &updated).sqrt());
            *centroid = updated;
        }
        if shift < CLUSTER_CONVERGENCE {
            break;
        }
    }

    // Final masses against the settled centroids; ties go to the lowest index.
    counts.iter_mut().for_each(|c| *c = 0);
    for point in &points {
        counts[nearest_centroid(&centroids, point)] += 1;
    }
    let mut best = 0;
    for (index, count) in counts.iter().enumerate() {
        if *count > counts[best] {
            best = index;
        }
    }

    let [l, a, b] = centroids[best];
    let rgb = Srgb::from_color(Oklab::new(l, a, b));
    [
        rgb.red.clamp(0.0, 1.0),
        rgb.green.clamp(0.0, 1.0),
        rgb.blue.clamp(0.0, 1.0),
    ]
}

/// Nearest-neighbour grid over the visible pixels, as Oklab triples.
fn sample_oklab(img: &RgbaImage) -> Vec<[f32; 3]> {
    let (width, height) = img.dimensions();
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let long = width.max(height);
    let scale = (CLUSTER_SAMPLE_LONG_SIDE as f64 / long as f64).min(1.0);
    let sample_w = ((width as f64 * scale).round() as u32).max(1);
    let sample_h = ((height as f64 * scale).round() as u32).max(1);
    let mut points = Vec::with_capacity((sample_w * sample_h) as usize);
    for sy in 0..sample_h {
        let y = (((sy as f64 + 0.5) * height as f64 / sample_h as f64) as u32).min(height - 1);
        for sx in 0..sample_w {
            let x = (((sx as f64 + 0.5) * width as f64 / sample_w as f64) as u32).min(width - 1);
            let pixel = img.get_pixel(x, y);
            if pixel[3] == 0 {
                continue;
            }
            let lab = Oklab::from_color(Srgb::new(
                pixel[0] as f32 / 255.0,
                pixel[1] as f32 / 255.0,
                pixel[2] as f32 / 255.0,
            ));
            points.push([lab.l, lab.a, lab.b]);
        }
    }
    points
}

/// k-means++ seeding with a fixed seed. Stops early when the remaining points
/// all coincide with a chosen centroid, so fewer than `CLUSTER_K` centroids
/// come back for images with fewer distinct colours.
fn seed_centroids(points: &[[f32; 3]]) -> Vec<[f32; 3]> {
    let mut rng = StdRng::seed_from_u64(CLUSTER_SEED);
    let mut centroids = Vec::with_capacity(CLUSTER_K);
    centroids.push(points[rng.random_range(0..points.len())]);
    let mut nearest_sq: Vec<f32> = points
        .iter()
        .map(|p| distance_sq(p, &centroids[0]))
        .collect();
    while centroids.len() < CLUSTER_K {
        let total: f64 = nearest_sq.iter().map(|d| *d as f64).sum();
        if total <= 1e-12 {
            break;
        }
        let mut target = rng.random::<f64>() * total;
        let mut chosen = points.len() - 1;
        for (index, d) in nearest_sq.iter().enumerate() {
            target -= *d as f64;
            if target <= 0.0 && *d > 0.0 {
                chosen = index;
                break;
            }
        }
        let next = points[chosen];
        for (d, p) in nearest_sq.iter_mut().zip(points) {
            *d = d.min(distance_sq(p, &next));
        }
        centroids.push(next);
    }
    centroids
}

fn nearest_centroid(centroids: &[[f32; 3]], point: &[f32; 3]) -> usize {
    let mut best = 0;
    let mut best_d = f32::INFINITY;
    for (index, centroid) in centroids.iter().enumerate() {
        let d = distance_sq(centroid, point);
        if d < best_d {
            best = index;
            best_d = d;
        }
    }
    best
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

    #[test]
    fn swatches_compute_each_statistic_once() {
        let img = split_image(40, 40, 24);
        let swatches = PhotoSwatches::new(&img);
        assert!(swatches.cluster.get().is_none());
        assert_eq!(swatches.cluster(), cluster_color(&img));
        assert_eq!(swatches.average(), average_color(&img));
    }
}
