# Raspberry Pi Blur Mat Performance Notes

## Platform capabilities

- The viewer spawns one CPU matting worker per logical core using `std::thread::available_parallelism()`, so a quad-core Pi 4 runs four concurrent matting jobs.
- Gaussian blur for the matting background now supports three execution paths:
  - A portable CPU renderer (the previous default).
  - A NEON-accelerated branch that uses SIMD intrinsics when running on 64-bit ARM cores.
  - A WGPU compute shader that runs the blur on the GPU when a compatible adapter is available.
- The viewer still composites the final frame with WGPU; the new compute shader path keeps the background blur on the GPU.

## Proposed optimization for Pi + 4K displays

- Large 4K canvases require ~8.3M pixels per blur, which is slow on Cortex-A72 cores.
- Because the background is heavily blurred, we can downsample before blurring without reducing visual quality.
- A new `max-sample-dim` option on the `blur` matting mode downsamples to the specified maximum dimension, scales the blur radius to compensate, and re-upscales to the canvas size.
- The `backend` selector lets you choose between the CPU, NEON, or WGPU compute branches so you can measure each one independently.
- Builds targeting 64-bit ARM (such as Raspberry Pi OS 64-bit) default this limit to 2048px to reduce CPU cost automatically. Other platforms remain unlimited unless configured.

## Recommended configuration

```yaml
matting:
  type: blur
  sigma: 20.0
  max-sample-dim: 1536
  backend: auto
```

- `max-sample-dim: 1536` keeps the blur staging surface near 1080p, roughly quartering the pixel count compared to 4K.
- Adjust upward if artifacts become noticeable on very large TVs; the default 2048 is a good balance for Pi 5.
- Leave `backend` at `auto` to let the viewer pick NEON on Pi 4/5 hardware. Switch to `wgpu-compute` to benchmark the GPU path or `cpu` to compare against the original implementation.

## Next steps

1. Deploy builds with the new defaults to Raspberry Pi hardware and measure blur preparation times for each backend (`auto`, `wgpu-compute`, and `cpu`).
2. Experiment with lower `sigma` values (e.g., 16) if further savings are needed.
3. Validate WGPU compute performance on Pi 5/400 systems with active cooling to ensure thermal headroom remains acceptable.
