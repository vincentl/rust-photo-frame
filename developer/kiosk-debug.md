# Kiosk Debug Playbook

This document is for developer-only debugging on a live Pi when normal operator runbooks are not enough.

For routine operations use [`../docs/sop.md`](../docs/sop.md).

## Manual debug launch (advanced)

In normal operation the `photoframe` app is started by greetd via `/usr/local/bin/photoframe-session`.
For low-level debugging you can run a standalone Sway session as `kiosk`.

### Prerequisite

```bash
sudo loginctl enable-linger kiosk
```

### Launch command

```bash
sudo -u kiosk \
  XDG_RUNTIME_DIR="/run/user/$(id -u kiosk)" \
  dbus-run-session \
sway -c /usr/local/share/photoframe/sway/config
```

## Debugging without journald capture

Print logs to stdout:

```bash
sudo -u kiosk \
  XDG_RUNTIME_DIR="/run/user/$(id -u kiosk)" \
  dbus-run-session \
  env PHOTOFRAME_LOG=stdout \
  env RUST_LOG='photoframe::tasks::viewer=debug,info' \
  sway -c /usr/local/share/photoframe/sway/config
```

Write logs to a file:

```bash
sudo -u kiosk \
  XDG_RUNTIME_DIR="/run/user/$(id -u kiosk)" \
  dbus-run-session \
  env PHOTOFRAME_LOG='file:/var/tmp/photoframe.log' \
  env RUST_LOG='photoframe::tasks::viewer=debug,info' \
  sway -c /usr/local/share/photoframe/sway/config

sudo tail -f /var/tmp/photoframe.log
```

## Overlay takeover test (dev)

Use this to verify overlay focus/fullscreen behavior before Wi-Fi recovery testing.

Install helper (one-time):

```bash
sudo install -D -m 0755 developer/overlay-test.sh /usr/local/bin/overlay-test
sudo install -D -m 0644 developer/systemd/wifi-overlay-test.service /etc/systemd/system/wifi-overlay-test.service
sudo systemctl daemon-reload
```

Run test:

```bash
sudo systemctl start wifi-overlay-test.service
sudo -u kiosk bash developer/overlay-test.sh status
```

Hide overlay:

```bash
sudo -u kiosk bash developer/overlay-test.sh hide
```

Troubleshooting:

- Ensure Sway session is up: `systemctl status greetd`
- Check Sway IPC socket: `sudo sh -lc 'uid=$(id -u kiosk); ls "/run/user/$uid"/sway-ipc.*.sock'`
- Review service journal: `journalctl -u wifi-overlay-test.service -n 50 --no-pager`

## Kiosk shell with live Sway environment

Open an interactive shell as `kiosk` with compositor environment exported:

```bash
sudo sh -lc '
  uid=$(id -u kiosk)
  RUNDIR="/run/user/$uid"
  SWAYSOCK="$(ls "$RUNDIR"/sway-ipc.*.sock | head -1)"
  WAYLAND_DISPLAY="$(basename "$(ls "$RUNDIR"/wayland-* | head -1)")"
  exec sudo --preserve-env=RUNDIR,SWAYSOCK,WAYLAND_DISPLAY \
    -u kiosk env XDG_RUNTIME_DIR="$RUNDIR" SWAYSOCK="$SWAYSOCK" WAYLAND_DISPLAY="$WAYLAND_DISPLAY" bash -l
'
```

Inside that shell:

- Verify env: `echo "$XDG_RUNTIME_DIR" "$SWAYSOCK" "$WAYLAND_DISPLAY"`
- Test IPC: `swaymsg -s "$SWAYSOCK" -t get_tree >/dev/null && echo ok || echo fail`
