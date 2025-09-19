struct Params {
    width: u32,
    height: u32,
    radius: u32,
    direction: u32,
};

struct PixelBuffer {
    pixels: array<vec4<f32>>,
};

struct Weights {
    values: array<f32>,
};

@group(0) @binding(0)
var<uniform> U: Params;

@group(0) @binding(1)
var<storage, read> Src: PixelBuffer;

@group(0) @binding(2)
var<storage, read_write> Dst: PixelBuffer;

@group(0) @binding(3)
var<storage, read> W: Weights;

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= U.width || id.y >= U.height) {
        return;
    }

    let idx = id.y * U.width + id.x;
    let kernel_width = U.radius * 2u + 1u;
    var acc: vec4<f32> = vec4<f32>(0.0);

    for (var i: u32 = 0u; i < kernel_width; i = i + 1u) {
        let offset = i32(i) - i32(U.radius);
        var sample_x = i32(id.x);
        var sample_y = i32(id.y);
        if (U.direction == 0u) {
            sample_x = clamp(i32(id.x) + offset, 0, i32(U.width) - 1);
        } else {
            sample_y = clamp(i32(id.y) + offset, 0, i32(U.height) - 1);
        }
        let sample_idx = u32(sample_y) * U.width + u32(sample_x);
        let weight = W.values[i];
        acc = acc + Src.pixels[sample_idx] * weight;
    }

    Dst.pixels[idx] = acc;
}
