//! Per-frame constants for `viewer_quad.wgsl`, computed once per frame on
//! the CPU instead of once per pixel on the GPU.
//!
//! At 4 threads per QPU the V3D cost of a transition is its instructions per
//! pixel (Pi measurement, 2026-10-21: a fade's 176 instructions at half-res
//! plus the 4K upscale took 17-20 ms of a 16.7 ms frame). Anything that is
//! the same for every pixel of a frame belongs here: where each photo plane
//! lands in texture coordinates (an affine map of the screen, so the vertex
//! stage interpolates it and the fragment stage does no division), and each
//! transition's edges, centers and scales. Every function states the
//! per-pixel formula it replaces; the tests check them against it.

// Pixel rectangles are (x, y, w, h), as `image_ops::rect_to_uniform` gives.
pub(super) type Rect = [f32; 4];

/// The texture-coordinate map of a photo plane drawn at `dest`:
/// `uv = screen_uv * xy + zw`, which equals the old per-pixel
/// `(screen_uv * screen - dest.xy) / dest.zw`. A plane with no area maps
/// every pixel to (-1, -1), outside the plane, which is what the old
/// `dest.z <= 0 || dest.w <= 0` guard returned (transparent).
pub(super) fn plane_xform(dest: Rect, screen: [f32; 2]) -> [f32; 4] {
    if dest[2] <= 0.0 || dest[3] <= 0.0 {
        return [0.0, 0.0, -1.0, -1.0];
    }
    [
        screen[0] / dest[2],
        screen[1] / dest[3],
        -dest[0] / dest[2],
        -dest[1] / dest[3],
    ]
}

/// The two smoothstep edges that reproduce a mask with explicit ends:
/// `mask = 1 - smoothstep(e0, e1, x)` for x in 0..1 is exactly 0 with
/// (-2, -1) and exactly 1 with (2, 3).
const MASK_ZERO: [f32; 2] = [-2.0, -1.0];
const MASK_ONE: [f32; 2] = [2.0, 3.0];

/// Wipe. Replaces, per pixel:
/// `normalized = clamp((dot(normal, pos) - min_proj) * inv_span, 0, 1)` with
/// `pos = screen_uv * screen`, and the progress branches around
/// `1 - smoothstep(leading, end, normalized)`.
/// Returns `(params0, params1)`: `normalized = clamp(dot(params0.xy,
/// screen_uv) + params0.z, 0, 1)`, `mask = 1 - smoothstep(params1.x,
/// params1.y, normalized)`.
pub(super) fn wipe(
    normal: [f32; 2],
    min_proj: f32,
    inv_span: f32,
    softness: f32,
    progress: f32,
    screen: [f32; 2],
) -> ([f32; 4], [f32; 4]) {
    let progress = progress.clamp(0.0, 1.0);
    let softness = softness.clamp(0.0, 0.5);
    let edges = if progress <= 0.0 {
        MASK_ZERO
    } else if progress >= 1.0 {
        MASK_ONE
    } else {
        let leading = (progress - softness).clamp(0.0, progress);
        let trailing = (progress + softness).clamp(progress, 1.0);
        [leading, trailing.max(leading + 1e-3)]
    };
    (
        [
            normal[0] * screen[0] * inv_span,
            normal[1] * screen[1] * inv_span,
            -min_proj * inv_span,
            0.0,
        ],
        [edges[0], edges[1], 0.0, 0.0],
    )
}

/// Push. Replaces sampling the current plane at `pos - translation *
/// progress` and the next at `pos + translation * (1 - progress)`: the
/// planes themselves move. Returns the moved `(current, next)` rectangles.
pub(super) fn push(
    current: Rect,
    next: Rect,
    translation: [f32; 2],
    progress: f32,
) -> (Rect, Rect) {
    let progress = progress.clamp(0.0, 1.0);
    let shift = |r: Rect, by: f32| {
        [
            r[0] + translation[0] * by,
            r[1] + translation[1] * by,
            r[2],
            r[3],
        ]
    };
    (shift(current, progress), shift(next, progress - 1.0))
}

/// Crossfade-zoom. Replaces sampling at `center + (pos - center) / scale`
/// with `scale = 1 + zoom * sin(pi * progress)` for a plane that zooms
/// (else 1): a plane drawn at `dest` and sampled that way is the plane drawn
/// at `dest` scaled by `scale` about the screen center. Returns the zoomed
/// planes' texture maps `(current, next)` (`plane_xform`).
pub(super) fn crossfade_zoom(
    current: Rect,
    next: Rect,
    zoom: f32,
    current_zooms_in: bool,
    next_zooms_in: bool,
    progress: f32,
    screen: [f32; 2],
) -> ([f32; 4], [f32; 4]) {
    let progress = progress.clamp(0.0, 1.0);
    let bump = zoom * (std::f32::consts::PI * progress).sin();
    let center = [screen[0] * 0.5, screen[1] * 0.5];
    let zoomed = |r: Rect, zooms: bool| {
        let scale = if zooms { 1.0 + bump } else { 1.0 }.max(1e-3);
        [
            center[0] + (r[0] - center[0]) * scale,
            center[1] + (r[1] - center[1]) * scale,
            r[2] * scale,
            r[3] * scale,
        ]
    };
    (
        plane_xform(zoomed(current, current_zooms_in), screen),
        plane_xform(zoomed(next, next_zooms_in), screen),
    )
}

/// Radial wipe. Replaces, per pixel: `dv = pos - center_uv * screen`,
/// `dist = length(dv) / max(length(screen), 1e-3)` (or `|dv.x| + |dv.y|`
/// over the same for a diamond), and the clamped edges around it. Returns
/// `(params0, params1)`: `dv = screen_uv * params0.xy - params0.zw` is
/// already in units of the screen diagonal; `params1 = (lo, hi, diamond)`.
pub(super) fn radial_wipe(
    center_uv: [f32; 2],
    softness: f32,
    diamond: bool,
    progress: f32,
    screen: [f32; 2],
) -> ([f32; 4], [f32; 4]) {
    let progress = progress.clamp(0.0, 1.0);
    let softness = softness.clamp(0.0, 0.5);
    let max_r = (screen[0] * screen[0] + screen[1] * screen[1])
        .sqrt()
        .max(1e-3);
    let lo = (progress - softness).clamp(0.0, 1.0);
    let hi = (progress + softness).clamp(0.0, 1.0);
    (
        [
            screen[0] / max_r,
            screen[1] / max_r,
            center_uv[0] * screen[0] / max_r,
            center_uv[1] * screen[1] / max_r,
        ],
        [lo, hi.max(lo + 1e-3), if diamond { 1.0 } else { 0.0 }, 0.0],
    )
}

/// Venetian blinds. Replaces, per pixel, picking the axis
/// (`select(uv.y, uv.x, vertical)`) and the edges `progress -/+ softness`.
/// Returns `(params0, params1)`: `params0 = (stripes, e0, e1)`,
/// `params1.xy` the axis as a unit vector.
pub(super) fn venetian_blinds(
    stripe_count: u32,
    softness: f32,
    vertical: bool,
    progress: f32,
) -> ([f32; 4], [f32; 4]) {
    let progress = progress.clamp(0.0, 1.0);
    let softness = softness.clamp(0.001, 0.5);
    (
        [
            stripe_count.max(1) as f32,
            progress - softness,
            progress + softness,
            0.0,
        ],
        if vertical {
            [1.0, 0.0, 0.0, 0.0]
        } else {
            [0.0, 1.0, 0.0, 0.0]
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: [f32; 2] = [3840.0, 2160.0];

    fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
        let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }

    /// Screen-uv sample points across and beyond the screen.
    fn points() -> Vec<[f32; 2]> {
        let mut v = Vec::new();
        for i in 0..=12 {
            for j in 0..=12 {
                v.push([i as f32 / 12.0, j as f32 / 12.0]);
            }
        }
        v
    }

    fn close(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol * (1.0 + a.abs().max(b.abs()))
    }

    #[test]
    fn plane_xform_is_the_old_division() {
        for dest in [
            [0.0, 0.0, 3840.0, 2160.0],
            [320.0, -40.0, 3200.0, 2240.0],
            [12.5, 7.25, 1000.0, 600.0],
        ] {
            let x = plane_xform(dest, SCREEN);
            for uv in points() {
                let pos = [uv[0] * SCREEN[0], uv[1] * SCREEN[1]];
                let old = [(pos[0] - dest[0]) / dest[2], (pos[1] - dest[1]) / dest[3]];
                let new = [uv[0] * x[0] + x[2], uv[1] * x[1] + x[3]];
                assert!(close(old[0], new[0], 1e-5) && close(old[1], new[1], 1e-5));
            }
        }
        // No area: always outside (the old guard's transparent result).
        let x = plane_xform([0.0; 4], SCREEN);
        for uv in points() {
            let new = [uv[0] * x[0] + x[2], uv[1] * x[1] + x[3]];
            assert!(new[0] < 0.0 && new[1] < 0.0);
        }
    }

    #[test]
    fn wipe_mask_matches_the_old_branches() {
        let normal = [0.8f32, 0.6];
        let (min_proj, inv_span) = (-120.0, 1.0 / 4300.0);
        for progress in [0.0, 0.2, 0.5, 0.97, 1.0] {
            let (p0, p1) = wipe(normal, min_proj, inv_span, 0.12, progress, SCREEN);
            for uv in points() {
                let pos = [uv[0] * SCREEN[0], uv[1] * SCREEN[1]];
                let n_old = ((normal[0] * pos[0] + normal[1] * pos[1] - min_proj) * inv_span)
                    .clamp(0.0, 1.0);
                let old = if progress <= 0.0 {
                    0.0
                } else if progress >= 1.0 {
                    1.0
                } else {
                    let leading = (progress - 0.12f32).clamp(0.0, progress);
                    let trailing = (progress + 0.12f32).clamp(progress, 1.0);
                    1.0 - smoothstep(leading, trailing.max(leading + 1e-3), n_old)
                };
                let n_new = (p0[0] * uv[0] + p0[1] * uv[1] + p0[2]).clamp(0.0, 1.0);
                let new = 1.0 - smoothstep(p1[0], p1[1], n_new);
                assert!(
                    (old - new).abs() < 1e-4,
                    "p={progress} uv={uv:?}: {old} vs {new}"
                );
            }
        }
    }

    #[test]
    fn push_moves_the_planes_as_the_old_sample_offsets_did() {
        let cur = [0.0, 0.0, 3840.0, 2160.0];
        let nxt = [100.0, 0.0, 3640.0, 2160.0];
        let t = [4406.0, -500.0];
        for progress in [0.0, 0.3, 1.0] {
            let (c, n) = push(cur, nxt, t, progress);
            let (xc, xn) = (plane_xform(c, SCREEN), plane_xform(n, SCREEN));
            for uv in points() {
                let pos = [uv[0] * SCREEN[0], uv[1] * SCREEN[1]];
                let old_c = [
                    (pos[0] - t[0] * progress - cur[0]) / cur[2],
                    (pos[1] - t[1] * progress - cur[1]) / cur[3],
                ];
                let old_n = [
                    (pos[0] + t[0] * (1.0 - progress) - nxt[0]) / nxt[2],
                    (pos[1] + t[1] * (1.0 - progress) - nxt[1]) / nxt[3],
                ];
                let new_c = [uv[0] * xc[0] + xc[2], uv[1] * xc[1] + xc[3]];
                let new_n = [uv[0] * xn[0] + xn[2], uv[1] * xn[1] + xn[3]];
                for k in 0..2 {
                    assert!(close(old_c[k], new_c[k], 1e-5), "{old_c:?} {new_c:?}");
                    assert!(close(old_n[k], new_n[k], 1e-5), "{old_n:?} {new_n:?}");
                }
            }
        }
    }

    #[test]
    fn crossfade_zoom_maps_match_the_old_sampling() {
        let cur = [0.0, -20.0, 3840.0, 2200.0];
        let nxt = [200.0, 0.0, 3440.0, 2160.0];
        for (progress, cur_in, next_in) in
            [(0.0, true, false), (0.5, true, true), (0.8, false, true)]
        {
            let (xc, xn) = crossfade_zoom(cur, nxt, 0.15, cur_in, next_in, progress, SCREEN);
            let bump = 0.15f32 * (std::f32::consts::PI * progress).sin();
            let center = [SCREEN[0] * 0.5, SCREEN[1] * 0.5];
            for uv in points() {
                let pos = [uv[0] * SCREEN[0], uv[1] * SCREEN[1]];
                for (dest, zooms, x) in [(cur, cur_in, xc), (nxt, next_in, xn)] {
                    let scale: f32 = if zooms { 1.0 + bump } else { 1.0 };
                    let sample = [
                        center[0] + (pos[0] - center[0]) / scale,
                        center[1] + (pos[1] - center[1]) / scale,
                    ];
                    let old = [
                        (sample[0] - dest[0]) / dest[2],
                        (sample[1] - dest[1]) / dest[3],
                    ];
                    let new = [uv[0] * x[0] + x[2], uv[1] * x[1] + x[3]];
                    assert!(close(old[0], new[0], 1e-4) && close(old[1], new[1], 1e-4));
                }
            }
        }
    }

    #[test]
    fn radial_wipe_distance_and_edges_match() {
        for (diamond, center) in [(false, [0.5f32, 0.5]), (true, [0.3, 0.7])] {
            for progress in [0.0, 0.4, 1.0] {
                let (p0, p1) = radial_wipe(center, 0.1, diamond, progress, SCREEN);
                let max_r = (SCREEN[0] * SCREEN[0] + SCREEN[1] * SCREEN[1]).sqrt();
                for uv in points() {
                    let pos = [uv[0] * SCREEN[0], uv[1] * SCREEN[1]];
                    let dv = [
                        pos[0] - center[0] * SCREEN[0],
                        pos[1] - center[1] * SCREEN[1],
                    ];
                    let old_dist = if diamond {
                        (dv[0].abs() + dv[1].abs()) / max_r
                    } else {
                        (dv[0] * dv[0] + dv[1] * dv[1]).sqrt() / max_r
                    };
                    let lo = (progress - 0.1f32).clamp(0.0, 1.0);
                    let hi = (progress + 0.1f32).clamp(0.0, 1.0);
                    let old = 1.0 - smoothstep(lo, hi.max(lo + 1e-3), old_dist);
                    let ndv = [uv[0] * p0[0] - p0[2], uv[1] * p0[1] - p0[3]];
                    let new_dist = if p1[2] > 0.5 {
                        ndv[0].abs() + ndv[1].abs()
                    } else {
                        (ndv[0] * ndv[0] + ndv[1] * ndv[1]).sqrt()
                    };
                    let new = 1.0 - smoothstep(p1[0], p1[1], new_dist);
                    assert!((old - new).abs() < 1e-4, "{old} vs {new}");
                }
            }
        }
    }

    #[test]
    fn venetian_axis_and_edges_match() {
        for vertical in [false, true] {
            let (p0, p1) = venetian_blinds(7, 0.08, vertical, 0.45);
            for uv in points() {
                let old_axis = if vertical { uv[0] } else { uv[1] };
                let new_axis = uv[0] * p1[0] + uv[1] * p1[1];
                assert_eq!(old_axis, new_axis);
            }
            assert_eq!(p0[0], 7.0);
            assert!((p0[1] - (0.45 - 0.08)).abs() < 1e-7);
            assert!((p0[2] - (0.45 + 0.08)).abs() < 1e-7);
        }
    }
}
