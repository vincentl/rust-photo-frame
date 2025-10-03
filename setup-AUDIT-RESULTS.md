# Setup Audit Results

## Inventory Overview
- **setup/app/run.sh** – **Modify**: removed `DRY_RUN` plumbing and preserved module orchestration.
- **setup/app/modules/10-build.sh** – **Modify**: build now always executes `cargo build` and rejects root-owned targets.
- **setup/app/modules/20-stage.sh** – **Modify**: rebuilt staging pipeline, adds photo-buttond binary, and syncs new systemd/pam assets.
- **setup/app/modules/30-install.sh** – **Modify**: installs staged assets into `/opt/photo-frame` and seeds `/var/lib/photo-frame/config` without touching deprecated `/opt/.../var`.
- **setup/app/modules/35-fonts.sh** – **Modify**: always installs fonts, refreshes cache, and drops dry-run noise.
- **setup/app/modules/40-systemd.sh** – **Delete**: superseded by `setup/system/install-systemd-units.sh`.
- **setup/app/modules/50-postcheck.sh** – **Modify**: validates new runtime layout and photoframe services.
- **setup/files/bin/photo-safe-shutdown** – **Keep (relocated)**: moved into the shared helpers directory for staging.
- **setup/files/systemd/** (sync-photos, wifi-manager) – **Delete**: replaced with standardized photoframe units.
- **setup/modules/buttond/** – **Delete**: button daemon now shipped via staged binary + new systemd unit.
- **setup/system/modules/** & `setup/system/run.sh` – **Delete**: legacy modular framework removed.
- **setup/packages/install-apt-packages.sh** – **Add**: consolidated apt dependencies, including `acl` and `rclone`.
- **setup/system/create-users-and-perms.sh** – **Add**: provisions `kiosk`/`frame`, directories, and ACLs.
- **setup/system/configure-networkmanager.sh** – **Add**: installs a minimal polkit rule for NetworkManager access.
- **setup/system/install-sudoers.sh** – **Add**: installs `/etc/sudoers.d/photoframe` with restricted commands.
- **setup/system/install-systemd-units.sh** – **Add**: installs new units, PAM drop-in, enables services, and prunes legacy units.
- **setup/system/cage@.service** – **Add**: template cage unit running as `kiosk` on real TTY sessions.
- **setup/system/pam.d/cage** – **Add**: PAM stack for cage sessions via logind.
- **setup/system/units/photoframe-*.service/.timer** – **Add**: kiosk-scoped wifi manager, sync timer, and button daemon units.
- **setup/system/sudoers/photoframe** – **Add**: sudoers policy for the `frame` operator.
- **setup/migrate/legacy-cleanup.sh** – **Add**: removes deprecated unit files and directories.
- **setup/README.md** – **Add**: documents the new provisioning flow and operator responsibilities.
- **docs/software.md** – **Modify**: documents new runtime paths and drops `DRY_RUN` guidance.

## Breaking Changes
- `DRY_RUN` environment flag and all related stubs have been removed.
- All legacy `setup/system/modules/*` scripts, `setup/system/run.sh`, and buttond installer have been deleted.
- Old unit names (`sync-photos.service`, `wifi-manager.service`, `photo-buttond.service`) are retired in favor of `photoframe-*` units.
- Runtime data no longer lives under `/opt/photo-frame/var`; it now resides in `/var/lib/photo-frame`.

## New Standard Layout
- `/opt/photo-frame` – `root:root`, `0755`; contains release binaries in `bin/`, configs in `etc/`, docs/assets in `share/`.
- `/var/lib/photo-frame` – `kiosk:kiosk`, `0750`; default ACL grants `frame` full access.
  - `/var/lib/photo-frame/photos` – `kiosk:kiosk`, `0770`, ACLs grant `frame` read/write (default + access ACL).
  - `/var/lib/photo-frame/config` – same ownership/ACL as photos for editable configs.
- `/var/cache/photo-frame` & `/var/log/photo-frame` – `kiosk:kiosk`, `0750`; logs expose read-only ACL to `frame`.
- `kiosk` user: member of `render`, `video`, `input`; shell `/usr/sbin/nologin`; home `/var/lib/photo-frame`.
- `frame` user: regular login shell with limited sudo via `/etc/sudoers.d/photoframe`.
- Canonical services: `cage@tty1.service`, `photoframe-wifi-manager.service`, `photoframe-sync.timer`, `photoframe-buttond.service` all run as `kiosk`.

## How to Operate (frame user)
- Start/stop compositor: `sudo systemctl restart cage@tty1.service`.
- Manage helpers: `sudo systemctl restart photoframe-wifi-manager.service`, `sudo systemctl start photoframe-sync.service`.
- Tail logs: `sudo journalctl -u cage@tty1.service -f` or `sudo journalctl -u photoframe-wifi-manager.service -b -f`.
- Deploy photos/config: copy into `/var/lib/photo-frame/photos/` or `/var/lib/photo-frame/config/`, ownership handled by ACLs.
- On-demand sync: `sudo systemctl start photoframe-sync.service`.

## Assumptions
- NetworkManager and polkit are present; the polkit rule relies on standard Debian paths.
- `kmscube` and `rclone` packages are available from Debian repositories.
- Systemd-logind is managing TTYs; no other display manager competes for `tty1`.

## Test Checklist
Run these commands after provisioning:

1. **Users & groups**
   - `id kiosk`
   - `id frame`
2. **ACLs & permissions**
   - `getfacl -p /var/lib/photo-frame/photos`
   - `sudo -u frame touch /var/lib/photo-frame/photos/test-upload && sudo -u kiosk stat /var/lib/photo-frame/photos/test-upload && sudo rm /var/lib/photo-frame/photos/test-upload`
3. **Systemd & sessions**
   - `systemctl status cage@tty1.service`
   - `loginctl`
   - `journalctl -u photoframe-wifi-manager.service -b | tail -n 50`
4. **GPU access**
   - `ls -l /dev/dri`
   - `sudo -u kiosk WAYLAND_DISPLAY=wayland-0 kmscube`
5. **Service control from frame account**
   - `sudo -iu frame -- systemctl restart photoframe-sync.service`
   - `sudo -iu frame -- journalctl -u photoframe-sync.timer -b -f`
6. **Legacy cleanup & regressions**
   - `sudo ./setup/migrate/legacy-cleanup.sh`
   - `systemctl status photoframe-buttond.service`
7. **No dry-run leftovers**
   - `rg -n "DRY_RUN|dryrun|dry-run" -S .`

