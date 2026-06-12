# Performance

How the viewer reaches smooth 4K60 transitions on a Raspberry Pi 5, how to
measure it, and what to check when frame rates regress. Everything here was
learned the hard way during a week of profiling in June 2026; the punchline
is that **none of the four root causes were in the rendering code**.

## Measuring

Two built-in tools, no extra software required:

**Per-transition stats** are logged automatically at info level when any
transition finishes:

```
journalctl -t photoframe -f | grep transition_frame_stats
# transition=iris frames=73 avg_fps=28.1 avg_frame_ms=35.5 best_frame_ms=2.3 worst_frame_ms=77.8
```

Use `-t photoframe` (syslog identifier), not `-u` — the app runs inside the
greetd session, not as its own systemd unit.

The stats measure *submission* cadence (when `present()` returns), which
matches what the panel displays only because transition redraws are paced
to the refresh interval. If `best_frame_ms` ever drops well below a vsync
(e.g. 0.4ms), frames are being submitted back-to-back and mailbox is
discarding some unseen — the displayed motion is then worse than the
numbers suggest. Trust your eyes (or a phone slow-mo recording of the
panel) over the log when they disagree.

**`frametest`** is a minimal fullscreen probe staged to
`/opt/photoframe/bin/frametest`. It renders selectable workloads — `solid`
(a clear, zero memory traffic), `tex` (one full-screen texture), `fade`
(two textures mixed, the cheapest real transition) — under a selectable
present mode (`fifo|mailbox|immediate`) and frame latency, printing cadence
once per second. Run it inside the kiosk session:

```bash
sudo -u kiosk env XDG_RUNTIME_DIR=/run/user/$(id -u kiosk) WAYLAND_DISPLAY=wayland-1 \
  /opt/photoframe/bin/frametest solid fifo
```

The ladder `solid fifo` → `solid mailbox` → `fade mailbox` separates pacing
problems (solid can't hit refresh rate) from throughput problems (solid
can, fade can't). When in doubt, start here — it takes five minutes and
rules out the entire application.

## The four root causes (June 2026)

1. **Bootloader EEPROM memory regression.** Pi 5 bootloaders v2025.01.22
   through v2025.10.x shipped a memory configuration (fake NUMA +
   `SDRAM_BANKLOW`) that cut GPU memory bandwidth by roughly a third. The
   bootloader lives on the board, not the SD card — a fresh OS install does
   not fix it. Setup now stages updates (`setup/system/modules/35-firmware.sh`)
   and `verify.sh` warns when one is pending. Manual check:
   `sudo rpi-eeprom-update`.

2. **Missing GL drivers → CPU compositing.** The kiosk installs packages
   with `--no-install-recommends`; without `libgl1-mesa-dri`, sway cannot
   create a GPU renderer and silently falls back to pixman, compositing
   every 4K frame on the CPU (~30+ ms each). Symptoms: all transitions cost
   the same regardless of shader weight; `sway -d` logs show no GLES2
   renderer. The package is now explicit in the apt list and checked by
   `verify.sh`.

3. **FIFO presentation costs two vsyncs per frame.** Vulkan FIFO emulated
   over Wayland frame callbacks serializes against the compositor on stacks
   without the `wp_fifo_v1` protocol (sway 1.10 / wlroots 0.18): `frametest
   solid fifo` locks at a metronomic 33.3 ms on a 60 Hz output. The viewer
   therefore defaults to **mailbox** presentation, which latches the newest
   complete frame each vsync — measured 62.9 fps average on a 4K radial
   wipe. The render loop stays frame-callback paced, so mailbox overdraws
   only ~5% during transitions and nothing during dwell.

   *Worth retrying with newer sway:* wlroots 0.19+ speaks `wp_fifo_v1`, so
   under sway 1.11+ proper FIFO should pace at full rate. The check is one
   command (`frametest solid fifo` ≈ 60 fps means FIFO is healthy); the
   switch is `PHOTOFRAME_PRESENT_MODE=fifo` in the launcher. There is no
   urgency — mailbox remains correct on both broken and fixed stacks.

4. **Photo uploads hitching mid-transition.** Preparing the next photo ends
   with a multi-megapixel `write_texture`; when it landed during an
   animation it showed up as 50–140 ms worst frames. Non-priority uploads
   are now deferred until the transition completes (dwell leaves ample
   idle time).

## Render-cost design

The Pi 5's budget at 4K60 is roughly 30 GPU-ops per pixel per frame, so
full-screen passes are spent carefully:

- **Resting photos render at native resolution.** Sharpness when the image
  is still is the product; nothing below applies to dwell frames.
- **Transition frames render at 1/`TRANSITION_HALF_SCALE` (default 2)**
  into an offscreen intermediate and are upsampled — quarter the fill cost,
  hidden by motion. The final ~1% of each transition renders native so the
  incoming photo settles in sharp. Override per deployment with
  `PHOTOFRAME_TRANSITION_SCALE` (1 bypasses the intermediate entirely).
- **Iris petals render at 1/`IRIS_LAYER_SCALE` (default 4)** in their own
  premultiplied layer (`PHOTOFRAME_IRIS_LAYER_SCALE` to override). The
  petal SDF math runs on CPU-precomputed per-frame constants and avoids
  dynamically indexed local arrays, which the V3D compiler demotes to slow
  per-pixel scratch memory.
- **The main pass renders opaquely** (no blending) and composites letterbox
  regions over the background color in-shader, saving a destination read
  per pixel and keeping the surface eligible for direct scanout.

## Known limitation: direct scanout

sway attempts to scan out the fullscreen buffer every frame but the import
fails: v3dv allocates UIF-tiled swapchain buffers (modifier
`0x0700000000000006`), which the display controller cannot scan out, and
`MESA_VK_WSI_DEBUG=linear` does not change the negotiated modifier (tested
on Mesa 25.0). sway therefore composites — on the GPU, costing a few ms per
frame. Parked: not worth chasing while transitions hold 60 fps. Revisit
alongside a sway/wlroots/Mesa upgrade (`journalctl -t photoframe` with
`sway -d` shows per-frame scan-out decisions).

## Environment knobs

Set in `/usr/local/bin/photoframe` (the launcher); all optional:

| Variable | Values | Default |
| --- | --- | --- |
| `PHOTOFRAME_PRESENT_MODE` | `fifo` / `mailbox` / `immediate` | mailbox when available |
| `PHOTOFRAME_TRANSITION_SCALE` | 1–4 | 2 |
| `PHOTOFRAME_IRIS_LAYER_SCALE` | 1–8 | 4 |
| `WGPU_BACKEND` | `vulkan` / `gl` | vulkan |
