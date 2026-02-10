# Kiosk Stack Notes

This appendix contains detailed kiosk provisioning behavior, idempotency notes, and deeper verification references for the greetd + Sway stack.

For the primary operator path, use [`kiosk.md`](kiosk.md).

## Provisioning behavior

Run system provisioning on a fresh Debian 13 (Trixie) or Raspberry Pi OS Trixie image:

```bash
sudo ./setup/system/install.sh
```

The script performs these kiosk-related actions:

- Verifies `/etc/os-release` reports `VERSION_CODENAME=trixie`.
- Applies Raspberry Pi 5 boot tweaks (set `ENABLE_4K_BOOT=0` to skip the 4K60 profile).
- Installs Wayland/kiosk dependencies including `greetd`, `sway`, `swaybg`, `swayidle`, `swaylock`, `wayland-protocols`, and supporting packages.
- Ensures `dbus-run-session` tooling is present (`dbus` and `dbus-user-session`).
- Creates locked `kiosk` user and ensures membership in `video`, `render`, and `input`.
- Provisions `/run/photo-frame` as `kiosk:kiosk` with mode `0770` plus tmpfiles entry for boot-time creation.
- Installs `/usr/local/bin/photoframe-session`.
- Writes `/etc/greetd/config.toml` so tty1 runs `photoframe-session` as `kiosk`.
- Disables competing display managers (`gdm3`, `sddm`, `lightdm`), enables `greetd` as `display-manager`, and masks `getty@tty1.service`.
- Deploys helper units (`photoframe-wifi-manager.service`, `buttond.service`, `photoframe-sync.timer`) and enables units when binaries exist.

## Idempotent reruns

Re-running provisioning after OS/app updates is expected and safe. On systems already configured for greetd/tty1, logs include idempotent status lines such as:

```text
[install.sh] INFO: getty@tty1.service already masked; skipping disable/mask
[install.sh] INFO: greetd.service is static; nothing to enable
```

## Canonical greetd configuration

`/etc/greetd/config.toml` should match:

```toml
[terminal]
vt = 1

[default_session]
command = "/usr/local/bin/photoframe-session"
user = "kiosk"
```

## Deep verification checklist

```bash
grep VERSION_CODENAME /etc/os-release
systemctl status greetd
systemctl status display-manager
journalctl -u greetd -b
```

Expected:

- `greetd` reports `active (running)`.
- Unit command line includes `/usr/local/bin/photoframe-session`.
- greetd journal includes kiosk session and slideshow startup logs.

## Kiosk stack assumptions

- Sway config path: `/usr/local/share/photoframe/sway/config`
- Kiosk launch path: `greetd -> photoframe-session -> dbus-run-session -> seatd-launch -> sway`
- Kiosk flow does not require login overrides, tty autologin hacks, or alternate display manager glue.
