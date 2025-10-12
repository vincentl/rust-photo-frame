# Operations SOP

This document captures routine operational procedures for the kiosk deployment.

## Viewing runtime logs

The kiosk session launches the photo frame through Sway and pipes stdout/stderr into journald with `systemd-cat`. All runtime log lines carry the identifier `rust-photo-frame` and default to the `info` level.

To follow the live log stream:

```bash
sudo journalctl -t rust-photo-frame -f
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

If you prefer a reusable helper, wrap the sequence in a shell alias or script (e.g. `restart-greetd()`); just keep the pause in place when you need to refresh the viewer.
