# Photo Frame Setup Pipeline

This directory houses idempotent provisioning scripts for Raspberry Pi photo frame deployments. Each script can be re-run safely after OS updates or image refreshes.

## Bootstrap the operating system (Trixie)

Provision a Raspberry Pi OS Trixie kiosk and install shared dependencies with:

```bash
sudo ./setup/bootstrap/run.sh
```

The bootstrap pipeline is ordered via numbered modules and performs the following actions:

- installs base apt packages (graphics stack, build tools, networking utilities),
- installs or updates the system-wide Rust toolchain under `/usr/local/cargo`,
- replaces the legacy swapfile with `systemd-zram-generator` configured for a half-RAM zram device,
- verifies the host is running Debian 13 (Trixie) and applies Raspberry Pi 5 boot firmware tweaks (set `ENABLE_4K_BOOT=0` before running if you need to skip the HDMI 4K60 profile),
- ensures the locked `kiosk` user exists with membership in the `render`, `video`, and `input` groups and provisions runtime directories and polkit policy,
- installs the greetd configuration and kiosk session wrapper at `/usr/local/bin/photoframe-session`, and
- deploys the `photoframe-*` systemd units, enabling them and starting them when the corresponding binaries are present in `/opt/photo-frame`.

Run the script before building the application so the toolchain and dependencies are ready. Re-run it after `./setup/app/run.sh` completes to let the kiosk services start once the binaries are installed. The modules are idempotent, so repeated invocations are safe.

## Diagnose kiosk health

Inspect the greetd session, kiosk user, and display-manager wiring:

```bash
sudo ./setup/bootstrap/tools/diagnostics.sh
```

Run this from the device when display login fails or the kiosk session will not start; it flags missing packages, disabled units, and other common misconfigurations.

## Application deployment

Build and install release artifacts from an unprivileged shell:

```bash
./setup/app/run.sh
```

The app stage compiles the workspace, stages binaries and documentation under `setup/app/build/stage`, ensures the kiosk service user exists, and installs the artifacts into `/opt/photo-frame`.

## Operator quick reference

- Inspect the running session: `sudo systemctl status greetd`
- Restart helpers: `sudo systemctl restart photoframe-wifi-manager.service`
- Tail kiosk logs: `sudo journalctl -u greetd -f`
- Upload new media: copy manual additions into `/var/lib/photo-frame/photos/local`.
- Configure sync tooling (e.g., rclone) to manage `/var/lib/photo-frame/photos/cloud` when mirroring from remote storage.

The kiosk account is unprivileged; use the `frame` operator account (see `docs/configuration.md`) for maintenance commands.
