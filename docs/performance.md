# Performance

How the viewer reaches smooth 4K60 transitions on a Raspberry Pi 5, how to
measure it, and what to check when frame rates regress. Everything here was
learned the hard way during a week of profiling in June 2026; the punchline
is that **not one of the root causes was a slow shader** — they were
firmware, a missing driver package, the presentation mode, and CPU/GPU
contention. Measure the whole pipeline before optimizing any pass.

## Measuring

Two built-in tools, no extra software required:

**Per-transition stats** are logged at info level when any transition
finishes, but only when `metrics: true` is set in `config.yaml` (off by
default to keep the journal quiet):

```
journalctl -t photoframe -f | grep transition_frame_metric
# transition=iris frames=73 avg_fps=28.1 avg_frame_ms=35.5 best_frame_ms=2.3 worst_frame_ms=77.8
```

The same `metrics: true` flag also emits a `photo_display_metric` line each
time a photo reaches the screen, for auditing the scheduler's randomness:

```
journalctl -t photoframe -f | grep photo_display_metric
# seq=412 inventory=1873 distinct=389 shown_count=1 gap=-1 weight=1.00 age_days=4.2 created_source=birthtime path=/var/lib/photoframe/photos/cloud/2021/img_0420.jpg
```

With [portrait pairing](configure.md#portrait-pairing) enabled there is still one
line per photo shown, also when two photos share a screen, and each line adds
screen fields before `path`:

```
# … created_source=birthtime screen=301 pair=first partner="/var/lib/photoframe/photos/cloud/2021/img_0421.jpg" pairable=true sent=305 removed=0 added=0 path=…
```

`screen` counts screens, `pair` is `solo`, `first` or `second` (left or right of
a pair), `partner` names the other photo of a pair, and `pairable` says whether
the photo could pair on the current canvas. `sent` is the index of the screen as
the manager sent it to the loader (a jump of more than one between consecutive
screens means a screen was planned but never shown: a failed decode, a priority
purge, a sleep), and `removed` / `added` count library changes since the
previous screen. The analyzer uses these to attribute a photo shown on two
consecutive screens to its cause. With pairing off the line is unchanged.

`gap` is the number of displays since that photo was last shown (`-1` the
first time it appears); under uniform random selection it averages the
inventory size. `age_days` is the photo's age — `now − created_at` — which
drives the weight, and `created_source` records where `created_at` came from:
`birthtime` (the intended filesystem staging time), `mtime`, or `now` (a
fallback when birth time is unavailable). If `created_source` is ever not
`birthtime`, photos are being aged by something other than when they landed on
the frame, so check the sync tooling and filesystem (see the note in
[configure.md → Playlist weighting](configure.md#playlist-weighting)). To audit coverage, grep these lines over a long run, list
the library recursively (`find <photo-library-path> -type f`), and diff the
two sets: paths that never appear are starved, and a `gap` distribution that
clusters well below `inventory` means photos repeat too soon.

Each process also logs a one-time `playlist_scheduler` line at startup (before
`seq=1`) recording the scheme and parameters that produced the series, so a
journal spanning restarts or config changes can be segmented and interpreted:

```
journalctl -t photoframe | grep -E 'playlist_scheduler|photo_display_metric'
# playlist_scheduler order=weighted-spread refractory=0.700 min_spacing=0.700 new_multiplicity=3 half_life_secs=259200
```

`refractory` is the effective minimum-gap fraction in play (`0.000` for
`weighted-random`, which ignores `min_spacing`); `min_spacing` echoes the raw
config value. With portrait pairing on, the line continues
`pairing=true pairing_probability=1 pairing_bar_ratio=0.618 pairing_dwell_multiplier=1.5`.

With pairing enabled, three more lines appear without `metrics`:
`pairing_header_pass` once at startup (photos read, the median header read
time), `pairing_canvas` whenever the viewer reports a canvas (how many photos
are pairable on it), and `pair_purged` when a prepared pair screen is dropped
because one of its photos was re-queued with priority.

Finally, the first time each photo reaches the screen it logs a one-shot
`photo_first_shown` line carrying `latency_secs` — the precise gap between the
file's birth time and its first appearance (`age_days` on the display line is too
coarse to see a minutes-scale lag):

```
journalctl -t photoframe | grep photo_first_shown
# seq=4104 inventory=822 weight=2.99 latency_secs=1624 created_source=birthtime path=/var/lib/photoframe/photos/local/2004/IMG_2875.jpeg
```

`latency_secs` is a true staging latency only for photos added at runtime; for
the startup-scanned library it is just the photo's age at first show, so filter
to photos whose birth time is later than the `playlist_scheduler` line when
measuring how quickly a fresh upload appears.

Use `-t photoframe` (syslog identifier), not `-u`: on the default stack the
app runs inside the greetd session, not as its own systemd unit. The KMS
stack (`photoframe-kms.service`) logs under the same tag.

The stats measure *submission* cadence (when `present()` returns), which
matches what the panel displays only because transition redraws are paced
to the compositor's frame callbacks (one per output refresh). If
`best_frame_ms` ever drops well below a vsync (e.g. 0.4ms), frames are
being submitted back-to-back and mailbox is discarding some unseen — the
displayed motion is then worse than the numbers suggest. The metric also
cannot see a frame that was submitted on time but *latched late*: a
periodic 33 ms double-step on the panel with a clean 60 fps log is a pacing
problem, not a throughput one. Trust your eyes (or a phone slow-mo
recording of the panel) over the log when they disagree.

For ground truth, film the panel with a phone's slo-mo camera and run
`developer/display-cadence.sh <video> <capture_fps>` — it recovers the true
displayed cadence from inter-frame differences: median update interval,
longest static runs (hangs), and an update timeline. This is the instrument
that separates pacing problems from animation-content problems.

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
rules out the entire application. `frametest` paces exactly like the viewer
(it calls `pre_present_notify` before every present, see below), so on a
healthy 60 Hz stack the expected signature of `fade mailbox` is

```
fps   60.0  avg  16.67ms  best  16.6ms  worst  16.8ms
```

— best ≈ worst ≈ one refresh. A `best` well under 16 ms means the frame
callback is not throttling (the probe is racing mailbox); a `worst` near
33 ms means a frame missed its refresh.

**`frametest --drm`** runs the same workloads with **no compositor**: it
opens the DRM card, becomes DRM master, and presents through Vulkan's
`VK_KHR_display` + `VK_EXT_acquire_drm_display` (wgpu's
`SurfaceTargetUnsafe::Drm`), so every frame goes straight to a KMS page
flip. This is the feasibility probe for running the viewer without sway
(see the direct-scanout note below). The kiosk session must be stopped
first, because sway holds DRM master while it runs:

```bash
sudo systemctl stop greetd.service
sudo -u kiosk /opt/photoframe/bin/frametest solid fifo --drm --seconds 15
sudo -u kiosk /opt/photoframe/bin/frametest fade fifo --drm --seconds 15
sudo -u kiosk /opt/photoframe/bin/frametest fade mailbox --drm --seconds 15
sudo systemctl start greetd.service
```

It prints what KMS reports (cards, connectors, modes), what Vulkan reports
(display modes, planes), then the usual per-second cadence line. Flags:
`--drm=/dev/dri/cardN` to pick a card (default: the first with a connected
connector — on a Pi 5 that is the `vc4` card, not the render-only `v3d`
one), `--connector HDMI-A-1`, `--mode 3840x2160@60` (default: the
connector's preferred mode), `--plane N` (default: the Vulkan plane that
reports the display), `--seconds N` (default: run until Ctrl-C). When it
exits the screen goes dark until greetd restarts sway. Read the ladder the
same way as on Wayland, with one addition: `solid fifo --drm` at a locked
60.0 fps with best ≈ worst ≈ 16.7 ms means page flips are vblank-locked
with no compositor handshake, which is what FIFO cannot deliver under
sway 1.10. If the probe fails before the first cadence line it says which
step refused — missing Vulkan extension, not DRM master, no matching
mode, or the surface/swapchain creation — and that answer is the result.

One such refusal is already known and worked around. The Pi's GPU (`v3d`,
`/dev/dri/card0`) and display controller (`vc4`, `/dev/dri/card1`) are
separate DRM devices, and v3dv reports the GPU's nodes through
`VK_EXT_physical_device_drm`. Upstream wgpu-hal matches the display fd
against those nodes by device number and gives up with "Failed to find
suitable drm device" (observed 2026-10-19 on Mesa 26.2.2, after
`vkGetDrmDisplayEXT` had already resolved the connector). The workspace
therefore carries a vendored copy of wgpu-hal 27.0.4 under
`vendor/wgpu-hal` with one fallback in `src/vulkan/drm.rs` (marked
`PHOTOFRAME PATCH`): when no device matches by number, use the first one
whose `vkGetDrmDisplayEXT` resolves the connector on that fd. It is wired
in through `[patch.crates-io]` in the root `Cargo.toml` and affects only
this code path; remove both once the fix is upstream.

### The viewer on KMS

The viewer itself can present the same way: set `PHOTOFRAME_SURFACE=drm`
(the opt-in `photoframe-kms.service` does; see
[advanced.md › KMS stack](advanced.md#kms-stack-opt-in)). It shares the
probe's target selection and ordering rules
(`crates/photoframe/src/drm_display.rs`):
- The card is opened before the wgpu instance, so the process is DRM
  master.
- The card is closed only after the instance is gone, because Mesa's
  display WSI keeps polling the fd until then.

**Pacing model.** Each transition frame is rendered right after a vblank.
The viewer waits for that vblank explicitly with `DRM_IOCTL_WAIT_VBLANK`
on the connector's CRTC, then draws the frame for that vblank's kernel
timestamp.

The transition's clock starts at the vblank before its first frame. So:
- the first frame shows progress 0;
- each later frame advances by exactly the time between the vblanks that
  it and the previous frame are shown on.

`transition_frame_metric` records the same clock, so it reports the
display's cadence.

**Why the blocking FIFO acquire alone is not enough.** After a dwell,
every swapchain image except the one on screen is idle. The first
frames of a transition would get those images back to back and be
presented a few ms apart, ahead of the display. The animation clock
would then run ahead of the panel, and the metric would read
`best_frame_ms` of 1–3 ms. With the explicit wait at most one frame is
queued, so FIFO only guarantees there is no tearing.

**Present mode.** FIFO stays the default on KMS.
`PHOTOFRAME_PRESENT_MODE=mailbox` is honoured; transitions are still
vblank-paced, and resting frames are single presents.

**Fallback.** If a vblank wait fails, the rest of that transition is
paced by FIFO alone, and the journal logs `kms_vblank_wait_failed`.

**Idle behaviour.** There is no 4 ms control tick on KMS. While nothing
animates, the viewer sleeps until one of these happens:
- a control command arrives;
- a matting result arrives;
- a decoded photo arrives while the preload queue is short;
- the dwell deadline passes.

A resting photo therefore costs no frames and no wakeups. The Wayland path
waits on the same plan (`crates/photoframe/src/tasks/viewer/wake.rs`): it
ticks every 4 ms only while a transition is animating or the window's size
is still settling at startup, and otherwise wakes just for those events, a
handful of times per photo instead of 250 times a second.

**Reading `transition_frame_metric` on KMS.** On KMS the line has one
more field than on Wayland, `late_frames`. The fields answer two
different questions:

- **`best_frame_ms` / `worst_frame_ms` / `avg_fps`: did the CPU loop
  keep up?** These are the times between the vblanks the frames were
  paced on. A `worst_frame_ms` of about 33 means the loop missed a
  vblank.
- **`late_frames`: did the panel show a new frame on every refresh?**
  It counts the refreshes during the transition on which the panel
  showed no new frame, whatever the cause: the CPU loop was late, or the
  GPU finished a frame after the vblank it was meant for. The interval
  fields cannot see the second case, because the CPU stays on time while
  the panel repeats a frame.

How `late_frames` is computed: each paced frame registers a
GPU-completion callback, and every pacing wake polls for it. The viewer
then replays FIFO: a frame flips at the first vblank by which it is
complete, and never before the frame ahead of it. That replay is
optimistic by at most the microseconds between a vblank and the wake
after it.

**What to expect.** For a transition that holds 60 at native 4K:
`avg_fps` ≈ 60, `best_frame_ms` ≈ `worst_frame_ms` ≈ 16.7, and
`late_frames=0`.

**When the field is missing.** It is absent if vblank pacing failed
during that transition. Look for `kms_vblank_wait_failed` in the
journal.

**GPU slack.** With the explicit vblank wait, a frame has less than one
refresh to finish. FIFO's run-ahead allowed up to two, but that
run-ahead is what put every transition's first frames ahead of the
display. A miss leaves one frame queued, which gives every later frame
of that transition two refreshes. So one miss costs exactly one
repeated refresh and does not cascade.

**For T4.** The native-resolution decision should require
`late_frames=0`, not just the interval fields.

## Root causes and fixes (June 2026)

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
   wipe.

   *Worth retrying with newer sway:* wlroots 0.19+ speaks `wp_fifo_v1`, so
   under sway 1.11+ proper FIFO should pace at full rate. The check is one
   command (`frametest solid fifo` ≈ 60 fps means FIFO is healthy); the
   switch is `PHOTOFRAME_PRESENT_MODE=fifo` in the launcher. There is no
   urgency — mailbox remains correct on both broken and fixed stacks.

   **Mailbox must be paced — by the compositor, not a clock.** Mailbox
   never blocks, so an unpaced redraw loop submits frames faster than the
   compositor latches them; mailbox then discards the older ones unseen
   while the animation clock keeps advancing, so displayed motion is
   uneven (jerky) even though `avg_fps` looks healthy — the tell is
   `best_frame_ms` far below a vsync (e.g. 0.4 ms).

   The first fix was a wall-clock floor: no transition redraw sooner than
   15 ms after the last present. That is *almost* a refresh, and "almost"
   is the problem — submitting every 15 ms against a 16.67 ms panel means
   one frame in every ~10 is overtaken by its successor before the
   compositor latches it, so mailbox drops it and the animation visibly
   double-steps (30 ms) every few hundred milliseconds. The log reads a
   clean 60 fps throughout, because it counts submissions, not latches.

   The viewer now paces to the compositor instead. Before every wake-scene
   `present()` it calls winit's `Window::pre_present_notify()`, which
   requests a `wl_surface.frame` callback on that commit; winit then
   withholds `RedrawRequested` until the compositor answers (winit 0.30,
   `platform_impl/linux/wayland/event_loop/mod.rs`, the
   `FrameCallbackState::Requested` check). sway answers once per output
   refresh, after it has latched the buffer, so the model is **one
   submission per frame callback = one per refresh**: nothing is ever
   discarded, the animation advances exactly one refresh per displayed
   frame, and the stats are honest. A transition in progress always keeps
   a redraw requested; the compositor decides when it fires. There is no
   wall-clock floor any more — any floor longer than the present-to-callback
   latency would push the next frame past its refresh deadline and halve
   the rate.

   Side effects worth knowing: frame callbacks arrive only while the
   surface is visible, so an occluded window (the Wi-Fi overlay) or a
   powered-off output pauses transition rendering rather than burning the
   GPU on unseen frames; `Occluded(false)` re-arms the redraw. The greeting
   and sleep screens draw once on demand and do not request callbacks, so
   they are never left waiting for one. On non-Wayland backends
   `pre_present_notify` is a no-op and pacing falls to the present mode —
   FIFO blocks at the refresh rate; mailbox would run unpaced, so use
   `PHOTOFRAME_PRESENT_MODE=fifo` there.

   Measuring the difference needs the panel, not the log: film a transition
   in phone slo-mo and run `developer/display-cadence.sh`. Before: median
   interval one refresh with a periodic two-refresh step. After: a flat
   one-refresh timeline. `frametest fade mailbox` should read 60.0 fps with
   best ≈ worst ≈ 16.7 ms.

4. **CPU/GPU contention during transitions.** Preparing the next photo
   runs a JPEG decode + NEON matting on worker threads and ends with a
   multi-megapixel `write_texture`. On the Pi's unified memory those bursts
   steal the same bandwidth the transition is rendering with, and a
   transition starting is exactly what drops the preload below target and
   kicks off the next prep — so the contention landed precisely during
   animations, as 50–140 ms worst frames. Both halves are now gated while a
   transition is animating: non-priority uploads are deferred, and new
   decode/matting work is not started. The long dwell (seconds) absorbs the
   deferral with room to spare.

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
  per-pixel scratch memory. The layer is `Rgba16Float`, not 8-bit: petals
  are very dark in linear light, so a smooth shading gradient spans only a
  few 8-bit levels and visibly bands — half-float has the dark-range
  precision to keep it smooth, at negligible cost on a quarter-res layer.
- **The main pass renders opaquely** (no blending) and composites letterbox
  regions over the background color in-shader, saving a destination read
  per pixel and keeping the surface eligible for direct scanout.

## Known limitation: direct scanout

sway attempts to scan out the fullscreen buffer every frame but the import
fails: v3dv allocates UIF-tiled swapchain buffers (modifier
`0x0700000000000006`), which the display controller cannot scan out, and
`MESA_VK_WSI_DEBUG=linear` does not change the negotiated modifier (tested
on Mesa 25.0). sway therefore composites — on the GPU, costing a few ms per
frame. In memory-traffic terms that pass reads the 33 MB client buffer and
writes a 33 MB output buffer every frame — about a third of all the
traffic a native 4K fade generates — on a bus that is the binding
constraint at 4K60. Two ways out: a sway/wlroots/Mesa upgrade that gets
scanout working (`journalctl -t photoframe` with `sway -d` shows per-frame
scan-out decisions), or no compositor at all. `frametest --drm` (above)
measures the second option without touching the viewer. The opt-in KMS
stack is that second option for the viewer itself (see
[The viewer on KMS](#the-viewer-on-kms)).

## Environment knobs

Set in `/usr/local/bin/photoframe` (the launcher), or for the KMS stack in
the unit (`sudo systemctl edit photoframe-kms.service`, `[Service]`
`Environment=`); all optional:

| Variable | Values | Default |
| --- | --- | --- |
| `PHOTOFRAME_SURFACE` | `wayland` / `drm` | `wayland` (the KMS unit sets `drm`) |
| `PHOTOFRAME_PRESENT_MODE` | `fifo` / `mailbox` / `immediate` | mailbox when available on Wayland; fifo on KMS |
| `PHOTOFRAME_DRM_CONNECTOR` | e.g. `HDMI-A-1` (KMS only) | first connected connector |
| `PHOTOFRAME_DRM_MODE` | `WxH[@Hz]` (KMS only; must exist) | `3840x2160@60` if offered, else the connector's preferred mode |
| `PHOTOFRAME_TRANSITION_SCALE` | 1–4 | 2 |
| `PHOTOFRAME_IRIS_LAYER_SCALE` | 1–8 | 4 |
| `WGPU_BACKEND` | `vulkan` / `gl` | vulkan |
