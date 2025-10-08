# Debian 13 Wayland Kiosk

The photo frame boots straight into the Wayland app using a greetd-managed session on Debian 13 (trixie). greetd starts `cage` on virtual terminal 1 and runs the photo frame as the dedicated `kiosk` user—no display manager shims or PAM templates are required.

## Provisioning workflow

Run the kiosk installer on a fresh Debian 13 (or Raspberry Pi OS trixie) image:

```bash
sudo ./setup/kiosk/provision-trixie.sh
```

The script performs the following actions:

- verifies `/etc/os-release` reports `VERSION_CODENAME=trixie`;
- installs the Wayland stack required for kiosk mode (`greetd`, `cage`, `mesa-vulkan-drivers`, `vulkan-tools`, `wlr-randr`, `wayland-protocols`, and `socat` for control-socket tooling);
- creates the `kiosk` account with a locked shell and ensures it belongs to the `video`, `render`, and `input` groups;
- provisions `/run/photo-frame` (owned by `kiosk:kiosk`, mode `0770`) and drops an `/etc/tmpfiles.d/photo-frame.conf` entry so the control socket directory exists on every boot;
- writes `/etc/greetd/config.toml` so virtual terminal 1 runs `cage -s -- systemd-cat --identifier=rust-photo-frame env RUST_LOG=info /opt/photo-frame/bin/rust-photo-frame /var/lib/photo-frame/config/config.yaml` as the `kiosk` user;
- disables other display managers (`gdm3`, `sddm`, `lightdm`), enables `greetd.service` as the system `display-manager.service`, sets `graphical.target` as the default boot target, and masks `getty@tty1.service` to keep greetd in control of tty1;
- deploys the `photoframe-*` helper units (wifi manager, sync timer, button daemon); and
- enables `greetd.service`, `photoframe-wifi-manager.service`, `photoframe-buttond.service`, and `photoframe-sync.timer`.

Re-run the script after OS updates to reapply package dependencies or to repair systemd units; it is idempotent.

## Resulting configuration

`/etc/greetd/config.toml` should match the canonical configuration:

```toml
[terminal]
vt = 1

[default_session]
command = "cage -s -- systemd-cat --identifier=rust-photo-frame env RUST_LOG=info /opt/photo-frame/bin/rust-photo-frame /var/lib/photo-frame/config/config.yaml"
user = "kiosk"
```

Check the kiosk stack with the following commands:

```bash
grep VERSION_CODENAME /etc/os-release
systemctl status greetd
systemctl status display-manager
journalctl -u greetd -b
```

`systemctl status greetd` should show the unit as `active (running)` with `/usr/bin/cage -s -- systemd-cat --identifier=rust-photo-frame env RUST_LOG=info /opt/photo-frame/bin/rust-photo-frame /var/lib/photo-frame/config/config.yaml` in the command line. The journal contains both greetd session logs and the photo frame application output.

## Operations quick reference

- Restart the kiosk session: `sudo systemctl stop greetd && sleep 1 && sudo systemctl start greetd`.
- Tail runtime logs: `sudo journalctl -u greetd -f`.
- Pause the slideshow: `sudo systemctl stop greetd` (resume with `start`).
- Inspect display state: `wlr-randr` (installed by the kiosk setup script).
- Send runtime commands to the app (requires the default control socket path):

  ```bash
  echo '{"command": "toggle-state"}' | sudo -u kiosk socat - UNIX-CONNECT:/run/photo-frame/control.sock
  echo '{"command": "set-state", "state": "asleep"}' | sudo -u kiosk socat - UNIX-CONNECT:/run/photo-frame/control.sock
  echo '{"command": "set-state", "state": "awake"}' | sudo -u kiosk socat - UNIX-CONNECT:/run/photo-frame/control.sock
  ```

`systemctl restart greetd` tends to relaunch the unit before logind releases tty1 and the DRM devices from the previous kiosk sess
ion. Stopping, waiting a beat, and then starting avoids that race.

No `display-manager.service`, login overrides, or tty autologin hacks are required—the greetd unit owns kiosk launch entirely.
