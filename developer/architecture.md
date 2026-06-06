# Architecture & kiosk-stack debugging

How the provisioning pipeline is structured, and how to debug the kiosk stack on
a live Pi. For the test plan and release gate see [testing.md](testing.md).

---

## Setup script architecture

The provisioning pipeline lives under `setup/` and is fully idempotent — every stage can be re-run safely after OS updates or image refreshes.

### Standard deployment flow

```bash
./setup/install-all.sh                # all-in-one
./setup/tools/verify.sh               # post-install health check
sudo ./setup/system/tools/diagnostics.sh
```

`install-all.sh` provisions the OS (sudo), builds and deploys the app as your unprivileged user, and activates kiosk services. Set `CARGO_BUILD_JOBS` to cap build parallelism on low-memory devices.

### System provisioning (Trixie only)

```bash
sudo ./setup/system/install.sh
```

Run before building so toolchain and kiosk dependencies are ready. `install.sh` executes numbered modules under `setup/system/modules/`:

- `10-apt-packages.sh` — graphics stack, NetworkManager, Sway, build tools.
- `20-rust.sh` — system-wide Rust toolchain under `/usr/local/cargo`.
- `40-kiosk-user.sh` — locked `kiosk` user, runtime directories, polkit rule.
- `50-greetd.sh` — Sway session wrapper greetd launches on tty1.
- `60-systemd.sh` — `photoframe-wifi-manager.service`, `buttond.service`, `photoframe-sync.timer`.

Plus `dtoverlay=vc4-kms-v3d-pi5,cma-512` boot config for GPU CMA, swapfile replaced with `systemd-zram-generator` (half-RAM zram), Pi 5 firmware tweaks (`ENABLE_4K_BOOT=0` to skip the 4K60 profile).

### Application deployment

```bash
./setup/application/deploy.sh
```

Compiles the workspace, stages binaries and documentation under `setup/application/build/stage`, ensures the kiosk service user exists, and installs artifacts into `/opt/photoframe`. Modules:

- `10-build.sh` — compiles `wifi-manager`, `photoframe`, `buttond` in release mode (never as root).
- `20-stage.sh` — stages binaries, config templates, wordlist, and `docs/` content.
- `30-install.sh` — installs into `/opt/photoframe`; seeds `/etc/photoframe/config.yaml` if missing.

### Diagnostics

```bash
sudo ./setup/system/tools/diagnostics.sh
```

Inspects the greetd session, kiosk user, and display-manager wiring. Run from the device when display login fails or the kiosk session won't start.

### Shared systemd helper library

Provisioning and diagnostics scripts use `setup/lib/systemd.sh`. Source pattern:

```bash
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib/systemd.sh
source "${SCRIPT_DIR}/../lib/systemd.sh"
```

Helper groups:

- Availability and reload: `systemd_available`, `systemd_daemon_reload`
- Unit lifecycle: `systemd_enable_unit`, `systemd_start_unit`, `systemd_restart_unit`, `systemd_stop_unit`
- Enable/disable/mask: `systemd_enable_now_unit`, `systemd_disable_unit`, `systemd_disable_now_unit`, `systemd_mask_unit`, `systemd_unmask_unit`, `systemd_set_default_target`
- State checks: `systemd_unit_exists`, `systemd_is_active`, `systemd_is_enabled`, `systemd_status`, `systemd_unit_property`
- Unit/drop-in management: `systemd_install_unit_file`, `systemd_install_dropin`, `systemd_remove_dropins`

---

## Debugging the kiosk stack

For developer-only debugging on a live Pi when normal operator runbooks aren't enough.

### Manual debug launch (advanced)

In normal operation `photoframe` is started by greetd via `/usr/local/bin/photoframe-session`. For low-level debugging you can run a standalone Sway session as `kiosk`.

Prerequisite (one-time):

```bash
sudo loginctl enable-linger kiosk
```

Launch:

```bash
sudo -u kiosk \
  XDG_RUNTIME_DIR="/run/user/$(id -u kiosk)" \
  dbus-run-session \
  sway -c /usr/local/share/photoframe/sway/config
```

### Debugging without journald capture

Logs to stdout:

```bash
sudo -u kiosk \
  XDG_RUNTIME_DIR="/run/user/$(id -u kiosk)" \
  dbus-run-session \
  env PHOTOFRAME_LOG=stdout \
  env RUST_LOG='photoframe::tasks::viewer=debug,info' \
  sway -c /usr/local/share/photoframe/sway/config
```

Logs to file:

```bash
sudo -u kiosk \
  XDG_RUNTIME_DIR="/run/user/$(id -u kiosk)" \
  dbus-run-session \
  env PHOTOFRAME_LOG='file:/var/tmp/photoframe.log' \
  env RUST_LOG='photoframe::tasks::viewer=debug,info' \
  sway -c /usr/local/share/photoframe/sway/config

sudo tail -f /var/tmp/photoframe.log
```

### Overlay takeover test

Verify overlay focus/fullscreen behavior before Wi-Fi recovery testing.

One-time install:

```bash
sudo install -D -m 0755 developer/overlay-test.sh /usr/local/bin/overlay-test
sudo install -D -m 0644 developer/systemd/wifi-overlay-test.service /etc/systemd/system/wifi-overlay-test.service
sudo systemctl daemon-reload
```

Run:

```bash
sudo systemctl start wifi-overlay-test.service
sudo -u kiosk bash developer/overlay-test.sh status
sudo -u kiosk bash developer/overlay-test.sh hide
```

Troubleshooting:

- `systemctl status greetd` — Sway session up?
- `sudo sh -lc 'uid=$(id -u kiosk); ls "/run/user/$uid"/sway-ipc.*.sock'` — IPC socket present?
- `journalctl -u wifi-overlay-test.service -n 50 --no-pager`

### Kiosk shell with live Sway environment

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

### Manually simulate a Wi-Fi failure

`developer/suspend-wifi.sh` stashes the active profile keyfile, swaps in a Wi-Fi connection with a deliberately wrong PSK, and tries to activate it. Run from a multiplexer or with `nohup` so it survives SSH drops:

```bash
sudo nohup bash developer/suspend-wifi.sh wlan0 >/tmp/wifi-test.log 2>&1 & disown
```
