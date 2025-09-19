const MAX_RADIUS : u32 = 32u;
const TAP_COUNT : u32 = MAX_RADIUS * 2u + 1u;
const KERNEL_SIZE : u32 = ((TAP_COUNT + 3u) / 4u) * 4u;

struct Params {
    width : u32,
    height : u32,
    radius : u32,
    direction : u32,
    weights : array<f32, KERNEL_SIZE>,
};

@group(0) @binding(0)
var src_tex : texture_2d<f32>;

@group(0) @binding(1)
var dst_tex : texture_storage_2d<rgba8unorm, write>;

@group(0) @binding(2)
var<uniform> params : Params;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid : vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) {
        return;
    }
    let coord = vec2<i32>(i32(gid.x), i32(gid.y));
    let max_x = i32(params.width) - 1;
    let max_y = i32(params.height) - 1;
    let radius = i32(params.radius);
    var accum = vec4<f32>(0.0);
    for (var i = -radius; i <= radius; i = i + 1) {
        var sample_coord = coord;
        if (params.direction == 0u) {
            sample_coord.x = clamp(coord.x + i, 0, max_x);
        } else {
            sample_coord.y = clamp(coord.y + i, 0, max_y);
        }
        let index = u32(i + radius);
        let weight = params.weights[index];
        let texel = textureLoad(src_tex, sample_coord, 0);
        accum = accum + texel * weight;
    }
    textureStore(dst_tex, coord, accum);
}
