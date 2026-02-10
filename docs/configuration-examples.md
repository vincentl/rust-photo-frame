# Configuration Examples

Use this file for copy/paste recipes. Keep [`configuration.md`](configuration.md) open for field-level definitions and constraints.

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

### Single inline fade

```yaml
transition:
  active:
    - kind: fade
      duration-ms: 600
      through-black: true
```

Omitting `selection` with one entry locks the viewer to that transition.

### Weighted random mix

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

### Sequential rotation with duplicates

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

Sequential mode loops through the entries exactly as written, so repeating `push` forces a `push -> wipe -> push` cadence before returning to the first entry.

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

The first entry contributes two canonical slots (dark swatches), the second adds two more (light swatches), and the blur entry adds one slot. With `selection: random`, four out of five draws land on a solid mat while blur shows roughly 20% of the time.

### Sequential rotation with duplicates

```yaml
matting:
  selection: sequential
  active:
    - kind: studio
      minimum-mat-percentage: 6.0
    - kind: fixed-image
      path: [/var/lib/photo-frame/backgrounds/linen.png]
      fit: contain
    - kind: studio
      minimum-mat-percentage: 4.0
```

Sequential mode walks the list in order and loops, so repeating `studio` enforces a `studio -> fixed-image -> studio` cadence.
