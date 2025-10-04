# Raspberry Pi OS Bookworm Kiosk

This guide documents the canonical Raspberry Pi 5 kiosk stack for Bookworm: a
headless boot into Cage on `tty1`, managed entirely by systemd.

## Canonical recipe

1. **Packages** – install `cage`, `seatd`, and `plymouth`, then enable
   `seatd.service`.
2. **Kiosk user** – ensure the `kiosk` account exists and belongs to the
   `render`, `video`, and `input` groups.
3. **Systemd** – install the templated `cage@.service` and PAM stack from
   `assets/`, plus the `photoframe-*` helper units. Cage runs the target app
   directly (`ExecStart=/usr/bin/cage /usr/local/bin/photo-app`).
4. **Boot target** – set the default to `graphical.target`, enable
   `cage@tty1.service`, and disable competing display managers and
   `getty@tty1.service`.
5. **Seat/session** – Cage starts After `systemd-logind`; `seatd` provides DRM
   access. The session acquires a real logind seat so Wayland permissions flow
   without sudo.
6. **No console flash** – remove `console=tty1` from `/boot/firmware/cmdline.txt`
   to keep Plymouth (or the splash) visible until Cage starts.

## One-liner setup

Run the provisioning script as root (it re-execs with `sudo` if needed):

```bash
sudo ./setup/kiosk-bookworm.sh --user kiosk --app /usr/local/bin/photo-app
```

Flags:

- `--user` – kiosk account name (default `kiosk`).
- `--app` – binary Cage launches (default `/usr/local/bin/photo-app`).

The script is idempotent; re-running it updates packages, rewrites the Cage unit
from the template, and reapplies group membership without duplicating entries.

## Installed assets

- `assets/systemd/cage@.service` – canonical unit template (renders with the
  selected user/app path).
- `assets/pam/cage` – PAM stack for kiosk sessions.
- `assets/systemd/photoframe-*.service|.timer` – wifi manager, sync timer, and
  button daemon units copied into `/etc/systemd/system/`.

## Acceptance checklist

After provisioning on real hardware:

- **Cold boot** – Plymouth (if installed) stays on screen until Cage launches
  the photo app; no `tty1` text flash.
- **Crash restart** – `sudo systemctl kill -s SIGKILL cage@tty1` respawns the
  kiosk automatically.
- **Permissions** – `loginctl` shows a seat for the kiosk session and the app
  has GPU/input access without sudo.
- **Idempotency** – running the setup script twice produces no additional
  changes.
- **Logging** – `journalctl -u cage@tty1.service` contains app logs without PAM
  or seat errors.
- **Rollback** – restore `console=tty1` and disable `cage@tty1.service` to bring
  back the interactive getty.

## Migration notes

The following legacy paths were removed in favour of the single Cage workflow:

- `setup/system/**` – replaced by `setup/kiosk-bookworm.sh`.
- `setup/migrate/legacy-cleanup.sh` – superseded by the idempotent installer.
- `setup/system/cage@.service` and `setup/system/pam.d/cage` – consolidated in
  `assets/systemd/` and `assets/pam/`.
- `setup/system/units/photoframe-*` – moved to `assets/systemd/` for staging and
  provisioning.

All legacy flows have been removed; rely solely on the Bookworm script outlined
above.
