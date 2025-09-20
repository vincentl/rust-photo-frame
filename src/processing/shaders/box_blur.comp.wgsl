struct Params {
  width: u32,
  height: u32,
  radius: u32,
  _pad: u32,
};

@group(0) @binding(0)
var src_tex: texture_2d<f32>;

@group(0) @binding(1)
var dst_tex: texture_storage_2d<rgba8unorm, write>;

@group(0) @binding(2)
var<uniform> params: Params;

const WORKGROUP_SIZE: u32 = 8u;

@compute @workgroup_size(WORKGROUP_SIZE, WORKGROUP_SIZE, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
  if (gid.x >= params.width || gid.y >= params.height) {
    return;
  }

  let radius: i32 = i32(params.radius);
  let max_x: i32 = i32(params.width) - 1;
  let max_y: i32 = i32(params.height) - 1;
  let base: vec2<i32> = vec2<i32>(i32(gid.x), i32(gid.y));

  var accum: vec3<f32> = vec3<f32>(0.0, 0.0, 0.0);
  var count: f32 = 0.0;

  for (var dy: i32 = -radius; dy <= radius; dy = dy + 1) {
    for (var dx: i32 = -radius; dx <= radius; dx = dx + 1) {
      let sample_pos: vec2<i32> = clamp(base + vec2<i32>(dx, dy), vec2<i32>(0, 0), vec2<i32>(max_x, max_y));
      let sample_color: vec3<f32> = textureLoad(src_tex, sample_pos, 0).rgb;
      accum = accum + sample_color;
      count = count + 1.0;
    }
  }

  let color: vec3<f32> = accum / count;
  textureStore(dst_tex, base, vec4<f32>(color, 1.0));
}
