struct TransitionUniforms {
  screen_size: vec2<f32>,
  progress: f32,
  kind: u32,
  current_dest: vec4<f32>,
  next_dest: vec4<f32>,
  params0: vec4<f32>,
  params1: vec4<f32>,
  params3: vec4<f32>,
  // Per-petal constants for the iris transition, solved on the CPU each
  // frame (see the Iris arm in viewer.rs):
  // petals_a[i] = (annulus_center.xy, tip_dir.xy)
  // petals_b[i] = (trail_dir.xy, petal_tone, unused)
  petals_a: array<vec4<f32>, 16>,
  petals_b: array<vec4<f32>, 16>,
};

@group(0) @binding(0)
var<uniform> U: TransitionUniforms;

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
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VSOut {
  var positions = array<vec2<f32>, 6>(
    vec2<f32>(-1.0, -1.0),
    vec2<f32>(1.0, -1.0),
    vec2<f32>(1.0, 1.0),
    vec2<f32>(-1.0, -1.0),
    vec2<f32>(1.0, 1.0),
    vec2<f32>(-1.0, 1.0),
  );

  let p = positions[vid];
  var out: VSOut;
  out.pos = vec4<f32>(p, 0.0, 1.0);
  out.screen_uv = vec2<f32>((p.x + 1.0) * 0.5, (1.0 - p.y) * 0.5);
  return out;
}

fn sample_plane(
  tex: texture_2d<f32>,
  samp: sampler,
  dest: vec4<f32>,
  sample_pos: vec2<f32>,
) -> vec4<f32> {
  if (dest.z <= 0.0 || dest.w <= 0.0) {
    return vec4<f32>(0.0, 0.0, 0.0, 0.0);
  }
  let uv = (sample_pos - dest.xy) / dest.zw;
  if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
    return vec4<f32>(0.0, 0.0, 0.0, 0.0);
  }
  let c = textureSample(tex, samp, uv);
  return vec4<f32>(c.rgb, 1.0);
}

// Signed distance to iris petal `i` using the CPU-precomputed constants in
// U.petals_a / U.petals_b. Returns (distance, radius about the petal's
// annulus center). On-screen the only petal boundaries are the inner arc and
// the two end caps, so this band+cap distance is exact for visible pixels.
fn iris_petal(i: i32, p: vec2<f32>, r_mid: f32, w: f32) -> vec2<f32> {
  let a = U.petals_a[i];
  let b = U.petals_b[i];
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
// params0 = (blades, petal_contrast, overlap_shadow, photo_swap_mix)
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
  for (var i: i32 = 0; i < 16; i++) {
    if (i >= n) { break; }
    let d = iris_petal(i, p, r_mid, w).x;
    d_min = min(d_min, d);
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
  // Across-the-petal gradient on top of the CPU-baked sheen tone.
  let r_top = iris_petal(top, p, r_mid, w).y;
  let g = clamp((r_top - r_mid) / w, -1.0, 1.0);
  let tone = max(U.petals_b[top].z - contrast * 0.30 * g, 0.0);
  // Soft shadow cast by the petals stacked above the top one.
  let shadow_w = max(0.012 * r_in, 4.0);
  let dn1 = max(iris_petal(j1, p, r_mid, w).x, 0.0);
  let dn2 = max(iris_petal(j2, p, r_mid, w).x, 0.0);
  let occ = 0.5 * (1.0 - smoothstep(0.0, shadow_w, dn1))
    + 0.22 * (1.0 - smoothstep(0.0, shadow_w * 0.6, dn2));
  let vign = 1.0 - 0.25 * length(p) / (0.5 * length(U.screen_size));
  let rim = smoothstep(3.5 * aa, 0.5 * aa, abs(d_min)) * (0.05 + 0.10 * contrast);
  let blade_rgb = clamp(U.params1.yzw, vec3<f32>(0.0), vec3<f32>(1.0));
  let blade_col = blade_rgb * tone * vign * (1.0 - shadow_amt * occ) + vec3<f32>(rim);
  return vec4<f32>(blade_col * cov, cov);
}

@fragment
fn fs_main(in: VSOut) -> @location(0) vec4<f32> {
  let screen_pos = in.screen_uv * U.screen_size;
  var current = sample_plane(cur_tex, cur_samp, U.current_dest, screen_pos);
  var next = sample_plane(next_tex, next_samp, U.next_dest, screen_pos);
  var color = current;
  let progress = clamp(U.progress, 0.0, 1.0);
switch (U.kind) {
    case 0u: {
      color = current;
    }
    case 1u: {
      if (U.params0.x > 0.5) {
        let half = progress * 2.0;
        let fade_out = clamp(1.0 - half, 0.0, 1.0);
        let fade_in = clamp(half - 1.0, 0.0, 1.0);
        let black_weight = clamp(1.0 - fade_out - fade_in, 0.0, 1.0);
        color = current * fade_out + next * fade_in + vec4<f32>(0.0, 0.0, 0.0, black_weight);
      } else {
        color = mix(current, next, progress);
      }
    }
    case 2u: {
      let min_proj = U.params0.z;
      let inv_span = U.params0.w;
      let softness = clamp(U.params1.x, 0.0, 0.5);
      let normalized = clamp((dot(U.params0.xy, screen_pos) - min_proj) * inv_span, 0.0, 1.0);
      var mask = 0.0;
      if (progress <= 0.0) {
        mask = 0.0;
      } else if (progress >= 1.0) {
        mask = 1.0;
      } else {
        let leading = clamp(progress - softness, 0.0, progress);
        let trailing = clamp(progress + softness, progress, 1.0);
        let end = max(trailing, leading + 1e-3);
        let smooth_mask = smoothstep(leading, end, normalized);
        mask = 1.0 - smooth_mask;
      }
      color = current * (1.0 - mask) + next * mask;
    }
    case 3u: {
      let translation = U.params0.xy;
      let cur_pos = screen_pos - translation * progress;
      let next_pos = screen_pos + translation * (1.0 - progress);
      current = sample_plane(cur_tex, cur_samp, U.current_dest, cur_pos);
      next = sample_plane(next_tex, next_samp, U.next_dest, next_pos);
      let mask = step(0.5, next.a);
      color = current * (1.0 - mask) + next * mask;
    }
    case 4u: {
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
        color = mix(current, flash_color, clamp(flash_mix, 0.0, 1.0));
      } else {
        let reveal_ratio = (progress - reveal_start) / max(1.0 - reveal_start, 1e-3);
        let stripe_idx = floor(in.screen_uv.y * stripes);
        let stripe_phase = stripe_idx / stripes;
        let noise_vec = vec2<f32>(stripe_idx, floor(in.screen_uv.x * stripes)) + seed;
        let noise = fract(sin(dot(noise_vec, vec2<f32>(12.9898, 78.233))) * 43758.5453);
        let gate = clamp(reveal_ratio * 1.15 - stripe_phase * 0.85 + noise * 0.25, 0.0, 1.0);
        let mask = smoothstep(0.25, 0.75, gate);
        let ghost = mix(current, vec4<f32>(flash_rgb, 1.0), 0.55);
        color = mix(ghost, next, mask);
      }
    }
    case 5u: {
      // dissolve: threshold value-noise by progress
      let softness = clamp(U.params0.x, 0.0, 0.5);
      let scale = max(U.params0.y, 1.0);
      let cell = floor(screen_pos / scale);
      let n = fract(sin(dot(cell, vec2<f32>(12.9898, 78.233))) * 43758.5453);
      let lo = clamp(progress - softness, 0.0, 1.0);
      let hi = clamp(progress + softness, 0.0, 1.0);
      let mask = smoothstep(lo, max(hi, lo + 1e-3), n);
      color = mix(next, current, mask);
    }
    case 8u: {
      // radial-wipe: circle or diamond reveal from center
      let center = U.params0.xy * U.screen_size;
      let softness = clamp(U.params0.z, 0.0, 0.5);
      let diamond = U.params0.w > 0.5;
      let dv = screen_pos - center;
      let max_r = length(U.screen_size);
      var dist = length(dv) / max(max_r, 1e-3);
      if (diamond) { dist = (abs(dv.x) + abs(dv.y)) / max(max_r, 1e-3); }
      let lo = clamp(progress - softness, 0.0, 1.0);
      let hi = clamp(progress + softness, 0.0, 1.0);
      let mask = 1.0 - smoothstep(lo, max(hi, lo + 1e-3), dist);
      color = mix(current, next, mask);
    }
    case 9u: {
      // venetian-blinds: stripe reveal
      let stripes = max(U.params0.x, 1.0);
      let softness = clamp(U.params0.y, 0.001, 0.5);
      let vertical = U.params0.z > 0.5;
      let axis = select(in.screen_uv.y, in.screen_uv.x, vertical);
      let local = fract(axis * stripes);
      let mask = smoothstep(progress - softness, progress + softness, 1.0 - local);
      color = mix(current, next, 1.0 - mask);
    }
    case 10u: {
      // crossfade-zoom: fade + a shared, gentle Ken-Burns scale.
      // Both planes share ONE scale that is 0 at the ends and bumps up in the
      // middle (1 + zoom*sin(pi*progress)). Locking both layers to the same
      // scale avoids the double-image "swim" of opposing zoom directions, is
      // pop-free at both ends, and keeps scale >= 1 so edges never go empty.
      let zoom = U.params0.x;
      let cur_in = U.params0.y > 0.5;
      let next_in = U.params0.z > 0.5;
      let center = U.screen_size * 0.5;
      let bump = zoom * sin(3.14159265 * progress);
      let cur_scale = select(1.0, 1.0 + bump, cur_in);
      let next_scale = select(1.0, 1.0 + bump, next_in);
      let cur_pos = center + (screen_pos - center) / max(cur_scale, 1e-3);
      let next_pos = center + (screen_pos - center) / max(next_scale, 1e-3);
      let c = sample_plane(cur_tex, cur_samp, U.current_dest, cur_pos);
      let nxt = sample_plane(next_tex, next_samp, U.next_dest, next_pos);
      color = mix(c, nxt, progress);
    }
    case 11u: {
      // iris: mechanical camera-iris diaphragm. The petals (shape + shading)
      // were rendered into the reduced-resolution petal layer by
      // fs_iris_layer this frame; here we just composite it (premultiplied
      // alpha) over the crossfading photos.
      // params0.w = photo_swap_mix
      let photo = mix(current, next, clamp(U.params0.w, 0.0, 1.0));
      let petal = textureSample(petal_tex, petal_samp, in.screen_uv);
      color = vec4<f32>(photo.rgb * (1.0 - petal.a) + petal.rgb, max(photo.a, petal.a));
    }
    case 6u: {
      // Debug: stroke a single quadratic Bezier over the current image
      // params0.xy = P0 (uv), params0.zw = P1 (uv), params1.xy = P2 (uv)
      // params1.z = stroke width (px), params3 = stroke rgba
      color = current;

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
      let p = in.screen_uv * U.screen_size;
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
        let ap = p - a;
        let h = clamp(dot(ap, ab) / max(dot(ab, ab), 1e-6), 0.0, 1.0);
        let d = length(ap - ab * h);
        min_d = min(min_d, d);
        a = b;
      }

      let half_w = max(0.5 * stroke_px, 0.0);
      let aa = max(fwidth(min_d), 1.0);
      let edge = 1.0 - smoothstep(half_w, half_w + aa, min_d);
      let alpha = edge * stroke.a;
      color = mix(color, vec4<f32>(stroke.rgb, 1.0), alpha);
    }
    default: {
      color = current;
    }
  }
  var alpha = max(max(current.a, next.a), color.a);
  if ((U.kind == 1u && U.params0.x > 0.5) || U.kind == 4u) {
    alpha = 1.0;
  }
  return vec4<f32>(color.rgb, clamp(alpha, 0.0, 1.0));
}
