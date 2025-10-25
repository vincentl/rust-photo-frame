# Photo Frame Setup Pipeline

This directory houses idempotent provisioning scripts for Raspberry Pi photo frame deployments. Each stage can be re-run safely after OS updates or image refreshes.

## Quick start

Run both stages in one command:

```bash
./setup/install-all.sh
```

The script provisions the OS (sudo), then builds and deploys the app as your unprivileged user, and finally activates the kiosk services.

Set `CARGO_BUILD_JOBS` to cap build parallelism on low-memory devices.

## System provisioning (Trixie)

Provision Raspberry Pi OS (Trixie) for kiosk duty and install shared dependencies with:

```bash
sudo ./setup/system/install.sh
```

The system pipeline executes its numbered modules in order and performs the following actions:

- installs base apt packages (graphics stack, build tools, networking utilities),
- installs or updates the system-wide Rust toolchain under `/usr/local/cargo`,
- replaces the legacy swapfile with `systemd-zram-generator` configured for a half-RAM zram device,
- verifies the host is running Debian 13 (Trixie) and applies Raspberry Pi 5 boot firmware tweaks (set `ENABLE_4K_BOOT=0` before running if you need to skip the HDMI 4K60 profile),
- ensures the locked `kiosk` user exists with membership in the `render`, `video`, and `input` groups and provisions runtime directories and polkit policy,
- installs the greetd configuration and kiosk session wrapper at `/usr/local/bin/photoframe-session`, and
- deploys the `photoframe-*` systemd units, enabling them and starting them when the corresponding binaries are present in `/opt/photo-frame`.

Run the script before building the application so the toolchain and dependencies are ready. After the application deploy completes, the deploy pipeline installs/updates the app unit files and starts the kiosk services automatically; re-running the system script is optional and safe but no longer required just to bring services up.

## Diagnose kiosk health

Inspect the greetd session, kiosk user, and display-manager wiring:

```bash
sudo ./setup/system/tools/diagnostics.sh
```

Run this from the device when display login fails or the kiosk session will not start; it flags missing packages, disabled units, and other common misconfigurations.

## Application deployment

Build and install release artifacts from an unprivileged shell:

```bash
./setup/application/deploy.sh
```

The application stage compiles the workspace, stages binaries and documentation under `setup/application/build/stage`, ensures the kiosk service user exists, and installs the artifacts into `/opt/photo-frame`.

## systemd helper library

Provisioning and diagnostics scripts reuse a shared wrapper library at `setup/lib/systemd.sh`. Source it from modules using the canonical pattern:

```bash
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib/systemd.sh
source "${SCRIPT_DIR}/../lib/systemd.sh"
```

The library exposes helpers to keep systemd orchestration consistent:

- `systemd_available` – detect whether `systemctl` can be used without aborting the caller.
- `systemd_daemon_reload`, `systemd_enable_unit`, `systemd_start_unit`, `systemd_restart_unit`, `systemd_stop_unit` – manage unit lifecycle.
- `systemd_enable_now_unit`, `systemd_disable_unit`, `systemd_disable_now_unit`, `systemd_mask_unit`, `systemd_unmask_unit`, `systemd_set_default_target` – configure unit state and boot targets.
- `systemd_unit_exists`, `systemd_is_active`, `systemd_is_enabled`, `systemd_status`, `systemd_unit_property` – inspect unit presence and health.
- `systemd_install_unit_file`, `systemd_install_dropin`, `systemd_remove_dropins` – install or prune unit definitions and drop-ins.

## Operator quick reference

- Inspect the running session: `sudo systemctl status greetd`
- Restart helpers: `sudo systemctl restart photoframe-wifi-manager.service`
- Tail kiosk logs: `sudo journalctl -u greetd -f`
- Upload new media: copy manual additions into `/var/lib/photo-frame/photos/local`.
- Configure sync tooling (e.g., rclone) to manage `/var/lib/photo-frame/photos/cloud` when mirroring from remote storage.

The kiosk account is unprivileged; use the `frame` operator account (see `docs/configuration.md`) for maintenance commands.
