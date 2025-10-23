struct TransitionUniforms {
  screen_size: vec2<f32>,
  progress: f32,
  kind: u32,
  current_dest: vec4<f32>,
  next_dest: vec4<f32>,
  params0: vec4<f32>,
  params1: vec4<f32>,
  params2: vec4<f32>,
  t: f32,
  aspect: f32,
  iris_rotate_rad: f32,
  // Pad f32 to align next u32 pair to 16 bytes boundary
  _pad0: f32,
  iris_blades: u32,
  _pad1: u32,
  // Final pad so the struct size rounds up to 128 bytes (std140 multiple of 16)
  _pad2: vec2<u32>,
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

fn rot2(p: vec2<f32>, a: f32) -> vec2<f32> {
  let c = cos(a);
  let s = sin(a);
  return vec2<f32>(c * p.x - s * p.y, s * p.x + c * p.y);
}

fn sd_ngon(p: vec2<f32>, n: i32, r: f32) -> f32 {
  let pi = 3.141592653589793;
  let nn = max(n, 3);
  let an = pi / f32(nn);
  let ang = atan2(p.y, p.x);
  let m = floor(0.5 + ang / (2.0 * an));
  let a = ang - (m * 2.0 * an);
  return length(p) * cos(an) - r * cos(a - an);
}

// Analytic curved-blade iris mask using intersection of equally spaced disks.
// Returns 1.0 inside the aperture and 0.0 outside. The aperture size is
// controlled by `open_scale` in [0,1], and fully open spans the screen width.
fn iris_boundary(
  uv: vec2<f32>,
  aspect: f32,
  blades: u32,
  rotate_rad: f32,
  // Returns the unscaled boundary radius (in aspect-corrected units) and a
  // boolean indicating if a valid boundary was computed.
) -> f32 {
  // Map screen uv to centered, aspect-corrected space in [-1,1]
  let p0 = (uv * 2.0 - vec2<f32>(1.0, 1.0)) * vec2<f32>(aspect, 1.0);
  let p = rot2(p0, rotate_rad);
  if (blades < 3u) { return 0.0; }

  // Curved-blade boundary from intersection of equally spaced disks.
  // Each blade is a circle of radius R whose center lies on a circle of
  // radius d around the origin. For a ray at angle theta, the first hit
  // with circle i (center c_i) is:
  //   λ_i = dot(c_i, u) + sqrt(R^2 - ||c_i||^2 + dot(c_i,u)^2),  u = (cosθ,sinθ)
  // The iris boundary is min_i λ_i.
  // We choose R = aspect (half screen width in isotropic units) so fully
  // open spans the screen horizontally, and d = (1 - open_scale) * R so
  // the aperture closes as centers move outward.
  let p_iso = p; // already in aspect-corrected space
  let theta = atan2(p_iso.y, p_iso.x);
  let u = vec2<f32>(cos(theta), sin(theta));
  let R = aspect;
  let d = (1.0 - open_scale) * R;
  let step = 6.283185307179586 / f32(blades);
  // Rotate centers by rotate_rad
  let rot_c = mat2x2<f32>(cos(rotate_rad), -sin(rotate_rad),
                          sin(rotate_rad),  cos(rotate_rad));
  var r_min = 1e9;
  for (var i: u32 = 0u; i < blades; i = i + 1u) {
    let ang = f32(i) * step;
    let c_local = vec2<f32>(d * cos(ang), d * sin(ang));
    let c = rot_c * c_local;
    let cu = dot(c, u);
    let disc = max(R * R - dot(c, c) + cu * cu, 0.0);
    let lam = cu + sqrt(disc);
    r_min = min(r_min, lam);
  }
  // Return factor so that boundary = factor (caller multiplies by aspect*open_scale)
  return r_min / max(aspect * open_scale, 1e-4);  
}

// Polygon path scales directly by apothem (screen aspect), no extra factor.

fn iris_mask(
  uv: vec2<f32>,
  aspect: f32,
  blades: u32,
  rotate_rad: f32,
  open_scale: f32,
) -> f32 {
  // Map to isotropic space where circles are round in pixels
  let p = (uv * 2.0 - vec2<f32>(1.0, 1.0)) * vec2<f32>(aspect, 1.0);
  let pr = rot2(p, rotate_rad);
  let r = length(pr);
  let factor = iris_boundary(uv, aspect, blades, rotate_rad);
  let boundary = clamp(open_scale, 0.0, 1.0) * aspect * factor;
  let sdf = boundary - r;
  let aa = max(fwidth(sdf), 1e-4);
  return smoothstep(0.0, aa, sdf);
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
      // Two-phase iris: start fully open, close to black (first half), then
      // open to reveal the next photo (second half). `U.t` is eased already.
      let t = clamp(U.t, 0.0, 1.0);
      // Avoid tiny numerical flickers at endpoints
      if (t <= 1e-4) { return current; }
      if (t >= 0.9999) { return next; }
      let first = t < 0.5;
      let t1 = clamp(t * 2.0, 0.0, 1.0);          // 0..1 over first half
      let t2 = clamp((t - 0.5) * 2.0, 0.0, 1.0);  // 0..1 over second half

      // Keep some numerical floor to avoid zero area causing banding
      let open1 = max(1.0 - t1, 0.0); // close from 1 -> 0
      let open2 = max(t2, 0.0);       // open from 0 -> 1

      // Rotate blades gradually using provided rotate angle over the timeline
      let rot = U.iris_rotate_rad * t;
      let blades = max(U.iris_blades, 3u);

      // Compute masks for both halves.
      let mask_close = iris_mask(in.screen_uv, U.aspect, blades, rot, open1);
      let mask_open  = iris_mask(in.screen_uv, U.aspect, blades, rot, open2);

      // Occluder fill color
      let fill = vec4<f32>(
        clamp(U.params2.xyz, vec3<f32>(0.0), vec3<f32>(1.0)),
        clamp(U.params2.w, 0.0, 1.0)
      );

      // Base composition over occluder fill
      if (first) {
        color = mix(fill, current, mask_close);
      } else {
        color = mix(fill, next, mask_open);
      }
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
