# Operations SOP

This document captures routine operational procedures for the kiosk deployment.

## Viewing runtime logs

The kiosk session launches the photo frame through `cage` and pipes stdout/stderr into journald with `systemd-cat`. All runtime log lines carry the identifier `rust-photo-frame` and default to the `info` level.

To follow the live log stream:

```bash
sudo journalctl -t rust-photo-frame -f
```

Use `Ctrl+C` to stop tailing.

## Increasing log verbosity

When additional detail is required, edit `/etc/greetd/config.toml` so the launch command exports a higher `RUST_LOG` level. For example, swap the default `RUST_LOG=info` for `RUST_LOG=debug`:

```toml
[default_session]
command = "cage -s -- systemd-cat --identifier=rust-photo-frame env RUST_LOG=debug /opt/photo-frame/bin/rust-photo-frame /var/lib/photo-frame/config/config.yaml"
user = "kiosk"
```

Apply the change by restarting the kiosk session:

```bash
sudo systemctl restart greetd.service
```

Revert the command to `RUST_LOG=info` once troubleshooting is complete to reduce noise in the journal.
