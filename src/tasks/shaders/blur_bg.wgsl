struct Uniforms {
  screen_w: f32,
  screen_h: f32,
  dest_x: f32,
  dest_y: f32,
  dest_w: f32,
  dest_h: f32,
  alpha: f32, // re-used as dim amount
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
  // Fullscreen triangle (covers entire target)
  var positions = array<vec2<f32>, 3>(
    vec2<f32>(-1.0, -3.0),
    vec2<f32>(-1.0, 1.0),
    vec2<f32>(3.0, 1.0),
  );
  var uvs = array<vec2<f32>, 3>(
    vec2<f32>(0.0, 2.0),
    vec2<f32>(0.0, 0.0),
    vec2<f32>(2.0, 0.0),
  );
  var out: VSOut;
  out.pos = vec4<f32>(positions[vid], 0.0, 1.0);
  out.uv = uvs[vid];
  return out;
}

@fragment
fn fs_main(in: VSOut) -> @location(0) vec4<f32> {
  // Simple 9-tap box blur around UV
  let texel = vec2<f32>(1.0 / U.screen_w, 1.0 / U.screen_h);
  var sum: vec3<f32> = vec3<f32>(0.0);
  var count: f32 = 0.0;
  for (var dy: i32 = -1; dy <= 1; dy = dy + 1) {
    for (var dx: i32 = -1; dx <= 1; dx = dx + 1) {
      let uv = in.uv + vec2<f32>(f32(dx), f32(dy)) * texel * 4.0;
      let c = textureSample(t_tex, t_samp, uv).rgb;
      sum = sum + c;
      count = count + 1.0;
    }
  }
  var color = sum / count;
  // Dim the background using alpha as dim factor
  color = mix(color, vec3<f32>(0.0), clamp(U.alpha, 0.0, 1.0));
  return vec4<f32>(color, 1.0);
}

