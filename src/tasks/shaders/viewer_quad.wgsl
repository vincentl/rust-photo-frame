struct Uniforms {
  screen: vec4<f32>,
  dest: vec4<f32>,
  factors: vec4<f32>,
  wipe_main: vec4<f32>,
  wipe_aux: vec4<f32>,
};

const MODE_HOLD: u32 = 0u;
const MODE_FADE: u32 = 1u;
const MODE_WIPE: u32 = 2u;
const MODE_PUSH: u32 = 3u;

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
  let px = U.dest.x + p.x * U.dest.z;
  let py = U.dest.y + p.y * U.dest.w;
  // Convert from pixels to NDC (-1..1)
  let ndc_x = (px / U.screen.x) * 2.0 - 1.0;
  let ndc_y = 1.0 - (py / U.screen.y) * 2.0;

  var out: VSOut;
  out.pos = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
  // Use texture UV with origin at top-left (no extra flip)
  out.uv = vec2<f32>(p.x, p.y);
  return out;
}

@fragment
fn fs_main(in: VSOut) -> @location(0) vec4<f32> {
  let c = textureSample(t_tex, t_samp, in.uv);
  let mode = u32(U.factors.z + 0.5);
  let is_next = U.factors.w > 0.5;
  var alpha = clamp(U.factors.x, 0.0, 1.0);
  if (mode == MODE_WIPE && is_next) {
    let dir = normalize(vec2<f32>(U.wipe_main.x, U.wipe_main.y));
    let local = vec2<f32>(in.uv.x * U.dest.z, in.uv.y * U.dest.w);
    let proj = dot(local, dir);
    let start = U.wipe_main.z;
    let range = max(U.wipe_main.w, 1e-5);
    let threshold = start + clamp(U.factors.y, 0.0, 1.0) * range;
    let softness = max(U.wipe_aux.x, 0.0);
    if (softness <= 0.0) {
      alpha = select(0.0, 1.0, proj >= threshold);
    } else {
      alpha = smoothstep(threshold - softness, threshold + softness, proj);
    }
  }
  return vec4<f32>(c.rgb, clamp(alpha, 0.0, 1.0));
}
