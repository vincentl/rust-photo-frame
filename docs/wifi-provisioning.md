# Wi-Fi Provisioning and Hotspot Workflow

The `codex/refactor-wi-fi-provisioning-codebase` branch refactors Wi-Fi onboarding into a set of
services that cooperate with NetworkManager. This document explains how the pieces fit together, the
packages the setup pipeline installs, and what to do when you need to diagnose connectivity issues.

## Architecture overview

The provisioning stack is composed of three systemd units plus a small status helper:

| Component | Purpose | Unit / Path |
|-----------|---------|-------------|
| **Wi-Fi watcher** | Monitors NetworkManager connectivity, starts/stops the hotspot, launches the UI, and restarts the slideshow when Wi-Fi returns. | `wifi-watcher.service` |
| **Wi-Fi setter** | Serves the captive portal-style web UI and applies SSID/PSK selections via NetworkManager. | `wifi-setter.service` |
| **Hotspot** | Brings up a temporary access point with a human-friendly passphrase when Wi-Fi is unavailable. | `wifi-hotspot@<ifname>.service` |
| **Status helper** | Prints the connectivity summary, service states, and sync timers for quick audits. | `/opt/photo-frame/bin/photo-frame-status` |

The watcher runs as root so it can manipulate NetworkManager, then spawns an egui UI as the frame
user when the hotspot is active. The setter runs independently and listens on `http://192.168.4.1/`
by default. Both binaries share the same configuration file that the installer copies to
`/opt/photo-frame/var/config.yaml` (override the path with `INSTALL_ROOT=/custom/path` when running the
setup scripts).

## Setup automation

The provisioning refactor integrates with the standard `setup/system` and `setup/app` stages:

- `setup/system/modules/10-packages.sh` installs NetworkManager, dnsmasq, and iptables so that
  `nmcli` can host an access point.
- `setup/system/modules/40-network-manager.sh` drops a NetworkManager configuration snippet,
  disables `dhcpcd`/`wpa_supplicant`, and restarts the new service. Raspberry Pi OS leaves those
  daemons enabled by default; masking them avoids conflicts when NetworkManager claims `wlan0`.
- `setup/app/modules/10-build.sh` builds three binaries (`rust-photo-frame`, `wifi-watcher`,
  `wifi-setter`) into a shared `target` directory so the stage module can package them together.
- `setup/app/modules/20-stage.sh` now stages the Wi-Fi binaries, the hotspot wordlist, documentation,
  and the `photo-frame-status` helper into the install tree.
- `setup/app/modules/40-systemd.sh` installs and enables the new service units so the watcher boots
  automatically with the slideshow.

If you rerun the setup scripts after editing configuration or upgrading the codebase, the steps are
idempotent—existing NetworkManager config files and staged artifacts are replaced in-place.

## What happens when Wi-Fi drops?

1. `wifi-watcher.service` polls NetworkManager. When connectivity drops below `full` the watcher:
   - Stops the `photo-frame.service` slideshow to free GPU resources.
   - Creates `/run/photo-frame/hotspot.env` with a randomly generated passphrase drawn from
     `/opt/photo-frame/share/wordlist.txt`.
   - Starts `wifi-hotspot@wlan0.service`, which brings up an access point (default SSID `Frame-Setup`).
   - Starts `wifi-setter.service` and launches the on-device UI as the frame user so viewers can scan
     a QR code or read the hotspot credentials directly.
2. When the frame reconnects to a real network, the watcher stops the hotspot and setter services,
   clears the temporary environment file, writes `/run/photo-frame/wifi_up`, and restarts the
   `photo-frame.service` slideshow.

## Using the provisioning UI

1. From a phone or laptop, join the `Frame-Setup` network (or the SSID configured in
   `hotspot-ssid`). The passphrase rotates every time the hotspot starts; read it from the on-device
   UI or by running `sudo /opt/photo-frame/bin/photo-frame-status` over SSH.
2. Navigate to `http://192.168.4.1/` (replace the IP if you customized `hotspot-ip`). The page lists
   nearby SSIDs and lets you submit a password.
3. After you submit credentials the setter updates or creates a NetworkManager connection. The
   watcher notices connectivity, stops the hotspot, and restarts the slideshow. The status helper will
   report `wifi-watcher.service: active` and `Hotspot (wlan0): inactive` once the frame rejoins your
   main network.

## Customising provisioning

- **Configuration file:** Add overrides for `wifi-ifname`, `hotspot-ssid`, and `hotspot-ip` to the
  writable config at `/opt/photo-frame/var/config.yaml`. These values are read by both the watcher and
  the setter on startup.
- **Environment variables:** Adjust behaviour per-boot via `systemctl edit` to set environment
  overrides such as `WIFI_IFNAME`, `HOTSPOT_IP`, `HOTSPOT_WORDLIST`, or
  `PHOTO_FRAME_SERVICE_UNIT`. The watcher honours these variables before falling back to defaults.
- **Hotspot wordlist:** Replace `/opt/photo-frame/share/wordlist.txt` with your own lower-case word list
  to customise generated passphrases.

## Troubleshooting

- `/opt/photo-frame/bin/photo-frame-status` prints the active Wi-Fi connection, hotspot state, and
  service health. Run it with `sudo` so it can query systemd and NetworkManager.
- `journalctl -u wifi-watcher.service` shows detailed logs about hotspot creation, credential
  application attempts, and UI launch failures.
- `nmcli device status` confirms that `wlan0` is managed by NetworkManager; if it shows `unmanaged`,
  re-run `./setup/system/run.sh` to reapply the configuration snippet.
- If the hotspot refuses to start, confirm that `dhcpcd` is masked and that no other services own the
  Wi-Fi interface: `sudo systemctl status dhcpcd.service wpa_supplicant.service`.
- To test the setter API manually, POST JSON to `http://192.168.4.1/apply` with `ssid` and
  `password` keys. The service logs each request with structured context.

With these components wired into the main setup pipeline, the branch is now mergeable with `main`
without carrying a separate documentation tree or bespoke setup instructions.
