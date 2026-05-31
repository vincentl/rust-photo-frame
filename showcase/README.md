# Photo frame showcase

A self-maintaining tour that cycles through **every** registered transition and
mat, with each photo labeled in the bottom-left corner so you can map what you
see to a config key.

- **Config:** [`showcase.yaml`](showcase.yaml) — `showcase.enabled: true` with a
  4-second dwell and `startup-shuffle-seed: 1` for a reproducible run.
- **How it works:** when `showcase.enabled: true`, the frame ignores any
  `transition.active` / `matting.active` in the config and builds both lists
  automatically from its internal `TransitionKind::ALL` / `MattingKind::ALL`
  arrays. Adding a new effect to the frame automatically adds it to the tour —
  no manual edits required.

## Quick start (on the Pi)

```bash
# 1. Stage your media (see "The photo set" below for what to pick)
sudo install -d -o kiosk -g kiosk -m 0755 \
  /var/lib/photoframe/showcase/photos \
  /var/lib/photoframe/showcase/backgrounds
sudo cp /path/to/your/*.jpg /var/lib/photoframe/showcase/photos/
sudo cp /path/to/backdrop.jpg /var/lib/photoframe/showcase/backgrounds/background.jpg
sudo chown -R kiosk:kiosk /var/lib/photoframe/showcase

# 2. Activate (backs up your config, swaps in showcase.yaml, restarts the kiosk)
./showcase/activate.sh
#    watch it: journalctl -t photoframe -f

# 3. Restore your normal slideshow when done
./showcase/deactivate.sh
```

`activate.sh` creates the media directories for you (owned by the `kiosk`
service user) and warns if `photos/` is empty, so you can run it first and stage
photos after — but the slideshow stays empty until photos are present.

## Media locations

| Path | Purpose |
|------|---------|
| `/var/lib/photoframe/showcase/photos/` | Slideshow photos. Must be readable by the `kiosk` service user — staging here (not under your home directory) avoids permission issues. |
| `/var/lib/photoframe/showcase/backgrounds/background.jpg` | Backdrop for the `fixed-image` mat. Kept outside `photos/` so it isn't shown as a slideshow photo. If absent, the `fixed-image` mat is simply skipped. |

## What the caption shows

Each frame displays a single line in the bottom-left, e.g.:

```
transition: crossfade-zoom    mat: passe-partout
```

When `fill-when-fits` triggers (a near-16:9 photo renders full-bleed), the mat
label reads `full-bleed`. Copy the names you like directly into your real
`config.yaml`.

## The photo set

Provide **9 photos** in `photos/`. The spread of aspect ratios is what makes the
mat showcase effective (portrait photos in a widescreen frame show the most mat).

| Count | Role | Aspect | Notes |
|---|---|---|---|
| 3 | **Fill** (full-bleed) | ≈ 16:9 (1.69–1.87) | render edge-to-edge with no mat |
| 4 | Portrait, matted | e.g. 2:3, 3:4, 4:5 | big pillarbox mats — the best mat showcase |
| 1 | Landscape, matted | ≈ 3:2 | close-ish but crops > 5%, so it shows the threshold declining a near-fit photo |
| 1 | **Panorama**, matted | extreme wide, e.g. 3:1 | best mat demo, and proves fill-when-fits declines wide photos |

Target balance: **4 landscape (3 fill + 1 matted) : 4 portrait : 1 panorama**.

For the `fixed-image` backdrop, use a 16:9 image ≥ 3840×2160 at
`backgrounds/background.jpg`.

## Turning off the caption

Set `showcase.caption: false` in `showcase.yaml` for a clean loop with no labels
(useful for screen-recordings of the effects), then re-run `./activate.sh`.

## Running locally (dev machine)

`make showcase` runs the tour against the path in `showcase.yaml`. That path
defaults to the Pi location `/var/lib/photoframe/showcase/photos`; for a local
run, copy `showcase.yaml` and point `photo-library-path` at a local folder, then
`cargo run -p photoframe -- your-local-showcase.yaml`. Note the frame needs a
Wayland/X compositor — it won't open a window from a bare SSH shell.
