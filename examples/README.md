# Example configurations

Starting points for `photoframe`'s config. The installer seeds
`/etc/photoframe/config.yaml` from the repo's [`config.yaml`](../config.yaml) on
first install; these are alternatives to copy over it (or to crib blocks from).
See [docs/configure.md](../docs/configure.md) for the full option reference.

| File | Best for | What it is |
| --- | --- | --- |
| [`../config.yaml`](../config.yaml) | **Most people (the default)** | A rich, tasteful, fully-commented config: every transition and mat with varied colors and angles, `fill-when-fits` on. What the installer ships. |
| [`minimal.yaml`](minimal.yaml) | "I'll build up from scratch" | The smallest valid file — just `config-version` and `photo-library-path`. Everything else uses built-in defaults. |
| [`use-showcase.yaml`](use-showcase.yaml) | "Show me what's possible" | Enables **showcase mode**: a labeled tour of every transition and mat, one at a time. Previews each *kind* at default settings. See [showcase/README.md](../showcase/README.md) for the activate/deactivate tooling. |
| [`everything.yaml`](everything.yaml) | "Give me the kitchen sink to trim" | Exercises every kind **and** the parameter variants within each — palettes + `photo-average`, multi-angle, both radial shapes, gradients in all three directions, fixed-image in all three fits, the print-simulation effect. Not meant to run as-is; delete what you don't want. (The `fixed-image` mats reference placeholder files — supply your own or remove them.) |

## Using one

Copy it over the active config and restart the kiosk, e.g. on the frame:

```bash
sudo cp examples/minimal.yaml /etc/photoframe/config.yaml
sudo systemctl restart greetd   # or your kiosk unit
```

Validate any edit without opening the render window:

```bash
cargo run -p photoframe -- /etc/photoframe/config.yaml --playlist-dry-run 1
```

> `showcase` vs `everything`: showcase auto-generates one entry per *kind* at
> default settings (great for a quick captioned tour), while `everything` is a
> hand-written config that also walks the *parameter* surface (palettes, angles,
> fits, the print effect). They are complementary, not the same.
