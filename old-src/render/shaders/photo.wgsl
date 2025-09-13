// src/render/shaders/photo.wgsl

// Per-image UV scale (letterbox/pillarbox). We use .xy; .zw reserved.
// Two vec4s => 32 bytes; safe across Metal/Vulkan/D3D/GL.
struct Params {
  data: vec4<f32>,   // (scale_x, scale_y, _, _)
  _pad: vec4<f32>,   // unused padding
};

// Crossfade factor (alpha in .x). Two vec4s => 32 bytes.
struct Fade {
  data: vec4<f32>,   // (alpha, 0, 0, 0)
  _pad: vec4<f32>,   // unused padding
};

@group(0) @binding(0) var texA  : texture_2d<f32>;
@group(0) @binding(1) var sampA : sampler;
@group(0) @binding(2) var texB  : texture_2d<f32>;
@group(0) @binding(3) var sampB : sampler;

@group(0) @binding(4) var<uniform> pA   : Params;
@group(0) @binding(5) var<uniform> pB   : Params;
@group(0) @binding(6) var<uniform> fade : Fade;

struct VsIn {
  @location(0) pos: vec2<f32>, // NDC quad [-1,1]
  @location(1) uv : vec2<f32>, // 0..1 across the screen
};

struct VsOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
  var out: VsOut;
  out.pos = vec4<f32>(in.pos, 0.0, 1.0);
  out.uv  = in.uv;
  return out;
}

fn remap_uv(uv: vec2<f32>, scale_uv: vec2<f32>) -> vec2<f32> {
  return (uv - vec2<f32>(0.5, 0.5)) * scale_uv + vec2<f32>(0.5, 0.5);
}

fn sample_letterboxed(tex: texture_2d<f32>, samp: sampler, uv: vec2<f32>, scale_uv: vec2<f32>) -> vec4<f32> {
  let t = remap_uv(uv, scale_uv);
  if (t.x < 0.0 || t.x > 1.0 || t.y < 0.0 || t.y > 1.0) {
    return vec4<f32>(0.0, 0.0, 0.0, 1.0);
  }
  return textureSample(tex, samp, t);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
  let colA = sample_letterboxed(texA, sampA, in.uv, pA.data.xy);
  let colB = sample_letterboxed(texB, sampB, in.uv, pB.data.xy);
  let a    = clamp(fade.data.x, 0.0, 1.0); // alpha in .x
  return mix(colA, colB, a);
}
