# Wi-Fi Manager

The `wifi-manager` crate is the frame's single entry point for Wi-Fi monitoring, hotspot recovery, and captive portal provisioning. It wraps NetworkManager's `nmcli` tooling, spawns the recovery web UI, and persists all operational breadcrumbs under `/var/lib/photo-frame`.

This document is the implementation/reference guide. For fresh-install validation use [`software.md`](software.md), for full QA coverage use [`../developer/test-plan.md`](../developer/test-plan.md), and for incident triage use [`sop.md`](sop.md).

## Fast path

Use this sequence for a quick sanity check after changing Wi-Fi logic:

1. Verify watcher health:
   - `sudo systemctl status photoframe-wifi-manager.service`
   - `/opt/photo-frame/bin/print-status.sh`
2. Confirm configuration values in `/opt/photo-frame/etc/wifi-manager.yaml` and restart watcher:
   - `sudo systemctl restart photoframe-wifi-manager.service`
3. Run the fresh-install acceptance flow:
   - [`software.md#fresh-install-wi-fi-recovery-test`](software.md#fresh-install-wi-fi-recovery-test)
4. If it fails, follow:
   - [`sop.md#wi-fi-failure-triage`](sop.md#wi-fi-failure-triage)
5. For deeper operational commands and troubleshooting:
   - [`wifi-manager-operations.md`](wifi-manager-operations.md)

## Capabilities at a glance

- Detects connectivity loss by polling NetworkManager for the interface's connection state.
- Treats Wi-Fi as online when the interface is associated to an infrastructure SSID (link-level), without requiring internet reachability.
- Creates or updates an idempotent hotspot profile (`pf-hotspot`) and brings it online with a random three-word passphrase.
- Serves a lightweight HTTP UI for submitting replacement SSID/password credentials by writing an ephemeral provisioning request file for the watcher.
- Renders a QR code that points to the recovery portal so phones can jump directly to the setup page.
- Uses Sway IPC to present a fullscreen overlay with hotspot instructions whenever Wi-Fi needs attention, and can hand off by stopping/relaunching the photo app.
- Emits structured logs for deterministic watcher states (`Online`, `OfflineGrace`, `RecoveryHotspotActive`, `ProvisioningAttempt`, `RecoveryBackoff`) and stores state/attempt records in JSON.

## Binary layout and subcommands

The release build installs to `/opt/photo-frame/bin/wifi-manager` and exposes the following subcommands:

| Subcommand | Purpose                                                                                                                                        |
| ---------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| `watch`    | Default daemon mode. Monitors connectivity, raises the hotspot/UI when offline, and reconnects when provisioning succeeds.                     |
| `ui`       | Runs only the HTTP UI server. This is spawned automatically by `watch` when the hotspot is active but can be used independently for debugging. |
| `qr`       | Generates `/var/lib/photo-frame/wifi-qr.png`, a QR code pointing to the configured UI URL.                                                     |
| `nm`       | Thin wrapper around `nmcli` operations (`add`, `modify`, `connect`) used internally. Safe to run manually for diagnostics.                     |
| `overlay`  | Renders the on-device recovery overlay window. Invoked automatically by the watcher; exposed for manual testing.                                |

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
recovery-mode: app-handoff
recovery-reconnect-probe-sec: 60
recovery-connect-timeout-sec: 20
wordlist-path: /opt/photo-frame/share/wordlist.txt
var-dir: /var/lib/photo-frame
hotspot:
  connection-id: pf-hotspot
  ssid: PhotoFrame-Setup
  ipv4-addr: 192.168.4.1
ui:
  bind-address: 0.0.0.0
  port: 8080
photo-app:
  launch-command:
    - /usr/local/bin/photo-frame
    - /etc/photo-frame/config.yaml
  app-id: photo-frame
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
| `recovery-mode`                | Recovery strategy: `app-handoff` (default) stops/relaunches photo app, `overlay` keeps slideshow running under overlay. |
| `recovery-reconnect-probe-sec` | Seconds between auto-reconnect probes while hotspot mode is active. |
| `recovery-connect-timeout-sec` | Maximum wait time for infrastructure association when applying credentials or running reconnect probes. |
| `wordlist-path`                | Source file for the random three-word hotspot passphrase. Installed via setup. |
| `var-dir`                      | Directory for runtime artifacts (password file, QR PNG, status JSON, temp sockets). |
| `hotspot.connection-id`        | NetworkManager profile name used for the AP. The manager will create or update it automatically. |
| `hotspot.ssid`                 | Broadcast SSID for the recovery hotspot.                                     |
| `hotspot.ipv4-addr`            | Address assigned to the hotspot interface and advertised via DHCP.          |
| `ui.bind-address`              | Bind address for the HTTP UI. Normally `0.0.0.0`.                           |
| `ui.port`                      | HTTP UI port (default `8080`).                                              |
| `photo-app.launch-command`     | Command launched inside Sway after recovery completes in `app-handoff` mode. |
| `photo-app.app-id`             | Sway `app_id` for the slideshow window that watcher kill/relaunch targets. |
| `overlay.command`              | Executable invoked to render the on-device hotspot instructions (default `swaymsg`; the watcher constructs an `exec …` command to run the overlay inside Sway). |
| `overlay.photo-app-id`         | Sway `app_id` assigned to the photo frame window so it can be re-focused after recovery. |
| `overlay.overlay-app-id`       | Sway `app_id` that the overlay binary advertises; used for focus/teardown commands. |
| `overlay.sway-socket`          | Optional override for the Sway IPC socket. Detected automatically from `/run/user/<uid>` when omitted. |

Whenever you change the config, run `sudo systemctl restart photoframe-wifi-manager.service` for the daemon to pick up the new settings.

## Runtime files

All mutable state lives under `/var/lib/photo-frame` and is owned by the `kiosk` user (0755 directory permissions). Key files include:

- `hotspot-password.txt` – the currently active random passphrase for **PhotoFrame-Setup**.
- `wifi-qr.png` – QR code pointing to `http://<hotspot-ip>:<port>/`.
- `wifi-request.json` – ephemeral credential request written by `POST /submit` and consumed by the watcher (mode `0600`).
- `wifi-last.json` – JSON log of the latest provisioning attempt (inputs masked, result + timestamps recorded).
- `wifi-state.json` – watcher state record (`state`, `reason`, optional `attempt_id`) for operator diagnostics.
- `wifi-manager.log` (optional) – when configured, the service can redirect logs here for offline analysis; otherwise use `journalctl`.

## Web provisioning flow

1. The `watch` loop transitions `Online → OfflineGrace` after NetworkManager reports the interface disconnected. If connectivity remains down for `offline-grace-sec`, it enters `RecoveryHotspotActive`.
2. The hotspot profile (`pf-hotspot`) is ensured, then activated on the configured interface with WPA2-PSK security. The watcher launches the `wifi-manager overlay` subcommand via Sway IPC and brings the web UI online so the on-device instructions, QR code, and portal stay in sync.
3. A random three-word passphrase is selected from the bundled wordlist and written to `/var/lib/photo-frame/hotspot-password.txt`.
4. The QR code generator produces `/var/lib/photo-frame/wifi-qr.png`, embedding the configured UI URL (default `http://192.168.4.1:8080/`).
5. The HTTP UI binds to `0.0.0.0:<port>` and serves:
   - `GET /` – single-page HTML form for SSID + password entry with inline guidance and QR instructions.
   - `POST /submit` – validates inputs and writes `wifi-request.json`; the watcher consumes the request and performs `nmcli` operations.
   - A status polling endpoint so the UI can reflect provisioning progress in near real time.
6. When a request exists, watcher transitions `RecoveryHotspotActive → ProvisioningAttempt`, applies credentials, temporarily brings hotspot down, and waits up to `recovery-connect-timeout-sec` for infrastructure association.
7. On success, watcher finalizes recovery (`ProvisioningAttempt → Online`), hides overlay, and in `app-handoff` mode relaunches the photo app via `photo-app.launch-command`.
8. On failure, watcher restores hotspot and transitions through `RecoveryBackoff` before returning to `RecoveryHotspotActive` for retry.
9. While hotspot mode is active and no request is pending, watcher runs reconnect probes every `recovery-reconnect-probe-sec` to recover automatically when the original AP comes back.

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

## Validation entry points

- Fresh-install acceptance flow: [`software.md#fresh-install-wi-fi-recovery-test`](software.md#fresh-install-wi-fi-recovery-test)
- Full Wi-Fi validation matrix (release QA): [`../developer/test-plan.md#phase-7--wi-fi-provisioning--watcher`](../developer/test-plan.md#phase-7--wi-fi-provisioning--watcher)
- Day-2 failure triage: [`sop.md#wi-fi-failure-triage`](sop.md#wi-fi-failure-triage)

## Advanced notes

For service commands, deep troubleshooting, and disable/reenable procedures, use [`wifi-manager-operations.md`](wifi-manager-operations.md).
