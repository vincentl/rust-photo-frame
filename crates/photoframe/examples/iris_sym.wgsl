// Iris petal layer, exploiting the petals' N-fold rotational symmetry.
// (Appended to viewer_quad.wgsl by examples/iris_bench.rs; experiment only.)
//
// Petal i is petal 0 rotated by i * TAU / n about the screen center
// (viewer.rs solves pivot, annulus center, tip and trail as rotated copies;
// only the sheen's facing differs, and that is applied after the loop). So
// the cover pass's answer at a point rotated by k sectors is the answer at
// the point, with every petal index shifted by k. fs_iris_sector runs the
// unchanged loop once, over one sector in polar texels (x: radius at the
// layer's spacing, y: angle at the layer's spacing on the outermost arc,
// one margin texel on every side); fs_iris_layer_sym maps each layer texel
// into the sector, reads the cover there, and recomputes the signed
// distance exactly at its own position from the two nearest petals (so the
// outline keeps the full-resolution layer's precision; only which petal is
// on top, and how many cover, come from the polar texel).

struct SymGeom {
  alpha: f32,     // sector angle, TAU / n
  dphi: f32,      // angle per sector texel row
  ls: f32,        // screen pixels per layer / sector texel
};

fn sym_geom() -> SymGeom {
  let n = max(i32(U.params0.x), 1);
  let ls = max(U.params3.x, 1.0);
  let layer = U.screen_size / ls;
  let r_max = 0.5 * length(layer);
  let alpha = 6.28318530718 / f32(n);
  let rows = ceil(alpha * r_max);
  var g: SymGeom;
  g.alpha = alpha;
  g.dphi = alpha / rows;
  g.ls = ls;
  return g;
}

@fragment
fn fs_iris_sector(in: VSOut) -> @location(0) vec4<f32> {
  let g = sym_geom();
  let n = max(i32(U.params0.x), 1);
  let r = max(in.pos.x - 1.0, 0.0) * g.ls;
  let phi = (in.pos.y - 1.0) * g.dphi;
  let p = r * vec2<f32>(cos(phi), sin(phi));
  let free = U.params1.x - r;
  if (free > iris_reach(g.ls)) {
    return vec4<f32>(free, 0.0, 0.0, 0.0);
  }
  let r_mid = U.params3.y;
  let w = U.params3.z;
  var d_min = 1e9;
  var d_2nd = 1e9;
  var nearest = 0;
  var second = 0;
  var covered = 0u;
  for (var i: i32 = 0; i < 16; i++) {
    if (i >= n) { break; }
    let d = iris_petal(i, p, r_mid, w);
    let is_min = d < d_min;
    let is_2nd = d < d_2nd;
    second = select(select(second, i, is_2nd), nearest, is_min);
    d_2nd = select(select(d_2nd, d, is_2nd), d_min, is_min);
    nearest = select(nearest, i, is_min);
    d_min = min(d_min, d);
    covered = covered | (select(0u, 1u, d < 0.0) << u32(i));
  }
  let un = u32(n);
  let all_petals = (1u << un) - 1u;
  let succ = ((covered >> 1u) | (covered << (un - 1u))) & all_petals;
  let candidates = covered & ~succ;
  let top = select(0, i32(firstLeadingBit(candidates)), candidates != 0u);
  let shown = select(nearest, top, covered != 0u);
  return vec4<f32>(d_min, f32(shown), f32(countOneBits(covered)), f32(nearest * 16 + second));
}

fn wrap_petal(i: i32, n: i32) -> i32 {
  var j = i;
  if (j >= n) { j -= n; }
  return j;
}

@fragment
fn fs_iris_layer_sym(in: VSOut) -> @location(0) vec4<f32> {
  let g = sym_geom();
  let n = max(i32(U.params0.x), 1);
  let p = in.screen_uv * U.screen_size - 0.5 * U.screen_size;
  let r = length(p);
  var theta = atan2(p.y, p.x);
  theta = select(theta, theta + 6.28318530718, theta < 0.0);
  let k = min(i32(floor(theta / g.alpha)), n - 1);
  let phi0 = theta - f32(k) * g.alpha;
  let texel = vec2<i32>(i32(floor(r / g.ls + 1.0)), i32(floor(phi0 / g.dphi + 1.0)));
  let cover = textureLoad(cur_tex, texel, 0);
  let reach = iris_reach(g.ls);
  let free = U.params1.x - r;
  if (cover.x > reach) {
    return vec4<f32>(0.0, 0.0, 0.0, iris_d8_encode(max(cover.x, free)));
  }
  let r_mid = U.params3.y;
  let w = U.params3.z;
  let packed = i32(cover.w);
  let nearest = wrap_petal((packed >> 4u) + k, n);
  let second = wrap_petal((packed & 15) + k, n);
  let d_min = min(iris_petal(nearest, p, r_mid, w), iris_petal(second, p, r_mid, w));
  let top = wrap_petal(i32(cover.y) + k, n);
  let covered_count = i32(cover.z);

  let contrast = clamp(U.params0.y, 0.0, 1.0);
  let shadow_amt = clamp(U.params0.z, 0.0, 1.0);
  let r_in = 2.0 * w;
  let j1 = wrap_petal(top + 1, n);
  let j2 = wrap_petal(top + 2, n);
  let IRIS_SHEEN_MAX = 0.38;
  let enclosure = f32(covered_count) / f32(n);
  let var_scale = 1.0 - smoothstep(1.0 - 2.0 / f32(n), 1.0, enclosure);
  let tone = max(1.0 + contrast * IRIS_SHEEN_MAX * P.petals_b[top].z * var_scale, 0.0);
  let shadow_w = max(0.012 * r_in, 4.0);
  let dn1 = max(iris_petal(j1, p, r_mid, w), 0.0);
  let dn2 = max(iris_petal(j2, p, r_mid, w), 0.0);
  let occ = 0.5 * (1.0 - smoothstep(0.0, shadow_w, dn1))
    + 0.22 * (1.0 - smoothstep(0.0, shadow_w * 0.6, dn2));
  let blade_rgb = clamp(U.params1.yzw, vec3<f32>(0.0), vec3<f32>(1.0));
  return vec4<f32>(blade_rgb * tone * (1.0 - shadow_amt * occ), iris_d8_encode(d_min));
}
