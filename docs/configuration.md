# Configuration

The repository ships with a sample [`config.yaml`](../config.yaml) that you can copy or edit directly. Place a YAML file alongside the binary (or somewhere readable) and pass its path as the CLI argument.

## Starter configuration

The example below targets a Pi driving a 4K portrait display backed by a NAS-mounted photo library. Inline comments explain why each value matters and what to tweak for common scenarios.

```yaml
photo-library-path: /var/lib/photo-frame/photos
# ├── cloud/  # managed by sync jobs; safe to resync or replace wholesale
# └── local/  # manual drops (USB, scp) that should survive sync resets

# Render/transition settings
transition:
  types: [fade] # List one entry for a fixed transition or multiple to randomize
  duration-ms: 400 # Duration for the selected transition
dwell-ms: 2000 # Time an image remains fully visible (ms)
viewer-preload-count: 3 # Images the viewer preloads; also sets viewer channel capacity
loader-max-concurrent-decodes: 4 # Concurrent decodes in the loader
oversample: 1.0 # GPU render oversample vs. screen size
startup-shuffle-seed: null # Optional deterministic seed for initial shuffle

photo-effect:
  types: [] # Optional effects; add entries (e.g., print-simulation) to enable
  options:
    print-simulation:
      relief-strength: 0.35
      sheen-strength: 0.22

playlist:
  new-multiplicity: 3 # How many copies of a brand-new photo to schedule per cycle
  half-life: 3 days # How quickly that multiplicity decays back toward 1

matting:
  types: [fixed-color, blur] # Single entry = fixed mat, multiple entries = random rotation
  options:
    fixed-color:
      minimum-mat-percentage: 0.0 # % of each screen edge reserved for the mat border
      max-upscale-factor: 1.0 # Limit for enlarging images when applying mats
      colors: [[0, 0, 0], [32, 32, 32]]
      color-selection: sequential
    blur:
      minimum-mat-percentage: 3.5
      sigma: 32.0
      sample-scale: 0.125
      backend: neon
```

If the frame launches to a black screen, double-check that `photo-library-path` points to a directory the runtime can read and that the user account has permission to access mounted network shares. The directory should contain `cloud` and `local` subdirectories—the runtime merges both so that cloud syncs can refresh `cloud/` while USB or ad-hoc transfers live under `local/`. You can validate a YAML edit quickly with `cargo run -p rust-photo-frame -- --playlist-dry-run 1`, which parses the config without opening the render window.

## Top-level keys

Use the quick reference below to locate the knobs you care about, then dive into the per-key cards for the details.

| Role                   | Keys                                                                  |
| ---------------------- | --------------------------------------------------------------------- |
| **Required**           | `photo-library-path`                                                  |
| **Core timing**        | `transition`, `dwell-ms`, `playlist`                                  |
| **Performance tuning** | `viewer-preload-count`, `loader-max-concurrent-decodes`, `oversample` |
| **Deterministic runs** | `startup-shuffle-seed`                                                |
| **Presentation**       | `photo-effect`, `matting`                                             |
| **Greeting Screen**    | `greeting-screen`                                                     |
| **Runtime control**    | `control-socket-path`                                                 |
| **Power schedule**     | `sleep-mode`                                                          |

## Key reference

### `photo-library-path`

- **Purpose:** Sets the root directory that will be scanned recursively for supported photo formats.
- **Required?** Yes. Leave it unset and the application has no images to display.
- **Accepted values & defaults:** Any absolute or relative filesystem path. The setup pipeline provisions `/var/lib/photo-frame/photos` with `cloud/` and `local/` subdirectories and points the default configuration there so both the runtime and any cloud sync job start from a known location.
- **Effect on behavior:** Switching the path changes the library the watcher monitors; the viewer reloads the playlist when the directory contents change.
- **Notes:** Keep the `cloud/` and `local/` folders under the configured root so the runtime can merge them. Use `cloud/` for content that will be overwritten by sync jobs (e.g., rclone, Nextcloud), and reserve `local/` for manual imports you do not want the sync job to prune. After the installer seeds `/var/lib/photo-frame/config.yaml`, edit that writable copy to move the library elsewhere (for example, to an attached drive or network share) if you do not want to keep photos under `/var/lib/photo-frame/photos`.

### `control-socket-path`

- **Purpose:** Selects where the application exposes its Unix domain control socket for runtime commands (sleep toggles, future remote controls).
- **Required?** Optional; defaults to `/run/photo-frame/control.sock`.
- **Accepted values & defaults:** Any filesystem path, typically under `/run`, `/run/user/<uid>`, or another writable runtime directory.
- **Effect on behavior:** The path is created on startup (along with any missing parent directories) and removed on shutdown. External helpers such as `photo-buttond` connect to this socket to send JSON commands.
- **Notes:** The default directory under `/run` usually requires elevated permissions; systems that launch the frame as an unprivileged account should point `control-socket-path` at a writable location like `/run/user/1000/photo-frame/control.sock` or `/var/lib/photo-frame/control.sock`. Permission errors during directory creation are reported with guidance to adjust the configuration.

### `transition`

- **Purpose:** Controls how the viewer blends between photos.
- **Required?** Optional; when omitted the frame uses a 400 ms fade.
- **Accepted values & defaults:** Provide a mapping with the keys documented in [Transition configuration](#transition-configuration). Defaults to `types: [fade]` with the standard fade options.
- **Effect on behavior:** Adjust the duration, direction, randomness, or transition family to match the feel you want—from subtle fades to bold pushes or e‑ink style reveals.

### `dwell-ms`

- **Purpose:** Defines how long the current photo remains fully visible before a transition kicks in.
- **Required?** Optional.
- **Accepted values & defaults:** Positive integer in milliseconds; default `2000`. Validation rejects zero or negative values.
- **Effect on behavior:** Raising the value slows the slideshow; lowering it speeds up how quickly the frame advances.

### `viewer-preload-count`

- **Purpose:** Sets the number of decoded images the viewer keeps queued ahead of the slide currently on screen.
- **Required?** Optional.
- **Accepted values & defaults:** Positive integer; default `3`. Validation ensures the count stays above zero.
- **Effect on behavior:** Higher counts buffer more content, smoothing playback on slower storage but increasing GPU and CPU memory usage; lower counts conserve memory at the risk of showing load hitches. See [Memory profile and tuning](memory.md) for concrete sizing guidance.

### `loader-max-concurrent-decodes`

- **Purpose:** Limits how many images the CPU decoding task processes simultaneously.
- **Required?** Optional.
- **Accepted values & defaults:** Positive integer; default `4`. Validation enforces a minimum of one.
- **Effect on behavior:** Increasing the cap can keep the pipeline fed on multi-core systems; decreasing it prevents lower-powered CPUs from thrashing under heavy decode loads.

### `oversample`

- **Purpose:** Adjusts the off-screen render resolution relative to the display.
- **Required?** Optional.
- **Accepted values & defaults:** Positive floating-point value; default `1.0`. Validation requires values above zero.
- **Effect on behavior:** Values slightly above `1.0` sharpen edges and reduce aliasing; values near `1.0` minimize GPU work. Sub-unit values are rejected to avoid undersampling artifacts.

### `startup-shuffle-seed`

- **Purpose:** Seeds the initial RNG used when shuffling the first playlist.
- **Required?** Optional.
- **Accepted values & defaults:** Unsigned 64-bit integer or `null`; default `null`. When omitted the shuffle derives entropy from the system RNG.
- **Effect on behavior:** Providing a seed freezes the opening playlist order, which is helpful for demos, debugging, or deterministic tests. Leaving it `null` keeps the slideshow fresh on every boot.

### `playlist`

- **Purpose:** Tunes how the weighting system surfaces new photos.
- **Required?** Optional.
- **Accepted values & defaults:** Mapping described in [Playlist weighting](#playlist-weighting); defaults to three copies for new images and a one-day half-life.
- **Effect on behavior:** Aggressive settings make new imports loop repeatedly until they age; conservative settings let the library settle into an even rotation.

### `photo-effect`

- **Type:** mapping (see [Photo effect configuration](#photo-effect-configuration))
- **Default:** disabled (`types: []`)
- **What it does:** Inserts an optional post-processing stage between the loader and viewer. The built-in `print-simulation` effect relights each frame with directional shading and paper sheen inspired by _3D Simulation of Prints for Improved Soft Proofing_. Add it to `types` when you want that treatment; leaving the list empty keeps the stage off.
- **When to change it:** Enable when you want the frame to mimic how ink interacts with paper under gallery lighting, or when you add additional effects in future releases.

### `greeting-screen`

- **Purpose:** Styles the GPU-rendered welcome card displayed while the library is still warming up.
- **Required?** Optional.
- **Accepted values & defaults:** Mapping with optional keys
  - `message` (string, default `Initializing…`),
  - `font` (string font name; falls back to the bundled face when missing),
  - `stroke-width` (float DIP, default `12.0`),
  - `corner-radius` (float DIP, default `0.75 × stroke-width`),
  - `duration-seconds` (float ≥ 0, default `4.0`),
  - `colors.background`, `colors.font`, `colors.accent` (hex sRGB strings; default palette keeps high contrast).
- **Effect on behavior:** The renderer fits and centers the configured message inside a rounded double-line frame. `duration-seconds` guarantees the greeting remains on screen for at least that many seconds before the first photo appears, even when decoding finishes instantly.
- **Notes:** Colors accept `#rgb`, `#rgba`, `#rrggbb`, or `#rrggbbaa` notation. Low-contrast combinations log a warning so you can tweak readability, and the viewer continues with sensible defaults if fonts or colors are omitted.

### `sleep-screen`

- **Purpose:** Styles the card shown as the frame transitions into sleep. Useful for confirming that the panel is intentionally dozing rather than frozen.
- **Required?** Optional.
- **Accepted values & defaults:** Mapping with optional keys
  - `message` (string, default `Going to Sleep`),
  - `font` (string font name; falls back to the bundled face when missing),
  - `stroke-width` (float DIP, default `12.0`),
  - `corner-radius` (float DIP, default `0.75 × stroke-width`),
  - `colors.background`, `colors.font`, `colors.accent` (hex sRGB strings; default palette keeps high contrast).
- **Effect on behavior:** Shares the same renderer as the greeting screen so your sleep banner uses identical sizing rules and readability checks. The message displays until the viewer fully enters sleep mode.
- **Notes:** All fields mirror `greeting-screen` aside from the `duration-seconds` delay, which does not apply when sleeping.

### `sleep-mode`

- **Purpose:** Defines when the frame should pause the slideshow, blank the screen to a dim level, and resume automatically.
- **Required?** Optional; when omitted the frame runs 24/7.
- **Accepted values & defaults:** Mapping with the keys below. Times accept `HH:MM` or `HH:MM:SS` strings and may optionally include a trailing IANA timezone name (for example `"07:30 America/Los_Angeles"`). When no timezone is specified on a field, the top-level `timezone` value applies.
  - `timezone` — Required IANA timezone identifier. Sets the base clock used to pick weekday/weekend overrides and interpret times that do not specify their own zone.
  - `on-hours.start` / `on-hours.end` — Required local times describing when the frame should be awake each day. Start and end may wrap past midnight (for example `22:00 → 08:00` keeps the panel awake overnight). Start and end must not be identical. When the clock reaches `start` the viewer immediately wakes, and when it reaches `end` the viewer immediately sleeps—even if a manual override was active.
  - `weekday-override` / `weekend-override` — Optional blocks with their own `start`/`end` that replace the default window on weekdays (`Mon–Fri`) or weekends (`Sat/Sun`).
  - `days` — Optional map keyed by weekday name (`monday`, `tues`, …) that replaces both default and weekday/weekend overrides for specific days. Precedence is `days[...]` → `weekday/weekend-override` → `on-hours`.
- `dim-brightness` — Optional float between `0.0` (black) and `1.0` (white). Controls the solid color used while sleeping. Defaults to `0.05`.
- `display-power` — Optional block that issues hardware sleep/wake actions in addition to dimming. Configure any combination of:
  - `backlight-path` plus the required `sleep-value`/`wake-value` strings to write to a backlight sysfs node (intended for DSI panels or laptop-class devices). HDMI monitors typically do **not** expose `/sys/class/backlight` entries.
  - `sleep-command` and/or `wake-command` shell snippets that run when the frame transitions into or out of sleep. The defaults issue Wayland DPMS requests via `wlr-randr --output @OUTPUT@ --off|--on` with a fallback to `vcgencmd display_power 0|1`. The `@OUTPUT@` placeholder is replaced at runtime with the first connected output reported by `wlr-randr`, or `HDMI-A-1` if auto-detection fails.
- **Effect on behavior:** Outside the configured "on" window the viewer stops advancing slides, cancels any in-flight transitions, and clears the surface to the dim color. When the schedule says to wake up, the currently loaded image is shown again and normal dwell/transition pacing resumes. The schedule is evaluated using the configured timezone on a wall-clock basis; DST transitions do not cause drift.
- **Manual override:** Writing a JSON command such as `{"command":"ToggleState"}` to the control socket (default `/run/photo-frame/control.sock`, override via `control-socket-path`) toggles the current state immediately—handy for a single-button GPIO input. Each press flips sleep ↔ wake regardless of the schedule. The next scheduled boundary still wins: hitting `on-hours.start` forces wake, `on-hours.end` forces sleep, and either transition clears any active manual override. Environment helpers `PHOTO_FRAME_SLEEP_OVERRIDE=sleep|wake` or `PHOTO_FRAME_SLEEP_OVERRIDE_FILE=/path/to/state` can seed the initial state at startup, but upcoming schedule boundaries will still apply. Overrides are logged with timestamps and the schedule timeline continues to update in the background.
- **CLI helpers:**
  - `--verbose-sleep` logs the parsed schedule and the next 24 hours of transitions during startup.
  - `--sleep-test <SECONDS>` forces the configured display-power commands to sleep, waits `SECONDS`, wakes the panel (retrying once after two seconds), and exits.

#### Canonical example

```yaml
sleep:
  timezone: America/New_York
  on-hours:
    # 8am–10pm; app sleeps outside this window
    start: "08:00"
    end:   "22:00"

  # Optional overrides (take precedence over on-hours)
  weekday-override:
    start: "07:30"
    end:   "23:00"
  days:
    sunday:
      start: "09:30"
      end:   "21:00"

  # Dim color brightness (0=black, 1=white). Default 0.05
  dim-brightness: 0.05

  # True power control (HDMI monitor over Wayland)
  display-power:
    # Leave the @OUTPUT@ placeholder so the app can substitute the detected
    # connector. You can hard-code a name like HDMI-A-1 if preferred.
    sleep-command: "wlr-randr --output @OUTPUT@ --off || vcgencmd display_power 0"
    wake-command:  "wlr-randr --output @OUTPUT@ --on  || vcgencmd display_power 1"
```

The setup pipeline installs `/opt/photo-frame/bin/powerctl`, a thin wrapper around the same logic. Swap the example strings for `/opt/photo-frame/bin/powerctl sleep` / `wake` if you prefer the helper script once the staged tree has been deployed to `/opt`.

## Power button daemon

`photo-buttond` watches the Raspberry Pi 5 power-pad button via evdev. The installer drops a systemd unit that starts the daemon as the `frame` user with the following defaults:

```
/opt/photo-frame/bin/photo-buttond \
  --single-window-ms 250 \
  --double-window-ms 400 \
  --debounce-ms 20 \
  --control-socket /run/photo-frame/control.sock \
  --shutdown /opt/photo-frame/bin/photo-safe-shutdown
```

- **Short press:** writes `{ "command": "ToggleState" }` to `/run/photo-frame/control.sock`.
- **Double press:** runs `/opt/photo-frame/bin/photo-safe-shutdown`, which wraps `shutdown -h now`.
- **Long press:** bypassed so the Pi firmware can force power-off.
- **System integration:** The provisioning script also installs a `systemd-logind` drop-in that sets `HandlePowerKey=ignore` so the desktop stack never interprets the press as a global poweroff request; only the daemon reacts to the event.

Auto-detection scans `/dev/input/by-path/*power*` before falling back to `/dev/input/event*`. If the wrong device is chosen, override it by editing the unit to pass `--device /dev/input/by-path/...-event`. Debounce, single-press, and double-press windows are configurable in milliseconds.

### `matting`

- **Purpose:** Chooses the mat/background style rendered behind every photo.
- **Required?** Optional.
- **Accepted values & defaults:** Mapping described in [Matting configuration](#matting-configuration); defaults to a black fixed-color mat.
- **Effect on behavior:** Selecting different mat types changes the visual framing—from gallery-style solids to soft blurs or custom imagery.

## Playlist weighting

The playlist treats every photo as a node in a cycle. Brand-new photos are temporarily duplicated so that they appear multiple times per cycle, then decay back toward a single appearance as they age.

The multiplicity for each photo is computed as:

```rust
multiplicity(age) = ceil(max(1, new_multiplicity) * 0.5^(age / half_life))
```

Where `age` is the difference between the active playlist clock and the photo's creation timestamp. By default the clock is `SystemTime::now()`, but you can freeze it for testing with the `--playlist-now <RFC3339>` CLI flag. The `half-life` duration controls how quickly the multiplicity decays; once a photo's age reaches one half-life the multiplicity halves. Each cycle shuffles the scheduled copies so every photo appears at least once, and new arrivals are pinned to the front of the queue so their first showing happens immediately.

### Testing the weighting

Use the dry-run tooling to validate a configuration without launching the UI:

```bash
cargo run -p rust-photo-frame --release -- \
  config.yaml \
  --playlist-now 2025-01-01T00:00:00Z \
  --playlist-dry-run 32 \
  --playlist-seed 1234
```

The command prints the multiplicity assigned to each discovered photo and the first 32 scheduled entries according to the weighted queue. Run with `RUST_LOG=info` (or `debug` for per-photo weights) during a normal session to watch the manager log the same multiplicity calculations as the playlist rebuilds.

### Playlist knobs

| Field              | Required? | Default | Accepted values                                                             | Effect on the slideshow                                                                                     |
| ------------------ | --------- | ------- | --------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| `new-multiplicity` | Optional  | `3`     | Integer ≥ 1                                                                 | Sets how many times a brand-new photo appears in the next loop; higher values surface newcomers more often. |
| `half-life`        | Optional  | `1 day` | Positive duration string parsed by [`humantime`](https://docs.rs/humantime) | Controls how quickly the extra repeats decay; shorter half-lives return the playlist to equilibrium faster. |

## Photo effect configuration

The optional `photo-effect` task sits between the loader and the viewer. When enabled it reconstructs the decoded RGBA pixels, applies any configured effects, and forwards the modified image downstream. Leave `types` empty (or omit the block entirely) to short-circuit the stage and pass photos through untouched. The sample configuration ships with the stage disabled by default.

### Scheduling effects

- **`types`** — List the effect kinds to rotate through. Supported values today: `print-simulation`. Set to `[]` to keep the stage disabled while preserving the scaffold for future effects.
- **`type-selection`** — Optional. `random` (default) or `sequential`. `random` draws an effect independently for each slide, while `sequential` walks the `types` list in order and loops back to the first entry after the last.
- **`options`** — Map of per-effect controls. Every effect referenced in `types` must appear here so the runtime can look up its parameters.

Example: enable the print simulation effect (disabled by default) while keeping its debug split active for quick before/after checks.

```yaml
photo-effect:
  types: [print-simulation]
  type-selection: sequential
  options:
    print-simulation:
      debug: true
```

### Print-simulation effect

`print-simulation` adapts ideas from _3D Simulation of Prints for Improved Soft Proofing_ to mimic how a framed print interacts with gallery lighting. It derives a shallow height-field from local luminance gradients, shades that relief with a configurable key light, and layers in ink compression plus paper sheen so highlights glow like coated stock. Tunable controls let operators dial in their paper stock and lighting rig:

- `light-angle-degrees` (float, default `135.0`): Direction of the simulated gallery lighting in degrees clockwise from the positive X axis.
- `relief-strength` (float ≥ 0, default `0.35`): Scale factor applied to the derived height-field before shading.
- `ink-spread` (float ≥ 0, default `0.18`): Tone compression coefficient that emulates dye absorption.
- `sheen-strength` (float ≥ 0, default `0.22`): How strongly the simulated paper sheen is blended into highlights.
- `paper-color` (RGB array, default `[245, 244, 240]`): Base tint of the reflective sheen layer.
- `debug` (bool, default `false`): When `true`, only the left half of the image receives the effect so you can compare it against the untouched right half.

## Transition configuration

The `transition` block controls how the viewer blends between photos. List one or more transition kinds under `transition.types`. A single entry locks the viewer to that transition, while multiple entries tell the app to pick a new one for each slide. Every type mentioned in `transition.types` must have matching settings either inline (when only one type is listed) or within `transition.options`. Legacy configs that still use `transition.type` continue to work; `type: random` now randomizes across the entries in `transition.options`.

### Transition top-level configuration

| Key              | Required?                                                       | Default                        | Accepted values                                                 | Effect                                                                                                                                                              |
| ---------------- | --------------------------------------------------------------- | ------------------------------ | --------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `types`          | Yes                                                             | `['fade']`                     | Array containing one or more of `fade`, `wipe`, `push`, `e-ink` | Determines which transition families are in play. Duplicates are ignored; at least one entry must be supplied.                                                      |
| `type-selection` | Optional (only valid when `types` has multiple entries)         | `random`                       | `random` or `sequential`                                        | Picks whether the app draws a new type randomly each slide or cycles through the list in order. Ignored when only one type is listed.                               |
| `options`        | Required when `types` has multiple entries (optional otherwise) | Defaults per transition family | Mapping keyed by transition kind                                | Provides per-type overrides for duration and mode-specific fields. When only one type is listed you can specify the same fields inline instead of creating the map. |

> **Note:** Reserve the word `random` for `type-selection`; adding it to `types` triggers a validation error.

Each transition option accepts the shared setting below:

- **`duration-ms`** (integer, default `400` for `fade`, `wipe`, `push`; `1600` for `e-ink`): Total runtime of the transition. Validation enforces values greater than zero; longer durations slow the hand-off between photos.

The remaining knobs depend on the transition family.

### Example: wipe with a single angle

```yaml
transition:
  types: [wipe]
  duration-ms: 600
  angle-list-degrees: [120.0]
  softness: 0.12
```

When the list contains only one entry the viewer always uses that direction, so an explicit `angle-selection` strategy is unnecessary.

### Example: randomized transition mix

```yaml
transition:
  types: [fade, wipe, push]
  type-selection: sequential
  options:
    fade:
      duration-ms: 500
    wipe:
      duration-ms: 600
      angle-list-degrees: [45.0, 225.0]
      angle-selection: sequential
      angle-jitter-degrees: 30.0
    push:
      duration-ms: 650
      angle-list-degrees: [0.0, 180.0]
```

Each transition exposes a focused set of fields:

- **`fade`**
  - **`through-black`** (boolean, default `false`): When `true`, fades to black completely before revealing the next image. Keeps cuts discreet at the cost of a slightly longer blackout.
- **`wipe`**
  - **`angle-list-degrees`** (array of floats, default `[0.0]`): Collection of wipe directions in degrees (`0°` sweeps left→right, `90°` sweeps top→bottom). At least one finite value is required.
  - **`angle-selection`** (`random` or `sequential`, default `random`): Governs how the app chooses from the angle list—either independently each slide or cycling in order.
  - **`angle-jitter-degrees`** (float ≥ 0, default `0.0`): Adds random jitter within ±the supplied degrees, preventing identical wipes.
  - **`softness`** (float, default `0.05`, clamped to `0.0–0.5`): Feathers the wipe edge; higher values create a softer blend.
- **`push`**
  - **`angle-list-degrees`** (array of floats, default `[0.0]`): Direction the new image pushes in from; the same rules as wipes apply.
  - **`angle-selection`** (`random` or `sequential`, default `random`): Selection strategy for the angle list.
  - **`angle-jitter-degrees`** (float ≥ 0, default `0.0`): Randomizes the push direction by ±the provided degrees.
- **`e-ink`**
  - **`flash-count`** (integer, default `0`, capped at `6`): Number of alternating black/flash-color pulses before the reveal.
  - **`reveal-portion`** (float, default `0.55`, clamped to `0.05–0.95`): Fraction of the timeline spent flashing before the stripes start uncovering the next slide.
  - **`stripe-count`** (integer ≥ 1, default `24`): How many horizontal bands sweep in; higher counts mimic a finer e-ink refresh.
  - **`flash-color`** (`[r, g, b]` array, default `[255, 255, 255]`): RGB color used for the bright flash phases before the black inversion. Channels outside `0–255` are clamped.

## Matting configuration

The `matting` table chooses how the background behind each photo is prepared. Each entry lives under `matting.options` and is keyed by the mat type (`fixed-color`, `blur`, `studio`, or `fixed-image`). Supply one or more entries in `matting.types`. A single entry locks the viewer to that mat, while multiple entries cause the app to pick a new mat for each slide. Every listed type must either be configured inline (when only one type is present) or provided inside `matting.options`. Older `matting.type` configurations are still accepted, and `type: random` rotates through the mats listed under `matting.options`.

### Matting top-level configuration

| Key              | Required?                                                       | Default               | Accepted values                                                                | Effect                                                                                                                  |
| ---------------- | --------------------------------------------------------------- | --------------------- | ------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------- |
| `types`          | Yes                                                             | `['fixed-color']`     | Array containing one or more of `fixed-color`, `blur`, `studio`, `fixed-image` | Chooses which mat styles are eligible. Duplicates are ignored; at least one entry must be supplied.                     |
| `type-selection` | Optional (only valid when `types` has multiple entries)         | `random`              | `random` or `sequential`                                                       | Switches between drawing mats randomly or cycling through them in order. Ignored when only one type is listed.          |
| `options`        | Required when `types` has multiple entries (optional otherwise) | Defaults per mat type | Mapping keyed by mat type                                                      | Provides per-style settings. When only one type is listed, you may set the same fields inline instead of using the map. |

> **Note:** Reserve the word `random` for `type-selection`; adding it to `types` triggers a validation error.

Every mat entry accepts the shared settings below:

- **`minimum-mat-percentage`** (float, default `0.0`): Fraction of each screen edge reserved for the mat border. The renderer clamps values to `0–45%` to maintain a visible photo area.
- **`max-upscale-factor`** (float, default `1.0`): Maximum enlargement applied to the photo when fitting inside the mat. Values below `1.0` are elevated to `1.0` to avoid shrinking detail; higher values allow the frame to gently zoom when extra border space is available.

A single studio mat:

```yaml
matting:
  types: [studio]
  options:
    studio:
      minimum-mat-percentage: 3.5
      bevel-width-px: 4.0
```

Random rotation between two mats:

```yaml
matting:
  types: [fixed-color, blur]
  options:
    fixed-color:
      minimum-mat-percentage: 0.0
      colors: [[0, 0, 0], [32, 32, 32]]
      color-selection: sequential
    blur:
      minimum-mat-percentage: 6.0
      sigma: 32.0
```

Every entry inside `matting.options` accepts the shared settings below:

- **map key** (string): Mat style to render. Use `fixed-color`, `blur`, `studio`, or `fixed-image`.

### `fixed-color`

- **`colors`** (array of `[r, g, b]` triples, default `[[0, 0, 0]]`): One or more RGB swatches (0–255 per channel) to rotate through. Channels outside the valid range are clamped before rendering. Supply multiple entries to keep the frame palette fresh or stick with a single swatch for a consistent backdrop.
- **`color`** (`[r, g, b]` triple): Convenience alias for `colors` when you only want to specify a single swatch.
- **`color-selection`** (`sequential` or `random`; default `sequential`): Chooses how the viewer advances through the configured swatches. Sequential mode cycles in order, while random selects a new color for each slide.

### `blur`

- **`sigma`** (float, default `32.0`): Gaussian blur radius applied to a scaled copy of the photo that covers the screen. Larger values yield softer backgrounds; zero disables the blur but keeps the scaled image.
- **`sample-scale`** (float, default `0.125`): Ratio between the canvas resolution and the intermediate blur buffer. The renderer first scales the photo to the canvas at 1:1, then optionally downsamples by this factor before blurring. Raising the value toward `1.0` sharpens the backdrop at the expense of more GPU/CPU work.
- **`backend`** (`cpu` or `neon`, default `neon`): Blur implementation to use. `neon` opts into the vector-accelerated path on 64-bit ARM; if unsupported at runtime the app gracefully falls back to the CPU renderer.

### `studio`

- **`colors`** (array containing `[r, g, b]` triples and/or the string `photo-average`; default `[photo-average]`): Palette entries used for the mat base. Plain RGB swatches render exactly as specified, while `photo-average` reuses the current slide’s average color. Provide multiple entries to mix custom hues with adaptive tones.
- **`color-selection`** (`sequential` or `random`; default `sequential`): Governs how the mat palette advances between slides. Sequential mode steps through the configured colors, and random picks one each time.
- **`bevel-width-px`** (float, default `3.0`): Visible width of the bevel band in pixels. The renderer clamps the bevel if the mat border is thinner than the requested width.
- **`bevel-color`** (`[r, g, b]` array, default `[255, 255, 255]`): RGB values (0–255) used for the bevel band.
- **`texture-strength`** (float, default `1.0`): Strength of the simulated paper weave. `0.0` yields a flat matte; values above `1.0` exaggerate the texture.
- **`warp-period-px`** (float, default `5.6`): Horizontal spacing between vertical warp threads, in pixels.
- **`weft-period-px`** (float, default `5.2`): Vertical spacing between horizontal weft threads, in pixels.

The studio mat shades a mitred bevel band and textured paper surface using the selected palette entry, blends a hint of pigment along the outer lip, and lights the bevel from a fixed direction so it reads as a cut paper core. The photo then sits flush against that inner frame.

### `fixed-image`

- **`path`** (string or string array, required): One or more filesystem paths to the backdrop image(s). All referenced files are loaded and cached at startup. Supplying an empty array disables the `fixed-image` mat without raising an error.
- **`path-selection`** (`sequential` or `random`; default `sequential`): Chooses how to rotate through the configured backgrounds when more than one path is supplied.
- **`fit`** (`cover`, `contain`, or `stretch`; default `cover`): Chooses how the background scales to the canvas—fill while cropping (`cover`), letterbox without cropping (`contain`), or distort to fit exactly (`stretch`).

Selecting `fixed-image` keeps the backdrop perfectly consistent across slides when only one path is listed, or rotates through a curated set of branded backgrounds when multiple paths are available.
