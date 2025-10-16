struct BladeUniforms {
  scale: vec2<f32>,
  opacity: f32,
  _pad0: f32,
  color: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> Blade: BladeUniforms;

struct BladeOut {
  @builtin(position) pos: vec4<f32>,
};

@vertex
fn vs_blade(@location(0) position: vec2<f32>, @location(1) rotation: vec2<f32>) -> BladeOut {
  let c = rotation.x;
  let s = rotation.y;
  let rotated = vec2<f32>(c * position.x - s * position.y, s * position.x + c * position.y);
  let clip = vec2<f32>(rotated.x * Blade.scale.x, rotated.y * Blade.scale.y);
  var out: BladeOut;
  out.pos = vec4<f32>(clip, 0.0, 1.0);
  return out;
}

@fragment
fn fs_mask() -> @location(0) f32 {
  return Blade.opacity;
}

@fragment
fn fs_color() -> @location(0) vec4<f32> {
  return vec4<f32>(Blade.color.rgb, Blade.color.a * Blade.opacity);
}

struct CompositeUniforms {
  screen_size: vec2<f32>,
  stage: u32,
  _pad0: u32,
  current_dest: vec4<f32>,
  next_dest: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> Comp: CompositeUniforms;

@group(1) @binding(0)
var cur_tex: texture_2d<f32>;
@group(1) @binding(1)
var cur_samp: sampler;

@group(2) @binding(0)
var next_tex: texture_2d<f32>;
@group(2) @binding(1)
var next_samp: sampler;

@group(3) @binding(0)
var mask_tex: texture_2d<f32>;
@group(3) @binding(1)
var mask_samp: sampler;

struct FullscreenOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) screen_uv: vec2<f32>,
};

@vertex
fn vs_fullscreen(@builtin(vertex_index) vid: u32) -> FullscreenOut {
  var positions = array<vec2<f32>, 6>(
    vec2<f32>(-1.0, -1.0),
    vec2<f32>(1.0, -1.0),
    vec2<f32>(1.0, 1.0),
    vec2<f32>(-1.0, -1.0),
    vec2<f32>(1.0, 1.0),
    vec2<f32>(-1.0, 1.0),
  );
  let p = positions[vid];
  var out: FullscreenOut;
  out.pos = vec4<f32>(p, 0.0, 1.0);
  out.screen_uv = vec2<f32>((p.x + 1.0) * 0.5, (1.0 - p.y) * 0.5);
  return out;
}

fn sample_plane(tex: texture_2d<f32>, samp: sampler, dest: vec4<f32>, sample_pos: vec2<f32>) -> vec4<f32> {
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
fn fs_composite(in: FullscreenOut) -> @location(0) vec4<f32> {
  // Convert normalized screen UV into pixel-space position
  let screen_pos = in.screen_uv * Comp.screen_size;
  // Sample current/next planes with cover-rect mapping
  let current = sample_plane(cur_tex, cur_samp, Comp.current_dest, screen_pos);
  let next    = sample_plane(next_tex, next_samp, Comp.next_dest, screen_pos);
  // Binary mask in screen UV space selects next over current
  let mask    = textureSample(mask_tex, mask_samp, in.screen_uv).r;
  let color   = mix(current, next, mask);
  let alpha   = mix(current.a, next.a, mask);
  return vec4<f32>(color.rgb, clamp(alpha, 0.0, 1.0));
}
