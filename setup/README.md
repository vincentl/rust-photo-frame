# Photo Frame Setup Pipeline

This directory houses idempotent provisioning scripts for Raspberry Pi photo frame deployments. Each script can be re-run safely after OS updates or image refreshes.

## One-command kiosk bootstrap (Trixie)

Provision a Raspberry Pi OS Trixie kiosk with the greetd + cage workflow:

```bash
sudo ./setup/kiosk-trixie.sh
```

The script performs the following actions:

- verifies the OS is Raspberry Pi OS Trixie,
- installs `greetd`, `cage`, `mesa-vulkan-drivers`, `vulkan-tools`, `wlr-randr`, and `wayland-protocols`,
- ensures the `kiosk` user exists with `/usr/sbin/nologin` and belongs to the `render`, `video`, and `input` groups,
- writes `/etc/greetd/config.toml` to launch `cage -s -- /opt/photo-frame/bin/rust-photo-frame /var/lib/photo-frame/config/config.yaml` on virtual terminal 1,
- disables conflicting display managers (`gdm3`, `sddm`, `lightdm`), enables `greetd.service` as the system `display-manager.service`, sets the default boot target to `graphical.target`, and masks `getty@tty1.service` to avoid VT races, and
- deploys and enables the supporting `photoframe-*` helper units.

Re-run the script after OS updates to reapply package dependencies or repair systemd state; it is safe and idempotent.

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
- Upload new media: copy into `/var/lib/photo-frame/photos`

The kiosk account is unprivileged; use the `frame` operator account (see `docs/configuration.md`) for maintenance commands.
