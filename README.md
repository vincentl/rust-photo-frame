# Photoframe

A Rust-based digital photo frame pipeline tuned for Raspberry Pi hardware. It watches your photo library, weights the playlist so new images appear more frequently, and renders each slide with configurable matting and transitions.

## Project Status

This project is **alpha and under active development**. Expect rough edges and incomplete documentation.

## Table of Contents

1. [Quickstart](#quickstart)
2. [Architecture Overview](#architecture-overview)
3. [Features](#features)
4. [Configuration](#configuration)
5. [Playlist Weighting](#playlist-weighting)
6. [Matting Configuration](#matting-configuration)
7. [License](#license)

## Quickstart

### Prerequisites _(TODO: document Raspberry Pi OS version, GPU requirements, and external dependencies)_

### Build & Run

```bash
cargo run --release -- <path/to/config.yaml>
```

The binary accepts several optional CLI flags for playlist testing and determinism:

| Flag | Description |
| --- | --- |
| `--playlist-now <RFC3339>` | Overrides `SystemTime::now()` when computing playlist weights. Useful for reproducible simulations. |
| `--playlist-dry-run <ITERATIONS>` | Emits a textual preview of the weighted playlist order without launching the UI. |
| `--playlist-seed <SEED>` | Forces deterministic playlist shuffling for both dry-run and live modes. |

### Deployment _(TODO: outline systemd service, auto-start configuration, and graceful shutdown strategy)_

### Testing _(TODO: enumerate unit/integration test commands and GPU validation steps)_

## Architecture Overview

The runtime is composed of four asynchronous tasks orchestrated by `main.rs`. They communicate over bounded channels to keep memory predictable and to respect GPU/CPU parallelism limits.

```mermaid
flowchart LR
  MAIN[Main] --> FILES[PhotoFiles]
  MAIN --> MAN[PhotoManager]
  MAIN --> LOAD[PhotoLoader]
  MAIN --> VIEW[PhotoViewer]

  FILES -->|inventory updates| MAN
  MAN -->|invalid photo| FILES
  MAN -->|photo requests| LOAD
  LOAD -->|decoded image| VIEW
  LOAD -->|invalid photo| FILES
  VIEW -->|displayed event| MAN
```

## Features

### Tier 1 (implemented)

- Recursive/scoped directory scanning (configurable)
- Image type filtering (jpg/png/gif/webp/bmp/tiff)
- Circular buffer (infinite loop)
- Fixed per-image delay (configurable)
- Weighted playlist that repeats new photos using an exponential half-life decay
- Error handling and structured logging

### Tier 2+ _(TODO: summarize roadmap items from `Roadmap.md` and planned UX polish)_

## Configuration

Place a YAML file and pass its path as the CLI argument. Example:

```yaml
photo-library-path: /path/to/photos

# Render/transition settings
transition:
  types: [fade] # List one entry for a fixed transition or multiple to randomize
  duration-ms: 400 # Duration for the selected transition
dwell-ms: 2000 # Time an image remains fully visible (ms)
viewer-preload-count: 3 # Images the viewer preloads; also sets viewer channel capacity
loader-max-concurrent-decodes: 4 # Concurrent decodes in the loader
oversample: 1.0 # GPU render oversample vs. screen size
startup-shuffle-seed: null # Optional deterministic seed for initial shuffle

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

### Top-level keys

| Key | Required? | Default | What changes when you tweak it? |
| --- | --- | --- | --- |
| `photo-library-path` | Yes | _none_ (must point to a real directory) | Determines which folders are scanned recursively for displayable images. |
| `transition` | Optional | Single 400 ms `fade` | Selects the transition style(s) between photos; see the dedicated section below for fine control. |
| `dwell-ms` | Optional | `2000` | Sets how long each image remains fully visible before the next transition begins. |
| `viewer-preload-count` | Optional | `3` | Controls how many decoded frames are staged ahead of the current slide, trading memory for smoothness. |
| `loader-max-concurrent-decodes` | Optional | `4` | Caps the number of images decoded in parallel, balancing throughput against CPU contention. |
| `oversample` | Optional | `1.0` | Multiplies the render target resolution; higher values reduce aliasing at the cost of GPU time. |
| `startup-shuffle-seed` | Optional | `null` | Forces a reproducible initial shuffle when set, aiding demos and regression tests. |
| `playlist` | Optional | `new-multiplicity: 3`, `half-life: 1 day` | Tunes how aggressively fresh photos repeat before they age into the long-term cadence. |
| `matting` | Optional | Single `fixed-color` mat in black | Chooses the background treatment (solid, blur, studio bevel, or fixed image) for each photo. |

Use the quick reference above to locate the knobs you care about, then dive into the per-key cards below for the details.

#### `photo-library-path`
- **Purpose:** Sets the root directory that will be scanned recursively for supported photo formats.
- **Required?** Yes. Leave it unset and the application has no images to display.
- **Accepted values & defaults:** Any absolute or relative filesystem path. There is no usable default.
- **Effect on behavior:** Switching the path changes the library the watcher monitors; the viewer reloads the playlist when the directory contents change.

#### `transition`
- **Purpose:** Controls how the viewer blends between photos.
- **Required?** Optional; when omitted the frame uses a 400 ms fade.
- **Accepted values & defaults:** Provide a mapping with the keys documented in [Transition configuration](#transition-configuration). Defaults to `types: [fade]` with the standard fade options.
- **Effect on behavior:** Adjust the duration, direction, randomness, or transition family to match the feel you want—from subtle fades to bold pushes or e‑ink style reveals.

#### `dwell-ms`
- **Purpose:** Defines how long the current photo remains fully visible before a transition kicks in.
- **Required?** Optional.
- **Accepted values & defaults:** Positive integer in milliseconds; default `2000`. Validation rejects zero or negative values.
- **Effect on behavior:** Raising the value slows the slideshow; lowering it speeds up how quickly the frame advances.

#### `viewer-preload-count`
- **Purpose:** Sets the number of decoded images the viewer keeps queued ahead of the slide currently on screen.
- **Required?** Optional.
- **Accepted values & defaults:** Positive integer; default `3`. Validation ensures the count stays above zero.
- **Effect on behavior:** Higher counts buffer more content, smoothing playback on slower storage but increasing GPU memory usage; lower counts conserve memory at the risk of showing load hitches.

#### `loader-max-concurrent-decodes`
- **Purpose:** Limits how many images the CPU decoding task processes simultaneously.
- **Required?** Optional.
- **Accepted values & defaults:** Positive integer; default `4`. Validation enforces a minimum of one.
- **Effect on behavior:** Increasing the cap can keep the pipeline fed on multi-core systems; decreasing it prevents lower-powered CPUs from thrashing under heavy decode loads.

#### `oversample`
- **Purpose:** Adjusts the off-screen render resolution relative to the display.
- **Required?** Optional.
- **Accepted values & defaults:** Positive floating-point value; default `1.0`. Validation requires values above zero.
- **Effect on behavior:** Values slightly above `1.0` sharpen edges and reduce aliasing; values near `1.0` minimize GPU work. Sub-unit values are rejected to avoid undersampling artifacts.

#### `startup-shuffle-seed`
- **Purpose:** Seeds the initial RNG used when shuffling the first playlist.
- **Required?** Optional.
- **Accepted values & defaults:** Unsigned 64-bit integer or `null`; default `null`. When omitted the shuffle derives entropy from the system RNG.
- **Effect on behavior:** Providing a seed freezes the opening playlist order, which is helpful for demos, debugging, or deterministic tests. Leaving it `null` keeps the slideshow fresh on every boot.

#### `playlist`
- **Purpose:** Tunes how the weighting system surfaces new photos.
- **Required?** Optional.
- **Accepted values & defaults:** Mapping described in [Playlist weighting](#playlist-weighting); defaults to three copies for new images and a one-day half-life.
- **Effect on behavior:** Aggressive settings make new imports loop repeatedly until they age; conservative settings let the library settle into an even rotation.

#### `matting`
- **Purpose:** Chooses the mat/background style rendered behind every photo.
- **Required?** Optional.
- **Accepted values & defaults:** Mapping described in [Matting configuration](#matting-configuration); defaults to a black fixed-color mat.
- **Effect on behavior:** Selecting different mat types changes the visual framing—from gallery-style solids to soft blurs or custom imagery.

### Playlist weighting

The playlist treats every photo as a node in a cycle. Brand-new photos are temporarily duplicated so that they appear multiple times per cycle, then decay back toward a single appearance as they age.

The multiplicity for each photo is computed as:

```
multiplicity(age) = ceil(max(1, new_multiplicity) * 0.5^(age / half_life))
```

Where `age` is the difference between the active playlist clock and the photo's creation timestamp. By default the clock is `SystemTime::now()`, but you can freeze it for testing with the `--playlist-now <RFC3339>` CLI flag. The `half-life` duration controls how quickly the multiplicity decays; once a photo's age reaches one half-life the multiplicity halves. Each cycle shuffles the scheduled copies so every photo appears at least once, and new arrivals are pinned to the front of the queue so their first showing happens immediately.

#### Testing the weighting

Use the new dry-run tooling to validate a configuration without launching the UI:

```bash
cargo run --release -- \
  config.yaml \
  --playlist-now 2025-01-01T00:00:00Z \
  --playlist-dry-run 32 \
  --playlist-seed 1234
```

The command prints the multiplicity assigned to each discovered photo and the first 32 scheduled entries according to the weighted queue. Run with `RUST_LOG=info` (or `debug` for per-photo weights) during a normal session to watch the manager log the same multiplicity calculations as the playlist rebuilds.

#### Playlist knobs

| Field | Required? | Default | Accepted values | Effect on the slideshow |
| --- | --- | --- | --- | --- |
| `new-multiplicity` | Optional | `3` | Integer ≥ 1 | Sets how many times a brand-new photo appears in the next loop; higher values surface newcomers more often. |
| `half-life` | Optional | `1 day` | Positive duration string parsed by [`humantime`](https://docs.rs/humantime) | Controls how quickly the extra repeats decay; shorter half-lives return the playlist to equilibrium faster. |

### Transition configuration

The `transition` block controls how the viewer blends between photos. List one or more transition kinds under `transition.types`. A single entry locks the viewer to that transition, while multiple entries tell the app to pick a new one for each slide. Every type mentioned in `transition.types` must have matching settings either inline (when only one type is listed) or within `transition.options`. Legacy configs that still use `transition.type` continue to work; `type: random` now randomizes across the entries in `transition.options`.

#### Structure

| Key | Required? | Default | Accepted values | Effect |
| --- | --- | --- | --- | --- |
| `types` | Yes | `['fade']` | Array containing one or more of `fade`, `wipe`, `push`, `e-ink` | Determines which transition families are in play. Duplicates are ignored; at least one entry must be supplied. |
| `type-selection` | Optional (only valid when `types` has multiple entries) | `random` | `random` or `round-robin` | Picks whether the app draws a new type randomly each slide or cycles through the list in order. Ignored when only one type is listed. |
| `options` | Required when `types` has multiple entries (optional otherwise) | Defaults per transition family | Mapping keyed by transition kind | Provides per-type overrides for duration and mode-specific fields. When only one type is listed you can specify the same fields inline instead of creating the map. |

> **Note:** Reserve the word `random` for `type-selection`; adding it to `types` triggers a validation error.

Each transition option accepts the shared setting below:

- **`duration-ms`** (integer, default `400` for `fade`, `wipe`, `push`; `1600` for `e-ink`): Total runtime of the transition. Validation enforces values greater than zero; longer durations slow the hand-off between photos.

The remaining knobs depend on the transition family.

#### Example: wipe with a single angle

```yaml
transition:
  types: [wipe]
  duration-ms: 600
  angle-list-degrees: [120.0]
  softness: 0.12
```

When the list contains only one entry the viewer always uses that direction, so an explicit `angle-selection` strategy is unnecessary.

#### Example: randomized transition mix

```yaml
transition:
  types: [fade, wipe, push]
  type-selection: round-robin
  options:
    fade:
      duration-ms: 500
    wipe:
      duration-ms: 600
      angle-list-degrees: [45.0, 225.0]
      angle-selection: round-robin
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
  - **`angle-selection`** (`random` or `round-robin`, default `random`): Governs how the app chooses from the angle list—either independently each slide or cycling in order.
  - **`angle-jitter-degrees`** (float ≥ 0, default `0.0`): Adds random jitter within ±the supplied degrees, preventing identical wipes.
  - **`softness`** (float, default `0.05`, clamped to `0.0–0.5`): Feathers the wipe edge; higher values create a softer blend.
- **`push`**
  - **`angle-list-degrees`** (array of floats, default `[0.0]`): Direction the new image pushes in from; the same rules as wipes apply.
  - **`angle-selection`** (`random` or `round-robin`, default `random`): Selection strategy for the angle list.
  - **`angle-jitter-degrees`** (float ≥ 0, default `0.0`): Randomizes the push direction by ±the provided degrees.
- **`e-ink`**
  - **`flash-count`** (integer, default `3`, capped at `6`): Number of alternating black/flash-color pulses before the reveal.
  - **`reveal-portion`** (float, default `0.55`, clamped to `0.05–0.95`): Fraction of the timeline spent flashing before the stripes start uncovering the next slide.
  - **`stripe-count`** (integer ≥ 1, default `24`): How many horizontal bands sweep in; higher counts mimic a finer e-ink refresh.
  - **`flash-color`** (`[r, g, b]` array, default `[255, 255, 255]`): RGB color used for the bright flash phases before the black inversion. Channels outside `0–255` are clamped.

### Matting configuration

The `matting` table chooses how the background behind each photo is prepared. Each entry lives under `matting.options` and is keyed by the mat type (`fixed-color`, `blur`, `studio`, or `fixed-image`). Supply one or more entries in `matting.types`. A single entry locks the viewer to that mat, while multiple entries cause the app to pick a new mat for each slide. Every listed type must either be configured inline (when only one type is present) or provided inside `matting.options`. Older `matting.type` configurations are still accepted, and `type: random` rotates through the mats listed under `matting.options`.

#### Structure

| Key | Required? | Default | Accepted values | Effect |
| --- | --- | --- | --- | --- |
| `types` | Yes | `['fixed-color']` | Array containing one or more of `fixed-color`, `blur`, `studio`, `fixed-image` | Chooses which mat styles are eligible. Duplicates are ignored; at least one entry must be supplied. |
| `type-selection` | Optional (only valid when `types` has multiple entries) | `random` | `random` or `round-robin` | Switches between drawing mats randomly or cycling through them in order. Ignored when only one type is listed. |
| `options` | Required when `types` has multiple entries (optional otherwise) | Defaults per mat type | Mapping keyed by mat type | Provides per-style settings. When only one type is listed, you may set the same fields inline instead of using the map. |

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

#### `fixed-color`

- **`color`** (`[r, g, b]` array, default `[0, 0, 0]`): RGB values (0–255) used to fill the mat background. Channels outside the range are clamped. Choose lighter colors to mimic gallery mats or darker tones for a cinematic look.

#### `blur`

- **`sigma`** (float, default `20.0`): Gaussian blur radius applied to a scaled copy of the photo that covers the screen. Larger values yield softer backgrounds; zero disables the blur but keeps the scaled image.
- **`max-sample-dim`** (integer or `null`, default `null`; falls back to `2048` on 64-bit ARM, otherwise the canvas size): Optional cap on the intermediate blur resolution. Lower caps downsample before blurring, cutting CPU/GPU cost while preserving the dreamy backdrop.
- **`backend`** (`cpu` or `neon`, default `cpu`): Blur implementation to use. `neon` opts into the vector-accelerated path on 64-bit ARM; if unsupported at runtime the app gracefully falls back to the CPU renderer.

#### `studio`

- **`bevel-width-px`** (float, default `3.0`): Visible width of the bevel band in pixels. The renderer clamps the bevel if the mat border is thinner than the requested width.
- **`bevel-color`** (`[r, g, b]` array, default `[255, 255, 255]`): RGB values (0–255) used for the bevel band.
- **`texture-strength`** (float, default `1.0`): Strength of the simulated paper weave. `0.0` yields a flat matte; values above `1.0` exaggerate the texture.
- **`warp-period-px`** (float, default `5.6`): Horizontal spacing between vertical warp threads, in pixels.
- **`weft-period-px`** (float, default `5.2`): Vertical spacing between horizontal weft threads, in pixels.

The studio mat derives a uniform base color from the photo’s average RGB, renders a mitred bevel band with the configured width and color, blends a hint of the mat pigment along the outer lip, and shades the bevel from a fixed light direction so it reads as a cut paper core. The photo then sits flush against that inner frame.

#### `fixed-image`

- **`path`** (string, required): Filesystem path to the background image that should appear behind every photo. The file is loaded once at startup and cached for reuse.
- **`fit`** (`cover`, `contain`, or `stretch`; default `cover`): Chooses how the background scales to the canvas—fill while cropping (`cover`), letterbox without cropping (`contain`), or distort to fit exactly (`stretch`).

Selecting `fixed-image` keeps the backdrop perfectly consistent across slides, which is ideal for branded frames or themed installations.

## References

- **Procedural studio mat weave texture.** Our weave shading is adapted from Mike Cauchi’s breakdown of tillable cloth shading, which layers sine-profiled warp/weft threads with randomized grain to keep the pattern from banding. See ["Research – Tillable Images and Cloth Shading"](https://www.mikecauchiart.com/single-post/2017/01/23/research-tillable-images-and-cloth-shading).

## License

This project is licensed under the **MIT License**.
See the [LICENSE](LICENSE) file for full text.

### Third-Party Assets

This project may bundle fonts licensed under the SIL Open Font License (OFL) 1.1.

- Inconsolata (example font for UI text)
  - License: SIL OFL 1.1
  - License text: `assets/fonts/OFL.txt`
  - Notes: The font remains under OFL, which permits bundling with MIT-licensed code. If the font is modified, the Reserved Font Name rules apply.

© 2025 Vincent Lucarelli
