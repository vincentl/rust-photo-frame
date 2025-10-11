struct FrameUniforms {
  color: vec4<f32>;
  screen_size: vec2<f32>;
  _pad: vec2<f32>;
};

@group(0) @binding(0)
var<uniform> U: FrameUniforms;

struct VSOut {
  @builtin(position) pos: vec4<f32>;
};

@vertex
fn vs_main(@location(0) position: vec2<f32>) -> VSOut {
  let size = max(U.screen_size, vec2<f32>(1.0, 1.0));
  let ndc = vec2<f32>(
    position.x / size.x * 2.0 - 1.0,
    1.0 - position.y / size.y * 2.0,
  );
  var out: VSOut;
  out.pos = vec4<f32>(ndc, 0.0, 1.0);
  return out;
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
  return U.color;
}
