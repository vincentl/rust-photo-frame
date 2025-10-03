# Photo Frame Setup Pipeline

The setup tooling provisions a Raspberry Pi for the kiosk workload using two
service accounts:

- `kiosk` runs the compositor, the slideshow process, and helper daemons.
- `frame` is the operator account used to deploy content, inspect logs, and
  restart services.

The scripts in this directory are idempotent and can be re-run after package
updates or image refreshes.

## Provisioning workflow

The provisioning pipeline now runs in three stages:

1. `sudo ./setup/packages/run.sh` installs the operating system dependencies
   including the Rust toolchain.
2. `./setup/app/run.sh` builds the workspace and installs artifacts into
   `/opt/photo-frame`.
3. `sudo ./setup/system/run.sh` creates service accounts, applies permissions,
   and configures systemd.

Pass `--with-legacy-cleanup` to the system stage to run optional migrations.

See [`docs/software.md`](../docs/software.md) for detailed guidance, expected
outputs, and operational notes for each step.

## Operators: using the `frame` account

The `frame` user does not run the compositor but can manage services without a
password prompt:

- `sudo systemctl status cage@tty1.service`
- `sudo systemctl restart photoframe-wifi-manager.service`
- `sudo journalctl -u photoframe-sync.timer -b -f`

Content uploads should be placed in `/var/lib/photo-frame/photos`. ACLs ensure
both `frame` and `kiosk` can read and write new files.

To view kiosk logs live:

```bash
sudo journalctl -u cage@tty1.service -f
```

For GPU sanity checks, run `kmscube` inside the kiosk session:

```bash
sudo -u kiosk WAYLAND_DISPLAY=wayland-0 kmscube
```
