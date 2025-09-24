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
  type: fade # fade | wipe | push | random
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
  type: random # Set to a specific option key to pin a single mat
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

- **`photo-library-path`** _(string, default `""`)_ – Root directory that will be scanned recursively for photos.
- **`transition`** _(mapping, see [Transition configuration](#transition-configuration))_ – Controls how the viewer blends between photos.
- **`dwell-ms`** _(integer, default `2000`)_ – Time an image remains fully visible before the next fade begins.
- **`viewer-preload-count`** _(integer, default `3`)_ – Number of prepared images the viewer keeps queued; controls GPU upload backlog.
- **`loader-max-concurrent-decodes`** _(integer, default `4`)_ – Maximum number of CPU decodes that can run in parallel.
- **`oversample`** _(float, default `1.0`)_ – Render target scale relative to the screen; values >1.0 reduce aliasing but cost GPU time.
- **`startup-shuffle-seed`** _(integer or `null`, default `null`)_ – Optional deterministic seed used for the initial photo shuffle.
- **`playlist`** _(mapping, see [Playlist weighting](#playlist-weighting))_ – Controls how aggressively new photos repeat before settling into the long-term cadence.
- **`matting`** _(mapping, see [Matting configuration](#matting-configuration))_ – Controls how mats are generated around each photo.

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

Key options inside the `playlist` mapping:

- **`new-multiplicity`** _(integer, default `3`)_ – Number of copies a brand-new photo receives in the next playlist cycle.
- **`half-life`** _(duration string, default `1 day`)_ – Exponential half-life governing how quickly the multiplicity decays toward `1`. Accepts human-friendly strings via [`humantime`](https://docs.rs/humantime).

### Transition configuration

The `transition` block controls how the viewer blends between photos. Set `transition.type` to one of the supported transitions or to `random` to choose among multiple configured options. When `type` is `random`, list the available transitions under `transition.options` keyed by their type.

Example fixed wipe configuration:

```yaml
transition:
  type: wipe
  duration-ms: 600
  angle-deg: 120.0
  softness: 0.12
  randomize-direction: true
```

Randomized mix:

```yaml
transition:
  type: random
  options:
    fade:
      duration-ms: 500
    wipe:
      duration-ms: 600
      angle-deg: 45.0
      angle-jitter-deg: 30.0
    push:
      duration-ms: 650
      angle-deg: 0.0
      randomize-direction: true
```

Each option accepts the following fields:

- **`fade`**
  - `duration-ms`: Transition length in milliseconds.
  - `through-black` _(bool)_: Fade via black before revealing the next image.
- **`wipe`**
  - `duration-ms`
  - `angle-deg`: Direction of the wipe.
  - `angle-jitter-deg`: Random +/- degrees per transition.
  - `softness`: Edge feather between `0.0` and `0.5`.
  - `reverse`: Invert direction.
  - `randomize-direction`: Randomly flip the wipe direction by 180°.
- **`push`**
  - `duration-ms`
  - `angle-deg`
  - `angle-jitter-deg`
  - `reverse`
  - `randomize-direction`
  - `vertical-axis`: Switch to vertical motion instead of horizontal.

Use `vertical-axis: true` with the push transition to move images vertically instead of horizontally. Combine it with `reverse: true` to push upward, or `randomize-direction: true` to shuffle between up and down. Providing an explicit `angle-deg` continues to override the base direction if you need diagonal pushes.

### Matting configuration

The `matting` table chooses how the background behind each photo is prepared. Each entry lives under `matting.options` and is keyed by the mat type (`fixed-color`, `blur`, `studio`, or `fixed-image`). Set `matting.type` to one of those keys to lock in a single style, or to `random` to shuffle between every configured option on a per-photo basis. The configuration is invalid if `matting.type` is `random` and no options are provided, or if the selected key is missing from `matting.options`.

A single studio mat:

```yaml
matting:
  type: studio
  options:
    studio:
      minimum-mat-percentage: 3.5
      bevel-width-px: 4.0
```

Random rotation between two mats:

```yaml
matting:
  type: random
  options:
    fixed-color:
      minimum-mat-percentage: 0.0
      color: [0, 0, 0]
    blur:
      minimum-mat-percentage: 6.0
      sigma: 18.0
```

Each `matting.options` entry accepts the following shared knobs:

- **`minimum-mat-percentage`** _(float, default `0.0`)_ – Fraction (0–45%) of each screen edge reserved for the mat border.
- **`max-upscale-factor`** _(float, default `1.0`)_ – Maximum enlargement factor when fitting inside the mat; `1.0` disables upscaling.
- **map key** _(string)_ – Mat style to render. Use `fixed-color`, `blur`, `studio`, or `fixed-image`.

#### `fixed-color`

- `color` _(array `[r, g, b]`, default `[0, 0, 0]`)_ – RGB values (0–255) used to fill the mat background.

#### `blur`

- `sigma` _(float, default `20.0`)_ – Gaussian blur radius applied to a scaled copy of the photo that covers the screen.
- `max-sample-dim` _(integer or `null`, default `null`; defaults to `2048` on 64-bit ARM builds, otherwise unlimited)_ – Optional cap on the background texture size used for the blur. When set, the background is downscaled to this maximum dimension before blurring and then upscaled back to the screen size, preserving the soft-focus look while reducing CPU cost on small GPUs.
- `backend` _(string, default `cpu`)_ – Blur implementation to use. Set to `cpu` for the high-quality software renderer (default) or `neon` to request the vector-accelerated path on 64-bit ARM. When `neon` is selected but unsupported at runtime, the code automatically falls back to the CPU backend.

#### `studio`

- `bevel-width-px` _(float, default `3.0`)_ – Visible width of the bevel band in pixels. The renderer clamps this value to the available mat border if the photo touches an edge.
- `bevel-color` _(array `[r, g, b]`, default `[255, 255, 255]`)_ – RGB values (0–255) used for the bevel band.
- `texture-strength` _(float, default `1.0`)_ – Strength of the simulated paper texture (`0.0` disables the effect, `1.0` keeps the default weave).
- `warp-period-px` _(float, default `5.6`)_ – Horizontal spacing between vertical warp threads in pixels.
- `weft-period-px` _(float, default `5.2`)_ – Vertical spacing between horizontal weft threads in pixels.

The studio mat derives a uniform base color from the photo’s average RGB, renders a mitred bevel band with the configured width and color, blends a hint of the mat pigment along the outer lip, and shades the bevel from a fixed light direction so it reads as a cut paper core. The photo then sits flush against that inner frame.

#### `fixed-image`

- `path` _(string, required)_ – Filesystem path to the background image that should appear behind every photo.
- `fit` _(string, default `cover`)_ – How the background image is scaled to the canvas. Options: `cover` (default, fills while cropping as needed), `contain` (letterboxes to preserve the whole image), or `stretch` (distorts to exactly fill).

The fixed background image is loaded once at startup and reused for every slide, ensuring smooth transitions even with large source files.

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
