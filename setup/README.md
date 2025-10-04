# Photo Frame Setup Pipeline

This directory houses idempotent provisioning scripts for Raspberry Pi photo
frame deployments. Each script can be re-run safely after OS updates or image
refreshes.

## One-command kiosk bootstrap (Trixie)

Provision a Raspberry Pi OS Trixie kiosk with the canonical Cage + systemd
recipe:

```bash
sudo ./setup/kiosk-trixie.sh --user kiosk --app /usr/local/bin/photo-app
```

The script performs the following actions:

- verifies the OS is Raspberry Pi OS Trixie,
- installs `cage`, `seatd`, and `plymouth` and enables the seatd service,
- ensures the `kiosk` user exists and belongs to the `render`, `video`, and
  `input` groups,
- installs the templated `cage@.service`, PAM stack, and supporting
  `photoframe-*` units,
- disables conflicting display managers and `getty@tty1.service`,
- removes `console=tty1` from `/boot/firmware/cmdline.txt`, and
- boots the system into `graphical.target` with Cage on `tty1`.

Override the kiosk user or application binary path with the `--user` and
`--app` options as needed.

## Package provisioning helpers

For development images you may still want the extended toolchain provided by
`setup/packages/run.sh`:

```bash
sudo ./setup/packages/run.sh
```

This installs the Rust toolchain under `/usr/local/cargo` and pulls additional
utilities (e.g., `rclone`, `kmscube`) useful during development.

## Application deployment

Build and install release artifacts from an unprivileged shell:

```bash
./setup/app/run.sh
```

The app stage compiles the workspace, stages binaries and documentation under
`setup/app/build/stage`, ensures the kiosk service user exists, and installs
the artifacts into `/opt/photo-frame`.

## Operator quick reference

- Inspect the running session: `sudo systemctl status cage@tty1.service`
- Restart helpers: `sudo systemctl restart photoframe-wifi-manager.service`
- Tail compositor logs: `sudo journalctl -u cage@tty1.service -f`
- Upload new media: copy into `/var/lib/photo-frame/photos`

The kiosk account is unprivileged; use the `frame` operator account (see
`docs/configuration.md`) for maintenance commands.
