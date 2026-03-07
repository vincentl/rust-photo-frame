# Memory Profile and Tuning

How `photoframe` uses RAM at runtime, what to expect on different Pi models, and how to tune it down when needed.

---

## Budget by Pi model

The frame keeps several decoded frames in memory simultaneously (configurable via `viewer-preload-count`). On a 3840×2160 display with `oversample: 1.0`, a single RGBA image consumes roughly 33 MiB. With the default preload count of 3, and accounting for intermediate copies during matting, steady-state memory can easily exceed 400 MiB before OS overhead.

| Pi RAM | OS + system | Available to frame | Default preload | Recommended oversample |
| --- | --- | --- | --- | --- |
| 2 GiB | ~500 MiB | ~1.5 GiB | Reduce to 1 | 0.75 |
| 4 GiB | ~500 MiB | ~3.5 GiB | 3 (default) | 1.0 |
| 8 GiB | ~500 MiB | ~7.5 GiB | 4–5 | 1.0–1.5 |

These are rough estimates. Heavy matting styles (studio, blur, fixed-image) and large backgrounds increase usage. Monitor with:

```bash
ps -o pid,rss,vsz,comm -p $(pgrep photoframe)
# or in htop: filter by 'photoframe'
```

---

## What happens when memory runs out

The Linux kernel OOM killer terminates processes to recover memory. `photoframe` is not always the first target — processes with a high `oom_score_adj` (such as `wireplumber`) may be killed first. Signs of OOM:

- Photos stop updating / slideshow freezes
- `photoframe` exits and greetd restarts the session (greeting screen reappears)
- `dmesg` shows "Out of memory: Kill process" entries

Check:

```bash
sudo dmesg | grep -i "oom\|killed" | tail -20
sudo journalctl -t photoframe -n 100 | grep -i "killed\|error"
```

---

## Pipeline: where memory goes

Each photo passes through several stages, accumulating temporary copies:

1. **Loader decode buffer** — source photo decoded to raw RGBA. The channel between loader and viewer holds `viewer-preload-count` of these simultaneously.
2. **Matting worker input** — a clone of the decoded frame sent to the CPU matting pipeline.
3. **Matting output canvas** — a full-screen RGBA canvas at display resolution × `oversample`. On a 4K display at oversample 1.0, this is ~33 MiB per frame.
4. **GPU upload staging** — a padded staging buffer aligned for WGPU row requirements, held until the GPU upload completes.
5. **Fixed-image backgrounds** — each configured background image is decoded once and cached at full canvas resolution indefinitely.

With 3 frames in flight, copies 1–4 stack up across all three, yielding 400+ MiB steady-state on 4K.

---

## Mitigation levers

Apply these in order — each one has diminishing returns so start with the highest-impact options:

**1. Reduce `viewer-preload-count`** (highest impact)

```yaml
viewer-preload-count: 1   # default: 3
```

Cutting from 3 to 1 roughly trims 200 MiB on a 4K display. Values of 1–2 still hide most decode latency on fast SD cards.

**2. Dial back `oversample`**

```yaml
global-photo-settings:
  oversample: 0.75   # default: 1.0
```

Reduces every matting canvas and GPU texture by 44% (0.75² = 0.56). Start here if you want to keep preload count at 2.

**3. Cull large backgrounds** (if using `fixed-image` matting)

Each background image is cached forever at the full canvas resolution. A list of five 4K backgrounds adds ~165 MiB permanently. Keep the list short or pre-scale backgrounds to near-screen resolution before deploying.

**4. Switch to lighter matting styles**

The `fixed-color` mat style requires no intermediate copies. `blur` and `studio` are heavier. Disabling matting entirely (`active: []`) is the most aggressive option.

**5. Constrain source photo resolution**

Very large source photos (e.g. 50 MP RAW exports) create oversized decode buffers. Pre-scale photos to the display resolution before adding them to the library.

---

## Profiling

Watch RSS (resident set size) while changing settings:

```bash
# Snapshot every 2 seconds
watch -n 2 'ps -o pid,rss,vsz,comm -p $(pgrep photoframe)'
```

Or in `htop`: press `F4` to filter, type `photoframe`.

The footprint stabilizes after the first few photo transitions. Take a steady-state reading with the frame awake and cycling, then compare before and after each tuning change.
