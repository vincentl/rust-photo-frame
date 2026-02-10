# Debian 13 Wayland Kiosk

The photo frame boots straight into the Wayland app using a greetd-managed session on Debian 13 (trixie). greetd launches a dedicated Sway session on virtual terminal 1 and runs the photo frame as the `kiosk` user—no display manager shims or PAM templates are required.

This document is the kiosk stack reference (greetd/Sway wiring and expected system state). For fresh install steps use [`software.md`](software.md), and for day-2 operations use [`sop.md`](sop.md).

## Provisioning sequence

Use this sequence for a standard kiosk bring-up:

```bash
sudo ./setup/system/install.sh
./setup/application/deploy.sh
./setup/tools/verify.sh
systemctl status greetd
systemctl status display-manager
journalctl -u greetd -b
```

If `greetd` is active and `photoframe-session` appears in the unit command line, kiosk wiring is healthy.

## Provisioning behavior

`sudo ./setup/system/install.sh` performs these kiosk-related actions:

- Verifies `/etc/os-release` reports `VERSION_CODENAME=trixie`.
- Applies Raspberry Pi 5 boot tweaks (set `ENABLE_4K_BOOT=0` to skip the 4K60 profile).
- Installs Wayland/kiosk dependencies including `greetd`, `sway`, `swaybg`, `swayidle`, `swaylock`, and `wayland-protocols`.
- Ensures `dbus-run-session` tooling is present (`dbus` and `dbus-user-session`).
- Creates locked `kiosk` user and ensures membership in `video`, `render`, and `input`.
- Provisions `/run/photo-frame` as `kiosk:kiosk` with mode `0770` plus tmpfiles entry for boot-time creation.
- Installs `/usr/local/bin/photoframe-session`.
- Writes `/etc/greetd/config.toml` so tty1 runs `photoframe-session` as `kiosk`.
- Disables competing display managers (`gdm3`, `sddm`, `lightdm`), enables `greetd` as `display-manager`, and masks `getty@tty1.service`.
- Deploys helper units (`photoframe-wifi-manager.service`, `buttond.service`, `photoframe-sync.timer`) and enables units when binaries exist.

## Expected state

`/etc/greetd/config.toml` should include:

```toml
[terminal]
vt = 1

[default_session]
command = "/usr/local/bin/photoframe-session"
user = "kiosk"
```

- The wrapper launches Sway via `dbus-run-session`/`seatd-launch`.
- Sway config lives at `/usr/local/share/photoframe/sway/config` and handles fullscreen/cursor behavior.
- `greetd` owns tty1; no autologin hacks or alternate display managers are required.

## Operations quick reference

- Day-2 runtime commands (restart, logs, control socket): [`sop.md`](sop.md)
- Advanced Sway/kiosk-shell debug workflows: [`../developer/kiosk-debug.md`](../developer/kiosk-debug.md)

No `display-manager.service`, login overrides, or tty autologin hacks are required. The greetd unit owns kiosk launch entirely.

## Idempotent reruns

Re-running provisioning after OS/app updates is expected and safe. On systems already configured for greetd/tty1, logs include idempotent status lines such as:

```text
[install.sh] INFO: getty@tty1.service already masked; skipping disable/mask
[install.sh] INFO: greetd.service is static; nothing to enable
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
