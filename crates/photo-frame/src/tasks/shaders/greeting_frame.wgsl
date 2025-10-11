struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(3.0, 1.0),
        vec2<f32>(-1.0, 1.0),
    );
    var uvs = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 2.0),
        vec2<f32>(2.0, 0.0),
        vec2<f32>(0.0, 0.0),
    );

    var out: VertexOutput;
    out.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    out.uv = uvs[vertex_index];
    return out;
}

struct FrameUniforms {
    size_outer_inner: vec4<f32>;
    gap_radius: vec4<f32>;
    color: vec4<f32>;
};

@group(0) @binding(0)
var<uniform> uniforms: FrameUniforms;

fn sd_round_rect(p: vec2<f32>, size: vec2<f32>, radius: f32) -> f32 {
    let half_size = size * 0.5;
    let clamped_radius = min(radius, min(half_size.x, half_size.y));
    let q = abs(p) - half_size + vec2<f32>(clamped_radius, clamped_radius);
    return length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - clamped_radius;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let size = uniforms.size_outer_inner.xy;
    if (size.x <= 0.0 || size.y <= 0.0) {
        discard;
    }

    let outer_stroke = uniforms.size_outer_inner.z;
    let inner_stroke = uniforms.size_outer_inner.w;
    let gap = uniforms.gap_radius.x;
    let corner_radius = uniforms.gap_radius.y;

    let p = (in.uv * size) - (size * 0.5);

    let d_outer = sd_round_rect(p, size, corner_radius);
    if (d_outer > 0.0 && inner_stroke <= 0.0 && outer_stroke <= 0.0) {
        discard;
    }

    if (d_outer > 0.0 || d_outer < -outer_stroke) {
        // Potentially inside gap region; check inner stroke if present.
        if (inner_stroke <= 0.0) {
            discard;
        }

        let offset_inner = outer_stroke + gap;
        let inner_size = size - vec2<f32>(2.0 * offset_inner, 2.0 * offset_inner);
        if (inner_size.x <= 0.0 || inner_size.y <= 0.0) {
            discard;
        }

        let inner_radius = max(corner_radius - offset_inner, 0.0);
        let d_inner = sd_round_rect(p, inner_size, inner_radius);
        if (d_inner > 0.0 || d_inner < -inner_stroke) {
            discard;
        }
        return uniforms.color;
    }

    return uniforms.color;
}
