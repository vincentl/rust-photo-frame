# Photo Frame Setup Pipeline

This directory houses idempotent provisioning scripts for Raspberry Pi photo frame deployments. Each script can be re-run safely after OS updates or image refreshes.

## One-command kiosk bootstrap (Trixie)

Provision a Raspberry Pi OS Trixie kiosk with the greetd + cage workflow:

```bash
sudo ./setup/kiosk/provision-trixie.sh
```

The script performs the following actions:

- verifies the OS is Raspberry Pi OS Trixie,
- installs `greetd`, `cage`, `mesa-vulkan-drivers`, `vulkan-tools`, `wlr-randr`, and `wayland-protocols`,
- programs the Raspberry Pi boot configuration for HDMI 4K60 output and enables the Pi 5 active cooling overlay (set `ENABLE_4K_BOOT=0` before running if you need different firmware settings),
- ensures the `kiosk` user exists with `/usr/sbin/nologin` and belongs to the `render`, `video`, and `input` groups,
- writes `/etc/greetd/config.toml` to launch `cage -s -- systemd-cat --identifier=rust-photo-frame env RUST_LOG=info /opt/photo-frame/bin/rust-photo-frame /var/lib/photo-frame/config/config.yaml` on virtual terminal 1,
- disables conflicting display managers (`gdm3`, `sddm`, `lightdm`), enables `greetd.service` as the system `display-manager.service`, sets the default boot target to `graphical.target`, and masks `getty@tty1.service` to avoid VT races, and
- deploys and enables the supporting `photoframe-*` helper units.

Re-run the script after OS updates to reapply package dependencies or repair systemd state; it is safe and idempotent.

## Diagnose kiosk health

Inspect the greetd session, kiosk user, and display-manager wiring:

```bash
sudo ./setup/kiosk/diagnostics.sh
```

Run this from the device when display login fails or the kiosk session will not start; it flags missing packages, disabled units, and other common misconfigurations.

## Replace the legacy swapfile with zram

Raspberry Pi OS ships with a disk-backed swapfile that can wear out SD cards
and competes with the photo frame's IO needs. Replace it with compressed
in-memory swap backed by zram during provisioning:

```bash
sudo ./setup/install-zram.sh
```

The helper script disables and removes the default `dphys-swapfile` service,
installs `systemd-zram-generator`, writes `/etc/systemd/zram-generator.conf.d/photoframe.conf`
to size the zram swap device to half of physical RAM (capped at 2 GiB), and
restarts the generated `systemd-zram-setup@zram0.service` unit.

## Package provisioning helpers

For development images you may still want the extended toolchain provided by `setup/packages/run.sh`:

```bash
sudo ./setup/packages/run.sh
```

This installs the Rust toolchain under `/usr/local/cargo` and pulls additional utilities (e.g., `rclone`, `kmscube`) useful during development.

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
