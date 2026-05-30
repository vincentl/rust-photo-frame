# Deferred tasks — release backlog

Captured for future review (parked while debugging the button reliability issue).

## Must do before release

### Finish `docs/photo-tutorial.md` (physical build walkthrough)
- Currently a stub: only the "Wooden Frame" section + one `oak-stock.jpeg` image.
- Build it out to mirror the section flow of `docs/build.md` (BOM → power button
  wiring → planning → physical assembly → checklist), with a captioned photo per
  step. Existing images in `docs/images/`: `oak-stock.jpeg`,
  `frame-face-measurements.jpeg`, `frame-box-measurements.jpeg`.
- Blocked on: more build photos from the maker. Scaffolding (sections +
  captioned image placeholders) can be done first.

### Add a hero picture to `README.md`
- Add a finished-frame beauty shot near the top (after the tagline, before
  "What you'll build").
- Blocked on: a finished-frame photo. No suitable image exists in `docs/images/`
  yet (only oak stock + measurement shots).

## Considering (post-v1 candidates)

### Re-attempt the aperture / iris transition
- Prior attempt failed because an **analytic iris model** (overlapping rotating
  blades, polygon-intersection geometry) doesn't translate cleanly to per-pixel
  GPU work.
- Recommended GPU-friendly approach: an **SDF of a rounded regular N-gon in
  polar coords**. Per pixel: compute radius + angle from center, evaluate a
  regular-polygon signed-distance function whose inscribed radius is driven by
  `progress`; `next` inside the shape, `current` outside, soft edge via
  `smoothstep`. Rotate the polygon angle with `progress` for the "blades
  turning" feel. Cheap, branch-light, no analytic blade intersection.
- Implementation shape (see transition architecture note below): new
  `TransitionKind::Iris` + `TransitionMode` (blade-count, rotation, softness,
  center) → pack into `params0/1` → new shader `case` in
  `crates/photoframe/src/tasks/shaders/viewer_quad.wgsl`.

### Survey: additional mat options
Existing mats: `fixed-color`, `blur`, `studio` (linen weave + bevel),
`fixed-image`, plus the new `fill-when-fits`. Candidates, roughly by
impact/effort:
- **Gradient / vignette mat** (cheap): linear or radial gradient between two
  colors; or a solid with a darkened vignette. Very GPU-cheap, visually rich.
- **Auto / dominant-color solid** (cheap): solid mat using the photo's average
  (already computed for studio `photo-average`) or a dominant/complementary
  palette color. Expose average for `fixed-color` as an "auto" swatch.
- **Cinematic blur** (cheap): blur variant with a darken + vignette overlay
  (Apple-TV-aerial look). Small extension of the existing blur path.
- **Clean passe-partout bevel** (moderate): museum mat board with a 45° core
  bevel but *without* the linen weave — a crisper alternative to `studio`.
- **Drop-shadow / floating photo** (moderate): soft drop shadow under the photo
  on a solid mat for depth.

### Survey: additional transition options
Existing transitions: `fade` (+through-black), `wipe` (angles, softness),
`push` (angles), `e-ink` (flash + stripe reveal). Candidates:
- **Dissolve** (cheapest): threshold a value-noise field by `progress`. Classic.
- **Iris / aperture** (see above) — the headline candidate.
- **Radial / shape wipe** (cheap): `wipe` variant using distance-from-center (or
  a corner) instead of a directional dot product — circle/diamond reveal.
- **Venetian blinds** (cheap): clean stripe reveal, reusing the e-ink stripe
  machinery without the flash phase.
- **Crossfade-through-zoom** (cheap): fade combined with a subtle scale on
  current/next for a gentle Ken-Burns dissolve.

## Architecture notes (for whoever picks these up)

### Transition shader dispatch
- File: `crates/photoframe/src/tasks/shaders/viewer_quad.wgsl`. Fragment shader
  switches on `U.kind` (u32) with `params0/1/3: vec4<f32>` uniforms.
- Kinds in use: `0` none, `1` fade, `2` wipe, `3` push, `4` e-ink. **`6` is NOT
  free** — it's a debug quadratic-Bezier stroke. A new transition should claim
  the next free integer (e.g. `5` or `7`); audit the Rust `kind` mapping first.
- Rust side: `TransitionKind` / `TransitionMode` in `crates/photoframe/src/config.rs`;
  uniform packing + `kind` assignment in the viewer scene code
  (`crates/photoframe/src/tasks/viewer/scenes/mod.rs` and `viewer.rs`).
- Adding a transition = enum variant + config struct + uniform packing + shader
  `case`. Well-contained, no pipeline changes.

### Mat rendering
- Mats are baked on the CPU into a full-canvas texture in `process_mat_task`
  (`crates/photoframe/src/tasks/viewer.rs`); `MattingMode` lives in `config.rs`.
- Per-photo average color is already available (`average_color`) for any mat that
  wants an auto color.
