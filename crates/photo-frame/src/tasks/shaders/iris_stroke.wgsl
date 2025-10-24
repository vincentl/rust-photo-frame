// Curved-blade iris stroke using the same SDF as the fill shader.
// Matches the SVG construction (arcs from equal-radius disks).
// Stroke width is in pixels and kept visually constant with proper AA.

const PI  : f32 = 3.1415926535897932384626433832795;
const TAU : f32 = 6.283185307179586476925286766559;

fn rot2(p: vec2<f32>, a: f32) -> vec2<f32> {
  let c = cos(a);
  let s = sin(a);
  return vec2<f32>(c * p.x - s * p.y, s * p.x + c * p.y);
}

// --- uniforms ---------------------------------------------------------------
// Adjust these names/types to match your existing bind group if needed.
struct Globals {
  resolution    : vec2<f32>; // framebuffer size in pixels (width, height)
  aspect        : f32;       // width/height
  open_scale    : f32;       // 0 (closed) .. 1 (fully open)
  rotate_rad    : f32;       // iris rotation in radians
  blades        : u32;       // number of blades
  stroke_px     : f32;       // desired stroke width in *pixels*
  _pad0         : vec3<f32>; // alignment padding
};
@group(0) @binding(0) var<uniform> G : Globals;

struct VsOut {
  @location(0) uv : vec2<f32>;
  @builtin(position) pos: vec4<f32>;
};

// --- curved-blade boundary ---------------------------------------------------
// Returns factor s so that boundary radius = s * (G.aspect * G.open_scale).
fn iris_boundary_factor(uv: vec2<f32>) -> f32 {
  // Work in isotropic space so circles stay round: scale X by aspect
  let p_iso = (uv * 2.0 - vec2<f32>(1.0, 1.0)) * vec2<f32>(G.aspect, 1.0);
  let theta = atan2(p_iso.y, p_iso.x);
  let u = vec2<f32>(cos(theta), sin(theta));

  // Circle radius R sets the fully-open horizontal span.
  let R = G.aspect; // half-width in isotropic units
  // Centers move outward as the iris closes.
  let d = (1.0 - clamp(G.open_scale, 0.0, 1.0)) * R;

  let step = TAU / f32(G.blades);
  let rc = mat2x2<f32>(cos(G.rotate_rad), -sin(G.rotate_rad),
                       sin(G.rotate_rad),  cos(G.rotate_rad));

  var r_min = 1e9;
  for (var i: u32 = 0u; i < G.blades; i = i + 1u) {
    let ang = f32(i) * step;
    let c_local = vec2<f32>(d * cos(ang), d * sin(ang));
    let c = rc * c_local;
    let cu = dot(c, u);
    let disc = max(R * R - dot(c, c) + cu * cu, 0.0);
    let lam = cu + sqrt(disc);   // ray–circle first hit
    r_min = min(r_min, lam);
  }
  // Factor so boundary radius = factor * (aspect * open_scale)
  return r_min / max(G.aspect * max(G.open_scale, 1e-4), 1e-4);
}

// Signed distance to the iris edge in isotropic space (positive inside).
fn iris_sdf(uv: vec2<f32>) -> f32 {
  let p_iso = (uv * 2.0 - vec2<f32>(1.0, 1.0)) * vec2<f32>(G.aspect, 1.0);
  let pr = rot2(p_iso, G.rotate_rad);
  let r = length(pr);
  let factor = iris_boundary_factor(uv);
  let boundary = clamp(G.open_scale, 0.0, 1.0) * G.aspect * factor;
  return boundary - r;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
  let sdf = iris_sdf(in.uv);

  // Convert stroke width from pixels to isotropic NDC units (Y maps to 2.0)
  let stroke_ndc = (G.stroke_px / max(G.resolution.y, 1.0)) * 2.0;

  // Draw where |sdf| <= w/2, with smooth AA around the edges.
  let edge = abs(sdf) - 0.5 * stroke_ndc;
  let aa  = max(fwidth(edge), 1e-4);
  let alpha = 1.0 - smoothstep(0.0, aa, edge);

  // Colors — wire these in from your pipeline as needed.
  let stroke_color = vec3<f32>(0.0, 0.0, 0.0); // black outline
  let bg_color     = vec3<f32>(0.0, 0.0, 0.0); // premultiplied over your fill

  let rgb = mix(bg_color, stroke_color, alpha);
  return vec4<f32>(rgb, alpha);
}
