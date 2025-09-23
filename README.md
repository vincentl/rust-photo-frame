# Photoframe

A Rust-based digital photo frame application designed to run on a Raspberry Pi.

## Status

This project is **alpha and under development**

## Features (Tier 1)

- Recursive/scoped directory scanning (configurable)
- Image type filtering (jpg/png/gif/webp/bmp/tiff)
- Circular buffer (infinite loop)
- Fixed per-image delay (configurable)
- Weighted playlist that repeats new photos using an exponential half-life decay
- Error handling and structured logging

## Event Flow

```mermaid
flowchart LR
  MAIN[Main] --> FILES[PhotoFiles]
  MAIN --> MAN[PhotoManager]
  MAIN --> LOAD[PhotoLoader]
  MAIN --> VIEW[PhotoViewer]

  FILES -->|add/remove| MAN
  MAN -->|invalid photo| FILES
  MAN -->|load| LOAD
  LOAD -->|loaded| VIEW
  LOAD -->|invalid photo| FILES
```

## Configuration

Place a YAML file and pass its path as the CLI argument. Example:

```yaml
photo-library-path: /path/to/photos

# Render/transition settings
transition:
  type: fade # fade, wipe, push, or random
  duration-ms: 400 # Transition duration (ms)
  fade:
    through-black: false # Optional fade-to-black midpoint
  wipe:
    angle-deg: 0.0 # Angle of the wipe front
    softness-px: 32.0 # Soft-edge width
    reverse: false # Flip wipe direction
  push:
    direction: left # Slide direction for push transitions
  random:
    fade-weight: 1.0 # Relative chance of picking fade
    wipe-weight: 1.0 # Relative chance of picking wipe
    push-weight: 1.0 # Relative chance of picking push
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

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `photo-library-path` | string | `""` | Root directory that will be scanned recursively for photos. |
| `transition` | mapping | see below | Controls which transition mode is active and its parameters. |
| `dwell-ms` | integer | `2000` | Time an image remains fully visible before the next transition begins. |
| `viewer-preload-count` | integer | `3` | Number of prepared images the viewer keeps queued; controls GPU upload backlog. |
| `loader-max-concurrent-decodes` | integer | `4` | Maximum number of CPU decodes that can run in parallel. |
| `oversample` | float | `1.0` | Render target scale relative to the screen; values >1.0 reduce aliasing but cost GPU time. |
| `startup-shuffle-seed` | integer or `null` | `null` | Optional deterministic seed used for the initial photo shuffle. |
| `playlist` | mapping | see below | Controls how aggressively new photos repeat before settling into the long-term cadence. |
| `matting` | mapping | see below | Controls how mats are generated around each photo. |
| `transition` | mapping | see below | Chooses the transition mode between photos and its tuning parameters. |

### Transition configuration

The `transition` block mirrors the matting configuration style: set `transition.type` to one of the supported modes, then tune that mode's sub-table. When `transition.type` is `random`, the viewer shuffles between the configured modes according to the weights in `transition.random`.

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `type` | string | `fade` | Active transition. Use `fade`, `wipe`, `push`, or `random`. |
| `duration-ms` | integer | `400` | Duration of the transition animation. Must be greater than zero. |
| `fade.through-black` | boolean | `false` | When enabled the fade darkens to black before revealing the next image. |
| `wipe.angle-deg` | float | `0.0` | Angle (in degrees) of the wipe front measured clockwise from the positive X axis. |
| `wipe.softness-px` | float | `32.0` | Width of the feathered edge along the wipe front, in pixels. Set to `0` for a hard edge. |
| `wipe.reverse` | boolean | `false` | When `true`, the wipe travels in the opposite direction. |
| `push.direction` | string | `left` | Direction the outgoing photo travels. Use `left`, `right`, `up`, or `down`. |
| `random.fade-weight` | float | `1.0` | Relative probability of choosing the fade effect when `transition.type` is `random`. |
| `random.wipe-weight` | float | `1.0` | Relative probability of choosing the wipe effect when `transition.type` is `random`. |
| `random.push-weight` | float | `1.0` | Relative probability of choosing the push effect when `transition.type` is `random`. |

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

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `new-multiplicity` | integer | `3` | Number of copies a brand-new photo receives in the next playlist cycle. |
| `half-life` | duration string | `1 day` | Exponential half-life governing how quickly the multiplicity decays toward `1`. Accepts human-friendly strings via [`humantime`](https://docs.rs/humantime). |

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

| Key | Type | Default | Notes |
| --- | --- | --- | --- |
| `minimum-mat-percentage` | float | `0.0` | Fraction (0–45%) of each screen edge reserved for the mat border. |
| `max-upscale-factor` | float | `1.0` | Maximum enlargement factor when fitting inside the mat; `1.0` disables upscaling. |
| map key | string | — | Mat style to render. Use `fixed-color`, `blur`, `studio`, or `fixed-image`. |

#### `fixed-color`

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `color` | `[r, g, b]` array | `[0, 0, 0]` | The RGB values (0–255) used to fill the mat background. |

#### `blur`

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `sigma` | float | `20.0` | Gaussian blur radius applied to a scaled copy of the photo that covers the screen. |
| `max-sample-dim` | integer or `null` | `null` (defaults to `2048` on 64-bit ARM builds, otherwise unlimited) | Optional cap on the background texture size used for the blur. When set, the background is downscaled to this maximum dimension before blurring and then upscaled back to the screen size, preserving the soft-focus look while reducing CPU cost on small GPUs. |
| `backend` | string | `cpu` | Blur implementation to use. Set to `cpu` for the high-quality software renderer (default) or `neon` to request the vector-accelerated path on 64-bit ARM. When `neon` is selected but unsupported at runtime, the code automatically falls back to the CPU backend. |

#### `studio`

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `bevel-width-px` | float | `3.0` | Visible width of the bevel band in pixels. The renderer clamps this value to the available mat border if the photo touches an edge. |
| `bevel-color` | `[r, g, b]` array | `[255, 255, 255]` | RGB values (0–255) used for the bevel band. |
| `texture-strength` | float | `1.0` | Strength of the simulated paper texture (0.0 disables the effect, 1.0 keeps the default weave). |
| `warp-period-px` | float | `5.6` | Horizontal spacing between vertical warp threads in pixels. |
| `weft-period-px` | float | `5.2` | Vertical spacing between horizontal weft threads in pixels. |

The studio mat derives a uniform base color from the photo’s average RGB, renders a mitred bevel band with the configured width and color, blends a hint of the mat pigment along the outer lip, and shades the bevel from a fixed light direction so it reads as a cut paper core. The photo then sits flush against that inner frame.

#### `fixed-image`

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `path` | string | (required) | Filesystem path to the background image that should appear behind every photo. |
| `fit` | string | `cover` | How the background image is scaled to the canvas. Options: `cover` (default, fills while cropping as needed), `contain` (letterboxes to preserve the whole image), or `stretch` (distorts to exactly fill). |

The fixed background image is loaded once at startup and reused for every slide, ensuring smooth transitions even with large source files.

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
