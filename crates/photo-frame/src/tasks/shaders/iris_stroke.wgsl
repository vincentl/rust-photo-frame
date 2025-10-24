// Geometry-driven iris stroke shader matching the pipeline layout in
// crates/photo-frame/src/tasks/viewer/iris/mod.rs and geometry.rs

const PI  : f32 = 3.1415926535897932384626433832795;
const TAU : f32 = 6.283185307179586476925286766559;

fn rot2(p: vec2<f32>, a: f32) -> vec2<f32> {
  let c = cos(a);
  let s = sin(a);
  return vec2<f32>(c * p.x - s * p.y, s * p.x + c * p.y);
}

// Storage buffer with cubic Bezier segments (built on CPU)
struct Cubic {
  p0: vec2<f32>,
  p1: vec2<f32>,
  p2: vec2<f32>,
  p3: vec2<f32>,
};
struct CubicBuf { data: array<Cubic>, };
@group(0) @binding(0) var<storage, read> CUBICS: CubicBuf;

// Uniform parameters (kept in sync with geometry.rs::Params)
struct Params {
  mvp: mat4x4<f32>,
  viewport_px: vec2<f32>,
  half_width_px: f32,
  _pad0: f32,
  segments_per_cubic: u32,
  petal_count: u32,
  cubics_count: u32,
  _pad1: u32,
  color_rgba: vec4<f32>,
};
@group(0) @binding(1) var<uniform> P: Params;

fn cubic_point(c: Cubic, t: f32) -> vec2<f32> {
  let u = 1.0 - t;
  return (u * u * u) * c.p0 +
         (3.0 * u * u * t) * c.p1 +
         (3.0 * u * t * t) * c.p2 +
         (t * t * t) * c.p3;
}

fn cubic_tangent(c: Cubic, t: f32) -> vec2<f32> {
  let u = 1.0 - t;
  return 3.0 * u * u * (c.p1 - c.p0) +
         6.0 * u * t * (c.p2 - c.p1) +
         3.0 * t * t * (c.p3 - c.p2);
}

fn to_clip(p: vec2<f32>) -> vec2<f32> {
  return (P.mvp * vec4<f32>(p, 0.0, 1.0)).xy;
}

fn dir_to_clip(d: vec2<f32>) -> vec2<f32> {
  return (P.mvp * vec4<f32>(d, 0.0, 0.0)).xy;
}

struct VsOut {
  @builtin(position) pos: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32, @builtin(instance_index) iid: u32) -> VsOut {
  // Map vertex index to (cubic, segment, side)
  let segs = max(P.segments_per_cubic, 1u);
  let seg_idx_global = vid >> 1u;            // 1 pair per segment
  let side = (vid & 1u);                     // 0 -> one side, 1 -> other
  let cubic_idx = seg_idx_global / segs;
  let seg_idx_local = seg_idx_global - cubic_idx * segs;

  let cubic = CUBICS.data[min(cubic_idx, P.cubics_count - 1u)];
  let t = f32(seg_idx_local) / f32(segs);

  // Instance rotation around origin to duplicate petals
  let petals = max(P.petal_count, 1u);
  let phi = TAU * (f32(iid) / f32(petals));

  let p_local = rot2(cubic_point(cubic, t), phi);
  let d_local = rot2(cubic_tangent(cubic, t), phi);

  // Convert to clip, then compute pixel-constant normal offset
  let p_clip = to_clip(p_local);
  let t_clip = dir_to_clip(d_local);

  // Convert clip-space direction to pixel space for constant width
  let px_per_clip = 0.5 * P.viewport_px; // since clip spans [-1,1]
  let t_px = vec2<f32>(t_clip.x * px_per_clip.x, t_clip.y * px_per_clip.y);
  let len_t = max(length(t_px), 1e-6);
  let n_px = vec2<f32>(-t_px.y, t_px.x) / len_t;
  let sign = select(-1.0, 1.0, side == 1u);
  let off_px = n_px * (P.half_width_px * sign);
  let off_clip = vec2<f32>(off_px.x / px_per_clip.x, off_px.y / px_per_clip.y);

  var out: VsOut;
  out.pos = vec4<f32>(p_clip + off_clip, 0.0, 1.0);
  return out;
}

@fragment
fn fs_main(_in: VsOut) -> @location(0) vec4<f32> {
  return P.color_rgba;
}
