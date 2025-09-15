struct Uniforms {
  screen_w: f32,
  screen_h: f32,
  dest_x: f32,
  dest_y: f32,
  dest_w: f32,
  dest_h: f32,
  alpha: f32,
  _pad0: f32,
  _pad1: f32,
  _pad2: f32,
};

@group(0) @binding(0)
var<uniform> U: Uniforms;

@group(1) @binding(0)
var t_tex: texture_2d<f32>;
@group(1) @binding(1)
var t_samp: sampler;

struct VSOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VSOut {
  // Two triangles for the destination rect
  var positions = array<vec2<f32>, 6>(
    vec2<f32>(0.0, 0.0),
    vec2<f32>(1.0, 0.0),
    vec2<f32>(1.0, 1.0),
    vec2<f32>(0.0, 0.0),
    vec2<f32>(1.0, 1.0),
    vec2<f32>(0.0, 1.0),
  );

  let p = positions[vid];
  let px = U.dest_x + p.x * U.dest_w;
  let py = U.dest_y + p.y * U.dest_h;
  // Convert from pixels to NDC (-1..1)
  let ndc_x = (px / U.screen_w) * 2.0 - 1.0;
  let ndc_y = 1.0 - (py / U.screen_h) * 2.0;

  var out: VSOut;
  out.pos = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
  // Use texture UV with origin at top-left (no extra flip)
  out.uv = vec2<f32>(p.x, p.y);
  return out;
}

@fragment
fn fs_main(in: VSOut) -> @location(0) vec4<f32> {
  let c = textureSample(t_tex, t_samp, in.uv);
  return vec4<f32>(c.rgb, U.alpha);
}
