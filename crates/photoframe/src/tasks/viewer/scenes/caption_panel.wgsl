// Solid rounded-less rectangle used as a legibility backing panel behind the
// showcase caption. Draws a single alpha-blended quad positioned in pixel space.

struct PanelUniforms {
    resolution: vec2<f32>,  // surface size in px
    _pad0: vec2<f32>,
    rect: vec4<f32>,        // x, y, w, h in px (top-left origin)
    color: vec4<f32>,       // straight-alpha rgba, 0..1
};

@group(0) @binding(0) var<uniform> U: PanelUniforms;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
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
    return vec4<f32>(ndc, 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return U.color;
}
