# Operations SOP

This document captures routine operational procedures for the kiosk deployment.

## Viewing runtime logs

The kiosk session launches the photo frame through Sway and pipes stdout/stderr into journald with `systemd-cat`. All runtime log lines carry the identifier `photo-frame` and default to the `info` level.

To follow the live log stream:

```bash
sudo journalctl -t photo-frame -f
```

Use `Ctrl+C` to stop tailing.

## Increasing log verbosity

When additional detail is required, edit `/etc/greetd/config.toml` so the launch command exports a higher `RUST_LOG` level. For example, swap the default `RUST_LOG=info` for `RUST_LOG=debug`:

```toml
[default_session]
command = "/usr/local/bin/photoframe-session"
user = "kiosk"
```

Apply the change by bouncing the kiosk session:

```bash
sudo systemctl stop greetd.service
sleep 1
sudo systemctl start greetd.service
```

`systemctl restart` tends to race with logind releasing tty1/DRM to the new session; inserting a short pause keeps the relaunch reliable. Revert the command to `RUST_LOG=info` once troubleshooting is complete to reduce noise in the journal.

## Starting, stopping, and restarting the viewer

- **Stop**: `sudo systemctl stop greetd.service` - immediately tears down the kiosk session and blanks the display.
- **Start**: `sudo systemctl start greetd.service` - brings greetd back on tty1, which in turn launches Sway and the photo frame.
- **Restart**: `sudo systemctl stop greetd.service && sleep 1 && sudo systemctl start greetd.service` - give logind a moment to release the seat before greetd grabs it again.

## Manual Debug Launch (Advanced)

In normal operation, the `photo-frame` application is started automatically by **greetd** via the `photoframe-session` wrapper. However, for debugging or development purposes, you may wish to launch it manually under the `kiosk` account within a Wayland session.

### Prerequisites

Ensure the `kiosk` user is permitted to run lingering user sessions (so its runtime directory is preserved):

```bash
sudo loginctl enable-linger kiosk
```

### Launch Command

Use the following command to start a standalone Sway session and run the photo-frame stack manually:

```bash
sudo -u kiosk \
  XDG_RUNTIME_DIR="/run/user/$(id -u kiosk)" \
  dbus-run-session \
sway -c /usr/local/share/photoframe/sway/config
```

### Debugging without journald capture

By default, the Sway config launches the app via `/usr/local/bin/photo-frame`, which pipes logs to journald using `systemd-cat`. For iterative debugging you can direct logs to stdout or a file without editing the binary:

- To print logs to the terminal (stdout) and control verbosity with `RUST_LOG`:

```bash
sudo -u kiosk \
  XDG_RUNTIME_DIR="/run/user/$(id -u kiosk)" \
  dbus-run-session \
  env PHOTOFRAME_LOG=stdout \
  env RUST_LOG='photo_frame::tasks::viewer=debug,info' \
  sway -c /usr/local/share/photoframe/sway/config
```

- To write logs to a file you can tail:

```bash
sudo -u kiosk \
  XDG_RUNTIME_DIR="/run/user/$(id -u kiosk)" \
  dbus-run-session \
  env PHOTOFRAME_LOG='file:/var/tmp/photo-frame.log' \
  env RUST_LOG='photo_frame::tasks::viewer=debug,info' \
  sway -c /usr/local/share/photoframe/sway/config

sudo tail -f /var/tmp/photo-frame.log
```

When you’re done, remove the `PHOTOFRAME_LOG` override to return to the default journald capture. You can always watch the kiosk logs via:

```bash
  sudo journalctl -t photo-frame -f
```
