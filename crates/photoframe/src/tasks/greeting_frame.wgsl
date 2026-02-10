struct FrameUniforms {
    resolution: vec2<f32>,
    _pad0: vec2<f32>,
    insets: vec4<f32>,
    radii: vec4<f32>,
    accent: vec4<f32>,
    background: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> uniforms: FrameUniforms;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(-1.0, 3.0),
        vec2<f32>(3.0, -1.0),
    );

    var out: VertexOutput;
    out.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    out.uv = out.position.xy * 0.5 + vec2<f32>(0.5, 0.5);
    return out;
}

fn rounded_rect_sdf(p: vec2<f32>, half_size: vec2<f32>, radius: f32) -> f32 {
    let clamped_half = max(half_size, vec2<f32>(0.0, 0.0));
    let clamped_radius = min(radius, min(clamped_half.x, clamped_half.y));
    let q = abs(p) - clamped_half + vec2<f32>(clamped_radius, clamped_radius);
    return length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - clamped_radius;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let resolution = uniforms.resolution;
    let coord = in.uv * resolution;
    let centered = coord - resolution * 0.5;
    let half_extent = resolution * 0.5;

    let outer_inset = uniforms.insets.x;
    let outer_inner_inset = uniforms.insets.y;
    let inner_outer_inset = uniforms.insets.z;
    let inner_inner_inset = uniforms.insets.w;

    let outer_outer_half = max(half_extent - vec2<f32>(outer_inset, outer_inset), vec2<f32>(0.0, 0.0));
    let outer_inner_half = max(half_extent - vec2<f32>(outer_inner_inset, outer_inner_inset), vec2<f32>(0.0, 0.0));
    let inner_outer_half = max(half_extent - vec2<f32>(inner_outer_inset, inner_outer_inset), vec2<f32>(0.0, 0.0));
    let inner_inner_half = max(half_extent - vec2<f32>(inner_inner_inset, inner_inner_inset), vec2<f32>(0.0, 0.0));

    let outer_outer_radius = max(uniforms.radii.x, 0.0);
    let outer_inner_radius = max(uniforms.radii.y, 0.0);
    let inner_outer_radius = max(uniforms.radii.z, 0.0);
    let inner_inner_radius = max(uniforms.radii.w, 0.0);

    let outer_outer_dist = rounded_rect_sdf(centered, outer_outer_half, outer_outer_radius);
    let outer_inner_dist = rounded_rect_sdf(centered, outer_inner_half, outer_inner_radius);
    let inner_outer_dist = rounded_rect_sdf(centered, inner_outer_half, inner_outer_radius);
    let inner_inner_dist = rounded_rect_sdf(centered, inner_inner_half, inner_inner_radius);

    let aa_outer_outer = max(fwidth(outer_outer_dist), 1e-3);
    let aa_outer_inner = max(fwidth(outer_inner_dist), 1e-3);
    let aa_inner_outer = max(fwidth(inner_outer_dist), 1e-3);
    let aa_inner_inner = max(fwidth(inner_inner_dist), 1e-3);

    let outer_shell = (1.0 - smoothstep(0.0, aa_outer_outer, outer_outer_dist)) *
        smoothstep(0.0, aa_outer_inner, outer_inner_dist);
    let inner_shell = (1.0 - smoothstep(0.0, aa_inner_outer, inner_outer_dist)) *
        smoothstep(0.0, aa_inner_inner, inner_inner_dist);

    let coverage = clamp(outer_shell + inner_shell, 0.0, 1.0);
    let color = mix(uniforms.background.rgb, uniforms.accent.rgb, coverage);

    return vec4<f32>(color, 1.0);
}
