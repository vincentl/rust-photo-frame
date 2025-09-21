# Agent Charter

## Mission
- Deliver a rock-solid, Raspberry Pi-friendly photo frame that feels effortless to own.
- Treat elegance and efficiency as non-negotiable: every commit should shrink complexity or earn its keep.
- Keep the loop tight between hardware realities, user experience, and the rendering pipeline.

## Operating Principles
- **Bias to clarity:** prefer straightforward control flow, narrow public APIs, and explicit data lifecycles.
- **Win with smart algorithms:** choose data structures and scheduling strategies that minimize CPU, heap, and I/O churn.
- **Guard performance:** profile early, watch hot paths (decode, upload, draw), and gate merges on measurable wins.
- **Design for resilience:** fail fast, surface structured context in logs, and recover without manual babysitting.
- **Automate proof:** back critical behavior with unit, async, and integration tests; refuse TODO-driven development.

## Collaboration Norms
- **Communicate intent:** describe the why alongside the diff; document config and ops-facing surfaces as you add them.
- **Respect context:** understand existing modules (PhotoFiles/Manager/Loader/Viewer) before reshaping them.
- **Simplify incrementally:** remove dead branches, collapse redundant types, and lean on composition over inheritance-style layering.
- **Leave breadcrumbs:** note invariants, tricky math, or concurrency guarantees directly where they matter.

## Coding Standards
- **Favor clarity over terseness:** use complete, descriptive variable and function names; skip abbreviations unless they are canonical in the domain (e.g., `GPU`).
- **Express configuration in kebab case:** include units in keys (e.g., `fade-ms`, `cache-capacity-count`) so consumers grasp scale at a glance.
- **Model data explicitly:** choose structs and enums over loosely typed maps; describe invariants in doc comments when types must uphold them.
- **Guard concurrency:** default to `Send`/`Sync` safe primitives, document ownership boundaries, and prefer channels/async streams over shared mutable state.
- **Keep modules cohesive:** each module should expose a tight public surface and hide implementation machinery behind private helpers.
- **Write tests with intent:** name tests after the behavior they prove; colocate async/integration tests near the systems they exercise.
- **Respect formatting tools:** keep `rustfmt` and `clippy -- -D warnings` green; add `allow` annotations only with a comment that justifies the exception.

## Continuous Improvement Targets
- Systematize performance baselines (frame time, decode latency, memory usage) and enforce drift alerts.
- Keep the setup pipeline turnkey: one script should deliver a Pi that joins the fleet and displays photos reliably.
- Pursue observability-first features (structured logging, remote ops hooks) to keep remote maintenance calm.
- Regularly audit configuration surface area; prune or merge knobs that create more cognitive load than value.

> We ship only when the code is easy to reason about, pleasant to maintain, and fast enough to disappear behind the photos.
