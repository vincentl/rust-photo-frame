# Photo frame showcase

A self-maintaining tour that cycles through **every** registered transition and
mat, with each photo labeled in the bottom-left corner so you can map what you
see to a config key.

- **Config:** [`showcase.yaml`](showcase.yaml) — `showcase.enabled: true` with a
  4-second dwell and `startup-shuffle-seed: 1` for a reproducible run.
- **Photos:** supply your own — drop 9 images (see below) into `demo/photos/`.
  That folder is gitignored, so the photos are **not** part of this repo.
- **How it works:** when `showcase.enabled: true`, the frame ignores any
  `transition.active` / `matting.active` in the config and builds both lists
  automatically from its internal `TransitionKind::ALL` / `MattingKind::ALL`
  arrays. Adding a new effect to the frame automatically adds it to the tour —
  no manual edits required.

## What the caption shows

Each frame displays a single line in the bottom-left, e.g.:

```
transition: iris    mat: passe-partout
```

When `fill-when-fits` triggers (a near-16:9 photo renders full-bleed), the mat
label reads `full-bleed`. Copy the names you like directly into your real
`config.yaml`.

## The photo set

Provide **9 photos**. The spread of aspect ratios is what makes the mat showcase
effective (portrait photos in a widescreen frame show the most mat).

| Count | Role | Aspect | Notes |
|---|---|---|---|
| 3 | **Fill** (full-bleed) | ≈ 16:9 (1.69–1.87) | render edge-to-edge with no mat |
| 4 | Portrait, matted | e.g. 2:3, 3:4, 4:5 | big pillarbox mats — the best mat showcase |
| 1 | Landscape, matted | ≈ 3:2 | close-ish but crops > 5%, so it shows the threshold declining a near-fit photo |
| 1 | **Panorama**, matted | extreme wide, e.g. 3:1 | best mat demo, and proves fill-when-fits declines wide photos |

Target balance: **4 landscape (3 fill + 1 matted) : 4 portrait : 1 panorama**.

**Optional: backdrop image for the `fixed-image` mat.** Uncomment
`fixed-image-path` in `showcase.yaml` and point it at a 16:9 image ≥ 3840×2160.
Keep it **outside** the photo library (e.g. `/var/lib/photoframe/backgrounds/`)
so it is not picked up as a slideshow photo.

## Running locally

```bash
# Drop your photos into demo/photos/, then:
cargo run -p photoframe -- demo/showcase.yaml
# Or use the Makefile shortcut:
make showcase
```

Edit `photo-library-path` in `showcase.yaml` to point at `demo/photos` for
local runs (it defaults to the absolute Pi path).

## Running on the Pi

The kiosk always launches `/etc/photoframe/config.yaml`, so running the
showcase means temporarily swapping that file and restarting the session.
Run from the repo root after `git pull`:

```bash
# 1. Rebuild the binary (if needed)
./setup/application/deploy.sh

# 2. Stage the photos
sudo mkdir -p /var/lib/photoframe/demo-photos
sudo cp demo/photos/* /var/lib/photoframe/demo-photos/
sudo chown -R kiosk:kiosk /var/lib/photoframe/demo-photos

# 3. Optional: stage a fixed-image backdrop
sudo mkdir -p /var/lib/photoframe/backgrounds
sudo cp /path/to/backdrop.jpg /var/lib/photoframe/backgrounds/backdrop.jpg
sudo chown -R kiosk:kiosk /var/lib/photoframe/backgrounds

# 4. Swap in the showcase config (back up your real one first)
sudo cp /etc/photoframe/config.yaml /etc/photoframe/config.yaml.bak
sudo install -m 0644 demo/showcase.yaml /etc/photoframe/config.yaml

# 5. Restart the kiosk
sudo systemctl restart greetd
#    Watch it: journalctl -t photoframe -f

# 6. Restore your normal config when done
sudo cp /etc/photoframe/config.yaml.bak /etc/photoframe/config.yaml
sudo systemctl restart greetd
```

## Turning off the caption

Set `showcase.caption: false` in `showcase.yaml` for a clean loop with no
labels (useful for screen-recordings of the effects).
