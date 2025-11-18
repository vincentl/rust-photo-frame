# Wi-Fi Manager

The `wifi-manager` crate is the frame's single entry point for Wi-Fi monitoring, hotspot recovery, and captive portal provisioning. It wraps NetworkManager's `nmcli` tooling, spawns the recovery web UI, and persists all operational breadcrumbs under `/var/lib/photo-frame`.

## Capabilities at a glance

- Detects connectivity loss by polling NetworkManager for the interface's connection state.
- Creates or updates an idempotent hotspot profile (`pf-hotspot`) and brings it online with a random three-word passphrase.
- Serves a lightweight HTTP UI for submitting replacement SSID/password credentials, then provisions them via `nmcli`.
- Renders a QR code that points to the recovery portal so phones can jump directly to the setup page.
- Uses Sway IPC to present a fullscreen overlay with hotspot instructions whenever Wi-Fi needs attention.
- Emits structured logs for every state transition (`ONLINE`, `OFFLINE`, `HOTSPOT`, `PROVISIONING`) and stores the last provisioning attempt in JSON.

## Binary layout and subcommands

The release build installs to `/opt/photo-frame/bin/wifi-manager` and exposes the following subcommands:

| Subcommand | Purpose                                                                                                                                        |
| ---------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| `watch`    | Default daemon mode. Monitors connectivity, raises the hotspot/UI when offline, and reconnects when provisioning succeeds.                     |
| `ui`       | Runs only the HTTP UI server. This is spawned automatically by `watch` when the hotspot is active but can be used independently for debugging. |
| `qr`       | Generates `/var/lib/photo-frame/wifi-qr.png`, a QR code pointing to the configured UI URL.                                                     |
| `nm`       | Thin wrapper around `nmcli` operations (`add`, `modify`, `connect`) used internally. Safe to run manually for diagnostics.                     |
| `overlay`  | Renders the on-device recovery overlay window. Invoked automatically by the watcher; exposed for manual testing.
                              |

Running the binary with `--help` or `--version` is permitted as root; all other modes refuse to start if `UID==0` to honour the project's "never run cargo as root" policy.

Overlay presentation runs the overlay window inside the active Sway session using `swaymsg exec …` so it inherits the session's Wayland environment. The watcher discovers Sway's IPC socket by preferring the uid/pid-specific path (e.g. `/run/user/1001/sway-ipc.1001.1069.sock`) and falls back to scanning the runtime dir.

### NetworkManager permissions

`wifi-manager` runs under the unprivileged `kiosk` account, so the setup pipeline installs a dedicated polkit rule (`/etc/polkit-1/rules.d/90-photoframe-nm.rules`) that grants the kiosk group the handful of NetworkManager actions required to add, modify, and activate system Wi-Fi profiles. Without this rule the manual `nm` subcommands will fail with `Insufficient privileges` even though the service is running.

## Configuration reference

The template lives at `/opt/photo-frame/etc/wifi-manager.yaml` and is staged from `setup/assets/app/etc/wifi-manager.yaml`. All keys use kebab-case to match the repository conventions.

```yaml
interface: wlan0
check-interval-sec: 5
offline-grace-sec: 30
wordlist-path: /opt/photo-frame/share/wordlist.txt
var-dir: /var/lib/photo-frame
hotspot:
  connection-id: pf-hotspot
  ssid: PhotoFrame-Setup
  ipv4-addr: 192.168.4.1
ui:
  bind-address: 0.0.0.0
  port: 8080
overlay:
  command:
    - swaymsg
  photo-app-id: photo-frame
  overlay-app-id: wifi-overlay
  # sway-socket: /run/user/1000/sway-ipc.1000.123.sock
```

| Key                            | Description                                                                                      |
| ------------------------------ | ------------------------------------------------------------------------------------------------ |
| `interface`                    | Wireless device monitored for connectivity (default `wlan0`).             |
| `check-interval-sec`           | Base delay between connectivity probes. A small jitter is added internally. |
| `offline-grace-sec`            | Seconds the frame must remain offline before the hotspot is activated.     |
| `wordlist-path`                | Source file for the random three-word hotspot passphrase. Installed via setup. |
| `var-dir`                      | Directory for runtime artifacts (password file, QR PNG, status JSON, temp sockets). |
| `hotspot.connection-id`        | NetworkManager profile name used for the AP. The manager will create or update it automatically. |
| `hotspot.ssid`                 | Broadcast SSID for the recovery hotspot.                                     |
| `hotspot.ipv4-addr`            | Address assigned to the hotspot interface and advertised via DHCP.          |
| `ui.bind-address`              | Bind address for the HTTP UI. Normally `0.0.0.0`.                           |
| `ui.port`                      | HTTP UI port (default `8080`).                                              |
| `overlay.command`              | Executable invoked to render the on-device hotspot instructions (default `swaymsg`; the watcher constructs an `exec …` command to run the overlay inside Sway). |
| `overlay.photo-app-id`         | Sway `app_id` assigned to the photo frame window so it can be re-focused after recovery. |
| `overlay.overlay-app-id`       | Sway `app_id` that the overlay binary advertises; used for focus/teardown commands. |
| `overlay.sway-socket`          | Optional override for the Sway IPC socket. Detected automatically from `/run/user/<uid>` when omitted. |

Whenever you change the config, run `sudo systemctl restart photoframe-wifi-manager.service` for the daemon to pick up the new settings.

## Runtime files

All mutable state lives under `/var/lib/photo-frame` and is owned by the `photo-frame` user (0755 directory permissions). Key files include:

- `hotspot-password.txt` – the currently active random passphrase for **PhotoFrame-Setup**.
- `wifi-qr.png` – QR code pointing to `http://<hotspot-ip>:<port>/`.
- `wifi-last.json` – JSON log of the latest provisioning attempt (inputs masked, result + timestamps recorded).
- `wifi-manager.log` (optional) – when configured, the service can redirect logs here for offline analysis; otherwise use `journalctl`.

## Web provisioning flow

1. The `watch` loop marks the frame `OFFLINE` after `offline-grace-sec` seconds of NetworkManager reporting the interface disconnected.
2. The hotspot profile (`pf-hotspot`) is ensured, then activated on the configured interface with WPA2-PSK security. The watcher simultaneously launches the `wifi-manager overlay` subcommand via Sway IPC and brings the web UI online so the on-device instructions, QR code, and portal stay in sync.
3. A random three-word passphrase is selected from the bundled wordlist and written to `/var/lib/photo-frame/hotspot-password.txt`.
4. The QR code generator produces `/var/lib/photo-frame/wifi-qr.png`, embedding the configured UI URL (default `http://192.168.4.1:8080/`).
5. The HTTP UI binds to `0.0.0.0:<port>` and serves:
   - `GET /` – single-page HTML form for SSID + password entry with inline guidance and QR instructions.
   - `POST /submit` – validates inputs, uses `nmcli` to add/modify the connection, and reports progress.
   - A status polling endpoint so the UI can reflect provisioning progress in near real time.
6. On success, the hotspot is torn down only after NetworkManager confirms association and DHCP success on the new SSID. At that point the watcher hides the overlay, refocuses the photo frame window, and continues monitoring online. Failures leave the hotspot active for another attempt.
7. Results (masked SSID, status, timestamps) are appended to `wifi-last.json` for later support review.

## Setup automation

The Wi-Fi manager is wired into the refreshed setup pipeline:

- `setup/system/modules/10-apt-packages.sh` pulls in NetworkManager, Sway, GPU drivers, and build prerequisites.
- `setup/system/modules/20-rust.sh` installs the system-wide Rust toolchain used to build the binaries under `/opt/photo-frame`.
- `setup/system/modules/40-kiosk-user.sh` provisions the kiosk user, runtime directories, and polkit rule that unlocks NetworkManager access for `kiosk`.
- `setup/system/modules/50-greetd.sh` installs the Sway session wrapper greetd launches on tty1.
- `setup/system/modules/60-systemd.sh` installs and enables `/etc/systemd/system/photoframe-wifi-manager.service` alongside the other kiosk units once the binaries exist.
- `setup/application/modules/10-build.sh` compiles `wifi-manager` in release mode as the invoking user (never root).
- `setup/application/modules/20-stage.sh` stages the binary, config template, wordlist, and docs.
- `setup/application/modules/30-install.sh` installs artifacts into `/opt/photo-frame` and seeds `/etc/photo-frame/config.yaml` if missing.

Re-running the scripts is idempotent: binaries are replaced in place, configs are preserved, ACLs stay intact, and systemd units reload cleanly.

## Service management

Common operational commands:

```bash
# Tail live logs
journalctl -u photoframe-wifi-manager.service -f

# Restart the watcher after editing the config
sudo systemctl restart photoframe-wifi-manager.service

# Check summary status (hotspot profile, active connection, artifacts)
/opt/photo-frame/bin/print-status.sh

# Manually seed a connection via the helper subcommand (requires the polkit rule)
sudo -u kiosk /opt/photo-frame/bin/wifi-manager nm add --ssid "HomeWiFi" --psk "correct-horse-battery-staple"

# Force the recovery hotspot for testing
sudo nmcli connection up pf-hotspot

# Simulate a bad PSK without losing your SSH session
sudo nohup bash developer/suspend-wifi.sh wlan0 >/tmp/wifi-test.log 2>&1 & disown
```

The helper script stashes the active profile's keyfile, swaps in a Wi-Fi
connection with a deliberately wrong PSK, and tries to activate it. Run it
from a terminal multiplexer or with `nohup` as shown so the process
survives your SSH session dropping when the interface goes offline. Once
NetworkManager rejects the credentials you can tail the service logs and
watch for the `ONLINE → OFFLINE → HOTSPOT` transition.

The systemd unit is defined in `assets/systemd/photoframe-wifi-manager.service` and runs under the `kiosk` user with `Restart=on-failure`. It depends on `network-online.target` so that the first connectivity check happens after boot networking is ready.

Notes on window focus and app_id:

- The slideshow is launched with `WINIT_APP_ID=photo-frame` by the `/usr/local/bin/photo-frame` wrapper so Sway rules and refocus from `wifi-manager` work consistently.
- The overlay advertises `app_id=wifi-overlay` and the watcher uses Sway IPC to focus and fullscreen it while the hotspot is active, then restores focus to the slideshow when connectivity returns.

## Acceptance test checklist

These behavioural checks validate the full provisioning lifecycle:

1. **Cold boot with no Wi-Fi configured** – within 30–60 seconds the hotspot appears, the QR code launches the UI, and submitting valid credentials connects the frame to the new network while shutting down the hotspot.
2. **Incorrect stored password** – when the saved SSID fails to authenticate, the service transitions through `OFFLINE → HOTSPOT`, accepts updated credentials, and resumes watching online once they succeed.
3. **Boot with valid Wi-Fi** – the hotspot never starts; `wifi-manager` logs remain in the `ONLINE` state while monitoring.
4. **Service restarts** – `systemctl restart photoframe-wifi-manager.service` leaves no stray hotspot or UI processes; monitoring resumes cleanly.
5. **Idempotent setup** – rerunning the setup scripts rebuilds and reinstalls `wifi-manager` without invoking `cargo` as root and without duplicating systemd units.

## Troubleshooting

- **Hotspot never appears:** confirm `journalctl -u photoframe-wifi-manager.service` shows the `OFFLINE → HOTSPOT` transition. If not, verify the interface name in the config matches the Pi's Wi-Fi adapter (often `wlan0`).
- **Portal unreachable:** ensure the UI port is free (`sudo lsof -iTCP:8080 -sTCP:LISTEN`). The daemon logs whenever it binds the HTTP server.
- **Overlay never appears:** verify Sway IPC is reachable from the service (`SWAYSOCK` points to `sway-ipc.<uid>.<pid>.sock`). Also ensure at least one system font is installed (e.g. `fonts-dejavu-core` or `fonts-noto-core`) — the overlay refuses to start without a font. For a manual check (ensure the glob expands inside the kiosk shell):

  ```bash
  sudo -u kiosk sh -lc '
    SWAYSOCK="$(find "$XDG_RUNTIME_DIR" -maxdepth 1 -type s -name "sway-ipc.*.sock" -print -quit)"
    swaymsg -s "$SWAYSOCK" exec "env WINIT_APP_ID=wifi-overlay /opt/photo-frame/bin/wifi-manager overlay --ssid Test --password-file /var/lib/photo-frame/hotspot-password.txt --ui-url http://192.168.4.1:8080/"
  '
  ```
- **Provisioning fails repeatedly:** inspect `/var/lib/photo-frame/wifi-last.json` for masked SSIDs and error codes. Run `sudo -u kiosk /opt/photo-frame/bin/wifi-manager nm add --ssid <name> --psk <pass>` manually to confirm NetworkManager feedback (if this reports `Insufficient privileges`, re-run the kiosk provisioning script to reinstall the polkit rule).
- **Wordlist missing:** rerun `setup/application/modules/20-stage.sh` and `30-install.sh` to restore `/opt/photo-frame/share/wordlist.txt`; the manager refuses to start without it so that hotspot passwords are never empty.

With the Wi-Fi manager in place, the frame can recover from outages autonomously and guide users through reconnecting without opening the enclosure or attaching keyboards.

## Disable permanently

If you don’t want the Wi‑Fi manager to ever start on an existing install, disable and mask its systemd unit. Masking ensures it stays off even if you rerun the setup scripts (which try to enable it):

```bash
# Stop the service if it’s running
sudo systemctl stop photoframe-wifi-manager.service

# Disable at boot
sudo systemctl disable photoframe-wifi-manager.service

# Prevent any start (including from installers or dependencies)
sudo systemctl mask photoframe-wifi-manager.service

# Optional: shut down and remove the recovery hotspot profile
sudo nmcli connection down pf-hotspot || true
sudo nmcli connection delete pf-hotspot || true

# Verify status
systemctl is-enabled photoframe-wifi-manager.service   # masked
systemctl is-active photoframe-wifi-manager.service    # inactive
```

To re‑enable later:

```bash
sudo systemctl unmask photoframe-wifi-manager.service
sudo systemctl enable --now photoframe-wifi-manager.service
```

Notes:

- The setup pipeline (`setup/system/modules/60-systemd.sh`) enables this unit when present, but a masked unit will not start. Leaving it masked keeps it off across upgrades.
- Removing the NetworkManager polkit rule is optional; it is harmless to keep. If desired: `sudo rm -f /etc/polkit-1/rules.d/90-photoframe-nm.rules`.
