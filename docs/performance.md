# Performance

How the viewer reaches smooth 4K60 transitions on a Raspberry Pi 5, how to
measure it, and what to check when frame rates regress. Everything here was
learned the hard way during a week of profiling in June 2026; the punchline
is that **not one of the root causes was a slow shader** — they were
firmware, a missing driver package, the presentation mode, and CPU/GPU
contention. Measure the whole pipeline before optimizing any pass.

## Why the default stack has no compositor

The viewer runs on DRM/KMS by default (`photoframe-kms.service`, see
[advanced.md › Display stacks](advanced.md#display-stacks)) because the
compositor, not the app, was the frame-rate ceiling. Measured with
`frametest` on 2026-10-19, 4K at 60 Hz, after the Pi's Mesa moved from
25.0.7 to 26.2.2 (sway stayed at 1.10.1 on wlroots 0.18):

| Stack | Run | Result |
| --- | --- | --- |
| sway (Wayland) | `solid mailbox`, `solid mailbox` with four frames of latency, `solid immediate` | about 31–32 fps in every present mode; best 0.3 ms, worst 63 ms (two submissions back to back, then a two-refresh wait for the compositor to release a buffer) |
| KMS (no compositor) | `solid fifo --drm` | 60.0 fps, every interval 16.5–16.9 ms |
| KMS | `fade fifo --drm`, `fade mailbox --drm` | 60 Hz, intervals 16.3–17.0 ms; a native-4K two-texture pass holds the refresh rate |

Even `immediate`, which ignores vsync, was capped on sway, so no present mode or
pacing in the viewer could fix it. The ceiling sits in the compositor path
(sway 1.10 with Mesa 26's Wayland WSI), and the same `solid` workload with the
compositor gone is vblank-locked at 60. The Wayland stack remains as a
fallback for one release; its ladder is below. Mesa 25.0 under sway reached
60 with mailbox in June, so a newer sway or Mesa may lift the cap, but the KMS
stack does not depend on either.

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

Use `-t photoframe` (syslog identifier), not `-u`: both stacks log under that
tag, and on the Wayland fallback the app runs inside the greetd session, not
as its own systemd unit.

The stats measure *submission* cadence (when `present()` returns). On the
Wayland fallback that matches what the panel displays only because transition
redraws are paced to the compositor's frame callbacks (one per output refresh);
on KMS the metric reports what the panel showed, from the flip audit (see
[The viewer on KMS](#the-viewer-on-kms)). If
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
once per second. There are two ladders, one per stack. On the default KMS
stack run it with `--drm` (next section). On the Wayland fallback run it
inside the kiosk session:

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
flip. This is the probe that showed the viewer could run without sway (see the
direct-scanout note below), and the KMS ladder. The viewer must be stopped
first, because whichever process holds DRM master (the viewer unit, or sway
on the fallback) blocks it:

```bash
sudo systemctl stop photoframe-kms.service
sudo -u kiosk /opt/photoframe/bin/frametest solid fifo --drm --seconds 15
sudo -u kiosk /opt/photoframe/bin/frametest fade fifo --drm --seconds 15
sudo -u kiosk /opt/photoframe/bin/frametest fade mailbox --drm --seconds 15
sudo systemctl start photoframe-kms.service
```

(On the Wayland fallback, stop and start `greetd.service` instead, with a
`sleep 1` between the two, never `restart`.)

It prints what KMS reports (cards, connectors, modes), what Vulkan reports
(display modes, planes), then the usual per-second cadence line. Flags:
`--drm=/dev/dri/cardN` to pick a card (default: the first with a connected
connector — on a Pi 5 that is the `vc4` card, not the render-only `v3d`
one), `--connector HDMI-A-1`, `--mode 3840x2160@60` (default: the
connector's preferred mode), `--plane N` (default: the Vulkan plane that
reports the display), `--seconds N` (default: run until Ctrl-C). When it
exits the screen goes dark until the viewer unit (or greetd) is started again.
Read the ladder the same way as on Wayland, with one addition: `solid fifo --drm` at a locked
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
this code path; remove both once the fix is upstream. The vendored copy also
carries one unrelated change, a lifetime lint fix in `src/gles/egl.rs:425`
(`MappedMutexGuard<'_, ..>`), so a mechanical diff against the published
27.0.4 shows two files: `drm.rs` (the patch) and `egl.rs` (the lint).

### The viewer on KMS

The viewer itself can present the same way: set `PHOTOFRAME_SURFACE=drm`
(`photoframe-kms.service`, the default stack, does; see
[advanced.md › KMS stack](advanced.md#kms-stack-default)). It shares the
probe's target selection and ordering rules
(`crates/photoframe/src/drm_display.rs`):
- The card is opened before the wgpu instance, so the process is DRM
  master.
- The card is closed only after the instance is gone, because Mesa's
  display WSI keeps polling the fd until then.

**Pacing law: render ahead, clock by frame index.** During a transition
the viewer renders frames back to back, exactly as `frametest --drm` does.
It has no wait of its own. The FIFO swapchain acquire blocks when every
image is on screen or queued, so the viewer runs at most two frames ahead
of the display.

The clock works by frame index:
- **Anchor.** When a transition's first frame is rendered, the viewer
  reads the most recent vblank without waiting (`DRM_IOCTL_WAIT_VBLANK`,
  relative 0). The anchor is the vblank after it.
- **Frame k** is meant for vblank `anchor + k` and is drawn for that
  vblank's time, `anchor time + k × T`. T is the mode's exact period,
  `htotal × vtotal / pixel clock`.
- **Why this is exact.** Under FIFO, frames that are not late flip on
  consecutive vblanks, so every frame is shown exactly when its clock
  says. That includes the first frames, which take the idle images back
  to back after a dwell.
- **When the GPU cannot keep up.** No frame is drawn for a time before
  it is rendered. A frame rendered after the vblank it was meant for is
  drawn for the moment it is rendered. So a GPU that cannot keep up
  gives a transition of the right length with repeated frames, not a
  slow-motion one.
- **End.** A transition ends when its frame for progress 1 has been
  rendered.

**Why not wait for the vblank before each frame (the v1.2.0-rc1 law).**
Pi gate 1 measured it on 2026-10-21. Every transition alternated 16.7 ms
and 33.3 ms frames, and none reached 60. Means at transition scale 2:

| Transition | avg_fps | late frames per frame |
| --- | --- | --- |
| fade | 45 | 0.35 |
| wipe | 42 | 0.46 |
| push | 39 | 0.51 |

`frametest --drm`, which renders ahead, held 60 on the same build.

Waiting for vblank V before starting frame k leaves less than one
refresh before the flip at V+1. In that time the frame needs its CPU
work, the transition pass and the 4K upscale, written straight into the
linear image the panel scans out (see "The 4K write floor").

With the loop and the GPU in series around the present, the cost
compounds:
- a frame of slightly over one refresh costs two;
- the next frame cannot start before the vblank after that.

Rendering ahead starts the next frame as soon as an image is free. A
frame over budget then loses only its excess, and the queue absorbs
single slow frames. The pacing tests in
`crates/photoframe/src/tasks/viewer/pacing.rs` cover this with a
simulated FIFO swapchain.

**Present mode.** FIFO stays the default on KMS, and it is the only mode
the Pi's display path offers.

**Idle behaviour.** There is no 4 ms control tick on KMS. While nothing
animates, the viewer sleeps until one of these happens:
- a control command arrives;
- a matting result arrives;
- a decoded photo arrives while the preload queue is short;
- the dwell deadline passes.

A resting photo therefore costs no frames and no wakeups. The Wayland path
waits on the same plan (`crates/photoframe/src/tasks/viewer/wake.rs`): it
ticks every 4 ms only while a transition is animating or the window's size
is still settling at startup, and otherwise wakes for those events plus a
once-a-second look at the window's size (sway can change the output mode
without telling winit), instead of 250 times a second.

**Reading `transition_frame_metric` on KMS.** On KMS the line reports
what the panel showed, not the loop's timing. The loop's timing would
mostly show how far ahead frames were rendered.

The fields:
- `frames`: the frames that flipped.
- `avg_fps`: frames divided by the time from the first frame's flip to
  the last's.
- `best_frame_ms` / `worst_frame_ms`: the shortest and longest time a
  frame stayed on screen.
- `late_frames`: the refreshes on which no new frame appeared.

How the fields are known:
- **Audit.** Each transition frame registers a GPU-completion callback.
  While a transition plays, a flip-observer thread waits on every vblank,
  on its own handle on the card, and polls the GPU. This runs off the
  render path, so it never delays a frame.
- **Replay.** The viewer replays FIFO from those observations: a frame
  flips at the first vblank by which it is complete, and never before
  the frame ahead of it. The replay is optimistic only by the fraction of
  a millisecond between a vblank and the observer's wake.
- **Closing.** After the transition's last frame, the viewer waits until
  every frame has flipped, normally two or three refreshes, and logs the
  line.

**What to expect.** A transition that holds 60 reads:
- `avg_fps` ≈ 60;
- `best_frame_ms` ≈ `worst_frame_ms` ≈ 16.7;
- `late_frames=0`.

`late_frames=0` is the test. A frame that stayed on screen for two
refreshes shows up both as a `worst_frame_ms` of 33.3 and as one late
frame.

**`audited=false`.** An audited line says `audited=true` and carries
`late_frames`. If the audit failed in that transition (the journal then
shows `kms_vblank_wait_failed`, or the vblank query failed when it
started), the line says `audited=false` and carries only the frame count:
on KMS the loop's own intervals are render-ahead spacing, not what the
panel showed.

**Per-frame detail for T4.** With `RUST_LOG=debug` (or `photoframe=debug`)
every transition frame logs a `kms_frame` line, with:
- the vblank the frame was meant for, the one it flipped on, and
  `late_refreshes`;
- bounds on its GPU time: `gpu_ms_at_least` comes from the last vblank
  at which it was still running, `gpu_ms_at_most` from the first at which
  it was seen done;
- the measured GPU time of its passes, from timestamp queries:
  `gpu_draw_ms` from the start of the first pass that draws the frame (the
  iris petal layer, when there is one) to the end of the main draw pass;
  for kinds with a pass before the main one, `gpu_layer_ms` (iris: both
  petal-layer passes; e-ink: its reveal-mask pass) and `gpu_main_ms` (the
  main pass), `-` for the others; and `gpu_upscale_ms` for the pass
  that upsamples the intermediate to 4K (`-` for a frame drawn natively,
  such as a transition's final frame).

The bounds are one refresh apart: enough to tell whether a kind fits in a
refresh, not by how much. The pass times give the margin and say which
pass a kind loses on. The acceptance for a transition kind at scale 2 is
`gpu_draw_ms + gpu_upscale_ms` ≤ 14 ms on every frame: 2.7 ms short of a
60 Hz refresh, left for the work around the passes (uniform uploads, the
caption overlay, submission).

Pass timing needs the device's `TIMESTAMP_QUERY` feature. The viewer
requests it where the adapter offers it (`viewer_gpu_pass_timing
available=…` at debug on startup) and runs without it otherwise; the two
fields then read `-`. Frames are timed only while `kms_frame` is logged, so
a deployment at info level pays nothing. The timestamps of a frame are
resolved in its own submission and read back asynchronously, through the
flip observer's poll at the next vblank, never on the render path. A
timestamp may add a small sync point between passes in the driver, so the
pass times are, if anything, slightly pessimistic. A pair whose end reads
before its start (Metal now and then leaves an end sample at 0) is
dropped and logs `-`.

**Panel off (`display-power off`).** Before switching off, the viewer
lets submitted frames reach the screen: GPU idle, then two vblanks, about
33 ms. With render-ahead at most two frames are queued behind the one on
screen. A display-power command first abandons any transition audit, so
the flip observer is parked and nothing waits on a vblank around the
power commit.

While the panel is dark the viewer presents nothing. The slideshow is
paused, so the loop sleeps until a command arrives: no frames and no
wakeups.

`display-power on` commits the modeset before any frame is presented, so
the CRTC is running and every later frame is paced as usual (see
[advanced.md › Display power on KMS](advanced.md#display-power-and-the-wi-fi-overlay-on-kms)). A transition that the panel
going dark interrupts is undone and logs no `transition_frame_metric`.

**The Wi-Fi overlay** costs one CPU rasterization and one texture upload
per change of content or size (lease refreshes cost nothing), then one
composite per redraw. It pauses the slideshow while it covers it, so no
transition runs underneath.

**For T4.** Decide native resolution from `late_frames=0`, not just the
interval fields. Use the `kms_frame` lines to see which passes fit in a
refresh. With render-ahead, the half-res intermediate plus the 4K upscale
pass have to be measured again against `PHOTOFRAME_TRANSITION_SCALE=1`.

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
   renderer. This applies to the Wayland fallback stack. The package is explicit
   in the apt list on both stacks (`kmscube` needs it too) and `verify.sh`
   checks it when the Wayland stack is chosen.

3. **FIFO presentation costs two vsyncs per frame.** Vulkan FIFO emulated
   over Wayland frame callbacks serializes against the compositor on stacks
   without the `wp_fifo_v1` protocol (sway 1.10 / wlroots 0.18): `frametest
   solid fifo` locks at a metronomic 33.3 ms on a 60 Hz output. The viewer
   therefore defaults to **mailbox** presentation on the Wayland fallback (KMS
   defaults to FIFO, which is vblank-locked there), which latches the newest
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
   surface is visible, so an occluded window (the fallback Wi-Fi overlay
   window) or a powered-off output pauses transition rendering rather than burning the
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
  hidden by motion. Only a transition's final frame (progress 1) renders
  native: it is the next photo at rest, drawn by `fs_rest`, so the photo
  settles in sharp and that frame costs what a dwell frame costs. Override
  per deployment with `PHOTOFRAME_TRANSITION_SCALE` (1 bypasses the
  intermediate entirely).

  Until v1.2 the native tail was every frame past *eased* progress 0.99.
  Smoothstep easing reaches 0.99 at about 94% of the time, so the last ~6%
  of each transition's frames ran the kind's own shader at 4K. E-ink, the heaviest, missed a refresh on each of
  them (33 ms frames at the end of the per-frame audit). Every kind's
  progress-1 image on a full-screen canvas is the next photo; dissolve and
  venetian blinds, whose soft edges never quite close at progress 1, now
  reach it one frame earlier instead of on the first dwell frame.

  **Where transition-time softness comes from.** Not from aliasing (a
  radial wipe's default feather is a tenth of the diagonal, about 880 px
  at 4K), but from the 2x upscale of the intermediate: the photos are read
  from their mip 1 at 1920x1080 and reconstructed to 4K. The eye tracks a
  slow front and sees that softness, then the native final frame snaps the
  photo to full sharpness. The upscale (`PHOTOFRAME_UPSCALE`, shared by
  the Wayland and KMS outputs) is one bilinear tap by default. A
  Catmull-Rom cubic is an opt-in (`bicubic`: 5 bilinear taps, the corner
  taps of the exact 9-tap form left out and the rest renormalized;
  `bicubic-9` is exact), but it is not usable at 4K60 on this GPU. Pi
  measurement, 2026-10-22, scale 2, `gpu_upscale_ms` median / p90:
  bilinear 11.02 / 11.43 ms, `bicubic` 18.03 / 18.58, `bicubic-9`
  25.48 / 25.90, and with either cubic every transition dropped frames.
  Each 4K tap costs about 1.75 ms: the pass is bound by V3D's texture
  sample rate (about 4.7 G bilinear fetches a second) once it takes more
  than one, not by memory. (`frametest --drm`'s 0.14 ms for one 4K read
  was the memory cost of a read, not the cost of a tap.)

  Mac GPU, five project photos (two native 4K showcase photos, a 3600x2000
  one, two 4032x3024 library photos; each cover-cropped to 3840x2160), the
  viewer's own mip 1 upscaled 2x against the native 4K canvas, sRGB 8-bit:

  | Upscale | Mean abs error | Pixels off by > 8 LSB |
  |---|---|---|
  | bilinear | 4.32 LSB | 15.8% |
  | bicubic (5 taps) | 3.47 LSB | 12.7% |
  | bicubic-9 | 3.49 LSB | 12.8% |

  The cubic cuts the error by a fifth on every photo; what remains is the
  detail mip 1 never held. The 5-tap form is as close as the exact one.
  Sharper transitions at no runtime cost have to come from the mip itself:
  see "The mip transitions read is pre-sharpened" below.
  The upscale filter applies to every transition kind, push included: push
  reads its photos from level 0, but its frame is drawn into the
  intermediate and upscaled like the others. (The sharpened mip below does
  not reach push, which never reads the mip.)

  **Not a sharper downsample.** A sharper-looking downsample than the box,
  Catmull-Rom stretched to 2:1 (16 bilinear taps, at upload), was measured
  against it on the same photos and on synthetic gratings and edges:

  | Mip 1 from | Upscale | Mean abs error | Pixels off by > 8 LSB |
  |---|---|---|---|
  | box | bilinear | 4.32 LSB | 15.8% |
  | box | bicubic | 3.47 LSB | 12.7% |
  | Catmull-Rom | bilinear | 4.41 LSB | 16.1% |
  | Catmull-Rom | bicubic | 3.64 LSB | 13.5% |

  The stretched cubic is the better anti-aliasing filter but the softer one
  where it matters: a grating at 0.20 cycles per source pixel (below the
  mip's Nyquist of 0.25) keeps 0.71 of its amplitude against the box's
  0.81, while above Nyquist it leaves 0.27 of a 0.30 grating where the box
  leaves 0.59 (moiré on fine stripes). Its negative lobes also ring: a
  0.2 to 0.8 step (linear) undershoots to 0.175 and overshoots to 0.831,
  where the box has none. It lost on every photo, so the box stays.

  **The mip transitions read is pre-sharpened.** The 2x bilinear upscale
  is a known low-pass, so the level transitions read (level 1 at scale 2)
  is box filtered and then unsharp-masked at upload (`fs_mip_sharpen`):
  box + a (box - blur), the blur the [1 2 1] / 4 binomial over the level's
  own pixels, clamped to the range of the taps it was built from. It is 9
  bilinear taps per pixel of a 1920x1080 level, once per photo, off the
  transition path (uploads wait out transitions); at the Pi's measured
  sample rate (about 4.7 G bilinear fetches a second) that is about 4 ms
  of GPU time per photo against about 0.5 ms for the plain box. Nothing
  changes per frame. The clamp is what makes it usable: unclamped, the mask
  rings on every edge.

  Mac GPU, the same five photos, mip 1 upscaled 2x bilinear against the
  native canvas, plus a 0.2 to 0.8 step and a one-pixel 0.8 line on 0.2
  (linear light; halos in sRGB 8-bit LSB beyond the plateaus):

  | Mip 1 | Mean abs error | Pixels off by > 8 LSB | Step under / over | Line halo |
  |---|---|---|---|---|
  | box | 4.32 LSB | 15.8% | 0 / 0 | 0 |
  | unsharp a = 0.25, unclamped | 3.88 | 14.1% | 4 / 2 | 4 |
  | unsharp a = 0.75, unclamped | 3.47 | 12.8% | 13 / 5 | 13 |
  | unsharp a = 1.0, unclamped | 3.49 | 13.1% | 17 / 7 | 17 |
  | clamped a = 0.25 | 4.03 | 14.6% | 0 / 0 | 0 |
  | clamped a = 0.5 | 3.88 | 14.0% | 0 / 0 | 0 |
  | clamped a = 0.75 | 3.81 | 13.8% | 0 / 0 | 0 |
  | **clamped a = 1.0** | **3.79** | **13.8%** | **0 / 0** | **0** |
  | clamped a = 1.25 | 3.80 | 13.9% | 0 / 0 | 0 |
  | clamped a = 1.5 | 3.82 | 14.0% | 0 / 0 | 0 |

  Clamped a = 1 (`PHOTOFRAME_MIP_SHARPEN`, 0 to 2, default 1; 0 is the
  plain box) takes back 12% of the error at no cost
  per frame and with no halo, about 60% of what the 5-tap cubic upscale
  recovered (3.47) at 7 ms a frame. Per photo: 5.14 to 4.38, 9.96 to 8.84,
  3.39 to 3.12, 1.35 to 1.15, 1.77 to 1.45 LSB. A thin line is not
  sharpened (the clamp keeps it within its neighborhood), only kept
  halo-free; at scale 4 only level 2 is sharpened, from a plain level 1.
  a = 1 was tuned for the 2x round trip (scale 2, the default) only:
  scales 3 and 4 are untuned, and at scale 3 the lod of 1.585 blends the
  plain level 1 with the sharpened level 2.

  With `PHOTOFRAME_MIP_SHARPEN=0` the level is exactly the plain box: the
  center tap is the box (one bilinear tap at the same position as
  `fs_blit`'s), and with no weight elsewhere the clamp returns it
  unchanged. The shader folds each tap into a running sum, minimum and
  maximum as it reads it, so three values stay live instead of nine.

  The mip pass runs at upload, which waits out transitions, except for a
  priority (operator-requested) photo, which uploads at once: with the
  sharpened level that pass is about 4 ms of GPU time, so a slip in the
  transition it lands on is likely.
- **Iris petals are a signed-distance layer at 1/`layer-scale`** (the
  iris transition's config key, 1–16, default 4;
  `PHOTOFRAME_IRIS_LAYER_SCALE` overrides it for tuning). The
  petal math runs on CPU-precomputed per-frame constants and avoids
  dynamically indexed local arrays, which the V3D compiler demotes to slow
  per-pixel scratch memory. The layer is drawn in two passes:
  - `fs_iris_cover` is the loop over petals. It carries only the nearest
    edge distance and petal and a 16-bit "inside" mask, reads which petal
    is on top and how many cover the pixel from the mask afterwards
    (`firstLeadingBit`, `countOneBits`), and stores the distance, the
    petal whose shade shows (the top one, or just outside the petals the
    nearest) and the count in an `Rgba16Float` cover target;
  - `fs_iris_layer` shades from that texel, with no loop, and writes the
    shade (rgb) and the signed distance (alpha).

  `fs_iris`, the main pass, samples the layer bilinearly and draws the
  petals' outline itself: coverage is a smoothstep of the interpolated
  distance over ±0.75 of its own pixel, and the edge highlight is added
  from the same distance. A signed distance interpolates cleanly, so the
  outline stays crisp however coarse the layer; the layer's resolution
  sets only how finely the shading is sampled (sheen, the overlap shadows,
  and where one petal's edge lies over another, whose two sides belong to
  different petals and so cannot be interpolated). Inside the aperture the
  cover pass stores the aperture circle's distance, a lower bound on every
  petal's, wherever no texel's reach can bring a petal into view.

  Mac GPU check against the 1/4 layer of v1.2 rc (21 frames at 4K geometry,
  5, 9 and 14 blades, open through closed; the main pass at 1920x1080):

  | Layer scale | Photo | Petal interior, max (pixels > 1 LSB) | Near overlap edges, max | Outline position error p99 / max |
  |---|---|---|---|---|
  | 4 | identical | 1 LSB (0) | 1 LSB | 0.09 / 1.4 px |
  | 8 | identical | 2 LSB (110 of 29M) | 15 LSB | 0.38 / 4.2 px |
  | 16 | identical | 5 LSB (11k of 27M) | 18 LSB | 1.2 / 7.2 px |

  The outline is crisper than before at every scale (±1.5 screen px
  instead of the old ±1 layer texel feather), so the edge band differs by
  design. The interior differences sit in the tails of the overlap-shadow
  ramps (27 px wide at 4K, sampled every 16 px); the outline error peaks
  where two petal edges meet at an angle (the interpolated distance rounds
  the corner). The overlap edges are sampled at the layer's resolution.

  Pi measurement, 2026-10-22: the cover loop is the cost, layer pixels ×
  blades, peaking where the petal band is widest (12.4 ms at scale 4,
  3.5 ms at 8, 1.0 at 16). At 16 iris holds 60, but by eye (Vincent,
  2026-10-22) the petal shading and the overlaps are too fuzzy, so the
  default is 4: exact shading at about 47 fps on a 4K Pi 5. 8 runs about
  55 fps with interiors within 2 LSB of 4. The choice is per iris
  transition (`layer-scale` in configure.md); the viewer logs each one's
  effective scale at startup (`viewer_iris_layer_scale`).
  All iris entry points compile at 4 threads (`fs_iris_cover` 188
  instructions; the one 2-thread shader in the build is `fs_debug_bezier`,
  used only with `PHOTOFRAME_DEBUG_BEZIER`).

  `fs_iris` fetches both photos and evaluates the outline on every pixel
  (see "Branch-free transition shaders" below). `kms_frame` reports the layer and the main pass separately (`gpu_layer_ms`,
  `gpu_main_ms`). The layer is `Rgba16Float`, not 8-bit: petals are very
  dark in linear light, so a smooth shading gradient spans only a few
  8-bit levels and visibly bands; half floats also hold the distance.
- **The main pass renders opaquely** (no blending) and composites letterbox
  regions over the background color in-shader, saving a destination read
  per pixel and keeping the surface eligible for direct scanout.
- **One shader and one pipeline per transition kind.** Each kind has its
  own fragment entry point in `viewer_quad.wgsl` (`fs_fade`, `fs_wipe`, …,
  `fs_rest` for a resting photo), compiled into its own render pipeline,
  and the CPU picks the pipeline (`DrawKind` in `viewer.rs`). Until
  v1.2.0-rc1 one `fs_main` switched over every kind. The V3D compiler
  sized its registers for the union of all cases: 975 instructions, 47
  temporaries, and the iris petal arrays in the uniform block. It could
  not allocate them at 4 threads per QPU and fell back to 2 threads.

  V3D hides texture latency by switching among 4 threads, so every
  transition ran its texture fetches at a fraction of the speed, even a
  fade. Pi measurement, 2026-10-21: 33–50 ms of GPU time per frame at
  transition scale 2, against a 4K fade in `frametest` at 4 threads that
  holds 60.

  The iris petal constants now live in their own uniform (`IrisPetals`),
  read only by `fs_iris_layer`. The vertex stage draws one fullscreen
  triangle computed from the vertex index, with no local array (V3D
  demotes dynamically indexed local arrays to scratch).

  To check on the Pi, run the viewer with `V3D_DEBUG=shaderdb`. Every
  `MESA_SHADER_FRAGMENT` line must read `4 threads`, and the journal must
  have no `Falling back to strategy` lines.
- **Per-frame constants on the CPU; branchless plane sampling.** At 4
  threads, a pass costs its instructions per pixel. The Pi measured a fade
  at 176 instructions, of which each photo-plane lookup was about 60: two
  divisions and four compares with branches.

  Everything that is the same for every pixel of a frame is now computed
  once on the CPU (`viewer/draw_params.rs`):
  - where each photo plane lands, as an affine texture-coordinate map that
    the vertex stage interpolates, so the fragment stage does no division;
  - the wipe front and its edges;
  - the push offsets, which move the planes;
  - the crossfade-zoom maps;
  - the radial-wipe center and scale;
  - the venetian-blinds axis and edges.

  A plane is sampled without branches: one sample, plus a select on
  "inside 0..1" for coverage.

  `fs_rest`, drawn at native 4K on every dwell and at every transition's
  end, samples only the current plane. If a next photo is ever staged at
  rest, the CPU draws a fade at progress 0 instead. Output is unchanged
  except for pixels whose center lies exactly on a plane edge, where the
  interpolated coordinate can round one ulp the other way.
- **Mipmapped canvases.** Pi measurement, 2026-10-21: wipe, fade, dissolve
  and radial cost about 17.7 ms per frame at every transition scale. Push,
  which reads one real plane per pixel, held 60. That is memory traffic,
  not ALU: a transition into the half-res intermediate minified both 4K
  canvases (2 × 33 MB a frame).

  Each canvas now carries the mip levels the transition scale needs:
  level 1 at scale 2, levels 1–2 at scale 4, none at scale 1. They are
  generated at upload, off a transition's frames: 2:1 box filtered in
  linear light, one `fs_blit` pass per level. Transition passes sample the
  level that matches the intermediate (`textureSampleLevel` at
  `U.lod = log2(scale)`). Native frames (resting photos, a transition's
  final frame) stay at level 0, unchanged, and so does push: it reads one
  plane per pixel, already held 60 at level 0, and at level 0 its moving
  planes keep their sharpness.

  Output, checked on the Mac's GPU against v1.2 builds:
  - native frames are identical;
  - a screen-sized canvas drawn full-screen (the default layout) is within
    1 LSB for every kind whose planes stay put;
  - planes that zoom (crossfade-zoom) land between canvas texels, where
    the old 2:1 bilinear read undersampled level 0 and the mip is a proper
    prefilter. Those frames are slightly softer, by up to about 20 LSB on
    fine detail. Push would be too, which is why it stays at level 0.

  Memory: +25% per canvas at scale 2 (8.3 MB on a 33 MB 4K canvas), +31% at
  scale 4. That is about +40 MB with the default preload of three plus the
  current and next photos. With `RUST_LOG=photoframe=debug` each upload
  logs `canvas_upload` with the canvas's bytes (every level) and the live
  total over all canvases (`live_canvases`, `live_canvas_mb`), so a
  process's RSS can be set against what the canvases account for. The
  mips' own share is isolated by running once with
  `PHOTOFRAME_TRANSITION_SCALE=1` (no mips, no render-target usage on the
  canvases): the RSS difference between the two runs should be
  `live_canvases` × 8.3 MB.
- **The main-pass budget.** Pi measurement, 2026-10-22: at scale 2 the
  bilinear upscale costs 10.9 ms median and 11.5 ms at p90 on every frame, so a
  kind holds 60 with no late frames only while its main pass stays under
  about 5.2 ms at p90. At 1920x1080 a two-photo pass with almost no
  arithmetic (fade) costs about 4.5 ms.
- **Branch-free transition shaders.** On V3D, keep transition fragment
  shaders straight-line: a per-pixel branch that skips work costs more
  than two mip-1 fetches. Every kind fetches both photos and evaluates its
  smoothsteps on every pixel, even where a weight is zero; nothing tests a
  soft band or skips a fetch under an opaque layer.

  This was measured, not assumed. Until v1.2.0-rc2 wipe, venetian blinds,
  radial-wipe, e-ink and iris skipped a photo's fetch where its weight was
  zero ("whole tiles lie on one side of the front"), and radial-wipe and
  iris also skipped their smoothsteps outside a soft band. An A/B from one
  binary (`PHOTOFRAME_PROBE=no-skip`, which compiled every such branch as
  taken, the same output from straight-line code), Pi 5, scale 2, 6 min
  each, 2026-10-22:

  | Kind | Main pass median / p90, branched | Straight-line |
  |---|---|---|
  | iris (main) | 6.43 / 6.75 ms | 4.89 / 5.06 ms |
  | radial-wipe | 6.36 / 7.12 ms | 5.27 / 5.68 ms |
  | wipe | 5.46 / 5.57 ms | 5.05 / 5.26 ms |
  | venetian blinds | 5.64 / 5.74 ms | 5.07 / 5.22 ms |
  | fade (control, no branch) | 4.47 ms | 4.36 ms |
  | push (control) | 4.97 ms | 4.91 ms |
  | dissolve (control) | 5.39 ms | 5.31 ms |
  | crossfade-zoom (control) | 5.80 ms | 5.67 ms |

  Straight-line, every kind in the window ran at 60 with no late frames,
  iris included (57.7 fps, 6 late, branched). At 1080p from mip 1 the
  fetches are cheap (about 16 MB a frame); the branch tests, the values
  kept live across them and the divergence within a quad cost 0.4–1.5 ms
  a frame and saved nothing. Two things that look like skipping stay,
  because they remove work rather than branch around it:
  - e-ink's reveal is decided per cell of its stripes x stripes grid (the
    row's stagger, a sine hash of the cell, the progress), so a tiny pass
    (`fs_eink_mask`, one texel per cell, timed as `gpu_layer_ms`; about
    0.02 ms) computes each cell's mask once, stored as a half float plus
    its rounding remainder, and `fs_eink` reads its cell's texel;
  - radial-wipe and iris composite with `over_background_rgb`: a mix of
    the two photos never covers more than the larger of their coverages,
    so the mix's alpha is not computed (e-ink's result is opaque by
    construction);
  - crossfade-zoom reads the unzoomed planes' coverage without fetching
    their colors, which only the zoomed planes show.

  The iris layer passes keep their early-outs (inside the aperture,
  beyond the outline's reach): they run at 240x135, where the loop over
  petals, not the branch, is the cost.
- **`PHOTOFRAME_PROBE`** (diagnostic, comma-separated; set it in the unit
  with `systemctl edit` and remove it afterwards):
  - `single-sample` compiles the transition shaders with the next photo's
    color replaced by a constant, so every kind reads one photo per pixel.
    Transitions look wrong on purpose. It measures what the second fetch
    costs on the device, with no rebuild.

  Answered and removed in rc2: `native-mip` (every kind drawn at 4K from
  mip 1, no intermediate: 16.7–23 ms a frame, worse than draw + upscale
  for every kind, Pi 2026-10-22) and `no-skip` (the A/B above).
- **Lead-in frames before a transition (KMS).** The V3D clock idles at
  500 MHz through a dwell and needs 150–200 ms of load to reach 960 MHz.
  The kernel has no devfreq knob for v3d, so this has to be handled in the
  viewer.

  For the last 300 ms of a dwell whose next photo is ready, the viewer
  renders the resting photo on every refresh. It is the same picture, so
  nothing visible changes, and the transition's first frames run at full
  clock. See `TRANSITION_LEAD_IN` and the KMS module docs.

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
measures the second option without touching the viewer. The KMS stack, the
default, is that second option for the viewer itself (see
[The viewer on KMS](#the-viewer-on-kms)).

## The 4K write floor (KMS)

Pi measurement, 2026-10-21 (`gpu_upscale_ms`): writing one 4K frame from a
fullscreen triangle with one bilinear read of the half-res intermediate
costs 10.3–10.8 ms, whatever the transition. That is 33 MB in about
10.6 ms, roughly 3 GB/s, and it is the real ceiling for every kind at
scale 2: draw plus upscale has to fit 16.7 ms.

**The swapchain images are linear.** From the sources (Mesa 26.2,
`rpi-6.12.y`):
- Mesa's `VK_KHR_display` path reads the plane's `IN_FORMATS` and offers
  those modifiers to the driver (`wsi_common_display.c`,
  `wsi_display_surface_create_swapchain`). v3dv advertises only `LINEAR`
  and `BROADCOM_UIF` (`v3dv_formats.c`) and takes UIF only when the plane
  lists it (`v3dv_image.c`).
- The Pi 5's planes list `{VC4_T_TILED, SAND128, SAND64, SAND256, LINEAR}`
  (`vc4_plane.c`); for RGB only `LINEAR` and T-tiled are accepted, and the
  BCM2712 path (`vc6_plane_mode_set`) handles only `LINEAR` and the
  YUV-only SAND layouts.
- V3D cannot render T-tiled at all (its store formats are raster,
  lineartile, UB-linear and UIF; `v3d_packet.xml`), and the GL driver also
  forces linear for scanout (`v3d_resource.c`).
- wgpu-hal passes no modifier list of its own.

So the only layout the GPU can write and the display can scan out is
linear: no tiled scanout path exists on this hardware. Whether V3D stores
a raster target more slowly than UIF is not documented in Mesa.

**Measured: the floor is the linear 4K write itself.** `frametest --drm`
reports its single pass's GPU time (timestamp queries). Pi 5, 3840x2160@60,
FIFO, 12 s per mode, 2026-10-22:

| `frametest` mode | What the pass does | GPU time median (max) |
|---|---|---|
| `solid` | one constant color per pixel, no texture read | 11.82 ms (12.2) |
| `tex` | one 4K texture read per pixel | 11.96 ms (12.7) |
| `fade` | two 4K texture reads mixed per pixel | 15.41 ms (16.4) |

A fullscreen pass that reads nothing costs 11.8 ms on the display
swapchain: 33 MB in about 12 ms, roughly 2.8 GB/s, so the store into the
linear image is the floor, and one texture read adds almost nothing on
top of it. The viewer's bilinear upscale (10.9 ms median, one read of the
1920x1080 intermediate) is already as cheap as writing a 4K frame gets
here: no shading change can lower it, and more taps only add to it (see
the cubic upscale's costs above). What remains for a transition is
its main pass, which has to fit the rest of the refresh (about 5.2 ms at
p90 at scale 2; see "The main-pass budget" above). Only the display
scaling below removes the 4K write from transition frames.

**Longer term: let the display scale.** The HVS can scale an RGB plane
(`vc4_plane.c`: any factor through the atomic plane check; a 2× upscale
uses the PPF scaler) and stacks up to 16 overlay planes by zpos. A
transition frame could then be a 1920×1080 framebuffer on a plane the HVS
scales to the mode, and the resting photo a 4K framebuffer on another
plane, which removes the 10.6 ms bilinear upscale entirely and leaves the draw
alone in the budget. Mesa's display WSI cannot do it: its atomic commit
pins the plane's source and destination to the mode size
(`wsi_common_display.c`, `SRC_W = CRTC_W = hdisplay`). It needs the
viewer's own atomic commits on dma-bufs exported from wgpu-hal images, the
framebuffers added with `drmModeAddFB2WithModifiers` and their fences
waited on, so the swapchain and its pacing become ours. Unverified: the
HVS clock and bandwidth limits for a scaled 4K plane on the Pi 5 (the
kernel enforces load limits only on the Pi 3 generation).

## Environment knobs

The variables the viewer reads (`PHOTOFRAME_SURFACE`, `PHOTOFRAME_PRESENT_MODE`,
`PHOTOFRAME_TRANSITION_SCALE`, `PHOTOFRAME_IRIS_LAYER_SCALE` (an override
of the iris transition's `layer-scale`),
`PHOTOFRAME_DRM_CONNECTOR`, `PHOTOFRAME_DRM_MODE`, `WGPU_BACKEND`), their
defaults and how to set them on each stack are listed in one place:
[advanced.md › The launcher and its environment](advanced.md#the-launcher-and-its-environment).
