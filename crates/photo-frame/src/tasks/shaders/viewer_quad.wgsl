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
      let blades = max(U.params0.x, 1.0);
      let direction_sign = U.params0.y;
      let line_thickness_px = max(U.params0.z, 0.0);
      let taper = clamp(U.params0.w, 0.0, 1.0);
      let rotation_amp = U.params1.x;
      let feather_factor = max(U.params1.y, 0.0);
      let vignette_strength = clamp(U.params1.z, 0.0, 1.0);
      let noise_amount = clamp(U.params1.w, 0.0, 1.0);
      let line_color = U.params2;
      let arc_color = U.params3;
      let base_progress = clamp(U.progress, 0.0, 1.0);
      var aperture_progress = base_progress;
      if (direction_sign < 0.0) {
        aperture_progress = 1.0 - base_progress;
      }
      let aspect = U.screen_size.x / max(U.screen_size.y, 1.0);
      let rel = vec2<f32>((in.screen_uv.x - 0.5) * aspect, in.screen_uv.y - 0.5);
      let dist = length(rel);
      let angle = atan2(rel.y, rel.x);
      let max_radius = length(vec2<f32>(aspect * 0.5, 0.5));
      let aperture_radius = max_radius * aperture_progress;
      let feather = max(max_radius * feather_factor, 1e-4);
      var cover = 1.0;
      if (aperture_progress > 1e-4) {
        cover = smoothstep(aperture_radius - feather, aperture_radius + feather, dist);
      }
      if (aperture_progress >= 0.9995) {
        cover = 0.0;
      }
      let next_weight = clamp(1.0 - cover, 0.0, 1.0);
      color = mix(current, next, next_weight);

      let rotation = rotation_amp * base_progress * direction_sign;
      let rotated_angle = angle + rotation;
      let blades_clamped = max(blades, 1.0);
      let sin_term = abs(sin(rotated_angle * blades_clamped));
      let delta_angle = sin_term / blades_clamped;
      let line_range = max(max_radius - aperture_radius, 1e-4);
      let outward = max(dist - aperture_radius, 0.0);
      var line_presence = 0.0;
      if (line_range > 1e-3) {
        let entry = smoothstep(0.0, feather * 4.0 + 1e-4, outward);
        let exit = smoothstep(line_range * 0.85, line_range, outward);
        line_presence = entry * (1.0 - exit);
      }
      let radius_px = max(dist * U.screen_size.y, 1.0);
      let outward_ratio = clamp(outward / line_range, 0.0, 1.0);
      let taper_scale = mix(1.0, 1.0 - outward_ratio, taper);
      let thickness_angle = max(line_thickness_px / radius_px * taper_scale, 1e-4);
      let angle_ratio = delta_angle / thickness_angle;
      let line_body = exp(-0.5 * angle_ratio * angle_ratio);
      let noise = fract(
        sin(dot(screen_pos, vec2<f32>(12.9898, 78.233))) * 43758.5453
      );
      let jitter = (noise - 0.5) * noise_amount;
      let line_alpha = clamp(line_color.w + jitter, 0.0, 1.0) * line_body * line_presence;
      color = vec4<f32>(
        mix(color.rgb, line_color.rgb, clamp(line_alpha, 0.0, 1.0)),
        color.a,
      );

      let arc_phase = abs(cos(rotated_angle * blades_clamped));
      let arc_delta = 1.0 - arc_phase;
      let arc_body = exp(-arc_delta * 12.0);
      let arc_inner = smoothstep(-feather * 2.0, feather * 2.0, dist - aperture_radius);
      let arc_outer = 1.0 - smoothstep(feather * 4.0, feather * 4.0 + 1e-3, dist - aperture_radius);
      let arc_presence = arc_inner * arc_outer;
      let arc_alpha = clamp(arc_color.w + jitter * 0.5, 0.0, 1.0) * arc_body * arc_presence;
      color = vec4<f32>(
        mix(color.rgb, arc_color.rgb, clamp(arc_alpha, 0.0, 1.0)),
        color.a,
      );

      if (vignette_strength > 0.0) {
        let radius_norm = clamp(dist / max_radius, 0.0, 1.0);
        let vignette = 1.0 - vignette_strength * smoothstep(0.55, 1.0, radius_norm);
        color = vec4<f32>(color.rgb * vignette, color.a);
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
