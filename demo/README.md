# Photo frame demo

A self-contained slideshow that shows off every transition, every mat style, and
the **fill-when-fits** full-bleed behavior in a single ~25-second loop. Use it
for screenshots or a short demo video.

- **Config:** [`demo.yaml`](demo.yaml) — deterministic (`selection: sequential`
  everywhere, fixed shuffle seed), 2-second dwell.
- **Photos:** supply your own — drop 9 images (see the set below) into
  `demo/photos/`. That folder is gitignored, so the photos are **not** part of
  this repo; copy them to the Pi separately.
- **Backdrop:** mat #6 (`fixed-image`) needs one more image — see the note under
  the photo set.
- Assumes a **16:9 display** (the Pi kiosk is hardwired to 3840×2160).

## What one lap demonstrates

| Stage | Shows |
|---|---|
| Transitions (7) | fade, fade-through-black, wipe→, wipe↓, push-from-left, push-from-top, e-ink |
| Mats (6) | black, crimson, blur, studio (photo-average), studio (fixed swatch), fixed-image backdrop |
| fill-when-fits | near-16:9 photos render edge-to-edge with **no mat** |
| Photo effect | print-simulation at two light angles |

`fill-when-fits` runs *before* mat selection, so eligible photos render
full-bleed and don't consume a mat slot — keeping the 6 matted photos aligned
with the 6 mat styles.

## The photo set

Provide **9 photos**. `fill-when-fits` eligibility is purely aspect-based: a
photo fills the screen when it is within ±5% of 16:9 — i.e. **aspect ≈ 1.69–1.87**
(16:9 = 1.778) — and large enough to fill the screen within `max-upscale-factor`
(2.0 here), so use crisp images at/above your panel resolution.

| Count | Role | Aspect | Notes |
|---|---|---|---|
| 3 | **Fill** (full-bleed) | ≈ 16:9 (1.69–1.87) | render edge-to-edge with no mat |
| 4 | Portrait, matted | e.g. 2:3, 3:4, 4:5 | big pillarbox mats — the best mat showcase |
| 1 | Landscape, matted | ≈ 3:2 | close-ish but crops > 5%, so it shows the threshold *declining* a near-fit photo |
| 1 | **Panorama**, matted | extreme wide, e.g. 3:1 | best mat demo, and proves fill-when-fits declines wide photos |

Target balance: **4 landscape (3 fill + 1 matted) : 4 portrait : 1 panorama**,
including exactly one extreme panorama. The 6 matted photos (4 portrait + 1
landscape + 1 panorama) line up one-to-one with the 6 mat styles per lap.

**Plus one backdrop image** for the `fixed-image` mat (#6): a **16:9 landscape,
≥3840×2160**. Keep it **outside** the photo library (the library is scanned
recursively, so anything in `demo-photos/` also becomes a slideshow photo).
`demo.yaml` points the mat at `/var/lib/photoframe/backgrounds/backdrop.jpg`.

## Running the demo on the Pi

The kiosk always launches `/etc/photoframe/config.yaml` (baked into the sway
config), so running the demo means temporarily swapping that file and restarting
the session. Run from the repo root after `git pull`.

```bash
# 1. Rebuild the binary so it understands the fill-when-fits key
./setup/application/deploy.sh

# 2. Stage the demo photos (service user is kiosk)
sudo mkdir -p /var/lib/photoframe/demo-photos
sudo cp demo/photos/* /var/lib/photoframe/demo-photos/
sudo chown -R kiosk:kiosk /var/lib/photoframe/demo-photos

# 2b. Stage the fixed-image mat backdrop OUTSIDE the library (mat #6)
sudo mkdir -p /var/lib/photoframe/backgrounds
sudo cp /path/to/backdrop.jpg /var/lib/photoframe/backgrounds/backdrop.jpg
sudo chown -R kiosk:kiosk /var/lib/photoframe/backgrounds

# 3. Swap in the demo config (back up your real one first)
sudo cp /etc/photoframe/config.yaml /etc/photoframe/config.yaml.bak
sudo install -m 0644 demo/demo.yaml /etc/photoframe/config.yaml

# 4. Restart the kiosk — the display relaunches into the demo
sudo systemctl restart greetd
#    Watch it fire: journalctl -t photoframe -f

# 5. Restore your normal config when done
sudo cp /etc/photoframe/config.yaml.bak /etc/photoframe/config.yaml
sudo systemctl restart greetd
```

`demo.yaml` points `photo-library-path` at the absolute Pi path
`/var/lib/photoframe/demo-photos`. For local testing on a dev machine,
change it to `demo/photos`.
