# Installation Guide

These steps provision a fresh Raspberry Pi OS trixie (Debian 13) image with the
photo frame application and the greetd-based kiosk environment.

## 1. Verify the base image

Confirm the operating system reports the expected codename before installing
anything:

```bash
grep VERSION_CODENAME /etc/os-release
```

The output must include `VERSION_CODENAME=trixie`. Earlier Debian releases are
no longer supported by the setup scripts.

## 2. Install application artifacts

From the repository root run:

```bash
./setup/app/run.sh
```

This compiles the workspace, stages binaries under `/opt/photo-frame`, and
ensures the `kiosk` account owns `/var/lib/photo-frame`.

## 3. Provision the kiosk session

Run the greetd installer with root privileges:

```bash
sudo ./setup/kiosk-trixie.sh
```

The script installs `greetd`, `cage`, `mesa-vulkan-drivers`, `vulkan-tools`,
`wlr-randr`, and `wayland-protocols`; creates the locked `kiosk` user; writes
`/etc/greetd/config.toml`; and enables `greetd` alongside the
`photoframe-*` helpers.

## 4. Validate the kiosk stack

Use the following checks to confirm the kiosk environment is live:

```bash
systemctl status greetd
journalctl -u greetd -b
```

`systemctl status` should report `active (running)` and show `cage -s --
/usr/local/bin/photo-app` in the command line. The journal should contain the
photo frame application logs for the current boot.

Once these checks pass, reboot the device to land directly in the fullscreen
photo frame experience.
