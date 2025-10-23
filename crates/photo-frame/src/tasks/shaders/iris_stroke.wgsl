struct Cubic {
  p0: vec2<f32>;
  p1: vec2<f32>;
  p2: vec2<f32>;
  p3: vec2<f32>;
};

struct Params {
  mvp: mat4x4<f32>;
  viewport_px: vec2<f32>;
  half_width_px: f32;
  _pad0: f32;
  segments_per_cubic: u32;
  petal_count: u32;
  cubics_count: u32;
  _pad1: u32;
  color_rgba: vec4<f32>;
};

@group(0) @binding(0) var<storage, read> cubics: array<Cubic>;
@group(0) @binding(1) var<uniform> params: Params;

fn eval_cubic(p0: vec2<f32>, p1: vec2<f32>, p2: vec2<f32>, p3: vec2<f32>, t: f32) -> vec2<f32> {
  let omt = 1.0 - t;
  let omt2 = omt * omt;
  let t2  = t * t;
  return p0 * (omt2 * omt) +
         p1 * (3.0 * omt2 * t) +
         p2 * (3.0 * omt * t2) +
         p3 * (t2 * t);
}

fn to_clip(p: vec2<f32>) -> vec4<f32> {
  return params.mvp * vec4<f32>(p, 0.0, 1.0);
}

fn clip_to_ndc_xy(c: vec4<f32>) -> vec2<f32> {
  return c.xy / c.w;
}

fn px_to_ndc(px: vec2<f32>) -> vec2<f32> {
  return 2.0 * px / params.viewport_px;
}

fn safe_normalize(v: vec2<f32>) -> vec2<f32> {
  let len = length(v);
  if len > 1e-5 {
    return v / len;
  }
  return vec2<f32>(0.0, 1.0);
}

struct VSOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) v_color: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32,
           @builtin(instance_index) iid: u32) -> VSOut {
  let segs = max(params.segments_per_cubic, 1u);
  let total_steps = segs * params.cubics_count;

  let k = min(vid >> 1u, total_steps);
  let side = (vid & 1u);

  let cubic_idx = select(k / segs, 0u, params.cubics_count == 0u);
  let step_in_cubic = select(k % segs, 0u, params.cubics_count == 0u);

  let inv_segs = 1.0 / f32(segs);
  let t0 = f32(step_in_cubic) * inv_segs;
  let t1 = min(1.0, t0 + inv_segs);

  let c = cubics[cubic_idx];
  var p0 = eval_cubic(c.p0, c.p1, c.p2, c.p3, t0);
  let p1 = eval_cubic(c.p0, c.p1, c.p2, c.p3, t1);

  let count = max(params.petal_count, 1u);
  let angle = (f32(iid) * 6.28318530718) / f32(count);
  let cs = cos(angle);
  let sn = sin(angle);
  let rot = mat2x2<f32>(cs, -sn, sn, cs);
  p0 = rot * p0;
  let p1r = rot * p1;

  let p0_clip = to_clip(p0);
  let p1_clip = to_clip(p1r);
  let p0_ndc = clip_to_ndc_xy(p0_clip);
  let p1_ndc = clip_to_ndc_xy(p1_clip);

  let dir = safe_normalize(p1_ndc - p0_ndc);
  let n_ndc = vec2<f32>(-dir.y, dir.x);

  let half_px = vec2<f32>(params.half_width_px, params.half_width_px);
  let half_ndc = px_to_ndc(half_px);

  let offset_ndc = safe_normalize(n_ndc) * half_ndc;
  let signed = select(-1.0, 1.0, side == 1u);
  let out_ndc = p0_ndc + signed * offset_ndc;

  var outv: VSOut;
  outv.pos = vec4<f32>(out_ndc, 0.0, 1.0);
  outv.v_color = params.color_rgba;
  return outv;
}

@fragment
fn fs_main(@location(0) v_color: vec4<f32>) -> @location(0) vec4<f32> {
  return v_color;
}
