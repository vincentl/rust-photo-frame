// Composites the cached caption texture as a single quad positioned in pixel
// space at the bottom-left of the surface. The cache holds PREMULTIPLIED alpha,
// so the pipeline blends it with premultiplied "over".

struct CompositeUniforms {
    resolution: vec2<f32>,  // surface size in px
    _pad0: vec2<f32>,
    rect: vec4<f32>,        // x, y, w, h in px (top-left origin)
};

@group(0) @binding(0) var<uniform> U: CompositeUniforms;
@group(0) @binding(1) var cap_tex: texture_2d<f32>;
@group(0) @binding(2) var cap_samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
    );
    let c = corners[vi];
    let px = U.rect.xy + c * U.rect.zw;
    let ndc = vec2<f32>(
        px.x / max(U.resolution.x, 1.0) * 2.0 - 1.0,
        1.0 - px.y / max(U.resolution.y, 1.0) * 2.0,
    );
    var out: VsOut;
    out.pos = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = c;  // 0..1 across the quad; v=0 maps to texture row 0 (top)
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Cache is already premultiplied; return as-is for premultiplied "over".
    return textureSample(cap_tex, cap_samp, in.uv);
}
