# Staged OS and Dependency Refresh

Use this runbook before merging Wi‑Fi recovery changes that also touch host dependencies.

## 1) Create a maintenance branch

```bash
git checkout -b maintenance/upgrade-$(date +%Y%m%d)
```

## 2) Capture baseline manifest

```bash
./developer/capture-system-baseline.sh
```

The script writes version snapshots under `artifacts/upgrade-baseline-<timestamp>/`.

## 3) Refresh system packages on Pi

```bash
sudo apt update
sudo apt full-upgrade -y
sudo reboot
```

After reboot:

```bash
sudo ./setup/system/install.sh
./setup/application/deploy.sh
./setup/tools/verify.sh
```

## 4) Refresh Rust dependencies

```bash
cargo update -w
cargo check --workspace
cargo test -p photoframe -- --nocapture
cargo test -p wifi-manager -- --nocapture
cargo clippy -p wifi-manager -- -D warnings
```

If `cargo update -w` introduces regressions, pin only the problematic crates and document why in commit messages.

## 5) Re-evaluate git patches

Review pinned git dependencies in `Cargo.toml`:

- `cosmic-text`
- `glyphon`

Keep pins only when they are required for reproducible runtime or compatibility fixes on Pi.

## 6) Recovery validation on device

Execute the Wi‑Fi SOP checks in `docs/sop.md`:

- wrong-password recovery path,
- temporary AP outage auto-reconnect probe,
- `WAN down / Wi‑Fi up` no false recovery trigger.
