# Configuration

The repository ships with a sample [`config.yaml`](../config.yaml) that you can copy or edit directly. Place a YAML file alongside the binary (or somewhere readable) and pass its path as the CLI argument.

> **Breaking change:** The `transition` and `matting` blocks now expect `selection` + `active` entries. Configurations that still rely on the legacy `types`/`options` layout will fail to load until they are migrated.

## Starter configuration

The example below targets a Pi driving a 4K portrait display backed by a NAS-mounted photo library. Inline comments explain why each value matters and what to tweak for common scenarios.

```yaml
photo-library-path: /var/lib/photo-frame/photos
# ├── cloud/  # managed by sync jobs; safe to resync or replace wholesale
# └── local/  # manual drops (USB, scp) that should survive sync resets

# Render/transition settings
transition:
  selection: random # fixed, random, or sequential
  active:
    - kind: fade
      duration-ms: 400
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
  selection: random
  active:
    - kind: fixed-color
      minimum-mat-percentage: 0.0 # % of each screen edge reserved for the mat border
      max-upscale-factor: 1.0 # Limit for enlarging images when applying mats
      colors: [[0, 0, 0], [32, 32, 32]]
    - kind: blur
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
| **External scheduling** | `awake-schedule` (consumed by `buttond`)                              |

## Key reference

### `photo-library-path`

- **Purpose:** Sets the root directory that will be scanned recursively for supported photo formats.
- **Required?** Yes. Leave it unset and the application has no images to display.
- **Accepted values & defaults:** Any absolute or relative filesystem path. The setup pipeline provisions `/var/lib/photo-frame/photos` with `cloud/` and `local/` subdirectories and points the default configuration there so both the runtime and any cloud sync job start from a known location.
- **Effect on behavior:** Switching the path changes the library the watcher monitors; the viewer reloads the playlist when the directory contents change.
- **Notes:** Keep the `cloud/` and `local/` folders under the configured root so the runtime can merge them. Use `cloud/` for content that will be overwritten by sync jobs (e.g., rclone, Nextcloud), and reserve `local/` for manual imports you do not want the sync job to prune. After the installer seeds `/etc/photo-frame/config.yaml`, update that system copy (via `sudo`) to move the library elsewhere if you do not want to keep photos under `/var/lib/photo-frame/photos`.

### `control-socket-path`

- **Purpose:** Selects where the application exposes its Unix domain control socket for runtime commands (sleep toggles, future remote controls).
- **Required?** Optional; defaults to `/run/photo-frame/control.sock`.
- **Accepted values & defaults:** Any filesystem path, typically under `/run`, `/run/user/<uid>`, or another writable runtime directory.
- **Effect on behavior:** The path is created on startup and removed on shutdown, but the parent directory must already exist with permissions that allow the kiosk user to create the socket. External helpers such as `buttond` connect to this socket to send JSON commands.
- **Notes:** The kiosk provisioning script creates `/run/photo-frame` (mode `0770`, owned by `kiosk:kiosk`) and installs an `/etc/tmpfiles.d/photo-frame.conf` entry so the directory exists after every boot. If you override the setting, make sure to pre-create the directory portion of the path with matching ownership, e.g. `sudo install -d -m 0770 -o kiosk -g kiosk /var/lib/photo-frame/runtime`. Systems running the frame under a different user should adjust the ownership in that command accordingly. Permission errors during startup mean the process could not create the socket—double-check the directory permissions or point `control-socket-path` at a writable location like `/run/user/1000/photo-frame/control.sock`.

### `transition`

- **Purpose:** Controls how the viewer blends between photos.
- **Required?** Optional; when omitted the frame uses a 400 ms fade.
- **Accepted values & defaults:** Provide a mapping with the keys documented in [Transition configuration](#transition-configuration). When omitted the runtime behaves as `{ selection: fixed, active: [{ kind: fade }] }` with the standard fade options.
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

### Wake/sleep control

- **Purpose:** Coordinates when the slideshow runs and when the panel naps.
- **Required?** Optional; without outside input the frame shows the greeting screen, transitions into sleep, and waits indefinitely.
- **How it works:** The application no longer owns an internal schedule. After startup it remains asleep until another client sends `set-state` or `ToggleState` commands over the control socket.
- **Manual control:** Pipe JSON such as `{"command":"set-state","state":"awake"}` or `{"command":"ToggleState"}` to `/run/photo-frame/control.sock` (override via `control-socket-path`). The chosen state persists until the next command arrives.
- **Automation:** Deploy [`buttond`](#power-button-daemon) and populate the shared `awake-schedule` block. buttond evaluates the schedule, issues `set-state` commands at the appropriate boundaries, and runs display power hooks on your behalf.

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

See [Display Power and Sleep Guide](power-and-sleep.md) for end-to-end wiring tips, DPMS command examples, and validation steps.

## Power button daemon

`buttond` watches the Raspberry Pi 5 power-pad button via evdev and now orchestrates scheduled wake/sleep transitions. The shared configuration file exposes a dedicated block so deployments can tune input timings, command hooks, and display handling without editing the systemd unit directly:

```yaml
buttond:
  device: null                      # optional explicit evdev path
  debounce-ms: 20                   # ignore chatter within this window
  single-window-ms: 250             # treat releases shorter than this as taps
  double-window-ms: 400             # wait this long for a second tap
  force-shutdown: true              # append -i to bypass logind interactive-user veto
  shutdown-command:
    program: /usr/bin/systemctl
    args: [poweroff, -i]
  sleep-grace-ms: 300000             # defer scheduled sleep this long after last activity
  screen:
    off-delay-ms: 3500
    display-name: null               # optional wlr-randr output name to monitor
    on-command:
      program: /opt/photo-frame/bin/powerctl
      args: [wake]
    off-command:
      program: /opt/photo-frame/bin/powerctl
      args: [sleep]
```

Pair the block above with a top-level `awake-schedule` section to describe the desired wake windows.

`force-shutdown` toggles whether buttond appends `-i` to the shutdown command. When enabled (the default) `systemctl poweroff` skips the interactive user check so a kiosk session left open on another VT cannot block shutdown. Set it to `false` if a deployment needs logind to prompt or veto a shutdown, and buttond will automatically strip `-i` even if it was present in the configured arguments.

The installer deploys `buttond.service`, which launches `/opt/photo-frame/bin/buttond --config /etc/photo-frame/config.yaml` as the `kiosk` user. At runtime the daemon behaves as follows:

- **Single press:** writes `{ "command": "ToggleState" }` to the control socket, then toggles the screen. If the display was off it immediately executes the configured wake command; if the display was on it delays for `off-delay-ms` (so the sleep screen is visible) before running the sleep command. The daemon inspects `wlr-randr` output on each press instead of relying on cached state, so restarts and manual overrides stay in sync with reality.
- **Double press:** executes the `shutdown-command`. The default uses `systemctl poweroff -i`, bypassing logind's interactive user veto so kiosk shutdowns succeed even when a user is logged in elsewhere. Provisioning installs a polkit rule so the `kiosk` user can issue the request without prompting.
- **Long press:** bypassed so the Pi firmware can force power-off.
- **Scheduled transitions:** when `awake-schedule` is present, buttond waits for the greeting delay, applies the schedule’s current state, and drives future wake/sleep transitions using `set-state` commands. `sleep-grace-ms` ensures recent manual activity can briefly delay an automatic sleep so the audience isn’t plunged into darkness mid-interaction.

Pin `buttond.screen.display-name` to the exact `wlr-randr` output name (for example `HDMI-A-2`) when a specific connector should always be probed. The daemon still falls back to the first connected non-internal display when the field is omitted, but when set it treats that output as authoritative—even if `wlr-randr` reports it as disabled—to keep the sleep command state machine aligned with panels that power down between presses.

Auto-detection scans `/dev/input/by-path/*power*` before falling back to `/dev/input/event*`. Set `buttond.device` if the wrong input is chosen. Provisioning also pins `HandlePowerKey=ignore` inside `/etc/systemd/logind.conf` so logind never interprets presses as global shutdown requests; only `buttond` reacts to the events.

### `matting`

- **Purpose:** Chooses the mat/background style rendered behind every photo.
- **Required?** Optional.
- **Accepted values & defaults:** Mapping described in [Matting configuration](#matting-configuration). When omitted the runtime behaves as `{ selection: fixed, active: [{ kind: fixed-color }] }` with a black swatch.
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

The optional `photo-effect` task sits between the loader and the viewer. When enabled it reconstructs the decoded RGBA pixels, applies any configured effects, and forwards the modified image downstream. Leave `photo-effect.active` empty (or omit the block entirely) to short-circuit the stage and pass photos through untouched. Duplicate entries in `photo-effect.active` to weight the random picker or to alternate presets when cycling sequentially.

### Scheduling effects

| Key         | Required? | Default                                                       | Accepted values            | Effect |
| ----------- | --------- | ------------------------------------------------------------- | -------------------------- | ------ |
| `selection` | Optional  | `fixed` when `active` has one entry, otherwise `random`       | `fixed`, `random`, `sequential` | Controls how the viewer iterates through `active`. `fixed` locks to the first entry, `random` chooses independently per slide, and `sequential` advances in order and loops. |
| `active`    | Yes       | —                                                             | Array of effect entry maps | Declares the effect variants that are eligible. Repeat entries—including duplicates of the same `kind`—to weight the random picker or alternate presets in sequential mode. |

> Legacy `photo-effect.types` and `photo-effect.options` keys are no longer supported. Copy each prior option into the `active` list with an explicit `kind` field to migrate.

Example: enable the print-simulation effect and alternate between two lighting presets when the stage runs in sequential mode.

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

### Print-simulation effect

`print-simulation` adapts ideas from _3D Simulation of Prints for Improved Soft Proofing_ to mimic how a framed print interacts with gallery lighting. It derives a shallow height-field from local luminance gradients, shades that relief with a configurable key light, and layers in ink compression plus paper sheen so highlights glow like coated stock. Tunable controls let operators dial in their paper stock and lighting rig:

- `light-angle-degrees` (float, default `135.0`): Direction of the simulated gallery lighting in degrees clockwise from the positive X axis.
- `relief-strength` (float ≥ 0, default `0.35`): Scale factor applied to the derived height-field before shading.
- `ink-spread` (float ≥ 0, default `0.18`): Tone compression coefficient that emulates dye absorption.
- `sheen-strength` (float ≥ 0, default `0.22`): How strongly the simulated paper sheen is blended into highlights.
- `paper-color` (RGB array, default `[245, 244, 240]`): Base tint of the reflective sheen layer.
- `debug` (bool, default `false`): When `true`, only the left half of the image receives the effect so you can compare it against the untouched right half.

## Transition configuration

The `transition` block controls how the viewer blends between photos. Supply one or more entries under `transition.active`; each entry begins with a required `kind` tag (`fade`, `wipe`, `push`, `e-ink`, or `iris`) followed by the fields that customise that transition. Use `transition.selection` to describe how the viewer steps through that list.

| Key         | Required? | Default                                                       | Accepted values                           | Effect |
| ----------- | --------- | ------------------------------------------------------------- | ----------------------------------------- | ------ |
| `selection` | Optional  | `fixed` when `active` has one entry, otherwise `random`       | `fixed`, `random`, or `sequential`        | Controls how the viewer iterates through `active`. `fixed` locks to the first entry, `random` chooses independently per slide, and `sequential` advances in order and loops. |
| `active`    | Yes       | —                                                             | Array of transition entry maps            | Declares the transition variants that are eligible. Repeat entries—including duplicates of the same `kind`—to weight the random picker or to alternate presets in sequential mode. |

> Legacy `transition.types` and `transition.options` keys are no longer supported. Migrate by copying each old option into the `active` list and moving the former map key into a `kind` field.

When `selection` is omitted, the runtime infers it: a single entry becomes `fixed`; multiple entries default to `random`. `selection: fixed` requires exactly one entry, while `selection: sequential` or `selection: random` accept any list length greater than zero.

Each active entry accepts the shared setting below:

- **`duration-ms`** (integer, default `400` for `fade`, `wipe`, `push`; `1600` for `e-ink`): Total runtime of the transition. Validation enforces values greater than zero; longer durations slow the hand-off between photos.

The remaining knobs depend on the transition family.

- **`fade`**
  - **`through-black`** (boolean, default `false`): When `true`, fades to black completely before revealing the next image. Keeps cuts discreet at the cost of a slightly longer blackout.
- **`wipe`**
  - **`angle-list-degrees`** (array of floats, default `[0.0]`): Collection of wipe directions in degrees (`0°` sweeps left→right, `90°` sweeps top→bottom). At least one finite value is required. The parser expands this list so each angle becomes its own canonical option; repeat values or duplicate the `active` entry to bias a particular direction.
  - **`angle-jitter-degrees`** (float ≥ 0, default `0.0`): Adds random jitter within ±the supplied degrees, preventing identical wipes.
  - **`softness`** (float, default `0.05`, clamped to `0.0–0.5`): Feathers the wipe edge; higher values create a softer blend.
- **`push`**
  - **`angle-list-degrees`** (array of floats, default `[0.0]`): Direction the new image pushes in from; each value expands into its own canonical option, so repeating angles or duplicating the entry weights the draw odds.
  - **`angle-jitter-degrees`** (float ≥ 0, default `0.0`): Randomizes the push direction by ±the provided degrees.
- **`e-ink`**
  - **`flash-count`** (integer, default `0`, capped at `6`): Number of alternating black/flash-color pulses before the reveal.
  - **`reveal-portion`** (float, default `0.55`, clamped to `0.05–0.95`): Fraction of the timeline spent flashing before the stripes start uncovering the next slide.
  - **`stripe-count`** (integer ≥ 1, default `24`): How many horizontal bands sweep in; higher counts mimic a finer e-ink refresh.
  - **`flash-color`** (`[r, g, b]` array, default `[255, 255, 255]`): RGB color used for the bright flash phases before the black inversion. Channels outside `0–255` are clamped.
- **`iris`**
  - **`blades`** (integer, default `7`, clamped to `5–18`): Number of shutter spokes sketched around the aperture.
  - **`blade-rgba`** (`[r, g, b, a]` float array, default `[0.12, 0.12, 0.12, 1.0]`): Base color for the iris blades. Channels outside `0–1` are clamped; the alpha component scales how dark the rendered blades appear.

  The iris transition renders shaded SLR-style blades that first close over the current photo, then reopen to reveal the next one. Each half of the timeline is dedicated to one of those motions, producing a mechanical shutter feel.

### Example: single inline fade

```yaml
transition:
  active:
    - kind: fade
      duration-ms: 600
      through-black: true
```

Omitting `selection` with one entry locks the viewer to that transition.

### Example: weighted random mix

```yaml
transition:
  selection: random
  active:
    - kind: fade
      duration-ms: 450
    - kind: push
      duration-ms: 520
      angle-list-degrees: [0.0]
    - kind: push
      duration-ms: 520
      angle-list-degrees: [180.0]
```

Repeating the `push` entry gives that family twice the draw weight versus `fade`, while still allowing different presets for horizontal and vertical motion.

### Example: sequential rotation with duplicates

```yaml
transition:
  selection: sequential
  active:
    - kind: push
      duration-ms: 520
      angle-list-degrees: [0.0]
    - kind: wipe
      duration-ms: 520
      angle-list-degrees: [90.0]
    - kind: push
      duration-ms: 520
      angle-list-degrees: [180.0]
```

Sequential mode loops through the entries exactly as written, so repeating `push` forces a push → wipe → push cadence before returning to the first entry.


## Matting configuration

The `matting` block prepares the background behind each photo. During parsing the viewer normalizes the section into a canonical list:

1. Read `matting.active` from top to bottom and record each entry’s `kind` plus its options.
2. Expand inline collections in place. Every swatch in a `colors` array, every `photo-average` token, and every fixed-image `path` becomes its own canonical slot while preserving the entry’s original order.
3. Attach the resulting slots to their underlying renderer (`fixed-color`, `blur`, `studio`, or `fixed-image`).

`matting.selection` operates on that expanded list. `random` samples from every canonical slot—duplicates simply weight the draw—while `sequential` walks the expanded order before looping. There are no per-entry selection strategies anymore: duplicating colors, paths, or entire `active` entries is the way to bias rotation, and the outer `selection` controls traversal.

| Key         | Required? | Default                                               | Accepted values                | Effect |
| ----------- | --------- | ----------------------------------------------------- | ------------------------------ | ------ |
| `selection` | Optional  | `fixed` when the canonical list has one slot; otherwise `random` | `fixed`, `random`, or `sequential` | Governs how the viewer iterates through the canonical mat list. `fixed` locks to the first slot, `random` samples independently for each slide, and `sequential` steps through the list in order before looping. |
| `active`    | Yes       | —                                                     | Array of mat entry maps        | Declares the mat variants that expand into the canonical slot list (including per-entry color or path arrays). Duplicate swatches or paths expand into multiple canonical slots, weighting the preset for `random` selection and repeating it when `sequential` mode loops. |

> Legacy `matting.types` and `matting.options` keys are no longer accepted. Copy each prior option into the `active` list with an explicit `kind` field to migrate.

When `selection` is omitted, the runtime infers it: a single canonical slot becomes `fixed`; multiple slots default to `random`. `selection: fixed` requires exactly one slot; the other modes accept any non-empty list.

Each active entry understands the shared settings below:

- **`minimum-mat-percentage`** (float, default `0.0`): Fraction of each screen edge reserved for the mat border. The renderer clamps values to `0–45%`.
- **`max-upscale-factor`** (float, default `1.0`): Maximum enlargement applied to the photo when fitting inside the mat.

The remaining controls depend on the mat `kind`:

- **`fixed-color`**
  - **`colors`** (array of `[r, g, b]` triples, default `[[0, 0, 0]]`): One or more RGB swatches (0–255 per channel) to rotate through. Channels outside the valid range are clamped before rendering. Duplicate entries increase the swatch’s weight when `selection: random`.
  - **`color`** (`[r, g, b]` triple): Convenience alias for `colors` when only one swatch is needed.
- **`blur`**
  - **`sigma`** (float, default `32.0`): Gaussian blur radius applied to a scaled copy of the photo.
  - **`sample-scale`** (float, default `0.125`): Ratio between the canvas resolution and the intermediate blur buffer. Raising it toward `1.0` sharpens the backdrop at higher cost.
  - **`backend`** (`cpu` or `neon`, default `neon`): Blur implementation to use. `neon` opts into the vector-accelerated path on 64-bit ARM and gracefully falls back to `cpu` when unavailable.
- **`studio`**
  - **`colors`** (array containing `[r, g, b]` triples and/or the string `photo-average`; default `[photo-average]`): Palette entries used for the mat base. Plain RGB swatches render exactly as specified, while `photo-average` reuses the current slide’s average color. Provide duplicates to weight certain swatches more heavily.
  - **`bevel-width-px`** (float, default `3.0`): Visible width of the bevel band in pixels.
  - **`bevel-color`** (`[r, g, b]` array, default `[255, 255, 255]`): RGB values used for the bevel band.
  - **`texture-strength`** (float, default `1.0`): Strength of the simulated paper weave (`0.0` yields a flat matte).
  - **`warp-period-px`** (float, default `5.6`): Horizontal spacing between vertical warp threads, in pixels.
  - **`weft-period-px`** (float, default `5.2`): Vertical spacing between horizontal weft threads, in pixels.
- **`fixed-image`**
  - **`path`** (string or string array, required): One or more filesystem paths to the backdrop image(s). The renderer loads referenced files at startup; an empty array disables the entry without error. Provide multiple paths (or repeat a path) to weight backgrounds after canonical expansion.
  - **`fit`** (`cover`, `contain`, or `stretch`; default `cover`): Controls how the background scales to the canvas.

### Example: single studio mat

```yaml
matting:
  active:
    - kind: studio
      minimum-mat-percentage: 3.5
      bevel-width-px: 4.0
```

### Example: weighted random palette with duplicates

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

The first entry contributes two canonical slots (dark swatches), the second adds two more (light swatches), and the blur entry adds a single slot. With `selection: random`, four out of five draws land on a solid mat while blur shows roughly 20 % of the time. Sequential sampling would step through the expanded list—dark → dark → light → light → blur—before looping.

### Example: sequential rotation with duplicates

```yaml
matting:
  selection: sequential
  active:
    - kind: studio
      minimum-mat-percentage: 6.0
    - kind: fixed-image
      path: [/opt/photo-frame/share/backgrounds/linen.png]
      fit: contain
    - kind: studio
      minimum-mat-percentage: 4.0
```

Sequential mode walks the list in order and loops, so repeating `studio` enforces a studio → fixed-image → studio cadence.
