# Configure

The active configuration on an installed system is `/etc/photoframe/config.yaml` (edit with `sudo`). When running from source, pass the config path on the CLI.

After editing, restart the kiosk to apply:

```bash
sudo systemctl restart photoframe-kms.service
```

(On the Wayland fallback stack, restart greetd instead: `sudo systemctl stop greetd.service && sleep 1 && sudo systemctl start greetd.service`. Never `restart` it.)

> Always edit `/etc/photoframe/config.yaml`, not the template at `/opt/photoframe/etc/photoframe/config.yaml` — the template gets overwritten on redeploy.

The visual feature blocks (`transition`, `matting`, `photo-effect`) all use a shared `selection` + `active` structure described below.

## Quick start

For most edits, this is the sequence:

1. Set `photo-library-path` to your actual photo root.
2. Tune slide pacing with `global-photo-settings.dwell-ms`.
3. Pick one transition preset in `transition.active`.
4. Pick one mat preset in `matting.active`.
5. Validate without launching the viewer:

   ```bash
   cargo run -p photoframe -- --playlist-dry-run 1
   ```

## Choosing a starting config

You don't have to start from scratch. The repo ships several starting points
(details and a comparison table in [examples/README.md](../examples/README.md)):

- [`config.yaml`](../config.yaml) — the rich, fully-commented default the installer
  seeds; every transition and mat with varied colors and angles. Most people start here.
- [`examples/minimal.yaml`](../examples/minimal.yaml) — the smallest valid file; build up
  from defaults.
- [`examples/use-showcase.yaml`](../examples/use-showcase.yaml) — a labeled tour of every
  transition and mat (see [showcase/README.md](../showcase/README.md)).
- [`examples/everything.yaml`](../examples/everything.yaml) — every kind *and* every
  parameter variant, to trim down.

Copy one over `/etc/photoframe/config.yaml` (or crib blocks from it) and restart the kiosk.

## Starter configuration

```yaml
photo-library-path: /var/lib/photoframe/photos
# ├── cloud/  # managed by sync jobs; safe to resync or replace wholesale
# └── local/  # manual drops (USB, scp) that should survive sync resets

# Render/transition settings
transition:
  selection: random # fixed, random, or sequential
  active:
    - kind: fade
      duration-ms: 400
global-photo-settings:
  dwell-ms: 12000 # Time an image remains fully visible (ms)
  oversample: 1.0 # GPU render oversample vs. screen size
  max-upscale-factor: 1.0 # Limit for enlarging small images
viewer-preload-count: 3 # Images the viewer preloads; also sets viewer channel capacity
loader-max-concurrent-decodes: 4 # Concurrent decodes in the loader
startup-shuffle-seed: null # Optional deterministic seed for initial shuffle

photo-effect:
  active: [] # Optional effects; add entries (e.g., print-simulation) to enable

playlist:
  new-multiplicity: 3 # How many copies of a brand-new photo to schedule per cycle
  half-life: 3 days # How quickly that multiplicity decays back toward 1

matting:
  selection: random
  active:
    - kind: fixed-color
      minimum-mat-percentage: 0.0 # % of each screen edge reserved for the mat border
      colors: [[0, 0, 0], [32, 32, 32]]
    - kind: blur
      minimum-mat-percentage: 3.5
      sigma: 32.0
      sample-scale: 0.125
      backend: neon  # arm64 vector path; falls back to cpu automatically
```

If the frame launches to a black screen, check that `photo-library-path` points to a directory the runtime can read and that the kiosk account has access. The directory should contain `cloud/` and `local/` subdirectories — the runtime merges both. Validate a YAML edit quickly with `cargo run -p photoframe -- --playlist-dry-run 1`, which parses the config without opening the render window.

## Top-level keys

| Role                    | Keys                                                                                       |
| ----------------------- | ------------------------------------------------------------------------------------------ |
| **Required**            | `photo-library-path`                                                                       |
| **Schema**              | `config-version`                                                                           |
| **Core timing**         | `transition`, `global-photo-settings`, `playlist`                                          |
| **Performance tuning**  | `viewer-preload-count`, `loader-max-concurrent-decodes`, `global-photo-settings.oversample` |
| **Deterministic runs**  | `startup-shuffle-seed`                                                                     |
| **Presentation**        | `photo-effect`, `matting`                                                                  |
| **Greeting / Sleep**    | `greeting-screen`, `sleep-screen`                                                          |
| **Runtime control**     | `control-socket-path`                                                                      |
| **External scheduling** | `awake-schedule` (consumed by `buttond`)                                                   |
| **Power button daemon** | `buttond`                                                                                  |
| **Showcase / preview**  | `showcase`                                                                                 |

## Key reference

### `config-version`

- **Purpose:** Declares which config-schema version the file targets, so a config written for a newer, incompatible schema fails fast with a clear message instead of a confusing per-key error.
- **Required?** Optional; defaults to the current supported version (`1`).
- **Accepted values & defaults:** A positive integer. This build supports `1`. A value higher than the build supports is rejected at startup with a message telling you to update photoframe.
- **Notes:** Leave at `1` unless a release note instructs otherwise. The version only bumps on a breaking config-schema change, which will be documented in the git history / release notes.

### `photo-library-path`

- **Purpose:** Sets the root directory that will be scanned recursively for supported photo formats.
- **Required?** Yes.
- **Accepted values & defaults:** Any absolute or relative filesystem path. The setup pipeline provisions `/var/lib/photoframe/photos` with `cloud/` and `local/` subdirectories.
- **Effect on behavior:** Switching the path changes the library the watcher monitors; the viewer reloads the playlist when the directory contents change.
- **Notes:** Keep the `cloud/` and `local/` folders under the configured root. Use `cloud/` for sync-managed content (rclone, Nextcloud) and `local/` for manual imports the sync should never prune.

### `control-socket-path`

- **Purpose:** Selects where the application exposes its Unix domain control socket.
- **Required?** Optional; defaults to `/run/photoframe/control.sock`.
- **Accepted values & defaults:** Any filesystem path, typically under `/run`, `/run/user/<uid>`, or another writable runtime directory.
- **Notes:** The kiosk provisioning script creates `/run/photoframe` (mode `0770`, owned by `kiosk:kiosk`) and installs a tmpfiles entry so the directory exists after every boot. If you override the setting, pre-create the directory with matching ownership: `sudo install -d -m 0770 -o kiosk -g kiosk /run/photoframe`. wifi-manager (`overlay.control-socket`) and `powerctl` (`PHOTOFRAME_CONTROL_SOCKET`) default to the same path; change them together.
- **Commands:** wake/sleep, display power on the KMS stack, and the Wi-Fi recovery overlay. See [Advanced › Control socket protocol](advanced.md#control-socket-protocol).

### `transition`

- **Purpose:** Controls how the viewer blends between photos.
- **Required?** Optional; when omitted the frame uses a 400 ms fade.
- **Accepted values & defaults:** Mapping documented in [Transition configuration](#transition-configuration).
- **Effect on behavior:** Adjust duration, direction, randomness, or transition family — from subtle fades to bold pushes or e‑ink reveals.

### `global-photo-settings`

- **Purpose:** Core photo timing and sizing parameters.
- **Required?** Optional; sensible defaults apply when omitted.
- **Keys and defaults:**
  - `dwell-ms` (u64, default `2000`): How long to show the current photo before transitioning.
  - `oversample` (float, default `1.0`): GPU render oversample relative to screen size. Must be positive.
  - `max-upscale-factor` (float, default `1.0`): Maximum enlargement applied when fitting small photos inside the mat.

### `viewer-preload-count`

- **Purpose:** Number of decoded images the viewer keeps queued ahead of the current slide.
- **Required?** Optional. Default `3`.
- **Effect on behavior:** Higher counts buffer more content, smoothing playback on slower storage but increasing memory usage. See [Advanced › Memory tuning](advanced.md#memory-tuning) for sizing guidance.

### `loader-max-concurrent-decodes`

- **Purpose:** Limits how many images the CPU decoding task processes simultaneously.
- **Required?** Optional. Default `4`. Minimum `1`.

### `startup-shuffle-seed`

- **Purpose:** Seeds the initial RNG used when shuffling the first playlist.
- **Required?** Optional. `null` (default) draws entropy from the system RNG.
- **Effect:** Providing a seed freezes the opening order — useful for demos, debugging, or deterministic tests.

### `playlist`

- **Purpose:** Tunes how the weighting system surfaces new photos and how evenly the slideshow spreads them.
- **Required?** Optional.
- **Defaults:** three copies for new images, one-day half-life, `weighted-spread` order at `min-spacing` 0.7, portrait pairing off.

See [Playlist weighting](#playlist-weighting) for the algorithm and [Portrait pairing](#portrait-pairing) for `playlist.pairing`.

### `photo-effect`

- **Type:** mapping (see [Photo effect configuration](#photo-effect-configuration))
- **Default:** disabled (`active: []`)
- **What it does:** Inserts an optional post-processing stage between the loader and viewer. The built-in `print-simulation` effect relights each frame with directional shading and paper sheen. Add it to `active` to enable; leave the list empty to keep the stage off.

### `greeting-screen`

- **Purpose:** Styles the GPU-rendered welcome card displayed while the library is warming up.
- **Required?** Optional.
- **Keys:**
  - `message` (string, default `Initializing…`)
  - `font` (string font name; falls back to bundled face)
  - `stroke-width` (float DIP, default `16.0`)
  - `corner-radius` (float DIP, default `0.75 × stroke-width`)
  - `duration-seconds` (float ≥ 0, default `4.0`)
  - `colors.background`, `colors.font`, `colors.accent` (hex sRGB strings)
- **Effect:** The renderer fits and centers the message inside a rounded double-line frame. `duration-seconds` guarantees the greeting remains on screen for at least that many seconds before the first photo appears.
- **Notes:** Colors accept `#rgb`, `#rgba`, `#rrggbb`, or `#rrggbbaa`. Low-contrast combinations log a warning.

### `sleep-screen`

- **Purpose:** Styles the card shown as the frame transitions into sleep.
- **Keys:** Mirror `greeting-screen` aside from `duration-seconds`, which does not apply when sleeping.
- **Effect:** Shares the same renderer as the greeting card, so sizing rules and readability checks are identical.

### Wake/sleep control

- **How it works:** The application has no internal schedule. After startup it remains asleep until another client sends `set-state` or `toggle-state` commands over the control socket.
- **Manual control:** Pipe JSON such as `{"command":"set-state","state":"awake"}` or `{"command":"toggle-state"}` to `/run/photoframe/control.sock` (override via `control-socket-path`). Each command answers with one JSON line; every command is listed in [Advanced › Control socket protocol](advanced.md#control-socket-protocol).
- **Automation:** Deploy `buttond` (see below) and populate the shared `awake-schedule` block. `buttond` evaluates the schedule, issues `set-state` commands at the appropriate boundaries, and runs display power hooks on your behalf.

Example schedule fragment consumed by `buttond`:

```yaml
awake-schedule:
  timezone: America/New_York
  awake-scheduled:
    daily:
      - ["07:30", "22:00"]
    weekend:
      - ["09:00", "23:00"]
```

`awake-schedule` supports wrap-past-midnight windows, weekday/weekend overrides, and per-day exceptions. Times use `HH:MM` or `HH:MM:SS`. To wrap past midnight, give a range whose start is **later** than its end — e.g. `["21:00", "07:00"]` keeps the frame awake from 9 PM until 7 AM the next morning. An empty list for a day key (e.g. `friday: []`) means **sleep all day on that day** — remove the key to fall back to the `daily` window.

### `buttond` (power button daemon)

`buttond` watches the Pi 5 power-pad button via evdev and orchestrates scheduled wake/sleep transitions. It also drives DPMS commands so the panel actually powers down between schedule windows.

```yaml
buttond:
  device: null                      # optional explicit evdev path
  debounce-ms: 20                   # ignore chatter within this window
  single-window-ms: 250             # treat releases shorter than this as taps
  double-window-ms: 400             # wait this long for a second tap
  force-shutdown: true              # for systemctl: add -i and --no-ask-password
  shutdown-command:
    program: /usr/bin/systemctl
    args: [poweroff]
  screen:
    off-delay-ms: 3500
    display-name: HDMI-A-2          # connector name (the viewer's `connector`); null = auto-detect
    on-command:
      program: /opt/photoframe/bin/powerctl
      args: [wake]
    off-command:
      program: /opt/photoframe/bin/powerctl
      args: [sleep]
```

Pair the block with a top-level `awake-schedule` to describe the desired wake windows.

**`buttond.screen.display-name` discovery.** Ask the viewer which connector it drives; the answer's `connector` is the name:

```bash
echo '{"command":"display-power"}' | sudo -u kiosk socat -t 3 - UNIX-CONNECT:/run/photoframe/control.sock
```

On the Wayland fallback the viewer cannot answer (`unsupported`); query the kiosk Sway session instead. The display is usually `wayland-1`; find it with `sudo ls /run/user/$(id -u kiosk)/wayland-*`:

```bash
sudo -u kiosk env XDG_RUNTIME_DIR=/run/user/$(id -u kiosk) WAYLAND_DISPLAY=wayland-1 wlr-randr
```

Common values: `HDMI-A-1`, `HDMI-A-2`. Setting `display-name` explicitly avoids auto-detect ambiguity on systems with multiple connectors.

**`force-shutdown`** controls whether `buttond` augments a systemctl command with `-i` (ignore inhibitors) and `--no-ask-password`. The default `true` makes `systemctl poweroff -i --no-ask-password` succeed without prompts. If you point `shutdown-command.program` at something other than `systemctl`, `buttond` strips those flags automatically.

**Runtime behavior:**

- **Single press:** resolves the current screen state and sends the appropriate `set-state` command to the control socket, then toggles the screen. If the display was off it immediately runs the wake command; if on, it delays for `off-delay-ms` (so the sleep card renders) before running the sleep command. The daemon asks `powerctl state` on each press (the viewer on KMS, sway on the Wayland fallback), so restarts and manual overrides stay in sync.
- **Double press:** executes `shutdown-command`. Polkit allows `kiosk` to issue the request without prompting.
- **Long press:** bypassed so Pi 5 firmware can force power-off.
- **Scheduled transitions:** when `awake-schedule` is present, `buttond` waits for the greeting delay, applies the schedule's current state, then drives transitions using `set-state`.
- **Manual override:** a single press overrides the schedule until the next scheduled wake/sleep boundary, then the frame resumes following the schedule automatically. Press again to undo immediately. For example, pressing to sleep during a wake window keeps the frame asleep until that window ends; pressing to wake during a sleep window keeps it awake until the next scheduled wake.

On the Wayland fallback `buttond` auto-derives `XDG_RUNTIME_DIR` and `WAYLAND_DISPLAY` for its sway probes; on KMS there is no Wayland session and it talks to the viewer's control socket instead. Auto-detection scans `/dev/input/by-path/*power*` before falling back to `/dev/input/event*`. Set `buttond.device` if the wrong input is chosen. Provisioning pins `HandlePowerKey=ignore` in `/etc/systemd/logind.conf` so logind doesn't interpret presses as shutdown requests; only `buttond` reacts.

### `matting`

- **Purpose:** Chooses the mat/background style rendered behind every photo.
- **Required?** Optional.
- **Defaults:** `{ selection: fixed, active: [{ kind: fixed-color }] }` with a black swatch.

See [Matting configuration](#matting-configuration) for the full reference.

### `showcase`

- **Purpose:** Turns the slideshow into a labeled, deterministic tour of every registered transition and mat. On-screen captions identify each effect so you can copy the name directly into your real config.
- **Required?** Optional; defaults to disabled.
- **Keys:**

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `enabled` | `bool` | `false` | Activates showcase mode. Overrides `transition.active` and `matting.active` with auto-enumerated sequential lists. |
| `caption` | `bool` | `true` | Show a text label (`transition: X    mat: Y`) in the bottom-left corner of each frame; a pair screen adds ` · pair`. |
| `dwell-ms` | `u64` | `4000` | Milliseconds each photo is held; overrides `global-photo-settings.dwell-ms` when showcase is on. |
| `fixed-image-path` | path | — | Opt-in: include the `fixed-image` mat in the tour using this file as the backdrop. Omitting the path silently skips `fixed-image`. |

When `showcase.enabled` is true, `startup-shuffle-seed` defaults to `1` if unset, and `transition.active`/`matting.active` in the same file are ignored.

See [showcase/README.md](../showcase/README.md) and [Showcase mode](#showcase-mode) for usage.

## Showcase mode

Showcase mode is the fastest way to choose transitions and mats. It auto-enumerates every built-in effect, displays each one with a live caption, and advances to the next after a configurable dwell.

**Workflow (on the Pi):**

1. Stage media under `/var/lib/photoframe/showcase/photos/` (and optionally a `backgrounds/background.jpg` backdrop). See [showcase/README.md](../showcase/README.md).
2. Run `./showcase/activate.sh` — it backs up your live config, swaps in `showcase/showcase.yaml`, and restarts the kiosk into the tour.
3. Watch the captions. When you see an effect you like, the caption tells you the exact `kind` key.
4. Copy the kind(s) into your real `config.yaml` under `transition.active` and/or `matting.active`.
5. Run `./showcase/deactivate.sh` to restore your normal slideshow.

**Self-maintaining:** the tour is built at startup from `TransitionKind::ALL` and `MattingKind::ALL`. When future effects are added to the frame, they automatically appear in the tour without any edits to `showcase.yaml`.

**Interactive control** (e.g. advancing manually via a socket command) is not implemented — the tour advances on a timed dwell. This fits the frame's passive, keyboard-less Pi kiosk model; a `showcase-next` control command may be added in the future.

## Playlist weighting

Each photo is scheduled on a virtual timeline. A photo's **weight** sets how often it
appears: `weight(age) = max(1, new_multiplicity × 0.5^(age / half_life))`. Brand-new
photos peak at `new-multiplicity` and decay by half every `half-life` toward the
equilibrium weight of 1. After each showing a photo is rescheduled a random gap ahead
whose average length is `1 / weight`, so higher-weight photos recur sooner while still
being spaced apart (no bursts, no back-to-back repeats). Adding or removing photos
updates the schedule incrementally without resetting progress.

**Scheduling `order`** selects how each photo's showings are *spaced* around that
`1 / weight` average, without changing the average itself (so weighting is identical
across orders):

- `weighted-spread` (default) draws a **refractory (minimum-wait) gap**: a deterministic
  floor that no showing may fall below, plus exponential jitter above it. The
  `min-spacing` knob (`[0.0, 1.0)`) sets the floor as a fraction of one library "lap" — a
  photo's mean recurrence interval, which equals the library size in displays. So
  `min-spacing = 0.5` means *"a photo cannot return until at least half the library has
  played."* `0.7` (the shipped value) is a good balance of even coverage and
  unpredictability.
- `weighted-random` draws a plain **exponential** gap — memoryless, which makes it
  statistically equivalent to i.i.d. uniform-with-replacement. Simple, but high gap
  variance: some photos clump together while others go unseen for a long time. Identical
  to `weighted-spread` with `min-spacing: 0.0`; opt in only if you want raw randomness.

The gap law is `gap = f/weight + (1 − f)/weight · Exp(1)` with `f = min-spacing`. Its
mean is `1/weight` for every `f`, so **a photo's long-run show frequency never
changes** — only its spacing does (coefficient of variation `= 1 − f`). The floor also
auto-scales with weight, so a high-weight new photo is still allowed back sooner.

**Prompt debut for new photos.** The minimum-spacing floor would otherwise delay a
freshly copied-in photo's *first* appearance by a full `min-spacing` lap (tens of
minutes on a large library). To avoid that, photos discovered while the frame is
running enter a small priority queue and debut ahead of the timeline, chosen by a
biased coin: the new photo wins against the normal rotation at odds equal to its weight
(`w : 1`). So a fresh photo (weight ≈ `new-multiplicity`) appears within a few slides,
while the rest of the rotation still shows ~`1/(w+1)` of the time — no stall. After its
debut the photo joins the spaced timeline normally. This applies only to runtime
additions; the startup scan and `--playlist-dry-run` schedule straight onto the
timeline, so simulation stays deterministic.

Turn on `metrics: true` (see [Performance → Measuring](performance.md#measuring)) and
grep `photo_display_metric` to measure the effect — the `gap` column is the number of
displays between successive showings of a photo. Under `weighted-spread` it stays around
or above `min-spacing × inventory` (the floor is exact in the scheduler's virtual clock;
display counts vary slightly), and its variance shrinks as `min-spacing` rises.

`age` is the difference between the current time and the photo's filesystem creation timestamp — i.e. **when the file was staged to the frame, not its EXIF capture date**. The frame prefers the filesystem birth time (`st_birthtime`), falling back to mtime and then to "now" only if birth time is unavailable; EXIF is never consulted. So copying photos onto the frame makes them "new" regardless of when they were taken. With `metrics: true`, each `photo_display_metric` line reports `age_days` and `created_source` (`birthtime`/`mtime`/`now`) so you can confirm photos are aged by birth time and not silently falling back to mtime — which a timestamp-preserving copy (`cp -p`, `rsync -t`/`-a`) could otherwise reintroduce. The clock defaults to `SystemTime::now()` but can be frozen via `--playlist-now <RFC3339>`.

### Testing the weighting

```bash
cargo run -p photoframe --release -- \
  config.yaml \
  --playlist-now 2025-01-01T00:00:00Z \
  --playlist-dry-run 32 \
  --playlist-seed 1234
```

Prints the **weight** (relative show frequency; equilibrium = 1.0) for each discovered photo and the first 32 scheduled entries. Run with the same seed twice to confirm deterministic output. With [portrait pairing](#portrait-pairing) enabled it prints screens instead (`--canvas WxH` sets the canvas pairability is judged on).

### Playlist knobs

| Field              | Required? | Default | Accepted values                                                                | Effect on the slideshow                                                                                     |
| ------------------ | --------- | ------- | ------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------- |
| `new-multiplicity` | Optional  | `3`               | Integer ≥ 1                                                                 | Sets the peak weight for a brand-new photo; higher values surface newcomers more often before they decay.   |
| `half-life`        | Optional  | `1 day`           | Positive duration string parsed by [`humantime`](https://docs.rs/humantime) | Controls how quickly the weight decays back to equilibrium; shorter half-lives return to normal faster.     |
| `order`            | Optional  | `weighted-spread` | `weighted-spread`, `weighted-random`                                        | Scheduling algorithm. `weighted-spread` (default) adds a minimum-wait floor before a photo can recur; `weighted-random` is plain exponential-gap (i.i.d. random). Weighting (average frequency) is identical for both. |
| `min-spacing`      | Optional  | `0.7`             | Float in `[0.0, 1.0)`                                                       | For `weighted-spread` only: minimum wait before a photo repeats, as a fraction of one library lap. `0.0` equals `weighted-random`; `0.5` waits half the library. Preserves show frequency; only spacing changes. Ignored by `weighted-random`. |
| `pairing`          | Optional  | off               | Mapping; see [Portrait pairing](#portrait-pairing)                          | Shows two back-to-back portraits side by side on one screen. |

## Portrait pairing

On a landscape panel a portrait photo uses less than half the screen. With
`playlist.pairing.enabled: true`, **when the playlist's next two photos are both
pairable portraits, they share one screen**, side by side in one mat. Pairing is off by
default.

```yaml
playlist:
  pairing:
    enabled: true
    probability: 1.0        # chance that two back-to-back pairable portraits share a screen
    bar-ratio: 0.618        # center bar = this × the mat border (1/φ)
    dwell-multiplier: 1.5   # a pair stays up dwell-ms × this
```

**The playlist is unchanged.** Pairing never reorders, skips or adds a showing. It only
groups two photos that were going to be shown one after the other anyway. So everything
in [Playlist weighting](#playlist-weighting) carries over exactly: each photo's long-run
share, the `min-spacing` floor (counted in photos shown), the spread, the prompt debut of
new photos. The dry run checks the paired sequence against the unpaired one photo for
photo. With pairing on, `photo_display_metric` still logs one line per photo shown, so
`seq` and `gap` mean what they always meant.

**No photo on two screens in a row.** Two back-to-back portraits are not paired when the
second one was on the previous screen, or when the photo after the second is the first
one again (in A, B, A, pairing A with B would put A on two screens in a row). With these
two rules a photo appears on consecutive screens only where the unpaired playlist itself
shows it twice in a row, which only a just-added photo's debut can do.

**Which portraits pair.** A photo is *pairable* when both hold:

- **Shape:** it is narrow enough for two to fit side by side with the center bar:
  `width / height ≤ (A − g_max) / 2`, where `A` is the canvas aspect (width / height) and
  `g_max` is the widest bar any of your mats produces, as a fraction of photo height
  (`bar-ratio × m / (1 − 2m)` for the largest non-zero `minimum-mat-percentage` `m`). On
  16:9 with the default bar ratio and a 6% mat the limit is 0.868: 4:5, 3:4, 2:3 and 9:16
  portraits pair, squares never do. On 16:10 it is 0.779, which leaves out 4:5. Studio
  and passe-partout mats cut a bevel around each photo, and the bar holds both of them,
  so with those mats the limit is lower by `2 × bevel-width-px / (canvas height × (1 − 2m))`
  (about 0.004 for a 4 px bevel on 4K).
- **Full height:** it is tall enough to fill the mat's height on its own:
  `height × max-upscale-factor ≥ canvas height × (1 − 2 × m_min)`, with `m_min` the
  smallest non-zero `minimum-mat-percentage`. On a 4K panel at `max-upscale-factor: 1.0`
  and a 2% mat that is 2074 px: phone and camera photos qualify, while 1080×1350 and
  1200×1600 exports always show alone.

Together the two conditions guarantee that **each photo of a pair is drawn exactly as
tall as it would be on its own**, for every mat a pair can get. Sizes come from each
file's header (read once at startup, and as files arrive, only while pairing is enabled)
and are corrected by every decode. Nothing pairs until the viewer has reported its canvas.

**How often.** With `π` the share of showings that are pairable portraits and `p` =
`probability`, about `2πp / (1 + πp)` of the pairable showings are paired: 57% for a
library that is 40% portraits, at `p = 1`. That formula treats the playlist as
independent draws, so the dry run and the analyzer print the measured share beside it; in
small libraries the two rules above refuse a few pairs and the share is slightly lower.
`probability` lowers the share without touching the order; at `0` the screens, and when
each is sent to the loader, are exactly those of pairing off (headers are still read,
the non-zero-mat rule still applies and the metric lines still carry the pairing fields).

**One mat, two windows.** The two photos share one mat. They are drawn at the same
height and never cropped, left to right in playlist order, with a center bar of
`bar-ratio` × the mat border between them (border = canvas height ×
`minimum-mat-percentage`; the default 1/φ ≈ 0.618 echoes a framer's bridge between two
openings, and gives an 80 px bar for a 6% mat on 4K). Blurred mats are drawn from the two
photos side by side, studio and passe-partout bevels frame each window, and a drop shadow
falls under both. On studio and passe-partout mats the bar also holds the two bevels, so
the flat board between them is `bar-ratio` × the border, like the flat board outside.
`bar-ratio: 0` puts the two photos edge to edge (on studio and passe-partout, bevel to
bevel). `fill-when-fits` never applies to a pair. If the canvas changes after a pair was
planned (a new display mode) and the two no longer qualify on it, they are shown one
after the other instead. Because the bar is measured from the border, pairing needs mats
that have one:

- With pairing enabled, at least one matting option must have `minimum-mat-percentage`
  above 0; otherwise the configuration is rejected.
- A pair screen picks its mat only among the non-zero options. Random selection keeps
  their relative weights; sequential selection takes the next non-zero entry, and the
  zero-margin entries it skips count as used. Solo screens use every option as before.
- So zero-margin mats are shown less often: under random selection they lose the share
  of screens that are pairs (about 13% of screens for a library that is 40% portraits);
  under sequential selection they lose every turn that lands on a pair screen.

**Dwell.** A pair stays up `dwell-ms × dwell-multiplier`; a single photo `dwell-ms`. Each
paired photo therefore gets `dwell-multiplier / 2` of a single photo's time: the default
1.5 gives a pair longer than one photo but less than two. A multiplier of
`2 + transition / dwell-ms` keeps the wall-clock time per photo as without pairing. Any
smaller value shortens the trip through the library, so every photo comes round
proportionally more often per hour; the shares stay the same.

| Field              | Default | Accepted values | Effect |
| ------------------ | ------- | --------------- | ------ |
| `enabled`          | `false` | `true`, `false` | Opt-in switch. Requires a matting option with `minimum-mat-percentage` > 0. |
| `probability`      | `1.0`   | `[0.0, 1.0]`    | Chance that two back-to-back pairable portraits share a screen. Never changes the playlist order. |
| `bar-ratio`        | `0.618` | `[0.0, 1.0]`    | Center bar between the two photos, as a fraction of the mat border (studio and passe-partout add their two bevels). `0` puts the photos edge to edge. |
| `dwell-multiplier` | `1.5`   | `[0.5, 4.0]`    | A pair screen stays up `dwell-ms ×` this. |

**Checking it.** With pairing enabled, `--playlist-dry-run N` reads every header, plans
`N` photos as screens (pairs print as `a.jpg + b.jpg`), and ends with a `# pairing` block:
pairable photos, `π`, screens vs photos, the measured paired share beside
`2πp / (1 + πp)`, how often a photo meets the same partner again, and photos on
consecutive screens (0 in the dry run, except where the unpaired playlist itself repeats
a photo). Pass `--canvas WxH` for a panel other than 4K; the default is 3840x2160
times `oversample`. On the frame, `metrics: true` adds the pairing fields to
`photo_display_metric` (see [Performance → Measuring](performance.md#measuring)) and
`developer/analyze-playlist-metrics.py` reports them in its `# PAIRING` section.

## Photo-effect configuration

The optional `photo-effect` task sits between the loader and the viewer. When enabled it reconstructs the decoded RGBA pixels, applies any configured effects, and forwards the modified image downstream. Leave `photo-effect.active` empty (or omit the block) to short-circuit the stage. Duplicate entries to weight the random picker or alternate presets sequentially.

### Scheduling effects

| Key         | Required? | Default                                                       | Accepted values                | Effect |
| ----------- | --------- | ------------------------------------------------------------- | ------------------------------ | ------ |
| `selection` | Optional  | `fixed` when `active` has one entry, otherwise `random`       | `fixed`, `random`, `sequential` | Controls how the viewer iterates through `active`. `fixed` locks to the first entry, `random` chooses independently per slide, `sequential` advances in order and loops. |
| `active`    | Yes       | —                                                             | Array of effect entry maps     | Declares the effect variants that are eligible. Repeat entries to weight the random picker or alternate presets in sequential mode. |

### Print-simulation effect

`print-simulation` mimics how a framed print interacts with gallery lighting. It derives a shallow height-field from local luminance gradients, shades that relief with a configurable key light, and layers in ink compression plus paper sheen so highlights glow like coated stock.

- `light-angle-degrees` (float, default `135.0`): direction of the simulated gallery lighting in degrees clockwise from +X.
- `relief-strength` (float ≥ 0, default `0.35`): scale factor on the derived height-field.
- `ink-spread` (float ≥ 0, default `0.18`): tone compression coefficient that emulates dye absorption.
- `sheen-strength` (float ≥ 0, default `0.22`): how strongly paper sheen blends into highlights.
- `paper-color` (RGB array, default `[245, 244, 240]`): base tint of the reflective sheen layer.
- `debug` (bool, default `false`): when `true`, only the left half of the image receives the effect — useful for A/B comparison.

## Transition configuration

The `transition` block controls how the viewer blends between photos. Supply one or more entries under `transition.active`; each begins with a required `kind` (`fade`, `wipe`, `push`, `e-ink`, `dissolve`, `radial-wipe`, `venetian-blinds`, `crossfade-zoom`, or `iris`) followed by family-specific fields.

| Key         | Required? | Default                                                       | Accepted values                           | Effect |
| ----------- | --------- | ------------------------------------------------------------- | ----------------------------------------- | ------ |
| `selection` | Optional  | `fixed` when `active` has one entry, otherwise `random`       | `fixed`, `random`, or `sequential`        | Controls how the viewer iterates through `active`. |
| `active`    | Yes       | —                                                             | Array of transition entry maps            | Declares the transition variants that are eligible. Repeat entries to weight the random picker or alternate presets in sequential mode. |

When `selection` is omitted, the runtime infers it: a single entry becomes `fixed`; multiple entries default to `random`. `selection: fixed` requires exactly one entry, while `selection: sequential` or `selection: random` accept any non-empty list.

Each active entry accepts:

- **`duration-ms`** (integer, default `400` for `fade`, `wipe`, `push`; `1600` for `e-ink`): total runtime of the transition. Must be positive.

The remaining knobs depend on the family:

- **`fade`**
  - **`through-black`** (boolean, default `false`): fade to black completely before revealing the next image.
- **`wipe`**
  - **`angles`** (array of floats, default `[0.0]`): wipe directions in degrees (`0°` left→right, `90°` top→bottom). At least one finite value required. Each angle expands into its own canonical option; repeat values to bias direction.
  - **`angle-jitter`** (float ≥ 0, default `0.0`): random jitter within ±the supplied degrees.
  - **`softness`** (float, default `0.05`, clamped `0.0–0.5`): feathers the wipe edge.
- **`push`**
  - **`angles`** (array of floats, default `[0.0]`): direction the new image pushes from.
  - **`angle-jitter`** (float ≥ 0, default `0.0`): randomizes the push direction.
- **`e-ink`**
  - **`flash-count`** (integer, default `0`, capped at `6`): alternating black/flash pulses before the reveal.
  - **`reveal-portion`** (float, default `0.55`, clamped `0.05–0.95`): fraction of the timeline spent flashing before stripes start uncovering.
  - **`stripe-count`** (integer ≥ 1, default `24`): horizontal bands sweeping in.
  - **`flash-color`** (`[r, g, b]` array, default `[255, 255, 255]`): RGB color for the bright flash phases. Channels outside `0–255` are clamped.
- **`dissolve`** — threshold a value-noise field by progress (classic film dissolve).
  - **`softness`** (float 0–0.5, default `0.1`): width of the smoothstep band around the threshold. `0` = hard per-pixel pop.
  - **`scale`** (float > 0, default `64.0`): noise cell size in pixels (smaller = finer grain).
- **`radial-wipe`** — circle or diamond reveal growing from a center point.
  - **`softness`** (float 0–0.5, default `0.1`): edge feather width.
  - **`shapes`** (list of `circle` and/or `diamond`, default `[circle]`): distance metric (Euclidean vs. Manhattan). Each entry expands into its own slot, so `[circle, diamond]` shows both.
  - **`center`** (`[x, y]` normalized UV, default `[0.5, 0.5]`): reveal origin.
- **`venetian-blinds`** — horizontal or vertical stripe reveal.
  - **`stripe-count`** (integer ≥ 1, default `16`): number of blind slats.
  - **`softness`** (float 0–0.5, default `0.1`): feather of each slat's edge.
  - **`orientations`** (list of `horizontal` and/or `vertical`, default `[horizontal]`): slat direction. Each entry expands into its own slot, so `[horizontal, vertical]` shows both.
- **`crossfade-zoom`** — fade combined with a subtle scale (gentle Ken-Burns dissolve).
  - **`zoom`** (float 0–0.5, default `0.06`): maximum fractional scale change.
  - **`current-zooms-in`** (boolean, default `true`): if true the outgoing photo scales up while fading.
  - **`next-zooms-in`** (boolean, default `true`): if true the incoming photo starts slightly zoomed and settles.
- **`iris`** — mechanical camera-iris diaphragm (default `duration-ms` `2600`): annular petals with rounded ends pivot closed over the current photo, then reopen on the next. Petal geometry (radii, width, pivots, swing) is derived from the screen size and blade count, so only look-related knobs are exposed.
  - **`blades`** (integer, default `9`, clamped `5–14`): petal count.
  - **`color`** (`[r, g, b]` array, default `[42, 42, 49]`): base petal color. Mid-dark colors show the petal shading best; near-black flattens it.
  - **`petal-sheen`** (float 0–1, default `0.45`): strength of the directional sheen — a fixed virtual light that brightens blades facing it and darkens those opposite, making the individual petals read as distinct. `0` renders perfectly flat petals, distinguished only by the overlap-shadow seams (the look of a real anodized-metal iris); `1.0` is the maximum lighting swing.
  - **`overlap-shadow`** (float 0–1, default `0.6`): darkness of the soft shadow each petal casts on the one beneath it along the overlap seam.
  - **`min-aperture`** (float 0–0.4, default `0.05`): fraction of the screen circumradius left open at full close; the photos crossfade behind the petals while the aperture is at its smallest.
  - **`swirl`** (float −1–1, default `-0.45`): signed twist of the whole petal rosette while closing, as if both iris rings counter-rotate. `0` keeps the rosette orientation fixed; the sign sets the twist direction.

Examples are in [Transition examples](#transition-examples).

## Matting configuration

The `matting` block prepares the background behind each photo. During parsing the viewer normalizes the section into a canonical list:

1. Read `matting.active` from top to bottom and record each entry's `kind` plus its options.
2. Expand inline collections in place. Every swatch in a `colors` array, every `photo-average` or `photo-cluster` token, and every fixed-image `path` becomes its own canonical slot while preserving the entry's order.
3. Attach the resulting slots to their underlying renderer (`fixed-color`, `blur`, `studio`, or `fixed-image`).

`matting.selection` operates on that expanded list. `random` samples from every canonical slot — duplicates weight the draw — while `sequential` walks the expanded order before looping. Duplicating colors, paths, or `active` entries is the way to bias rotation; the outer `selection` controls traversal.

| Key              | Required? | Default                                                          | Accepted values                | Effect |
| ---------------- | --------- | ---------------------------------------------------------------- | ------------------------------ | ------ |
| `selection`      | Optional  | `fixed` when the canonical list has one slot; otherwise `random` | `fixed`, `random`, or `sequential` | Governs how the viewer iterates through the canonical mat list. |
| `active`         | Yes       | —                                                                | Array of mat entry maps        | Declares the mat variants. Duplicate swatches or paths expand into multiple canonical slots. |
| `fill-when-fits` | Optional  | disabled (omit to keep matting on every photo)                   | Map (see below)                | Renders photos already close to the screen aspect full-bleed (no mat) so they fully use a large display. |

### `fill-when-fits`

When present, each photo is evaluated **before** mat selection: if its aspect ratio is close enough to the display that filling the screen only crops a little, the viewer may render it edge-to-edge (center-crop to cover) instead of matting it. Because the decision happens before selection, an ineligible photo simply falls through to normal mat selection — the `sequential` counter and `random`/`fixed` pools are untouched.

- **`maximum-crop-percentage`** (float, default `5.0`): a photo is eligible when filling the screen crops less than this percentage off the single overflowing axis. The check is purely aspect-ratio based, so it is independent of resolution. A photo is also only eligible when it is large enough to fill the screen within `global-photo-settings.max-upscale-factor`.
- **`skip-matting-probability`** (float, default `1.0`, clamped `0–1`): for an eligible photo, the biased-coin probability of actually skipping the mat. `1.0` always fills eligible photos, `0.0` never does (feature effectively off), and values in between mix full-bleed photos with matted ones.

Each active entry accepts:

- **`minimum-mat-percentage`** (float, default `0.0`): fraction of each screen edge reserved for the mat border. Clamped `0–45%`.

The remaining controls depend on `kind`:

- **`fixed-color`**
  - **`colors`** (array of `[r, g, b]` triples, default `[[0, 0, 0]]`): one or more RGB swatches to rotate through. Channels outside `0–255` are clamped.
  - **`color`** (`[r, g, b]` triple): convenience alias for `colors` with one swatch.
- **`blur`**
  - **`sigma`** (float, default `32.0`): Gaussian blur radius applied to a scaled copy of the photo.
  - **`sample-scale`** (float, default `0.125`): ratio between canvas resolution and the intermediate blur buffer. Higher values sharpen the backdrop at higher cost.
  - **`backend`** (`cpu` or `neon`, default `neon`): blur implementation. `neon` opts into the vector-accelerated path on 64-bit ARM and falls back to `cpu` when unavailable.
- **`studio`**
  - **`colors`** (array containing `[r, g, b]` triples and/or the strings `photo-average` and `photo-cluster`; default `[photo-average]`): palette entries used for the mat base. `photo-average` is the mean of all the photo's pixels, which for a photo with two strong opposing regions (orange sky over blue water) is a muted blend that appears nowhere in the picture; `photo-cluster` is the representative colour of the photo's largest colour region (a shaded sky is one region however far its gradient runs; the colour reported is one the photo contains; two very similar hues, closer than about 0.1 in Oklab, count as one region and report one of them). Regions are compared by how much of the photo lies within about 0.15 Oklab of them, so a sky gradient over a dark foreground wins down to roughly 65-75% of the frame for the very widest gradients (sunsets, dusk glows) and somewhat less for narrower ones; below that the foreground is the larger region and wins. **`color`** is a convenience alias for a single entry.
  - **`bevel-width-px`** (float, default `3.0`).
  - **`bevel-color`** (`[r, g, b]` array, default `[255, 255, 255]`).
  - **`texture-strength`** (float, default `1.0`): strength of the simulated paper weave (`0.0` = flat matte).
  - **`warp-period-px`** (float, default `5.6`): horizontal spacing between vertical warp threads.
  - **`weft-period-px`** (float, default `5.2`): vertical spacing between horizontal weft threads.
- **`fixed-image`**
  - **`path`** (string or string array, required): filesystem paths to the backdrop image(s). The renderer loads them at startup; an empty array disables the entry.
  - **`fit`** (`cover`, `contain`, or `stretch`; default `cover`).
- **`gradient`** — linear or radial gradient between two colors.
  - **`start-color`** (`[r, g, b]`, default `[20, 20, 28]`): color at the top / left / center.
  - **`end-color`** (`[r, g, b]`, default `[70, 70, 90]`): color at the bottom / right / outer edge.
  - **`direction`** (`vertical`, `horizontal`, or `radial`; default `vertical`).
  - **`angle-degrees`** (float, default `0.0`): optional rotation for linear gradients; ignored for `radial`.
- **`vignette`** — solid color with darkened edges.
  - **`colors`** (array of `[r, g, b]` triples and/or `photo-average` / `photo-cluster`; default `[[241, 233, 210]]` (parchment)): base mat color(s). Multiple entries expand to multiple slots. `photo-average` is the mean of all pixels; `photo-cluster` is the dominant colour actually present in the photo. **`color`** is a convenience alias for a single value (also accepts both tokens).
  - **`strength`** (float 0–1, default `0.6`): how dark the corners get.
  - **`radius`** (float 0–1, default `0.75`): where the falloff begins, as a fraction of the half-diagonal.
  - **`softness`** (float 0–1, default `0.5`): width of the falloff band.
- **`cinematic-blur`** — blurred photo backdrop with a darken and vignette overlay (Apple-TV-aerial look).
  - **`sigma`** (float, default `32.0`): same as `blur.sigma`.
  - **`sample-scale`** (float, default `0.125`): same as `blur.sample-scale`.
  - **`backend`** (`cpu` or `neon`, default `neon`): same as `blur.backend`.
  - **`darken`** (float 0–1, default `0.35`): uniform darkening applied over the blur.
  - **`vignette-strength`** (float 0–1, default `0.5`): extra edge darkening.
- **`passe-partout`** — clean 45° core-bevel mat board without linen weave (crisper alternative to `studio`).
  - **`colors`** (array of `[r, g, b]` triples and/or `photo-average` / `photo-cluster`; default `[photo-average]`): mat board color(s). Multiple entries expand to multiple slots. `photo-average` is the mean of all pixels; `photo-cluster` is the dominant colour actually present in the photo. **`color`** is a convenience alias for a single entry.
  - **`bevel-width-px`** (float, default `3.0`).
  - **`bevel-color`** (`[r, g, b]`, default `[255, 255, 255]`).
- **`drop-shadow`** — soft drop shadow under the photo on a solid mat.
  - **`colors`** (array of `[r, g, b]` triples and/or `photo-average` / `photo-cluster`; default `[[235, 235, 235]]`): mat background color(s). Multiple entries expand to multiple slots. `photo-average` is the mean of all pixels; `photo-cluster` is the dominant colour actually present in the photo. **`color`** is a convenience alias for a single value (also accepts both tokens).
  - **`shadow-color`** (`[r, g, b]`, default `[0, 0, 0]`): shadow tint.
  - **`shadow-opacity`** (float 0–1, default `0.4`): shadow strength at its darkest.
  - **`shadow-blur-px`** (float, default `24.0`): softness of the shadow edge.
  - **`shadow-offset-px`** (`[x, y]` integer pair, default `[0, 12]`): shadow displacement; positive y is downward.

> Store operator-managed background images under `/var/lib/photoframe/backgrounds`. The setup pipeline treats `/opt/photoframe` as read-only and refreshes it on redeploy.

Examples are in [Matting examples](#matting-examples).

## Photo-effect examples

### Sequential print-simulation presets

```yaml
photo-effect:
  selection: sequential
  active:
    - kind: print-simulation
      light-angle-degrees: 110.0
    - kind: print-simulation
      light-angle-degrees: 60.0
      debug: true
```

## Transition examples

### Simple fade

```yaml
transition:
  active:
    - kind: fade
      duration-ms: 450
```

### Fade through black

```yaml
transition:
  active:
    - kind: fade
      duration-ms: 600
      through-black: true
```

### Weighted random mix

```yaml
transition:
  selection: random
  active:
    - kind: fade
      duration-ms: 450
    - kind: push
      duration-ms: 520
      angles: [0.0]
    - kind: push
      duration-ms: 520
      angles: [180.0]
```

Repeating the `push` entry gives that family twice the draw weight versus `fade`.

### Sequential rotation with duplicates

```yaml
transition:
  selection: sequential
  active:
    - kind: push
      duration-ms: 520
      angles: [0.0]
    - kind: wipe
      duration-ms: 520
      angles: [90.0]
    - kind: push
      duration-ms: 520
      angles: [180.0]
```

## Matting examples

### Single studio mat

```yaml
matting:
  active:
    - kind: studio
      minimum-mat-percentage: 3.5
      bevel-width-px: 4.0
```

### Weighted random palette with duplicates

```yaml
matting:
  selection: random
  active:
    - kind: fixed-color
      colors:
        - [0, 0, 0]
        - [32, 32, 32]
    - kind: fixed-color
      colors:
        - [210, 210, 210]
        - [240, 240, 240]
      minimum-mat-percentage: 6.0
    - kind: blur
      minimum-mat-percentage: 7.5
      sigma: 18.0
```

The first entry contributes two canonical slots (dark swatches), the second adds two more (light swatches), and the blur entry adds one slot. With `selection: random`, four out of five draws land on a solid mat; blur shows roughly 20% of the time.

### Sequential rotation with fixed-image

```yaml
matting:
  selection: sequential
  active:
    - kind: studio
      minimum-mat-percentage: 6.0
    - kind: fixed-image
      path: [/var/lib/photoframe/backgrounds/linen.png]
      fit: contain
    - kind: studio
      minimum-mat-percentage: 4.0
```

### Full-bleed for near-fit photos

```yaml
matting:
  fill-when-fits:
    maximum-crop-percentage: 5.0
    skip-matting-probability: 1.0
  selection: random
  active:
    - kind: fixed-color
      colors:
        - [0, 0, 0]
    - kind: blur
```

Photos within 5% of the display's aspect ratio fill the screen edge-to-edge with no mat; everything else rotates through the black swatch and blur mats as usual. Lower `skip-matting-probability` (for example `0.5`) to occasionally still mat the near-fit photos.
