# Debian 13 Wayland Kiosk

The photo frame boots straight into the Wayland app using a greetd-managed session on Debian 13 (trixie). greetd launches a dedicated Sway session on virtual terminal 1 and runs the photo frame as the `kiosk` user—no display manager shims or PAM templates are required.

This document is the kiosk stack reference (greetd/Sway wiring and expected system state). For fresh install steps use [`software.md`](software.md), and for day-2 operations use [`sop.md`](sop.md).

## Fast path

Use this sequence for a quick kiosk bring-up:

```bash
sudo ./setup/system/install.sh
./setup/application/deploy.sh
./setup/tools/verify.sh
systemctl status greetd
systemctl status display-manager
journalctl -u greetd -b
```

If `greetd` is active and `photoframe-session` appears in the unit command line, kiosk wiring is healthy.

For full provisioning behavior, idempotent rerun notes, and deep verification references, use [`kiosk-notes.md`](kiosk-notes.md).

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

## Advanced notes

For provisioning internals, idempotent rerun details, and deep verification checks, use [`kiosk-notes.md`](kiosk-notes.md).
