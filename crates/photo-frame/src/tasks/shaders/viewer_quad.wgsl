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
  if (blades < 3u) {
    return 0.0;
  }

  // Curved-blade base shape via intersection-of-disks. Keep disk radius > ring
  // radius to avoid daisy topology.
  let c: f32 = 0.9;       // ring radius of disk centers (in screen-space units)
  let curve: f32 = 1.25;  // curvature factor; d = curve * c (> 1)
  let d: f32 = curve * c; // disk radius

  // Compute minimum radial limit across only the three nearest blade centers.
  let theta = atan2(p.y, p.x);
  let sector = 6.283185307179586 / f32(blades);
  let k = floor((theta + 0.5 * sector) / sector);
  let a0 = k * sector;
  let a1 = (k - 1.0) * sector;
  let a2 = (k + 1.0) * sector;

  let eval_ru = fn(phi: f32) -> f32 {
    let s = sin(phi);
    let cph = cos(phi);
    let disc = d * d - (c * c) * (s * s);
    if (disc <= 0.0) { return 1e9; }
    let ru = c * cph + sqrt(max(disc, 0.0));
    return select(ru, 1e9, ru <= 0.0);
  };

  let ru0 = eval_ru(theta - a0);
  let ru1 = eval_ru(theta - a1);
  let ru2 = eval_ru(theta - a2);
  return min(ru0, min(ru1, ru2));
}

fn iris_scale_to_width(
  aspect: f32,
  blades: u32,
) -> f32 {
  // Same geometry constants as iris_boundary
  let c: f32 = 0.9;
  let curve: f32 = 1.25;
  let d: f32 = curve * c;
  if (blades < 3u) { return 1.0; }

  // Evaluate the radial limit along the horizontal axis (theta=0) using
  // only three nearest centers: 0 and ±sector.
  let sector = 6.283185307179586 / f32(blades);
  let eval_ru0 = fn(phi: f32) -> f32 {
    let s = sin(phi);
    let cph = cos(phi);
    let disc = d * d - (c * c) * (s * s);
    if (disc <= 0.0) { return 1e9; }
    let ru = c * cph + sqrt(max(disc, 0.0));
    return select(ru, 1e9, ru <= 0.0);
  };
  let r0 = eval_ru0(0.0);
  let r1 = eval_ru0(sector);
  let r2 = eval_ru0(-sector);
  let denom = max(min(r0, min(r1, r2)), 1e-3);
  return aspect / denom;
}

fn iris_mask(
  uv: vec2<f32>,
  aspect: f32,
  blades: u32,
  rotate_rad: f32,
  open_scale: f32,
) -> f32 {
  let rvec = (uv * 2.0 - vec2<f32>(1.0, 1.0)) * vec2<f32>(aspect, 1.0);
  let r = length(rot2(rvec, rotate_rad));
  let min_ru = iris_boundary(uv, aspect, blades, rotate_rad);
  // Scale the boundary so fully open spans screen width (horizontal diameter).
  let scale_to_width = iris_scale_to_width(aspect, blades);
  let boundary = clamp(open_scale, 0.0, 1.0) * min_ru * scale_to_width;
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

      // Fill and stroke uniforms
      let fill = vec4<f32>(
        clamp(U.params2.xyz, vec3<f32>(0.0), vec3<f32>(1.0)),
        clamp(U.params2.w, 0.0, 1.0)
      );
      let stroke_col = vec4<f32>(
        clamp(U.params3.xyz, vec3<f32>(0.0), vec3<f32>(1.0)),
        clamp(U.params3.w, 0.0, 1.0) // if provided as alpha; otherwise 1.0
      );
      let stroke_px = max(U.iris_pad0, 0.0);

      // Base composition over occluder fill
      if (first) {
        color = mix(fill, current, mask_close);
      } else {
        color = mix(fill, next, mask_open);
      }

      // Stroke ring: compute SDF band around boundary using current open scale
      let open_scale = select(open2, open1, first);
      let rvec = (in.screen_uv * 2.0 - vec2<f32>(1.0, 1.0)) * vec2<f32>(U.aspect, 1.0);
      let p = rot2(rvec, rot);
      let rlen = length(p);
      let min_ru = iris_boundary(in.screen_uv, U.aspect, blades, rot);
      let scale_to_width = iris_scale_to_width(U.aspect, blades);
      let boundary = clamp(open_scale, 0.0, 1.0) * min_ru * scale_to_width;
      let sdf = boundary - rlen; // positive inside aperture

      // Convert pixel width to our coordinate system (~y-based scale)
      let px_to_ndc = 2.0 / max(U.screen_size.y, 1.0);
      let half_band = max(stroke_px * px_to_ndc * 0.5, 1e-4);
      let aa = max(fwidth(sdf), 1e-4);
      // Band around |sdf| < half_band, anti-aliased
      let edge = 1.0 - smoothstep(half_band, half_band + aa, abs(sdf));
      let stroke_alpha = edge * stroke_col.a;
      // Composite outer ring stroke over base color
      color = mix(color, vec4<f32>(stroke_col.rgb, 1.0), stroke_alpha);

      // Stroke blade seams (full length from center to boundary): thin lines at
      // blade boundaries in angular space. This evokes the petal outlines.
      // Distance from point to line through origin at angle m*sector is
      // |r * sin(theta - m*sector)|. We draw a pixel-constant band.
      let theta = atan2(p.y, p.x);
      let sector = 6.283185307179586 / f32(blades);
      // Reduce to nearest seam (multiple of sector)
      let kline = round(theta / sector);
      let seam_ang = kline * sector;
      let ang_dist = abs(sin(theta - seam_ang));
      // Convert stroke width to angular band: distance to line is r * sin Δ.
      // We want a pixel width at this radius. Map pixels -> NDC and divide by r.
      let eps_r = max(rlen, 1e-3);
      let half_band_ang = max(stroke_px * px_to_ndc * 0.5 / eps_r, 1e-4);
      let seam_edge = 1.0 - smoothstep(half_band_ang, half_band_ang + 1.5 * half_band_ang, ang_dist);
      // Only draw seams on occluder (outside current aperture)
      let occluder = select(1.0 - mask_open, 1.0 - mask_close, first);
      let seam_alpha = seam_edge * occluder * stroke_col.a;
      color = mix(color, vec4<f32>(stroke_col.rgb, 1.0), seam_alpha);
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
