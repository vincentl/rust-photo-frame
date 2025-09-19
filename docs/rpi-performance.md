# Raspberry Pi Blur Mat Performance Notes

## Platform capabilities

- The viewer spawns one CPU matting worker per logical core using `std::thread::available_parallelism()`, so a quad-core Pi 4 runs four concurrent matting jobs.
- Gaussian blur for the matting background currently runs on the CPU via `image::imageops::blur`, which does not use NEON/GPU acceleration on Raspberry Pi.
- WGPU is already used for final composition, but the blur background is prepared off-screen on the CPU before upload.

## Proposed optimization for Pi + 4K displays

- Large 4K canvases require ~8.3M pixels per blur, which is slow on Cortex-A72 cores.
- Because the background is heavily blurred, we can downsample before blurring without reducing visual quality.
- A new `max-sample-dim` option on the `blur` matting mode downsamples to the specified maximum dimension, scales the blur radius to compensate, and re-upscales to the canvas size.
- Builds targeting 64-bit ARM (such as Raspberry Pi OS 64-bit) default this limit to 2048px to reduce CPU cost automatically. Other platforms remain unlimited unless configured.

## Recommended configuration

```yaml
matting:
  type: blur
  sigma: 20.0
  max-sample-dim: 1536
```

- `max-sample-dim: 1536` keeps the blur staging surface near 1080p, roughly quartering the pixel count compared to 4K.
- Adjust upward if artifacts become noticeable on very large TVs; the default 2048 is a good balance for Pi 5.

## Next steps

1. Deploy builds with the new default to Raspberry Pi hardware and measure blur preparation times versus the main branch.
2. Experiment with lower `sigma` values (e.g., 16) if further savings are needed.
3. Explore moving the blur into a WGPU compute shader for future versions to leverage the VideoCore GPU when available.
