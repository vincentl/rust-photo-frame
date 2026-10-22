// One fragment entry point per transition kind (`fs_<kind>`), each with
// only its own math, compiled into its own render pipeline and chosen on the
// CPU (`DrawKind` in viewer.rs). The V3D compiler allocates registers for the
// whole shader: one `switch` over every kind made each kind pay the register
// pressure of the heaviest (975 instructions, 47 temporaries), which forced
// the shader down to 2 threads per QPU. V3D hides texture latency by
// switching among 4 threads, so every transition, even a fade, ran its
// texture fetches at a fraction of the speed (Pi measurement, 2026-10-21).
// Keep each entry point small enough to compile at 4 threads; check with
// V3D_DEBUG=shaderdb ("4 threads", no "Falling back" lines).
//
// At 4 threads the cost is instructions per pixel, so everything that is
// the same for every pixel of a frame is computed on the CPU
// (`draw_params.rs`): where each photo plane lands in texture coordinates
// (an affine map of the screen, interpolated by the vertex stage, so the
// fragment stage does no division), and each kind's per-frame constants.
// A plane is sampled without branches (`plane`).

struct TransitionUniforms {
  screen_size: vec2<f32>,
  progress: f32,
  // TransitionKind::as_index (kept for the CPU's bookkeeping; the shaders
  // do not branch on it).
  kind: u32,
  // Where the current / next photo plane lands: texture coordinate =
  // screen_uv * xform.xy + xform.zw (a plane that is not there maps every
  // pixel outside 0..1).
  current_xform: vec4<f32>,
  next_xform: vec4<f32>,
  params0: vec4<f32>,
  params1: vec4<f32>,
  params3: vec4<f32>,
  // Viewer background (linear RGB, alpha unused); letterbox regions are
  // composited over this in-shader so the pipeline can render opaquely.
  background: vec4<f32>,
};

// Per-petal constants for the iris transition, solved on the CPU each frame
// (see the Iris arm in viewer.rs). Only fs_iris_layer reads them; a separate
// uniform so no other pipeline loads them.
// petals_a[i] = (annulus_center.xy, tip_dir.xy)
// petals_b[i] = (trail_dir.xy, facing_cosine, unused)
struct IrisPetals {
  petals_a: array<vec4<f32>, 16>,
  petals_b: array<vec4<f32>, 16>,
};

@group(0) @binding(0)
var<uniform> U: TransitionUniforms;
@group(0) @binding(1)
var<uniform> P: IrisPetals;

@group(1) @binding(0)
var cur_tex: texture_2d<f32>;
@group(1) @binding(1)
var cur_samp: sampler;

@group(2) @binding(0)
var next_tex: texture_2d<f32>;
@group(2) @binding(1)
var next_samp: sampler;

// Reduced-resolution iris petal layer rendered by fs_iris_layer (premultiplied
// alpha). Bound to the blank texture when no iris transition is active.
@group(3) @binding(0)
var petal_tex: texture_2d<f32>;
@group(3) @binding(1)
var petal_samp: sampler;

struct VSOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) screen_uv: vec2<f32>,
  // Texture coordinates of the current / next photo plane (affine in the
  // screen, so interpolating them is exact).
  @location(1) current_uv: vec2<f32>,
  @location(2) next_uv: vec2<f32>,
};

// One triangle covering the screen (draw 3 vertices): (-1,-1), (3,-1),
// (-1,3). Computed from the vertex index with bit operations, so there is
// no dynamically indexed local array (V3D demotes those to scratch memory),
// and no diagonal seam between two triangles.
@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VSOut {
  let p = vec2<f32>(f32((vid << 1u) & 2u), f32(vid & 2u)) * 2.0 - 1.0;
  var out: VSOut;
  out.pos = vec4<f32>(p, 0.0, 1.0);
  out.screen_uv = vec2<f32>((p.x + 1.0) * 0.5, (1.0 - p.y) * 0.5);
  out.current_uv = out.screen_uv * U.current_xform.xy + U.current_xform.zw;
  out.next_uv = out.screen_uv * U.next_xform.xy + U.next_xform.zw;
  return out;
}

// A photo plane at texture coordinate `uv`: its color with coverage 1
// inside the plane, transparent outside. Branchless: the sample is always
// taken (the sampler clamps to the edge) and the coverage is a select.
fn plane(tex: texture_2d<f32>, samp: sampler, uv: vec2<f32>) -> vec4<f32> {
  let c = textureSample(tex, samp, uv);
  let inside = all(uv >= vec2<f32>(0.0)) && all(uv <= vec2<f32>(1.0));
  return select(vec4<f32>(0.0), vec4<f32>(c.rgb, 1.0), inside);
}

// Signed distance to iris petal `i` using the CPU-precomputed constants in
// P.petals_a / P.petals_b. Returns (distance, radius about the petal's
// annulus center). On-screen the only petal boundaries are the inner arc and
// the two end caps, so this band+cap distance is exact for visible pixels.
fn iris_petal(i: i32, p: vec2<f32>, r_mid: f32, w: f32) -> vec2<f32> {
  let a = P.petals_a[i];
  let b = P.petals_b[i];
  let v = p - a.xy;
  let r = length(v);
  var d: f32;
  if (a.z * v.y - a.w * v.x > 0.0) {
    // Past the tip: distance to the tip cap.
    d = length(v - r_mid * a.zw) - w;
  } else if (b.x * v.y - b.y * v.x < 0.0) {
    // Behind the pivot: distance to the trailing cap (always off-screen, so
    // this only needs to be a positive lower bound here).
    d = length(v - r_mid * b.xy) - w;
  } else {
    // Within the petal's angular span: the annular band.
    d = abs(r - r_mid) - w;
  }
  return vec2<f32>(d, r);
}

// Renders the iris petals (shape + shading, premultiplied alpha) into a
// reduced-resolution offscreen layer. Coordinates derive from screen_uv and
// U.screen_size, so the same math works at any target resolution; the main
// pass upsamples the result. At 4K the petal arithmetic is far too heavy to
// run per native pixel on the Pi, and the petals are large smooth shapes that
// survive upscaling — params3.x carries the upscale factor so edge feathering
// stays at least one layer texel wide.
// params0 = (blades, petal_sheen, overlap_shadow, photo_swap_mix)
// params1 = (open_radius_px, color.r, color.g, color.b)
// params3 = (layer_scale, unused, unused, unused)
@fragment
fn fs_iris_layer(in: VSOut) -> @location(0) vec4<f32> {
  let screen_pos = in.screen_uv * U.screen_size;
  let n = max(i32(U.params0.x), 1);
  let contrast = clamp(U.params0.y, 0.0, 1.0);
  let shadow_amt = clamp(U.params0.z, 0.0, 1.0);
  let aa = max(U.params3.x, 1.0);
  let p = screen_pos - 0.5 * U.screen_size;
  // Pixels inside the inscribed aperture circle are provably petal-free;
  // at the ends of the transition that is the entire screen.
  if (length(p) < U.params1.x - aa) {
    return vec4<f32>(0.0);
  }
  let r_in = 1.02 * 0.5 * length(U.screen_size);
  let r_mid = 1.5 * r_in;
  let w = 0.5 * r_in;
  // No local arrays: dynamically indexed function-scope arrays get demoted
  // to per-pixel scratch memory on V3D, which costs far more than
  // recomputing the few distances needed after the loop.
  // Petals shingle cyclically (each rests on the next); the covering petals
  // always form one contiguous run, so the topmost is the covered petal
  // whose successor is uncovered.
  var d_min = 1e9;
  var d_first = 0.0;
  var d_prev = 0.0;
  var top: i32 = 0;
  var covered_count = 0;
  for (var i: i32 = 0; i < 16; i++) {
    if (i >= n) { break; }
    let d = iris_petal(i, p, r_mid, w).x;
    d_min = min(d_min, d);
    if (d < 0.0) { covered_count = covered_count + 1; }
    if (i == 0) {
      d_first = d;
    } else if (d_prev < 0.0 && d >= 0.0) {
      top = i - 1;
    }
    d_prev = d;
  }
  if (d_prev < 0.0 && d_first >= 0.0) { top = n - 1; }
  let cov = 1.0 - smoothstep(-aa, aa, d_min);
  if (cov <= 0.0) {
    return vec4<f32>(0.0);
  }
  var j1 = top + 1;
  if (j1 >= n) { j1 -= n; }
  var j2 = top + 2;
  if (j2 >= n) { j2 -= n; }
  // Directional sheen: the per-petal facing cosine (baked in petals_b[*].z,
  // -1..1) scaled by petal_sheen. petal_sheen 0 => flat; 1 => the
  // IRIS_SHEEN_MAX swing. As a region becomes fully enclosed near closure
  // the top-petal pick is a tie that falls back to petal 0, so fade the
  // variation to the uniform base color over the last two petals of
  // enclosure — by full overlap every petal reads identically and the pick
  // can't pop. A closed iris center is uniform anyway.
  let IRIS_SHEEN_MAX = 0.38;
  let enclosure = f32(covered_count) / f32(n);
  let var_scale = 1.0 - smoothstep(1.0 - 2.0 / f32(n), 1.0, enclosure);
  let tone = max(1.0 + contrast * IRIS_SHEEN_MAX * P.petals_b[top].z * var_scale, 0.0);
  // Soft shadow cast by the petals stacked above the top one.
  let shadow_w = max(0.012 * r_in, 4.0);
  let dn1 = max(iris_petal(j1, p, r_mid, w).x, 0.0);
  let dn2 = max(iris_petal(j2, p, r_mid, w).x, 0.0);
  let occ = 0.5 * (1.0 - smoothstep(0.0, shadow_w, dn1))
    + 0.22 * (1.0 - smoothstep(0.0, shadow_w * 0.6, dn2));
  // Thin edge highlight, also under petal_sheen so 0 is perfectly flat.
  let rim = smoothstep(3.5 * aa, 0.5 * aa, abs(d_min)) * contrast * 0.2;
  let blade_rgb = clamp(U.params1.yzw, vec3<f32>(0.0), vec3<f32>(1.0));
  let blade_col = blade_rgb * tone * (1.0 - shadow_amt * occ) + vec3<f32>(rim);
  return vec4<f32>(blade_col * cov, cov);
}

// Upscales the half-resolution transition intermediate (bound at group 1)
// to the swapchain. Transitions render at reduced resolution because the
// 4K frame cost on the Pi is dominated by per-pixel fill; resting photos
// are still drawn at native resolution by the direct fs_main path.
@fragment
fn fs_blit(in: VSOut) -> @location(0) vec4<f32> {
  return vec4<f32>(textureSample(cur_tex, cur_samp, in.screen_uv).rgb, 1.0);
}

// The common tail of every photo pass: composite over the background here
// (previously done by alpha blending against the clear color): same result,
// no per-pixel destination read, and the output stays fully opaque for
// direct scanout. Coverage is the union of both photos and the result.
fn over_background(color: vec4<f32>, current: vec4<f32>, next: vec4<f32>) -> vec4<f32> {
  let a = clamp(max(max(current.a, next.a), color.a), 0.0, 1.0);
  return vec4<f32>(color.rgb * a + U.background.rgb * (1.0 - a), 1.0);
}

// For results that cover the whole screen by construction (fade through
// black, e-ink).
fn opaque(color: vec4<f32>) -> vec4<f32> {
  return vec4<f32>(color.rgb, 1.0);
}

fn screen_pos_of(in: VSOut) -> vec2<f32> {
  return in.screen_uv * U.screen_size;
}

fn current_of(in: VSOut) -> vec4<f32> {
  return plane(cur_tex, cur_samp, in.current_uv);
}

fn next_of(in: VSOut) -> vec4<f32> {
  return plane(next_tex, next_samp, in.next_uv);
}

fn progress01() -> f32 {
  return clamp(U.progress, 0.0, 1.0);
}

// A resting photo (no transition): only the current plane. The next photo
// is staged only while a transition runs; if one is ever staged at rest,
// the CPU draws with fs_fade at progress 0 instead (the same result with
// the next plane's coverage). This is the shader drawn at native 4K on every
// dwell and at the end of every transition, so it is kept minimal.
@fragment
fn fs_rest(in: VSOut) -> @location(0) vec4<f32> {
  let current = current_of(in);
  return over_background(current, current, current);
}

// params0.x > 0.5: through black.
@fragment
fn fs_fade(in: VSOut) -> @location(0) vec4<f32> {
  let current = current_of(in);
  let next = next_of(in);
  let progress = progress01();
  if (U.params0.x > 0.5) {
    let half = progress * 2.0;
    let fade_out = clamp(1.0 - half, 0.0, 1.0);
    let fade_in = clamp(half - 1.0, 0.0, 1.0);
    let black_weight = clamp(1.0 - fade_out - fade_in, 0.0, 1.0);
    let color = current * fade_out + next * fade_in + vec4<f32>(0.0, 0.0, 0.0, black_weight);
    return opaque(color);
  }
  return over_background(mix(current, next, progress), current, next);
}

// The wipe front as a function of screen_uv: normalized position along the
// wipe = clamp(dot(params0.xy, screen_uv) + params0.z), and the mask edges
// params1.xy (draw_params::wipe).
@fragment
fn fs_wipe(in: VSOut) -> @location(0) vec4<f32> {
  let current = current_of(in);
  let next = next_of(in);
  let normalized = clamp(dot(U.params0.xy, in.screen_uv) + U.params0.z, 0.0, 1.0);
  let mask = 1.0 - smoothstep(U.params1.x, U.params1.y, normalized);
  return over_background(current * (1.0 - mask) + next * mask, current, next);
}

// The planes are shifted on the CPU (draw_params::push).
@fragment
fn fs_push(in: VSOut) -> @location(0) vec4<f32> {
  let current = current_of(in);
  let next = next_of(in);
  let mask = step(0.5, next.a);
  return over_background(current * (1.0 - mask) + next * mask, current, next);
}

// params0 = (flash_count, reveal_portion, stripe_count, seed.x),
// params1 = (seed.y, flash_rgb).
@fragment
fn fs_eink(in: VSOut) -> @location(0) vec4<f32> {
  let current = current_of(in);
  let next = next_of(in);
  let progress = progress01();
  let flashes = max(U.params0.x, 0.0);
  let reveal_start = clamp(U.params0.y, 0.05, 0.95);
  let stripes = max(U.params0.z, 1.0);
  let seed = vec2<f32>(U.params0.w, U.params1.x);
  let flash_rgb = clamp(U.params1.yzw, vec3<f32>(0.0), vec3<f32>(1.0));
  let prep_ratio = progress / max(reveal_start, 1e-3);
  if (progress < reveal_start) {
    let segments = flashes * 2.0 + 1.0;
    let staged = clamp(floor(prep_ratio * segments), 0.0, segments);
    let stage_u = u32(staged);
    var flash_color = current;
    if (stage_u > 0u) {
      let toggle = (stage_u & 1u) == 1u;
      if (toggle) {
        flash_color = vec4<f32>(0.0, 0.0, 0.0, 1.0);
      } else {
        flash_color = vec4<f32>(flash_rgb, 1.0);
      }
    }
    let stage_pos = fract(prep_ratio * segments);
    let flash_mix = smoothstep(0.15, 0.85, stage_pos);
    return opaque(mix(current, flash_color, clamp(flash_mix, 0.0, 1.0)));
  }
  let reveal_ratio = (progress - reveal_start) / max(1.0 - reveal_start, 1e-3);
  let stripe_idx = floor(in.screen_uv.y * stripes);
  let stripe_phase = stripe_idx / stripes;
  let noise_vec = vec2<f32>(stripe_idx, floor(in.screen_uv.x * stripes)) + seed;
  let noise = fract(sin(dot(noise_vec, vec2<f32>(12.9898, 78.233))) * 43758.5453);
  // Stagger-normalized so the LAST stripe's gate reaches 1.0 by
  // reveal_ratio ~0.9: the old form (1.15r - 0.85*phase) left bottom
  // stripes only ~30-60% revealed at the end of the transition, which
  // then popped to the clean photo on the first dwell frame.
  let stagger = 0.45;
  let gate = clamp(
    (reveal_ratio - stripe_phase * stagger) / (1.0 - stagger) + noise * 0.15,
    0.0,
    1.0
  );
  let mask = smoothstep(0.05, 0.85, gate);
  let ghost = mix(current, vec4<f32>(flash_rgb, 1.0), 0.55);
  return opaque(mix(ghost, next, mask));
}

// Threshold value-noise by progress. params0 = (softness, cell scale).
@fragment
fn fs_dissolve(in: VSOut) -> @location(0) vec4<f32> {
  let current = current_of(in);
  let next = next_of(in);
  let progress = progress01();
  let softness = clamp(U.params0.x, 0.0, 0.5);
  let scale = max(U.params0.y, 1.0);
  let cell = floor(screen_pos_of(in) / scale);
  let n = fract(sin(dot(cell, vec2<f32>(12.9898, 78.233))) * 43758.5453);
  let lo = clamp(progress - softness, 0.0, 1.0);
  let hi = clamp(progress + softness, 0.0, 1.0);
  let mask = smoothstep(lo, max(hi, lo + 1e-3), n);
  return over_background(mix(next, current, mask), current, next);
}

// Circle or diamond reveal from a center, as a function of screen_uv:
// the offset from the center in units of the screen diagonal is
// screen_uv * params0.xy - params0.zw; params1 = (mask edges, diamond)
// (draw_params::radial_wipe).
@fragment
fn fs_radial_wipe(in: VSOut) -> @location(0) vec4<f32> {
  let current = current_of(in);
  let next = next_of(in);
  let dv = in.screen_uv * U.params0.xy - U.params0.zw;
  let dist = select(length(dv), abs(dv.x) + abs(dv.y), U.params1.z > 0.5);
  let mask = 1.0 - smoothstep(U.params1.x, U.params1.y, dist);
  return over_background(mix(current, next, mask), current, next);
}

// Stripe reveal: stripes along dot(screen_uv, params1.xy), mask edges
// params0.yz (draw_params::venetian_blinds).
@fragment
fn fs_venetian_blinds(in: VSOut) -> @location(0) vec4<f32> {
  let current = current_of(in);
  let next = next_of(in);
  let local = fract(dot(in.screen_uv, U.params1.xy) * U.params0.x);
  let mask = smoothstep(U.params0.y, U.params0.z, 1.0 - local);
  return over_background(mix(current, next, 1.0 - mask), current, next);
}

// Fade + a shared, gentle Ken-Burns scale. Both planes share ONE scale that
// is 0 at the ends and bumps up in the middle (1 + zoom*sin(pi*progress)).
// Locking both layers to the same scale avoids the double-image "swim" of
// opposing zoom directions, is pop-free at both ends, and keeps scale >= 1
// so edges never go empty. The zoomed planes' texture maps are params1 /
// params3 (draw_params::crossfade_zoom); coverage uses the unzoomed planes.
@fragment
fn fs_crossfade_zoom(in: VSOut) -> @location(0) vec4<f32> {
  let current = current_of(in);
  let next = next_of(in);
  let c = plane(cur_tex, cur_samp, in.screen_uv * U.params1.xy + U.params1.zw);
  let nxt = plane(next_tex, next_samp, in.screen_uv * U.params3.xy + U.params3.zw);
  return over_background(mix(c, nxt, progress01()), current, next);
}

// Mechanical camera-iris diaphragm. The petals (shape + shading) were
// rendered into the reduced-resolution petal layer by fs_iris_layer this
// frame; here they are composited (premultiplied alpha) over the
// crossfading photos. params0.w = photo_swap_mix.
@fragment
fn fs_iris(in: VSOut) -> @location(0) vec4<f32> {
  let current = current_of(in);
  let next = next_of(in);
  let photo = mix(current, next, clamp(U.params0.w, 0.0, 1.0));
  let petal = textureSample(petal_tex, petal_samp, in.screen_uv);
  let color = vec4<f32>(photo.rgb * (1.0 - petal.a) + petal.rgb, max(photo.a, petal.a));
  return over_background(color, current, next);
}

// Debug: stroke a single quadratic Bezier over the current image.
// params0.xy = P0 (uv), params0.zw = P1 (uv), params1.xy = P2 (uv),
// params1.z = stroke width (px), params3 = stroke rgba.
@fragment
fn fs_debug_bezier(in: VSOut) -> @location(0) vec4<f32> {
  let pos = screen_pos_of(in);
  let current = current_of(in);
  let next = next_of(in);
  // Control points in pixel space
  let P0 = U.screen_size * clamp(U.params0.xy, vec2<f32>(0.0), vec2<f32>(1.0));
  let P1 = U.screen_size * clamp(U.params0.zw, vec2<f32>(0.0), vec2<f32>(1.0));
  let P2 = U.screen_size * clamp(U.params1.xy, vec2<f32>(0.0), vec2<f32>(1.0));
  let stroke_px = max(U.params1.z, 0.0);
  let stroke = vec4<f32>(
    clamp(U.params3.xyz, vec3<f32>(0.0), vec3<f32>(1.0)),
    clamp(U.params3.w, 0.0, 1.0)
  );
  // Distance from this pixel to the Bezier, approximated by a polyline
  let N: i32 = 32; // segments
  var min_d = 1e9;
  var a = P0;
  for (var i: i32 = 1; i <= N; i = i + 1) {
    let t = f32(i) / f32(N);
    // Quadratic Bezier point via de Casteljau
    let q0 = mix(P0, P1, t);
    let q1 = mix(P1, P2, t);
    let b = mix(q0, q1, t);
    // Distance to segment a-b
    let ab = b - a;
    let ap = pos - a;
    let h = clamp(dot(ap, ab) / max(dot(ab, ab), 1e-6), 0.0, 1.0);
    let d = length(ap - ab * h);
    min_d = min(min_d, d);
    a = b;
  }
  let half_w = max(0.5 * stroke_px, 0.0);
  let aa = max(fwidth(min_d), 1.0);
  let edge = 1.0 - smoothstep(half_w, half_w + aa, min_d);
  let alpha = edge * stroke.a;
  return over_background(mix(current, vec4<f32>(stroke.rgb, 1.0), alpha), current, next);
}
