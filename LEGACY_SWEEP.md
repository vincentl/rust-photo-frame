# Legacy Sweep Report

## Summary
- KEEP: 0
- UPDATE: 2
- DELETE: 5

Major themes: retired historical setup documentation and unused tooling; consolidated staging assets under a single path.
CI status: repository ships no GitHub workflows after this sweep.

## Findings

| Path | Type | Status | Evidence | Action |
| --- | --- | --- | --- | --- |
| make-src-archive.sh | Script | DELETE | `rg` finds no references; superseded by modern release packaging. | Remove file. |
| setup-AUDIT-RESULTS.md | Documentation | DELETE | Historical audit referencing removed `setup/system/*` scripts and legacy units. | Remove file. |
| docs/setup-audit.md | Documentation | DELETE | Documents deleted `setup/system` scripts; no inbound links remain. | Remove file. |
| docs/images/.gitkeep | Asset placeholder | DELETE | Empty placeholder directory unused by docs. | Remove file. |
| tools/fake-btime.sh | Script | DELETE | Standalone helper never referenced by code, docs, or tests. | Remove file. |
| setup/files/bin/powerctl | Script | UPDATE | Only staged via bespoke logic despite other helpers living under `setup/files/bin`. | Move into shared bin asset tree and drop special casing. |
| docs/kiosk.md | Documentation | UPDATE | References the removed `setup-AUDIT-RESULTS.md` artifact. | Update prose to drop stale pointer. |

## Follow-ups
- None identified.

## Build & Test
- `cargo check`
