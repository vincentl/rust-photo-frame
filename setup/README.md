# Photo Frame Setup Pipeline

The setup tooling provisions a Raspberry Pi for the kiosk workload using two
service accounts:

- `kiosk` runs the compositor, the slideshow process, and helper daemons.
- `frame` is the operator account used to deploy content, inspect logs, and
  restart services.

The scripts in this directory are idempotent and can be re-run after package
updates or image refreshes.

## System provisioning

Run the system scripts as root (or with `sudo`). You can execute each script
individually in the following order on a fresh Bookworm install, or run the
wrapper to perform the full provisioning sequence:

```bash
sudo ./setup/system/run.sh
```

Pass `--with-legacy-cleanup` to also execute the optional migration step after
provisioning.

Individual scripts are available at:

1. `./setup/system/install-packages.sh`
   - Installs Cage, GPU/video dependencies, build toolchains, and helper
     utilities such as `rclone`.
2. `./setup/system/create-users-and-perms.sh`
   - Ensures the `kiosk` and `frame` users exist, adds `kiosk` to the
     `render`, `video`, and `input` groups, and prepares the runtime
     directories:
     - `/opt/photo-frame/{bin,etc,share}` owned by `root:root`.
     - `/var/lib/photo-frame`, `/var/cache/photo-frame`, and
       `/var/log/photo-frame` owned by `kiosk:kiosk`.
     - `/var/lib/photo-frame/photos` and `/var/lib/photo-frame/config` grant
       read/write ACLs to both `kiosk` and `frame`.
3. `./setup/system/configure-networkmanager.sh`
   - Installs a polkit rule so the `kiosk` user can control NetworkManager
     without broad sudo or extra groups.
4. `./setup/system/install-sudoers.sh`
   - Installs `/etc/sudoers.d/photoframe` so `frame` can manage photoframe
     services and read their logs.
5. `./setup/system/install-systemd-units.sh`
   - Copies the standard units (`cage@.service`, `photoframe-*`) and PAM
     drop-ins into `/etc`, enables them, and removes legacy unit files.
6. (Optional) `sudo ./setup/migrate/legacy-cleanup.sh`
   - Removes unit files and directories from pre-kiosk builds.

## Application deployment

Build and install the app artifacts from a non-root shell:

```bash
./setup/app/run.sh
```

The app stage builds the workspace, stages binaries and assets under
`setup/app/build/stage`, and installs them into `/opt/photo-frame`.
Default configuration files live in `/opt/photo-frame/etc`, while writable
state is stored under `/var/lib/photo-frame`.

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
