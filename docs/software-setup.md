# Software Setup

Follow these steps to turn a fresh Raspberry Pi into a self-updating photo frame.

## Configure the Raspberry Pi
1. Flash the latest Raspberry Pi OS (64-bit) onto the SD card.
2. Boot the Pi, connect to Wi-Fi or Ethernet, and apply system updates.
3. Clone this repository and install the toolchain prerequisites (`rustup`, `cargo`, system libraries for SDL2 and GBM).
4. Run `setup/system-setup.sh` to install OS-level dependencies and create the `frame` service user.

## Build and run locally
Use `cargo` to launch the slideshow against your configuration file:

```bash
cargo run --release -- <path/to/config.yaml>
```

Keep the command handy for iterative development. Pair it with the CLI flags below when you need to troubleshoot sequencing, record demo runs, or validate configuration edits.

| Flag                              | When to reach for it                                                                                                 |
| --------------------------------- | -------------------------------------------------------------------------------------------------------------------- |
| `--playlist-now <RFC3339>`        | Freeze the virtual clock to reproduce playlist weights for debugging or long-form tests.                            |
| `--playlist-dry-run <ITERATIONS>` | Generate a textual preview of the weighted queue to check ordering before lighting up the display.                  |
| `--playlist-seed <SEED>`          | Lock the shuffle RNG so repeated runs (dry-run or live) produce the same ordering—handy for demos and regression tests. |

## Provisioning services
The provisioning toolchain under `setup/` installs three long-running services that keep the frame online and the library fresh. Run the system bootstrap (`setup/system-setup.sh`) first, then execute `sudo ./setup/setup.sh` to install the modules below in order.

### Wi-Fi watcher and provisioning flow
- `wifi-watcher` monitors `nmcli` connectivity every 30 s. When the network is online it writes `/run/photo-frame/wifi_up`, starts `photo-app.target`, and ensures the provisioning stack is idle. When connectivity drops it stops the slideshow, spins up a hotspot via `wifi-hotspot@wlan0.service`, launches the captive portal (`wifi-setter.service`), and displays a full-screen QR-code UI so someone nearby can reconfigure credentials.
- The watcher generates three-word hotspot passwords from `/opt/photo-frame/share/wordlist.txt` and writes them to `/run/photo-frame/hotspot.env` for the templated hotspot unit. Passwords appear verbatim in the on-device UI and are never logged.
- `wifi-setter` is a minimal Axum web app bound to `0.0.0.0:80`. It scans for nearby SSIDs, differentiates between known and new connections, and updates NetworkManager in place without duplicating profiles. Its `/api/status` endpoint lets the captive page auto-refresh once the Pi is back online.
- Both binaries honor configuration from `/etc/photo-frame/config.yaml` when present and fall back to the environment variables exposed in their systemd units: `WIFI_IFNAME` (default `wlan0`), `FRAME_USER` (default `frame`), `HOTSPOT_IP` (default `192.168.4.1`). Override them with `systemctl edit` drop-ins if your hardware differs.
- Use `/opt/photo-frame/bin/print-status.sh` to inspect the current connectivity, hotspot state, slideshow target, and sync schedule in one place.

### Photo app orchestration
- `photo-app.target` gates startup of the slideshow, allowing the watcher to start and stop the renderer as Wi-Fi comes and goes. The unit keeps the process under the `frame` user, runs from `/opt/photo-frame`, and uses the existing `config.yaml` path. Do **not** enable `photo-app.service` directly; the watcher starts the target when connectivity is healthy.
- Optional kiosk tweaks—such as autologin to the `frame` user or hiding the cursor—can be layered on later using Raspberry Pi OS’ standard GUI settings. They are documented but intentionally not enforced by these modules.

### Cloud photo sync
- `sync-photos.service` is a oneshot wrapper around `rclone sync`, mirroring the configured remote directly into the live `photo-library-path`. It relies on rclone’s temp-file writes and atomic renames, so the viewer never sees partially copied images.
- The paired timer defaults to `OnCalendar=hourly`; override the cadence by editing `/etc/systemd/system/sync-photos.timer.d/override.conf`, where the setup module writes the initial schedule. Set `RCLONE_REMOTE` (for cloud endpoints) or switch to `SYNC_TOOL=rsync` with `RSYNC_SOURCE` when pulling from another host.
- Required paths come from `config.yaml` when present; if the YAML omits `photo-library-path`, the setup script writes `PHOTO_LIBRARY_PATH` into a drop-in so the service still knows where to sync.

## Quickstart checklist
1. Run the OS bootstrap (`setup/system-setup.sh`) to provision base dependencies, then execute `sudo ./setup/setup.sh` to install the watcher, hotspot, photo-app target, and sync timer.
2. Configure a cloud remote with `rclone config`, then set `RCLONE_REMOTE` (and optional `RCLONE_FLAGS`) via `systemctl edit sync-photos.service`.
3. Reboot or start `wifi-watcher.service`. If Wi-Fi is unavailable, connect to the “Frame-Setup” hotspot, scan the QR code, and submit new credentials through the captive portal.
4. Once online, the photo app launches automatically and `sync-photos.timer` keeps the library mirrored on the cadence defined by `SYNC_SCHEDULE` (default hourly).
