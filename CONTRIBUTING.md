# Contributing

For makers who want to modify the code. End-user setup lives in
[README.md](README.md) and [docs/](docs/); the deeper contributor guides — the
test plan and release checklist, setup-script architecture, and kiosk-stack
debugging — live under **[developer/](developer/)**.

## Developer guides

- **[developer/testing.md](developer/testing.md)** — automated checks, the on-device test plan, and the release checklist.
- **[developer/architecture.md](developer/architecture.md)** — setup-script architecture and kiosk-stack debugging.

---

## Coding standards & design principles

### Mission

- Deliver a rock-solid, Raspberry Pi-friendly photo frame that feels effortless to own.
- Treat elegance and efficiency as non-negotiable: every commit should shrink complexity or earn its keep.
- Keep the loop tight between hardware realities, user experience, and the rendering pipeline.

### Operating principles

- **Bias to clarity:** prefer straightforward control flow, narrow public APIs, explicit data lifecycles.
- **Win with smart algorithms:** choose data structures and scheduling strategies that minimize CPU, heap, and I/O churn.
- **Guard performance:** profile early, watch hot paths (decode, upload, draw), gate merges on measurable wins.
- **Design for resilience:** fail fast, surface structured context in logs, recover without manual babysitting.
- **Automate proof:** back critical behavior with unit, async, and integration tests; refuse TODO-driven development.

### Collaboration norms

- **Communicate intent:** describe the *why* alongside the diff; document config and ops-facing surfaces as you add them.
- **Respect context:** understand existing modules (PhotoFiles/Manager/Loader/Viewer) before reshaping them.
- **Simplify incrementally:** remove dead branches, collapse redundant types, lean on composition over inheritance-style layering.
- **Leave breadcrumbs:** note invariants, tricky math, or concurrency guarantees directly where they matter.

### Coding rules

- **Favor clarity over terseness:** complete, descriptive names; skip abbreviations unless canonical (e.g. `GPU`).
- **Configuration in kebab case:** include units in keys (`fade-ms`, `cache-capacity-count`) so consumers grasp scale at a glance.
- **Model data explicitly:** structs and enums over loosely typed maps; describe invariants in doc comments when types must uphold them.
- **Guard concurrency:** default to `Send`/`Sync` safe primitives, document ownership boundaries, prefer channels/async streams over shared mutable state.
- **Keep modules cohesive:** tight public surface; hide implementation machinery behind private helpers.
- **Test with intent:** name tests after the behavior they prove; colocate async/integration tests near the systems they exercise.
- **Respect formatting tools:** keep `rustfmt` and `clippy -- -D warnings` green; add `allow` annotations only with a comment that justifies the exception.

> We ship only when the code is easy to reason about, pleasant to maintain, and fast enough to disappear behind the photos.

---

## Local development

The four crates (`photoframe`, `buttond`, `wifi-manager`, `config-model`) build with stable Rust on macOS or Linux. You don't need a Pi to run unit tests; you do need one to validate the kiosk stack and Wi-Fi recovery end-to-end (see [developer/testing.md](developer/testing.md)).

```bash
cargo check --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt
```

Run the photo app from source against a local config:

```bash
cargo run -p photoframe -- path/to/config.yaml
```

Validate a config without opening the render window:

```bash
cargo run -p photoframe -- --playlist-dry-run 1 path/to/config.yaml
```
