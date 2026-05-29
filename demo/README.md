# Photo frame demo / guided tour

A self-contained slideshow that shows off every transition, every mat style, and
the **fill-when-fits** full-bleed behavior in a single ~22-second loop. Use it
for screenshots or a short demo video.

- **Config:** [`tutorial.yaml`](tutorial.yaml) — deterministic (`selection: sequential`
  everywhere, fixed shuffle seed), 2-second dwell.
- **Photos:** [`photos/`](photos) — the 8-image set below.
- Assumes a **16:9 display** (the Pi kiosk is hardwired to 3840×2160).

## What one lap demonstrates

| Stage | Shows |
|---|---|
| Transitions (7) | fade, fade-through-black, wipe→, wipe↓, push-from-left, push-from-top, e-ink |
| Mats (5) | black, gallery-white, blur, studio (photo-average), studio (fixed swatch) |
| fill-when-fits | near-16:9 photos render edge-to-edge with **no mat** |
| Photo effect | print-simulation at two light angles |

`fill-when-fits` runs *before* mat selection, so eligible photos render
full-bleed and don't consume a mat slot — keeping the 5 matted photos aligned
with the 5 mat styles.

## The photo set

`fill-when-fits` eligibility is purely aspect-based: a photo fills the screen
when it is within ±5% of 16:9 — i.e. **aspect ≈ 1.69–1.87** (16:9 = 1.778) — and
large enough to fill the screen within `max-upscale-factor` (2.0 here).

| File | Role | Aspect | Status |
|---|---|---|---|
| `3600x2000.jpeg` | **Fill** (full-bleed) | 1.80 | ✅ |
| `3840x2160-0.jpeg` | **Fill** (full-bleed) | 1.778 | ⚠️ source is only 1536×864 — re-export at ≥1920×1080 (ideally 3840×2160) or it upscales 2.5× and won't fill |
| *(8th photo — still needed)* | **Fill** (full-bleed) | ~1.778 | ➕ add a third crisp 16:9 landscape |
| `1365x2048.jpeg` | Portrait, matted | 2:3 | ✅ |
| `1526x2048.jpeg` | Portrait, matted | 3:4 | ✅ |
| `1638x2048.jpeg` | Portrait, matted | 4:5 (intended) | ⚠️ source is 2501×2000 = landscape 1.25 — crop to 4:5 or replace |
| `3072x2048.jpeg` | Landscape, matted | 3:2 | ✅ matted on purpose: crops ~16% > 5%, so it shows the threshold *declining* a near-fit photo |
| `3840x1200.jpeg` | **Panorama**, matted | 3:1 | ✅ extreme ratio: best mat demo, and proves fill-when-fits declines wide photos |

Target balance: **4 landscape (3 fill + 1 matted) : 3 portrait : 1 panorama**,
including exactly one extreme panorama.

## Running the tour on the Pi

The kiosk always launches `/etc/photoframe/config.yaml` (baked into the sway
config), so running the tour means temporarily swapping that file and restarting
the session. Run from the repo root after `git pull`.

```bash
# 1. Rebuild the binary so it understands the fill-when-fits key
./setup/application/deploy.sh

# 2. Stage the demo photos (service user is kiosk)
sudo mkdir -p /var/lib/photoframe/tutorial-photos
sudo cp demo/photos/* /var/lib/photoframe/tutorial-photos/
sudo chown -R kiosk:kiosk /var/lib/photoframe/tutorial-photos

# 3. Swap in the tutorial config (back up your real one first)
sudo cp /etc/photoframe/config.yaml /etc/photoframe/config.yaml.bak
sudo install -m 0644 demo/tutorial.yaml /etc/photoframe/config.yaml

# 4. Restart the kiosk — the display relaunches into the tour
sudo systemctl restart greetd
#    Watch it fire: journalctl -t photoframe -f

# 5. Restore your normal config when done
sudo cp /etc/photoframe/config.yaml.bak /etc/photoframe/config.yaml
sudo systemctl restart greetd
```

`tutorial.yaml` points `photo-library-path` at the absolute Pi path
`/var/lib/photoframe/tutorial-photos`. For local testing on a dev machine,
change it to `demo/photos`.
