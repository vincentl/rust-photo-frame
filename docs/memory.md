# Memory profile and tuning

This document describes how photo-frame uses RAM at runtime and what
tunables help keep the footprint under control on constrained systems such as
the Raspberry Pi. The goal is to explain why the recent matting work can push a
4 GiB device over the limit and outline mitigation levers.

## Pipeline overview

The slideshow stages create multiple temporary copies of each image. When these
copies stack up across the configured preload window, overall memory pressure
can spike.

1. **Loader decode buffer.** The loader converts source photos into raw RGBA8
   byte arrays (`PreparedImageCpu`). The `tokio` channel between the manager,
   loader, and viewer is sized by `viewer-preload-count`, so the system keeps
   that many decoded frames alive even before any matting work begins.【F:crates/photo-frame/src/main.rs†L118-L131】【F:crates/photo-frame/src/events.rs†L13-L32】【F:crates/photo-frame/src/tasks/loader.rs†L51-L111】
2. **Matting worker input.** Each queued frame is cloned into a `MatTask` so the
   CPU matting pipeline can resize and decorate it before display. This retains
   another RGBA copy while the worker thread is active.【F:crates/photo-frame/src/tasks/viewer.rs†L231-L334】
3. **Matting output canvas.** The worker renders a full-screen canvas (`ImagePlane`)
   per frame. The canvas dimensions track the display resolution and the
   `oversample` setting, so 4K panels or aggressive oversampling can easily
   generate tens of megabytes per image.【F:crates/photo-frame/src/tasks/viewer.rs†L296-L389】
4. **GPU upload staging.** When the viewer uploads the matted canvas to the
   GPU, it may allocate an additional padded staging buffer to satisfy WGPU row
   alignment requirements. This allocation lives until the upload completes on
   the GPU queue.【F:crates/photo-frame/src/tasks/viewer.rs†L600-L677】
5. **Fixed-image backgrounds.** When the `fixed-image` matting mode is enabled,
   each configured background is decoded once and cached indefinitely at the
   canvas resolution. Large background images or long background lists can
   therefore multiply steady-state memory usage.【F:crates/photo-frame/src/processing/fixed_image.rs†L13-L123】

In the current configuration it is common to have three frames in flight. On a
3840×2160 display with `global-photo-settings.oversample: 1.0`, a single RGBA image consumes roughly
33 MiB. With the copies above, a steady-state queue can therefore exceed
400 MiB—before accounting for GPU allocations, font caches, and the rest of the
system. That pressure encourages the kernel OOM killer to target unrelated
processes such as `wireplumber`, which carries a high `oom_score_adj`.

## Mitigation levers

* **Reduce `viewer-preload-count`.** Lowering the value trims the number of
  concurrent decoded frames across the loader, matting queue, and GPU upload.
  Values between 1 and 2 still hide most I/O hiccups on fast storage.
* **Dial back `oversample`.** Keeping it near 1.0 dramatically reduces the size
  of every matting canvas and GPU texture. Higher values should only be used
  when the GPU and RAM budget clearly allow it.
* **Cull large backgrounds.** When using the `fixed-image` matting mode, prefer
  a short list of modestly sized assets (ideally already scaled near the screen
  resolution) so the cached canvases do not balloon.
* **Limit matting styles.** Disabling the matting stage (`matting.types: []`) or
  sticking to lighter-weight options such as `fixed-color` cuts the CPU
  intermediate allocations completely.
* **Constrain source resolution.** Keeping the photo library near the panel
  resolution avoids oversized decode buffers in the loader.

Monitoring the resident set size of the `photo-frame` process while toggling
these settings helps confirm the impact before deploying broadly.
