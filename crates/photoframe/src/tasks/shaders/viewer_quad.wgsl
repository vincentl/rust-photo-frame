// One fragment entry point per transition kind (`fs_<kind>`), each with
// only its own math, compiled into its own render pipeline and chosen on the
// CPU (`DrawKind` in viewer.rs). The V3D compiler allocates registers for the
// whole shader: one `switch` over every kind made each kind pay the register
// pressure of the heaviest (975 instructions, 47 temporaries), which forced
// the shader down to 2 threads per QPU. V3D hides texture latency by
// switching among 4 threads, so every transition, even a fade, ran its
// texture fetches at a fraction of the speed (Pi measurement, 2026-10-21).
// Keep each entry point small enough to compile at 4 threads; check with
// V3D_DEBUG=shaderdb ("4 threads", no "Falling back" lines).
//
// At 4 threads the cost is instructions per pixel, so everything that is
// the same for every pixel of a frame is computed on the CPU
// (`draw_params.rs`): where each photo plane lands in texture coordinates
// (an affine map of the screen, interpolated by the vertex stage, so the
// fragment stage does no division), and each kind's per-frame constants.
// A plane is sampled without branches (`plane`).

struct TransitionUniforms {
  screen_size: vec2<f32>,
  progress: f32,
  // The canvas mip level the photo planes are sampled at: log2 of the
  // transition intermediate's downscale while one is drawn (the canvases
  // carry that many 2:1 box-filtered levels, generated at upload), 0 for
  // native frames. Sampling a level whose texels match the target's pixels
  // reads a quarter (scale 2) of the memory of minifying level 0.
  lod: f32,
  // Where the current / next photo plane lands: texture coordinate =
  // screen_uv * xform.xy + xform.zw (a plane that is not there maps every
  // pixel outside 0..1).
  current_xform: vec4<f32>,
  next_xform: vec4<f32>,
  params0: vec4<f32>,
  params1: vec4<f32>,
  params3: vec4<f32>,
  // Viewer background (linear RGB, alpha unused); letterbox regions are
  // composited over this in-shader so the pipeline can render opaquely.
  background: vec4<f32>,
};

// Per-petal constants for the iris transition, solved on the CPU each frame
// (see the Iris arm in viewer.rs). Only fs_iris_layer reads them; a separate
// uniform so no other pipeline loads them.
// petals_a[i] = (annulus_center.xy, tip_dir.xy)
// petals_b[i] = (trail_dir.xy, facing_cosine, unused)
struct IrisPetals {
  petals_a: array<vec4<f32>, 16>,
  petals_b: array<vec4<f32>, 16>,
};

@group(0) @binding(0)
var<uniform> U: TransitionUniforms;
@group(0) @binding(1)
var<uniform> P: IrisPetals;

@group(1) @binding(0)
var cur_tex: texture_2d<f32>;
@group(1) @binding(1)
var cur_samp: sampler;

@group(2) @binding(0)
var next_tex: texture_2d<f32>;
@group(2) @binding(1)
var next_samp: sampler;

// Reduced-resolution iris petal layer rendered by fs_iris_layer (premultiplied
// alpha). Bound to the blank texture when no iris transition is active.
@group(3) @binding(0)
var petal_tex: texture_2d<f32>;
@group(3) @binding(1)
var petal_samp: sampler;

struct VSOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) screen_uv: vec2<f32>,
  // Texture coordinates of the current / next photo plane (affine in the
  // screen, so interpolating them is exact).
  @location(1) current_uv: vec2<f32>,
  @location(2) next_uv: vec2<f32>,
};

// One triangle covering the screen (draw 3 vertices): (-1,-1), (3,-1),
// (-1,3). Computed from the vertex index with bit operations, so there is
// no dynamically indexed local array (V3D demotes those to scratch memory),
// and no diagonal seam between two triangles.
@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VSOut {
  let p = vec2<f32>(f32((vid << 1u) & 2u), f32(vid & 2u)) * 2.0 - 1.0;
  var out: VSOut;
  out.pos = vec4<f32>(p, 0.0, 1.0);
  out.screen_uv = vec2<f32>((p.x + 1.0) * 0.5, (1.0 - p.y) * 0.5);
  out.current_uv = out.screen_uv * U.current_xform.xy + U.current_xform.zw;
  out.next_uv = out.screen_uv * U.next_xform.xy + U.next_xform.zw;
  return out;
}

// Diagnostic (PHOTOFRAME_PROBE=single-sample, set as a pipeline constant):
// the next plane's color is a constant instead of a texture fetch, so every
// kind reads one photo per pixel. The output is wrong by design; it only
// measures what the second fetch costs. Off (and compiled out) otherwise.
override PROBE_SINGLE_SAMPLE: bool = false;

// Transition shaders are straight-line: every photo is fetched and every
// smoothstep evaluated on every pixel, even where a weight is zero. On V3D a
// per-pixel branch that skips work costs more than it saves (Pi, 2026-10-22:
// 0.4-1.5 ms per 1080p frame against two mip-1 fetches; see "Branch-free
// transition shaders" in docs/performance.md).

// 1 inside a photo plane (texture coordinate in 0..1), 0 outside.
fn covered(uv: vec2<f32>) -> f32 {
  return select(0.0, 1.0, all(uv >= vec2<f32>(0.0)) && all(uv <= vec2<f32>(1.0)));
}

// The color of a plane at `uv`, from the mip level U.lod.
fn fetch_current(uv: vec2<f32>) -> vec3<f32> {
  return textureSampleLevel(cur_tex, cur_samp, uv, U.lod).rgb;
}

fn fetch_next(uv: vec2<f32>) -> vec3<f32> {
  if (PROBE_SINGLE_SAMPLE) {
    return vec3<f32>(0.5);
  }
  return textureSampleLevel(next_tex, next_samp, uv, U.lod).rgb;
}

// A photo plane: its color with coverage 1 inside, transparent outside.
// Branchless: the fetch is always taken (the sampler clamps to the edge) and
// the coverage is a select.
fn current_plane(uv: vec2<f32>) -> vec4<f32> {
  let a = covered(uv);
  return vec4<f32>(fetch_current(uv) * a, a);
}

fn next_plane(uv: vec2<f32>) -> vec4<f32> {
  let a = covered(uv);
  return vec4<f32>(fetch_next(uv) * a, a);
}

// Signed distance to iris petal `i` using the CPU-precomputed constants in
// P.petals_a / P.petals_b. Returns (distance, radius about the petal's
// annulus center). On-screen the only petal boundaries are the inner arc and
// the two end caps, so this band+cap distance is exact for visible pixels.
fn iris_petal(i: i32, p: vec2<f32>, r_mid: f32, w: f32) -> f32 {
  let a = P.petals_a[i];
  let b = P.petals_b[i];
  let v = p - a.xy;
  let r = length(v);
  var d: f32;
  if (a.z * v.y - a.w * v.x > 0.0) {
    // Past the tip: distance to the tip cap.
    d = length(v - r_mid * a.zw) - w;
  } else if (b.x * v.y - b.y * v.x < 0.0) {
    // Behind the pivot: distance to the trailing cap (always off-screen, so
    // this only needs to be a positive lower bound here).
    d = length(v - r_mid * b.xy) - w;
  } else {
    // Within the petal's angular span: the annular band.
    d = abs(r - r_mid) - w;
  }
  return d;
}

// The iris petals render into a reduced-resolution offscreen layer as a
// signed distance and a shade, in two passes: fs_iris_cover finds, per layer
// pixel, the nearest petal edge and which petals cover it (the loop over
// petals); fs_iris_layer shades from that (no loop). The main pass (fs_iris)
// samples the layer bilinearly and draws the petals' outline itself from
// the interpolated distance, with an anti-aliasing band of its own pixel
// size: a signed distance interpolates cleanly, so the outline stays crisp
// however coarse the layer, and the layer resolution only sets how finely
// the shading (sheen, overlap shadows, and where one petal's edge lies over
// another) is sampled. Coordinates derive from screen_uv and U.screen_size,
// so the same math works at any target resolution.
// params0 = (blades, petal_sheen, overlap_shadow, photo_swap_mix)
// params1 = (open_radius_px, color.r, color.g, color.b)
// params3 = (layer_scale, r_mid, half band width w = r_in / 2, edge band):
// the petal ring's geometry, the same for every pixel, from the CPU; the
// edge band is the main pass's anti-aliasing half-width, in screen pixels.
//
// Register pressure decides these shaders' speed on V3D. Split, the loop
// carries only what it needs (the position, the nearest distance and petal,
// and a bitmask of covered petals) and the shading starts from four stored
// numbers; both compile at 4 threads (Pi, 2026-10-22).

// The edge highlight's scale, in screen pixels: the rim spans 0.5 to 3.5 of
// these inside a petal's outline (the look of the 1/4-resolution layer this
// replaced, whose feather was one layer texel).
const IRIS_RIM_PX: f32 = 4.0;

// How far outside the petals the layer must hold a true distance and a
// shade: a bilinear sample reads texels up to one texel (layer_scale screen
// pixels) per axis away, and the rim reaches 3.5 IRIS_RIM_PX in.
fn iris_reach(layer_scale: f32) -> f32 {
  return 2.0 * layer_scale + 3.5 * IRIS_RIM_PX;
}

// Per layer pixel: (signed distance to the petals' outline d_min, the petal
// whose shade shows, number of covered petals, 0), into an Rgba16Float
// cover target. The petal shown is the topmost covered one or, just outside
// the outline, the nearest, so the shade extends past the edge for the main
// pass to interpolate. Half floats hold the counts exactly and the distance
// to well under a hundredth of a pixel near an edge.
@fragment
fn fs_iris_cover(in: VSOut) -> @location(0) vec4<f32> {
  let screen_pos = in.screen_uv * U.screen_size;
  let n = max(i32(U.params0.x), 1);
  let p = screen_pos - 0.5 * U.screen_size;
  // The aperture circle is inscribed in the petals' opening, so its
  // distance bounds every petal's from below; far enough inside it, that
  // bound is all the main pass needs (it reads only that no petal is near).
  let free = U.params1.x - length(p);
  if (free > iris_reach(max(U.params3.x, 1.0))) {
    return vec4<f32>(free, 0.0, 0.0, 0.0);
  }
  let r_mid = U.params3.y;
  let w = U.params3.z;
  // No local arrays: dynamically indexed function-scope arrays get demoted
  // to per-pixel scratch memory on V3D.
  // Petals shingle cyclically (each rests on the next); the covering petals
  // always form one contiguous run, so the topmost is the covered petal
  // whose successor is uncovered.
  var d_min = 1e9;
  var nearest = 0;
  var covered = 0u;
  for (var i: i32 = 0; i < 16; i++) {
    if (i >= n) { break; }
    let d = iris_petal(i, p, r_mid, w);
    nearest = select(nearest, i, d < d_min);
    d_min = min(d_min, d);
    covered = covered | (select(0u, 1u, d < 0.0) << u32(i));
  }
  // Bit j of `succ` is petal (j + 1) mod n covered; the candidates are
  // covered petals with an uncovered successor. The topmost is the highest
  // candidate (the wrap-around candidate n - 1 wins); none (every petal
  // covers the pixel) gives 0.
  let un = u32(n);
  let all_petals = (1u << un) - 1u;
  let succ = ((covered >> 1u) | (covered << (un - 1u))) & all_petals;
  let candidates = covered & ~succ;
  let top = select(0, i32(firstLeadingBit(candidates)), candidates != 0u);
  let shown = select(nearest, top, covered != 0u);
  return vec4<f32>(d_min, f32(shown), f32(countOneBits(covered)), 0.0);
}

// The petals' shade (linear rgb, not premultiplied) and the signed distance
// (alpha), from this pixel's cover texel (bound at group 1, same resolution
// as this target). Beyond the main pass's reach only the distance matters.
@fragment
fn fs_iris_layer(in: VSOut) -> @location(0) vec4<f32> {
  let cover = textureLoad(cur_tex, vec2<i32>(in.pos.xy), 0);
  let d_min = cover.x;
  if (d_min > iris_reach(max(U.params3.x, 1.0))) {
    return vec4<f32>(0.0, 0.0, 0.0, d_min);
  }
  let n = max(i32(U.params0.x), 1);
  let contrast = clamp(U.params0.y, 0.0, 1.0);
  let shadow_amt = clamp(U.params0.z, 0.0, 1.0);
  let r_mid = U.params3.y;
  let w = U.params3.z;
  let r_in = 2.0 * w;
  let p = in.screen_uv * U.screen_size - 0.5 * U.screen_size;
  let top = i32(cover.y);
  let covered_count = i32(cover.z);
  var j1 = top + 1;
  if (j1 >= n) { j1 -= n; }
  var j2 = top + 2;
  if (j2 >= n) { j2 -= n; }
  // Directional sheen: the per-petal facing cosine (baked in petals_b[*].z,
  // -1..1) scaled by petal_sheen. petal_sheen 0 => flat; 1 => the
  // IRIS_SHEEN_MAX swing. As a region becomes fully enclosed near closure
  // the top-petal pick is a tie that falls back to petal 0, so fade the
  // variation to the uniform base color over the last two petals of
  // enclosure — by full overlap every petal reads identically and the pick
  // can't pop. A closed iris center is uniform anyway.
  let IRIS_SHEEN_MAX = 0.38;
  let enclosure = f32(covered_count) / f32(n);
  let var_scale = 1.0 - smoothstep(1.0 - 2.0 / f32(n), 1.0, enclosure);
  let tone = max(1.0 + contrast * IRIS_SHEEN_MAX * P.petals_b[top].z * var_scale, 0.0);
  // Soft shadow cast by the petals stacked above the top one.
  let shadow_w = max(0.012 * r_in, 4.0);
  let dn1 = max(iris_petal(j1, p, r_mid, w), 0.0);
  let dn2 = max(iris_petal(j2, p, r_mid, w), 0.0);
  let occ = 0.5 * (1.0 - smoothstep(0.0, shadow_w, dn1))
    + 0.22 * (1.0 - smoothstep(0.0, shadow_w * 0.6, dn2));
  let blade_rgb = clamp(U.params1.yzw, vec3<f32>(0.0), vec3<f32>(1.0));
  return vec4<f32>(blade_rgb * tone * (1.0 - shadow_amt * occ), d_min);
}

// Upscales the half-resolution transition intermediate (bound at group 1)
// to the swapchain with one bilinear tap (PHOTOFRAME_UPSCALE=bilinear), and
// writes each canvas mip level from the level above it at upload (2:1, where
// the bilinear tap is a box filter). Transitions render at reduced
// resolution because the 4K frame cost on the Pi is dominated by per-pixel
// fill; resting photos are still drawn at native resolution.
@fragment
fn fs_blit(in: VSOut) -> @location(0) vec4<f32> {
  return vec4<f32>(textureSample(cur_tex, cur_samp, in.screen_uv).rgb, 1.0);
}

// The level of a canvas that transitions read (bound at group 1: the level
// above it), 2:1, box filtered and then unsharp-masked:
// box + MIP_SHARPEN * (box - blur), where blur is the [1 2 1] / 4 binomial
// over the box-filtered level's own pixels, and the result clamped to the
// range of the taps it was built from. The bilinear upscale of the
// intermediate is a known low-pass; sharpening the level transitions read
// pre-compensates it at upload, at no cost per frame. The clamp keeps edges
// and thin lines at their own plateaus (an unclamped mask rings: 4 LSB at
// a = 0.25, 17 at a = 1 on a step), while texture, whose local range is
// wide, still sharpens.
//
// Both filters are read from the source level with bilinear taps: the box
// is one tap between the output's 2 x 2 source texels, and the blur of the
// box is, per axis, weights 1/4, 1/2, 1/4 at -2, 0, +2 source texels (six
// texels, paired), so the whole filter is 3 x 3 = 9 taps: 1 + 3/4 a at the
// center, -a/8 at the four edges, -a/16 at the four corners (a =
// MIP_SHARPEN; the weights sum to 1). Branch-free.
//
// a = 1 minimised the error of the 2x bilinear round trip against the
// native canvas on five photos (Mac GPU; see docs/performance.md); the
// viewer sets it from PHOTOFRAME_MIP_SHARPEN (0 = the plain box).
override MIP_SHARPEN: f32 = 1.0;

// The mask's running state: the weighted sum and the darkest and brightest
// tap so far. Each tap is folded in as it is read, so only these three
// stay live instead of all nine taps (as nine values held to the end the
// shader compiled at 2 threads on the Pi: 112 instructions, 2026-10-22).
struct MipSharpen {
  sum: vec3<f32>,
  lo: vec3<f32>,
  hi: vec3<f32>,
};

fn mip_fold(acc: MipSharpen, uv: vec2<f32>, w: f32) -> MipSharpen {
  let tap = textureSampleLevel(cur_tex, cur_samp, uv, 0.0).rgb;
  return MipSharpen(acc.sum + tap * w, min(acc.lo, tap), max(acc.hi, tap));
}

@fragment
fn fs_mip_sharpen(in: VSOut) -> @location(0) vec4<f32> {
  let texel = 1.0 / vec2<f32>(textureDimensions(cur_tex));
  // The output pixel's center in the source, and two source texels.
  let uv = in.screen_uv;
  let dx = vec2<f32>(2.0 * texel.x, 0.0);
  let dy = vec2<f32>(0.0, 2.0 * texel.y);
  let a = MIP_SHARPEN;
  let edge = -0.125 * a;
  let corner = -0.0625 * a;
  let c0 = textureSampleLevel(cur_tex, cur_samp, uv, 0.0).rgb;
  var acc = MipSharpen(c0 * (1.0 + 0.75 * a), c0, c0);
  acc = mip_fold(acc, uv - dx, edge);
  acc = mip_fold(acc, uv + dx, edge);
  acc = mip_fold(acc, uv - dy, edge);
  acc = mip_fold(acc, uv + dy, edge);
  acc = mip_fold(acc, uv - dx - dy, corner);
  acc = mip_fold(acc, uv + dx - dy, corner);
  acc = mip_fold(acc, uv - dx + dy, corner);
  acc = mip_fold(acc, uv + dx + dy, corner);
  // Never past the darkest or brightest of the taps it was built from:
  // edges and lines keep their plateaus (no halo), texture still sharpens.
  return vec4<f32>(clamp(acc.sum, acc.lo, acc.hi), 1.0);
}

// Catmull-Rom (bicubic) reconstruction of the intermediate, from bilinear
// taps of the linear-filtering, edge-clamped sampler. Per axis the cubic
// weighs the four texels around `uv` by w0 < 0, w1 > 0, w2 > 0, w3 < 0; the two
// inner texels share one tap placed between them in proportion to their
// weights (w1 + w2 at offset w2 / (w1 + w2)), the outer two get a tap each,
// so 3 x 3 = 9 taps reproduce the 16-texel cubic exactly. The four corner
// taps carry the products of two outer weights (each at most (2/27)^2, the
// four together at most 1/64, all positive), so leaving them out and
// renormalizing (5 taps) is the common close approximation. Branch-free:
// the taps are fixed.
//
// Not usable at 4K60 on the Pi 5: each tap of the 4K pass costs about
// 1.75 ms (V3D's sample rate, not memory), so the upscale takes 18.0 ms
// with 5 taps and 25.5 ms with 9 against 11.0 ms bilinear (Pi,
// 2026-10-22). Kept as opt-ins (PHOTOFRAME_UPSCALE).
struct CatmullRomTaps {
  t0: vec2<f32>,
  t12: vec2<f32>,
  t3: vec2<f32>,
  w0: vec2<f32>,
  w12: vec2<f32>,
  w3: vec2<f32>,
};

fn catmull_rom_taps(uv: vec2<f32>) -> CatmullRomTaps {
  let size = vec2<f32>(textureDimensions(cur_tex));
  let texel = 1.0 / size;
  let pos = uv * size;
  // The texel center at or before `pos`, and the position past it.
  let p1 = floor(pos - 0.5) + 0.5;
  let f = pos - p1;
  let w0 = f * (-0.5 + f * (1.0 - 0.5 * f));
  let w1 = 1.0 + f * f * (-2.5 + 1.5 * f);
  let w2 = f * (0.5 + f * (2.0 - 1.5 * f));
  let w3 = f * f * (-0.5 + 0.5 * f);
  let w12 = w1 + w2;
  return CatmullRomTaps(
    (p1 - 1.0) * texel,
    (p1 + w2 / w12) * texel,
    (p1 + 2.0) * texel,
    w0,
    w12,
    w3
  );
}

fn intermediate_tap(x: f32, y: f32) -> vec3<f32> {
  return textureSampleLevel(cur_tex, cur_samp, vec2<f32>(x, y), 0.0).rgb;
}

// The center and the four edge taps, and their total weight.
fn catmull_rom_cross(t: CatmullRomTaps) -> vec4<f32> {
  let c = intermediate_tap(t.t12.x, t.t12.y) * (t.w12.x * t.w12.y)
    + intermediate_tap(t.t0.x, t.t12.y) * (t.w0.x * t.w12.y)
    + intermediate_tap(t.t3.x, t.t12.y) * (t.w3.x * t.w12.y)
    + intermediate_tap(t.t12.x, t.t0.y) * (t.w12.x * t.w0.y)
    + intermediate_tap(t.t12.x, t.t3.y) * (t.w12.x * t.w3.y);
  let w = t.w12.x * t.w12.y
    + t.w0.x * t.w12.y
    + t.w3.x * t.w12.y
    + t.w12.x * t.w0.y
    + t.w12.x * t.w3.y;
  return vec4<f32>(c, w);
}

// PHOTOFRAME_UPSCALE=bicubic: 5 taps, renormalized.
@fragment
fn fs_upscale_bicubic(in: VSOut) -> @location(0) vec4<f32> {
  let taps = catmull_rom_cross(catmull_rom_taps(in.screen_uv));
  return vec4<f32>(taps.rgb / taps.a, 1.0);
}

// PHOTOFRAME_UPSCALE=bicubic-9: all 9 taps, the exact cubic (the weights
// sum to 1).
@fragment
fn fs_upscale_bicubic9(in: VSOut) -> @location(0) vec4<f32> {
  let t = catmull_rom_taps(in.screen_uv);
  let edge = catmull_rom_cross(t);
  let corners = intermediate_tap(t.t0.x, t.t0.y) * (t.w0.x * t.w0.y)
    + intermediate_tap(t.t3.x, t.t0.y) * (t.w3.x * t.w0.y)
    + intermediate_tap(t.t0.x, t.t3.y) * (t.w0.x * t.w3.y)
    + intermediate_tap(t.t3.x, t.t3.y) * (t.w3.x * t.w3.y);
  return vec4<f32>(edge.rgb + corners, 1.0);
}

// The common tail of every photo pass: composite over the background here
// (previously done by alpha blending against the clear color): same result,
// no per-pixel destination read, and the output stays fully opaque for
// direct scanout. Coverage is the union of both photos and the result.
fn over_background(color: vec4<f32>, current: vec4<f32>, next: vec4<f32>) -> vec4<f32> {
  let a = clamp(max(max(current.a, next.a), color.a), 0.0, 1.0);
  return vec4<f32>(color.rgb * a + U.background.rgb * (1.0 - a), 1.0);
}

// over_background for a result whose coverage `a` is known: the union of
// the photos' (and a layer's) coverage. For a mix of the two photos the
// mixed coverage never exceeds the larger of the two, so the union is just
// max(current.a, next.a), and the mix's alpha need not be computed.
fn over_background_rgb(rgb: vec3<f32>, a: f32) -> vec4<f32> {
  return vec4<f32>(rgb * a + U.background.rgb * (1.0 - a), 1.0);
}

// For results that cover the whole screen by construction (fade through
// black, e-ink).
fn opaque(color: vec4<f32>) -> vec4<f32> {
  return vec4<f32>(color.rgb, 1.0);
}

fn screen_pos_of(in: VSOut) -> vec2<f32> {
  return in.screen_uv * U.screen_size;
}

fn current_of(in: VSOut) -> vec4<f32> {
  return current_plane(in.current_uv);
}

fn next_of(in: VSOut) -> vec4<f32> {
  return next_plane(in.next_uv);
}

fn progress01() -> f32 {
  return clamp(U.progress, 0.0, 1.0);
}

// A resting photo (no transition): only the current plane. The next photo
// is staged only while a transition runs; if one is ever staged at rest,
// the CPU draws with fs_fade at progress 0 instead (the same result with
// the next plane's coverage). This is the shader drawn at native 4K on every
// dwell and at the end of every transition, so it is kept minimal.
@fragment
fn fs_rest(in: VSOut) -> @location(0) vec4<f32> {
  let current = current_of(in);
  return over_background(current, current, current);
}

// params0.x > 0.5: through black.
@fragment
fn fs_fade(in: VSOut) -> @location(0) vec4<f32> {
  let current = current_of(in);
  let next = next_of(in);
  let progress = progress01();
  if (U.params0.x > 0.5) {
    let half = progress * 2.0;
    let fade_out = clamp(1.0 - half, 0.0, 1.0);
    let fade_in = clamp(half - 1.0, 0.0, 1.0);
    let black_weight = clamp(1.0 - fade_out - fade_in, 0.0, 1.0);
    let color = current * fade_out + next * fade_in + vec4<f32>(0.0, 0.0, 0.0, black_weight);
    return opaque(color);
  }
  return over_background(mix(current, next, progress), current, next);
}

// The wipe front as a function of screen_uv: normalized position along the
// wipe = clamp(dot(params0.xy, screen_uv) + params0.z), and the mask edges
// params1.xy (draw_params::wipe).
@fragment
fn fs_wipe(in: VSOut) -> @location(0) vec4<f32> {
  let normalized = clamp(dot(U.params0.xy, in.screen_uv) + U.params0.z, 0.0, 1.0);
  let mask = 1.0 - smoothstep(U.params1.x, U.params1.y, normalized);
  let current = current_of(in);
  let next = next_of(in);
  return over_background(current * (1.0 - mask) + next * mask, current, next);
}

// The planes are shifted on the CPU (draw_params::push).
@fragment
fn fs_push(in: VSOut) -> @location(0) vec4<f32> {
  let current = current_of(in);
  let next = next_of(in);
  let mask = step(0.5, next.a);
  return over_background(current * (1.0 - mask) + next * mask, current, next);
}

// params0 = (flash_count, reveal_portion, stripe_count, seed.x),
// params1 = (seed.y, flash_rgb).
//
// The reveal is decided per cell of a stripe_count x stripe_count grid: a
// cell's gate depends only on its row (the stagger), a hash of the cell
// and the progress. fs_eink_mask computes each cell's mask once per frame
// into a stripes x stripes texture (bound at group 3); fs_eink reads its
// cell's texel.
@fragment
fn fs_eink_mask(in: VSOut) -> @location(0) vec4<f32> {
  let progress = progress01();
  let reveal_start = clamp(U.params0.y, 0.05, 0.95);
  let stripes = max(U.params0.z, 1.0);
  let seed = vec2<f32>(U.params0.w, U.params1.x);
  // This texel's cell: (column, stripe).
  let cell = floor(in.pos.xy);
  let reveal_ratio = (progress - reveal_start) / max(1.0 - reveal_start, 1e-3);
  let stripe_idx = cell.y;
  let stripe_phase = stripe_idx / stripes;
  let noise_vec = vec2<f32>(stripe_idx, cell.x) + seed;
  let noise = fract(sin(dot(noise_vec, vec2<f32>(12.9898, 78.233))) * 43758.5453);
  // Stagger-normalized so the LAST stripe's gate reaches 1.0 by
  // reveal_ratio ~0.9: the old form (1.15r - 0.85*phase) left bottom
  // stripes only ~30-60% revealed at the end of the transition, which
  // then popped to the clean photo on the first dwell frame.
  let stagger = 0.45;
  let gate = clamp(
    (reveal_ratio - stripe_phase * stagger) / (1.0 - stagger) + noise * 0.15,
    0.0,
    1.0
  );
  let mask = smoothstep(0.05, 0.85, gate);
  // A half float rounds the mask to about a 2048th, a step or two of the
  // result: store that value and, in the second channel, what it rounded
  // off, so their sum restores the mask to near f32 precision.
  let hi = unpack2x16float(pack2x16float(vec2<f32>(mask, 0.0))).x;
  return vec4<f32>(hi, mask - hi, 0.0, 0.0);
}

@fragment
fn fs_eink(in: VSOut) -> @location(0) vec4<f32> {
  let progress = progress01();
  let flashes = max(U.params0.x, 0.0);
  let reveal_start = clamp(U.params0.y, 0.05, 0.95);
  let stripes = max(U.params0.z, 1.0);
  let flash_rgb = clamp(U.params1.yzw, vec3<f32>(0.0), vec3<f32>(1.0));
  if (progress < reveal_start) {
    // Flashes: the same for every pixel but the current photo.
    let prep_ratio = progress / max(reveal_start, 1e-3);
    let segments = flashes * 2.0 + 1.0;
    let staged = clamp(floor(prep_ratio * segments), 0.0, segments);
    let stage_u = u32(staged);
    let stage_pos = fract(prep_ratio * segments);
    let flash_mix = clamp(smoothstep(0.15, 0.85, stage_pos), 0.0, 1.0);
    let current = current_of(in);
    var flash_color = current;
    if (stage_u > 0u) {
      let toggle = (stage_u & 1u) == 1u;
      if (toggle) {
        flash_color = vec4<f32>(0.0, 0.0, 0.0, 1.0);
      } else {
        flash_color = vec4<f32>(flash_rgb, 1.0);
      }
    }
    return opaque(mix(current, flash_color, flash_mix));
  }
  // Reveal: this pixel's cell mask (fs_eink_mask).
  let cell = vec2<i32>(floor(in.screen_uv * stripes));
  let stored = textureLoad(petal_tex, cell, 0);
  let mask = stored.r + stored.g;
  let ghost = mix(current_of(in).rgb, flash_rgb, 0.55);
  let next = next_of(in).rgb;
  return vec4<f32>(mix(ghost, next, mask), 1.0);
}

// Threshold value-noise by progress. params0 = (softness, cell scale).
@fragment
fn fs_dissolve(in: VSOut) -> @location(0) vec4<f32> {
  let current = current_of(in);
  let next = next_of(in);
  let progress = progress01();
  let softness = clamp(U.params0.x, 0.0, 0.5);
  let scale = max(U.params0.y, 1.0);
  let cell = floor(screen_pos_of(in) / scale);
  let n = fract(sin(dot(cell, vec2<f32>(12.9898, 78.233))) * 43758.5453);
  let lo = clamp(progress - softness, 0.0, 1.0);
  let hi = clamp(progress + softness, 0.0, 1.0);
  let mask = smoothstep(lo, max(hi, lo + 1e-3), n);
  return over_background(mix(next, current, mask), current, next);
}

// Circle or diamond reveal from a center, as a function of screen_uv:
// the offset from the center in units of the screen diagonal is
// screen_uv * params0.xy - params0.zw; params1 = (mask edges, diamond)
// (draw_params::radial_wipe).
@fragment
fn fs_radial_wipe(in: VSOut) -> @location(0) vec4<f32> {
  let dv = in.screen_uv * U.params0.xy - U.params0.zw;
  let dist = select(length(dv), abs(dv.x) + abs(dv.y), U.params1.z > 0.5);
  let mask = 1.0 - smoothstep(U.params1.x, U.params1.y, dist);
  let current = current_of(in);
  let next = next_of(in);
  return over_background_rgb(mix(current.rgb, next.rgb, mask), max(current.a, next.a));
}

// Stripe reveal: stripes along dot(screen_uv, params1.xy), mask edges
// params0.yz (draw_params::venetian_blinds).
@fragment
fn fs_venetian_blinds(in: VSOut) -> @location(0) vec4<f32> {
  let local = fract(dot(in.screen_uv, U.params1.xy) * U.params0.x);
  let mask = smoothstep(U.params0.y, U.params0.z, 1.0 - local);
  let t = 1.0 - mask;
  let current = current_of(in);
  let next = next_of(in);
  return over_background(mix(current, next, t), current, next);
}

// Fade + a shared, gentle Ken-Burns scale. Both planes share ONE scale that
// is 0 at the ends and bumps up in the middle (1 + zoom*sin(pi*progress)).
// Locking both layers to the same scale avoids the double-image "swim" of
// opposing zoom directions, is pop-free at both ends, and keeps scale >= 1
// so edges never go empty. The zoomed planes' texture maps are params1 /
// params3 (draw_params::crossfade_zoom); coverage uses the unzoomed planes
// (over_background reads only their alpha, so they are not fetched).
@fragment
fn fs_crossfade_zoom(in: VSOut) -> @location(0) vec4<f32> {
  let current = vec4<f32>(0.0, 0.0, 0.0, covered(in.current_uv));
  let next = vec4<f32>(0.0, 0.0, 0.0, covered(in.next_uv));
  let c = current_plane(in.screen_uv * U.params1.xy + U.params1.zw);
  let nxt = next_plane(in.screen_uv * U.params3.xy + U.params3.zw);
  return over_background(mix(c, nxt, progress01()), current, next);
}

// Mechanical camera-iris diaphragm. The petals' shade and signed distance
// were rendered into the reduced-resolution petal layer by fs_iris_layer
// this frame; here the outline is drawn from the interpolated distance with
// an anti-aliasing band of this pass's pixel size (params3.w, screen px),
// the edge highlight added inside it, and the petals composited over the
// crossfading photos. params0.w = photo_swap_mix.
@fragment
fn fs_iris(in: VSOut) -> @location(0) vec4<f32> {
  let layer = textureSample(petal_tex, petal_samp, in.screen_uv);
  let d = layer.a;
  let band = U.params3.w;
  let cov = 1.0 - smoothstep(-band, band, d);
  // Thin edge highlight, under petal_sheen so 0 is perfectly flat.
  let rim = smoothstep(3.5 * IRIS_RIM_PX, 0.5 * IRIS_RIM_PX, abs(d))
    * clamp(U.params0.y, 0.0, 1.0) * 0.2;
  let cur_a = covered(in.current_uv);
  let next_a = covered(in.next_uv);
  let swap = clamp(U.params0.w, 0.0, 1.0);
  let photo = mix(fetch_current(in.current_uv) * cur_a, fetch_next(in.next_uv) * next_a, swap);
  let rgb = photo * (1.0 - cov) + (layer.rgb + vec3<f32>(rim)) * cov;
  return over_background_rgb(rgb, max(max(cur_a, next_a), cov));
}

// Debug: stroke a single quadratic Bezier over the current image.
// params0.xy = P0 (uv), params0.zw = P1 (uv), params1.xy = P2 (uv),
// params1.z = stroke width (px), params3 = stroke rgba.
@fragment
fn fs_debug_bezier(in: VSOut) -> @location(0) vec4<f32> {
  let pos = screen_pos_of(in);
  let current = current_of(in);
  let next = next_of(in);
  // Control points in pixel space
  let P0 = U.screen_size * clamp(U.params0.xy, vec2<f32>(0.0), vec2<f32>(1.0));
  let P1 = U.screen_size * clamp(U.params0.zw, vec2<f32>(0.0), vec2<f32>(1.0));
  let P2 = U.screen_size * clamp(U.params1.xy, vec2<f32>(0.0), vec2<f32>(1.0));
  let stroke_px = max(U.params1.z, 0.0);
  let stroke = vec4<f32>(
    clamp(U.params3.xyz, vec3<f32>(0.0), vec3<f32>(1.0)),
    clamp(U.params3.w, 0.0, 1.0)
  );
  // Distance from this pixel to the Bezier, approximated by a polyline
  let N: i32 = 32; // segments
  var min_d = 1e9;
  var a = P0;
  for (var i: i32 = 1; i <= N; i = i + 1) {
    let t = f32(i) / f32(N);
    // Quadratic Bezier point via de Casteljau
    let q0 = mix(P0, P1, t);
    let q1 = mix(P1, P2, t);
    let b = mix(q0, q1, t);
    // Distance to segment a-b
    let ab = b - a;
    let ap = pos - a;
    let h = clamp(dot(ap, ab) / max(dot(ab, ab), 1e-6), 0.0, 1.0);
    let d = length(ap - ab * h);
    min_d = min(min_d, d);
    a = b;
  }
  let half_w = max(0.5 * stroke_px, 0.0);
  let aa = max(fwidth(min_d), 1.0);
  let edge = 1.0 - smoothstep(half_w, half_w + aa, min_d);
  let alpha = edge * stroke.a;
  return over_background(mix(current, vec4<f32>(stroke.rgb, 1.0), alpha), current, next);
}
