# Configure

The active configuration on an installed system is `/etc/photoframe/config.yaml` (edit with `sudo`). When running from source, pass the config path on the CLI.

After editing, restart the kiosk to apply:

```bash
sudo systemctl stop greetd.service && sleep 1 && sudo systemctl start greetd.service
```

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
- **Notes:** The kiosk provisioning script creates `/run/photoframe` (mode `0770`, owned by `kiosk:kiosk`) and installs a tmpfiles entry so the directory exists after every boot. If you override the setting, pre-create the directory with matching ownership: `sudo install -d -m 0770 -o kiosk -g kiosk /var/lib/photoframe/runtime`.

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

- **Purpose:** Tunes how the weighting system surfaces new photos.
- **Required?** Optional.
- **Defaults:** three copies for new images, one-day half-life.

See [Playlist weighting](#playlist-weighting) for the algorithm.

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
  - `stroke-width` (float DIP, default `12.0`)
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
- **Manual control:** Pipe JSON such as `{"command":"set-state","state":"awake"}` or `{"command":"toggle-state"}` to `/run/photoframe/control.sock` (override via `control-socket-path`).
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

`awake-schedule` supports wrap-past-midnight windows, weekday/weekend overrides, and per-day exceptions. Times use `HH:MM` or `HH:MM:SS`. An empty list for a day key (e.g. `friday: []`) means **sleep all day on that day** — remove the key to fall back to the `daily` window.

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
    display-name: HDMI-A-2          # wlr-randr output name; null = auto-detect
    on-command:
      program: /opt/photoframe/bin/powerctl
      args: [wake]
    off-command:
      program: /opt/photoframe/bin/powerctl
      args: [sleep]
```

Pair the block with a top-level `awake-schedule` to describe the desired wake windows.

**`buttond.screen.display-name` discovery.** The connector name must be queried inside the kiosk Wayland session:

```bash
sudo -u kiosk wlr-randr | grep connected
```

Common values: `HDMI-A-1`, `HDMI-A-2`. Setting `display-name` explicitly avoids auto-detect ambiguity on systems with multiple connectors.

**`force-shutdown`** controls whether `buttond` augments a systemctl command with `-i` (ignore inhibitors) and `--no-ask-password`. The default `true` makes `systemctl poweroff -i --no-ask-password` succeed without prompts. If you point `shutdown-command.program` at something other than `systemctl`, `buttond` strips those flags automatically.

**Runtime behavior:**

- **Single press:** resolves the current screen state and sends the appropriate `set-state` command to the control socket, then toggles the screen. If the display was off it immediately runs the wake command; if on, it delays for `off-delay-ms` (so the sleep card renders) before running the sleep command. The daemon inspects `wlr-randr` on each press, so restarts and manual overrides stay in sync.
- **Double press:** executes `shutdown-command`. Polkit allows `kiosk` to issue the request without prompting.
- **Long press:** bypassed so Pi 5 firmware can force power-off.
- **Scheduled transitions:** when `awake-schedule` is present, `buttond` waits for the greeting delay, applies the schedule's current state, then drives transitions using `set-state`.
- **Manual override:** a single press overrides the schedule until the next scheduled wake/sleep boundary, then the frame resumes following the schedule automatically. Press again to undo immediately. For example, pressing to sleep during a wake window keeps the frame asleep until that window ends; pressing to wake during a sleep window keeps it awake until the next scheduled wake.

`buttond` auto-derives `XDG_RUNTIME_DIR` and `WAYLAND_DISPLAY` for its `wlr-randr`/sway probes. Auto-detection scans `/dev/input/by-path/*power*` before falling back to `/dev/input/event*`. Set `buttond.device` if the wrong input is chosen. Provisioning pins `HandlePowerKey=ignore` in `/etc/systemd/logind.conf` so logind doesn't interpret presses as shutdown requests; only `buttond` reacts.

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
| `caption` | `bool` | `true` | Show a text label (`transition: X    mat: Y`) in the bottom-left corner of each frame. |
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

`age` is the difference between the current time and the photo's filesystem creation timestamp. The clock defaults to `SystemTime::now()` but can be frozen via `--playlist-now <RFC3339>`.

### Testing the weighting

```bash
cargo run -p photoframe --release -- \
  config.yaml \
  --playlist-now 2025-01-01T00:00:00Z \
  --playlist-dry-run 32 \
  --playlist-seed 1234
```

Prints the **weight** (relative show frequency; equilibrium = 1.0) for each discovered photo and the first 32 scheduled entries. Run with the same seed twice to confirm deterministic output.

### Playlist knobs

| Field              | Required? | Default | Accepted values                                                                | Effect on the slideshow                                                                                     |
| ------------------ | --------- | ------- | ------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------- |
| `new-multiplicity` | Optional  | `3`     | Integer ≥ 1                                                                    | Sets the peak weight for a brand-new photo; higher values surface newcomers more often before they decay.   |
| `half-life`        | Optional  | `1 day` | Positive duration string parsed by [`humantime`](https://docs.rs/humantime)    | Controls how quickly the weight decays back to equilibrium; shorter half-lives return to normal faster.     |

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

The `transition` block controls how the viewer blends between photos. Supply one or more entries under `transition.active`; each begins with a required `kind` (`fade`, `wipe`, `push`, `e-ink`, `dissolve`, `radial-wipe`, `venetian-blinds`, or `crossfade-zoom`) followed by family-specific fields.

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

Examples are in [Transition examples](#transition-examples).

## Matting configuration

The `matting` block prepares the background behind each photo. During parsing the viewer normalizes the section into a canonical list:

1. Read `matting.active` from top to bottom and record each entry's `kind` plus its options.
2. Expand inline collections in place. Every swatch in a `colors` array, every `photo-average` token, and every fixed-image `path` becomes its own canonical slot while preserving the entry's order.
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
  - **`colors`** (array containing `[r, g, b]` triples and/or the string `photo-average`; default `[photo-average]`): palette entries used for the mat base. `photo-average` reuses the slide's average color. **`color`** is a convenience alias for a single entry.
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
  - **`colors`** (array of `[r, g, b]` triples and/or `photo-average`; default `[[24, 24, 28]]`): base mat color(s). Multiple entries expand to multiple slots. **`color`** is a convenience alias for a single value (also accepts `photo-average`).
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
  - **`colors`** (array of `[r, g, b]` triples and/or `photo-average`; default `[photo-average]`): mat board color(s). Multiple entries expand to multiple slots. **`color`** is a convenience alias for a single entry.
  - **`bevel-width-px`** (float, default `3.0`).
  - **`bevel-color`** (`[r, g, b]`, default `[255, 255, 255]`).
- **`drop-shadow`** — soft drop shadow under the photo on a solid mat.
  - **`colors`** (array of `[r, g, b]` triples and/or `photo-average`; default `[[235, 235, 235]]`): mat background color(s). Multiple entries expand to multiple slots. **`color`** is a convenience alias for a single value (also accepts `photo-average`).
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
