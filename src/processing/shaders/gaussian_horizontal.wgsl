struct BlurParams {
    width: u32;
    height: u32;
    radius: u32;
    _pad: u32;
};

struct Weights {
    values: array<f32>;
};

@group(0) @binding(0)
var<uniform> params: BlurParams;

@group(0) @binding(1)
var src_tex: texture_2d<f32>;

@group(0) @binding(2)
var dst_tex: texture_storage_2d<rgba8unorm, write>;

@group(0) @binding(3)
var<storage, read> weights: Weights;

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= params.width || id.y >= params.height) {
        return;
    }

    let radius: i32 = i32(params.radius);
    var accum: vec3<f32> = vec3<f32>(0.0, 0.0, 0.0);
    var weight_sum: f32 = 0.0;

    for (var offset: i32 = -radius; offset <= radius; offset = offset + 1) {
        let weight_index: u32 = u32(offset + radius);
        let w: f32 = weights.values[weight_index];
        let sample_x: i32 = clamp(i32(id.x) + offset, 0, i32(params.width) - 1);
        let color: vec4<f32> = textureLoad(src_tex, vec2<i32>(sample_x, i32(id.y)), 0);
        accum = accum + color.rgb * w;
        weight_sum = weight_sum + w;
    }

    if (weight_sum > 0.0) {
        accum = accum / weight_sum;
    }

    dst_tex.write(vec2<i32>(i32(id.x), i32(id.y)), vec4<f32>(accum, textureLoad(src_tex, vec2<i32>(i32(id.x), i32(id.y)), 0).a));
}
