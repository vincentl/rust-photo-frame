# Debian 13 Wayland Kiosk

The photo frame boots straight into the Wayland app using a greetd-managed session on Debian 13 (trixie). greetd launches a dedicated Sway session on virtual terminal 1 and runs the photo frame as the `kiosk` user—no display manager shims or PAM templates are required.

## Provisioning workflow

Run the system provisioning pipeline on a fresh Debian 13 (or Raspberry Pi OS trixie) image:

```bash
sudo ./setup/system/install.sh
```

The script performs the following actions:

- verifies `/etc/os-release` reports `VERSION_CODENAME=trixie` and applies Raspberry Pi 5 boot tweaks (set `ENABLE_4K_BOOT=0` to skip the 4K60 profile),
- installs the Wayland stack required for kiosk mode (`greetd`, `sway`, `swaybg`, `swayidle`, `swaylock`, `mesa-vulkan-drivers`, `vulkan-tools`, `wayland-protocols`, and `socat` for control-socket tooling) alongside general dependencies, including `dbus`/`dbus-user-session` so `dbus-run-session` is present for the kiosk launch wrapper,
- creates the `kiosk` account with a locked shell and ensures it belongs to the `video`, `render`, and `input` groups,
- provisions `/run/photo-frame` (owned by `kiosk:kiosk`, mode `0770`) and drops an `/etc/tmpfiles.d/photo-frame.conf` entry so the control socket directory exists on every boot,
- installs `/usr/local/bin/photoframe-session` and writes `/etc/greetd/config.toml` so virtual terminal 1 runs `/usr/local/bin/photoframe-session` as the `kiosk` user,
- disables other display managers (`gdm3`, `sddm`, `lightdm`), enables `greetd.service` as the system `display-manager.service`, sets `graphical.target` as the default boot target, and masks `getty@tty1.service` to keep greetd in control of tty1,
- deploys the helper units (`photoframe-wifi-manager.service`, `buttond.service`, `photoframe-sync.timer`), and
- enables the kiosk units, starting them automatically once the corresponding binaries exist in `/opt/photo-frame`.

When the script encounters a host that already reserves tty1 for greetd, it now prints idempotent status lines instead of systemd's verbose output. Expect logs like the following on subsequent runs:

```
[install.sh] INFO: getty@tty1.service already masked; skipping disable/mask
[install.sh] INFO: greetd.service is static; nothing to enable
```

Re-run the script after OS or application updates to reapply package dependencies or repair systemd units; it is idempotent.

## Resulting configuration

`/etc/greetd/config.toml` should match the canonical configuration:

```toml
[terminal]
vt = 1

[default_session]
command = "/usr/local/bin/photoframe-session"
user = "kiosk"
```

The wrapper script launches Sway via `dbus-run-session`/`seatd-launch` and relies on `/usr/local/share/photoframe/sway/config` for output, cursor, and application rules. The config pins HDMI-A-1 to 3840×2160@60 Hz, hides the cursor, sets a solid black background with `swaybg`, and spawns the photo frame binary full-screen.

Check the kiosk stack with the following commands:

```bash
grep VERSION_CODENAME /etc/os-release
systemctl status greetd
systemctl status display-manager
journalctl -u greetd -b
```

`systemctl status greetd` should show the unit as `active (running)` with `/usr/local/bin/photoframe-session` in the command line. The journal contains both greetd session logs and the photo frame application output. Sway now handles the fullscreen placement and cursor hiding through the provisioned configuration, so the application no longer needs to manage compositor-specific kiosk tweaks.

## Operations quick reference

- Restart the kiosk session: `sudo systemctl stop greetd && sleep 1 && sudo systemctl start greetd`.
- Tail runtime logs: `sudo journalctl -u greetd -f`.
- Pause the slideshow: `sudo systemctl stop greetd` (resume with `start`).
- Inspect display state: `swaymsg -t get_outputs` (available once the kiosk session is up; over SSH export `SWAYSOCK` before invoking).
- Send runtime commands to the app (requires the default control socket path):

  ```bash
  echo '{"command": "toggle-state"}' | sudo -u kiosk socat - UNIX-CONNECT:/run/photo-frame/control.sock
  echo '{"command": "set-state", "state": "asleep"}' | sudo -u kiosk socat - UNIX-CONNECT:/run/photo-frame/control.sock
  echo '{"command": "set-state", "state": "awake"}' | sudo -u kiosk socat - UNIX-CONNECT:/run/photo-frame/control.sock
  ```

`systemctl restart greetd` tends to relaunch the unit before logind releases tty1 and the DRM devices from the previous kiosk sess
ion. Stopping, waiting a beat, and then starting avoids that race.

No `display-manager.service`, login overrides, or tty autologin hacks are required—the greetd unit owns kiosk launch entirely.
