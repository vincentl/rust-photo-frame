# Developer

Documentation and tooling for working on the photo frame. End-user docs live in
[../README.md](../README.md) and [../docs/](../docs/); the contributor entry
point is [../CONTRIBUTING.md](../CONTRIBUTING.md).

## Guides

- **[testing.md](testing.md)** — automated checks, the on-device test plan, and the release checklist.
- **[architecture.md](architecture.md)** — setup-script architecture and kiosk-stack debugging.

## Scripts

Run these on a live Pi unless noted (the guides give context):

- `capture-system-baseline.sh` — snapshot system + toolchain versions under `artifacts/upgrade-baseline-<timestamp>/` (used by the dependency upgrade runbook).
- `overlay-test.sh` — overlay takeover smoke-test for Sway (focus/fullscreen behavior before Wi-Fi recovery testing).
- `suspend-wifi.sh` — simulate a Wi-Fi authentication failure to exercise the wifi-manager recovery path.
- `fake-btime.sh` — copy a file with a forced creation time (btime); handy for testing the playlist's new-photo weighting.
- `gather-playlist-metrics.sh` — pull `photo-log.txt` + `photo-list.txt` off the frame (needs `metrics: true`) for the last N days or all retained journal. `gather-playlist-metrics.sh [DAYS|all] [OUTDIR]`.
- `analyze-playlist-metrics.py` — **runs off-device** on those two files: audits coverage/starvation, recurrence spacing, weighting, and FIFO debut latency. Self-documenting header (how to gather/run/interpret) plus `--help`.
- `systemd/wifi-overlay-test.service` — unit used by the overlay takeover test.
