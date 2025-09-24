struct TransitionUniforms {
  screen_size: vec2<f32>,
  progress: f32,
  kind: u32,
  current_dest: vec4<f32>,
  next_dest: vec4<f32>,
  params0: vec4<f32>,
  params1: vec4<f32>,
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
        let smooth = smoothstep(leading, end, normalized);
        mask = 1.0 - smooth;
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
    default: {
      color = current;
    }
  }
  var alpha = max(max(current.a, next.a), color.a);
  if (U.kind == 1u && U.params0.x > 0.5) {
    alpha = 1.0;
  }
  return vec4<f32>(color.rgb, clamp(alpha, 0.0, 1.0));
}
