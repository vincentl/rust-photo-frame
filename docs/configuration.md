# Configuration

The repository ships with a sample [`config.yaml`](../config.yaml) that you can copy or edit directly. Place a YAML file alongside the binary (or somewhere readable) and pass its path as the CLI argument.

## Starter configuration

The example below targets a Pi driving a 4K portrait display backed by a NAS-mounted photo library. Inline comments explain why each value matters and what to tweak for common scenarios.

```yaml
photo-library-path: /opt/photo-frame/var/photos

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
  types: [print-simulation] # Set to [] to disable all effects
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
      color: [0, 0, 0]
    blur:
      minimum-mat-percentage: 4.0
      sigma: 18.0
```

If the frame launches to a black screen, double-check that `photo-library-path` points to a directory the runtime can read and that the user account has permission to access mounted network shares. You can validate a YAML edit quickly with `cargo run -- --playlist-dry-run 1`, which parses the config without opening the render window.

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

## Key reference

### `photo-library-path`

- **Purpose:** Sets the root directory that will be scanned recursively for supported photo formats.
- **Required?** Yes. Leave it unset and the application has no images to display.
- **Accepted values & defaults:** Any absolute or relative filesystem path. The setup pipeline provisions `/opt/photo-frame/var/photos` and points the default configuration there so both the runtime and any cloud sync job start from a known location.
- **Effect on behavior:** Switching the path changes the library the watcher monitors; the viewer reloads the playlist when the directory contents change.
- **Notes:** After the installer seeds `/opt/photo-frame/var/config.yaml`, edit that writable copy to move the library elsewhere (for example, to an attached drive or network share) if you do not want to keep photos under `/opt/photo-frame/var/photos`.

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
- **Effect on behavior:** Higher counts buffer more content, smoothing playback on slower storage but increasing GPU memory usage; lower counts conserve memory at the risk of showing load hitches.

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
- **What it does:** Inserts an optional post-processing stage between the loader and viewer. The built-in `print-simulation` effect relights each frame with directional shading and paper sheen inspired by _3D Simulation of Prints for Improved Soft Proofing_.
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
cargo run --release -- \
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

The optional `photo-effect` task sits between the loader and the viewer. When enabled it reconstructs the decoded RGBA pixels, applies any configured effects, and forwards the modified image downstream. Leave `types` empty (or omit the block entirely) to short-circuit the stage and pass photos through untouched.

### Scheduling effects

- **`types`** — List the effect kinds to rotate through. Supported values today: `print-simulation`. Set to `[]` to keep the stage disabled while preserving the scaffold for future effects.
- **`type-selection`** — Optional. `random` (default) or `sequential`. `random` draws an effect independently for each slide, while `sequential` walks the `types` list in order and loops back to the first entry after the last.
- **`options`** — Map of per-effect controls. Every effect referenced in `types` must appear here so the runtime can look up its parameters.

Example: enable the print simulation effect while keeping its debug split active for quick before/after checks.

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
  - **`flash-count`** (integer, default `3`, capped at `6`): Number of alternating black/flash-color pulses before the reveal.
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
      color: [0, 0, 0]
    blur:
      minimum-mat-percentage: 6.0
      sigma: 18.0
```

Every entry inside `matting.options` accepts the shared settings below:

- **map key** (string): Mat style to render. Use `fixed-color`, `blur`, `studio`, or `fixed-image`.

### `fixed-color`

- **`color`** (`[r, g, b]` array, default `[0, 0, 0]`): RGB values (0–255) used to fill the mat background. Channels outside the range are clamped. Choose lighter colors to mimic gallery mats or darker tones for a cinematic look.

### `blur`

- **`sigma`** (float, default `20.0`): Gaussian blur radius applied to a scaled copy of the photo that covers the screen. Larger values yield softer backgrounds; zero disables the blur but keeps the scaled image.
- **`max-sample-dimension`** (integer or `null`, default `null`; falls back to `2048` on 64-bit ARM, otherwise the canvas size): Optional cap on the intermediate blur resolution. Lower caps downsample before blurring, cutting CPU/GPU cost while preserving the dreamy backdrop.
- **`backend`** (`cpu` or `neon`, default `cpu`): Blur implementation to use. `neon` opts into the vector-accelerated path on 64-bit ARM; if unsupported at runtime the app gracefully falls back to the CPU renderer.

### `studio`

- **`bevel-width-px`** (float, default `3.0`): Visible width of the bevel band in pixels. The renderer clamps the bevel if the mat border is thinner than the requested width.
- **`bevel-color`** (`[r, g, b]` array, default `[255, 255, 255]`): RGB values (0–255) used for the bevel band.
- **`texture-strength`** (float, default `1.0`): Strength of the simulated paper weave. `0.0` yields a flat matte; values above `1.0` exaggerate the texture.
- **`warp-period-px`** (float, default `5.6`): Horizontal spacing between vertical warp threads, in pixels.
- **`weft-period-px`** (float, default `5.2`): Vertical spacing between horizontal weft threads, in pixels.

The studio mat derives a uniform base color from the photo’s average RGB, renders a mitred bevel band with the configured width and color, blends a hint of the mat pigment along the outer lip, and shades the bevel from a fixed light direction so it reads as a cut paper core. The photo then sits flush against that inner frame.

### `fixed-image`

- **`path`** (string or string array, required): One or more filesystem paths to the backdrop image(s). All referenced files are loaded and cached at startup. Supplying an empty array disables the `fixed-image` mat without raising an error.
- **`path-selection`** (`sequential` or `random`; default `sequential`): Chooses how to rotate through the configured backgrounds when more than one path is supplied.
- **`fit`** (`cover`, `contain`, or `stretch`; default `cover`): Chooses how the background scales to the canvas—fill while cropping (`cover`), letterbox without cropping (`contain`), or distort to fit exactly (`stretch`).

Selecting `fixed-image` keeps the backdrop perfectly consistent across slides when only one path is listed, or rotates through a curated set of branded backgrounds when multiple paths are available.
