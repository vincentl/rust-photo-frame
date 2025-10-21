struct TransitionUniforms {
  screen_size: vec2<f32>,
  progress: f32,
  kind: u32,
  current_dest: vec4<f32>,
  next_dest: vec4<f32>,
  params0: vec4<f32>,
  params1: vec4<f32>,
  params2: vec4<f32>,
  params3: vec4<f32>,
  t: f32,
  aspect: f32,
  iris_rotate_rad: f32,
  iris_pad0: f32,
  iris_blades: u32,
  iris_direction: u32,
  iris_pad1: vec2<u32>,
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
// The iris is modeled as the intersection of N circles of radius `d` whose centers
// lie on a ring of radius `c` around the origin. For each fragment direction, the
// radial limit is the minimum of the upper roots across all disks. This produces
// camera-like curved blades without tessellation.
fn iris_mask(
  uv: vec2<f32>,
  aspect: f32,
  blades: u32,
  rotate_rad: f32,
  t_eased: f32,
  direction: u32,
) -> f32 {
  // Normalize timeline (account for open/close direction)
  let tt = select(t_eased, 1.0 - t_eased, direction == 1u);

  // Map screen uv to centered, aspect-corrected space in [-1,1]
  let p0 = (uv * 2.0 - vec2<f32>(1.0, 1.0)) * vec2<f32>(aspect, 1.0);
  let p = rot2(p0, rotate_rad * tt);
  let r = length(p);
  if (blades < 3u) {
    return 0.0;
  }

  // Iris geometry parameters. `c` is the ring radius for disk centers, `d` is
  // disk radius. When d < c, the intersection shrinks toward closed; when d >> c
  // the aperture opens wide. Tuned for screen-space [-1,1].
  let c: f32 = 0.9;                           // ring radius
  let d_closed: f32 = c * 0.80;               // fully closed threshold
  let d_open:   f32 = c * 2.20;               // fully open radius
  let d: f32 = mix(d_closed, d_open, clamp(tt, 0.0, 1.0));

  // Precompute angle of the fragment once; we iterate disk centers efficiently
  // using complex multiplication by e^{i*sector}.
  let theta = atan2(p.y, p.x);
  let sector = 6.283185307179586 / f32(blades);
  let cs = cos(sector);
  let ss = sin(sector);
  let ct = cos(theta);
  let st = sin(theta);

  // Start with center angle 0 and rotate per blade.
  var ci = 1.0; // cos(alpha)
  var si = 0.0; // sin(alpha)

  var min_ru = 1e9;
  var any = false;
  for (var i: u32 = 0u; i < blades; i = i + 1u) {
    // phi = theta - alpha; compute via trig identities for speed/precision
    let cos_phi = ct * ci + st * si;
    let sin_phi = st * ci - ct * si;
    let disc = d * d - (c * c) * (sin_phi * sin_phi);
    if (disc > 0.0) {
      // upper root of the quadratic r^2 - 2 r c cos(phi) + (c^2 - d^2) = 0
      let ru = c * cos_phi + sqrt(disc);
      if (ru > 0.0) {
        any = true;
        min_ru = min(min_ru, ru);
      }
    }
    // advance center angle: (ci, si) *= rot(sector)
    let ci_next = ci * cs - si * ss;
    let si_next = si * cs + ci * ss;
    ci = ci_next;
    si = si_next;
  }

  if (!any) {
    return 0.0;
  }

  // Signed distance (positive inside aperture): sdf = min_ru - r
  let sdf = min_ru - r;
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
      let eased_t = clamp(U.t, 0.0, 1.0);
      let mask = iris_mask(
        in.screen_uv,
        U.aspect,
        max(U.iris_blades, 3u),
        U.iris_rotate_rad,
        eased_t,
        U.iris_direction,
      );
      color = mix(current, next, mask);
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
