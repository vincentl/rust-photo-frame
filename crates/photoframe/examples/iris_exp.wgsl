// Main-pass variants for the iris layer format (experiment; appended to
// viewer_quad.wgsl by examples/iris_bench.rs with --main-entry).

// Rgba8UnormSrgb layer: rgb = the petal shade (sRGB-encoded, so the dark
// range keeps its precision), a = the signed distance mapped from
// [-IRIS_D8_RANGE, +IRIS_D8_RANGE] screen px to 0..1 (1/255 of 64 px =
// 0.25 px per step; beyond the range only "far" matters).

@fragment
fn fs_iris_srgb8(in: VSOut) -> @location(0) vec4<f32> {
  let layer = textureSample(petal_tex, petal_samp, in.screen_uv);
  let d = layer.a * 64.0 - 32.0;
  let band = U.params3.w;
  let cov = 1.0 - smoothstep(-band, band, d);
  let rim = smoothstep(3.5 * IRIS_RIM_PX, 0.5 * IRIS_RIM_PX, abs(d))
    * clamp(U.params0.y, 0.0, 1.0) * 0.2;
  let cur_a = covered(in.current_uv);
  let next_a = covered(in.next_uv);
  let swap = clamp(U.params0.w, 0.0, 1.0);
  let photo = mix(fetch_current(in.current_uv) * cur_a, fetch_next(in.next_uv) * next_a, swap);
  let rgb = photo * (1.0 - cov) + (layer.rgb + vec3<f32>(rim)) * cov;
  return over_background_rgb(rgb, max(max(cur_a, next_a), cov));
}

// The two-channel layer read without the color multiply (cost probe only:
// the petals come out grey).
@fragment
fn fs_iris_rg_nomul(in: VSOut) -> @location(0) vec4<f32> {
  let layer = textureSample(petal_tex, petal_samp, in.screen_uv);
  let d = layer.g;
  let band = U.params3.w;
  let cov = 1.0 - smoothstep(-band, band, d);
  let rim = smoothstep(3.5 * IRIS_RIM_PX, 0.5 * IRIS_RIM_PX, abs(d))
    * clamp(U.params0.y, 0.0, 1.0) * 0.2;
  let cur_a = covered(in.current_uv);
  let next_a = covered(in.next_uv);
  let swap = clamp(U.params0.w, 0.0, 1.0);
  let photo = mix(fetch_current(in.current_uv) * cur_a, fetch_next(in.next_uv) * next_a, swap);
  let rgb = photo * (1.0 - cov) + (vec3<f32>(layer.r) + vec3<f32>(rim)) * cov;
  return over_background_rgb(rgb, max(max(cur_a, next_a), cov));
}

// The old main pass verbatim (rgb + a), for timing against the same layer.
@fragment
fn fs_iris_rgba(in: VSOut) -> @location(0) vec4<f32> {
  let layer = textureSample(petal_tex, petal_samp, in.screen_uv);
  let d = layer.a;
  let band = U.params3.w;
  let cov = 1.0 - smoothstep(-band, band, d);
  let rim = smoothstep(3.5 * IRIS_RIM_PX, 0.5 * IRIS_RIM_PX, abs(d))
    * clamp(U.params0.y, 0.0, 1.0) * 0.2;
  let cur_a = covered(in.current_uv);
  let next_a = covered(in.next_uv);
  let swap = clamp(U.params0.w, 0.0, 1.0);
  let photo = mix(fetch_current(in.current_uv) * cur_a, fetch_next(in.next_uv) * next_a, swap);
  let rgb = photo * (1.0 - cov) + (layer.rgb + vec3<f32>(rim)) * cov;
  return over_background_rgb(rgb, max(max(cur_a, next_a), cov));
}

// Two-channel layer, color applied as one scalar weight per term.
@fragment
fn fs_iris_rg_f2(in: VSOut) -> @location(0) vec4<f32> {
  let layer = textureSample(petal_tex, petal_samp, in.screen_uv);
  let d = layer.g;
  let band = U.params3.w;
  let cov = 1.0 - smoothstep(-band, band, d);
  let rim = smoothstep(3.5 * IRIS_RIM_PX, 0.5 * IRIS_RIM_PX, abs(d))
    * clamp(U.params0.y, 0.0, 1.0) * 0.2;
  let cur_a = covered(in.current_uv);
  let next_a = covered(in.next_uv);
  let swap = clamp(U.params0.w, 0.0, 1.0);
  let photo = mix(fetch_current(in.current_uv) * cur_a, fetch_next(in.next_uv) * next_a, swap);
  let rgb = photo * (1.0 - cov) + U.params1.yzw * (layer.r * cov) + vec3<f32>(rim * cov);
  return over_background_rgb(rgb, max(max(cur_a, next_a), cov));
}

// Two-channel layer, color from the background slot of a second read of U
// avoided: the color is the uniform's own vec4 multiplied as a vec4.
@fragment
fn fs_iris_rg_f3(in: VSOut) -> @location(0) vec4<f32> {
  let layer = textureSample(petal_tex, petal_samp, in.screen_uv);
  let d = layer.g;
  let band = U.params3.w;
  let cov = 1.0 - smoothstep(-band, band, d);
  let rim = smoothstep(3.5 * IRIS_RIM_PX, 0.5 * IRIS_RIM_PX, abs(d))
    * clamp(U.params0.y, 0.0, 1.0) * 0.2;
  let cur_a = covered(in.current_uv);
  let next_a = covered(in.next_uv);
  let swap = clamp(U.params0.w, 0.0, 1.0);
  let photo = mix(fetch_current(in.current_uv) * cur_a, fetch_next(in.next_uv) * next_a, swap);
  let petal = fma(vec3<f32>(layer.r), U.params1.yzw, vec3<f32>(rim));
  let rgb = mix(photo, petal, cov);
  return over_background_rgb(rgb, max(max(cur_a, next_a), cov));
}
