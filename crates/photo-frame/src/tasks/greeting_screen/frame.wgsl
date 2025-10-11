struct VertexOutput {
    @builtin(position) position: vec4<f32>,
};

struct FrameUniform {
    color: vec4<f32>;
};

@group(0) @binding(0)
var<uniform> frame: FrameUniform;

@vertex
fn vs(@location(0) position: vec2<f32>) -> VertexOutput {
    var out: VertexOutput;
    out.position = vec4<f32>(position, 0.0, 1.0);
    return out;
}

@fragment
fn fs() -> @location(0) vec4<f32> {
    return frame.color;
}
