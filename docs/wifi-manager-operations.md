# Wi-Fi Manager Operations

This runbook covers operational commands and troubleshooting for the deployed `wifi-manager` service.

- Architecture and config reference: [`wifi-manager.md`](wifi-manager.md)
- Day-2 triage summary: [`sop.md`](sop.md)
- Advanced Sway/overlay debugging: [`../developer/kiosk-debug.md`](../developer/kiosk-debug.md)
- Fresh-install acceptance flow: [`software.md#fresh-install-wi-fi-recovery-test`](software.md#fresh-install-wi-fi-recovery-test)
- Full QA matrix: [`../developer/test-plan.md#phase-7--wi-fi-provisioning--watcher`](../developer/test-plan.md#phase-7--wi-fi-provisioning--watcher)

## Fast path

Run this quick sequence after changing Wi-Fi recovery logic:

```bash
sudo systemctl status photoframe-wifi-manager.service
/opt/photo-frame/bin/print-status.sh
sudo systemctl restart photoframe-wifi-manager.service
sudo journalctl -u photoframe-wifi-manager.service -f
```

## Service management

Common operational commands:

```bash
# Tail live logs
journalctl -u photoframe-wifi-manager.service -f

# Restart watcher after editing config
sudo systemctl restart photoframe-wifi-manager.service

# Check summary status (hotspot profile, active connection, artifacts)
/opt/photo-frame/bin/print-status.sh

# Manually seed a connection via helper subcommand (requires polkit rule)
sudo -u kiosk /opt/photo-frame/bin/wifi-manager nm add --ssid "HomeWiFi" --psk "correct-horse-battery-staple"

# Force recovery hotspot for testing
sudo nmcli connection up pf-hotspot

# Simulate a bad PSK without losing your SSH session
sudo nohup bash developer/suspend-wifi.sh wlan0 >/tmp/wifi-test.log 2>&1 & disown
```

The helper script stashes the active profile keyfile, swaps in a Wi-Fi connection with a deliberately wrong PSK, and tries to activate it. Run it from a multiplexer or with `nohup` so it survives SSH drops when the interface goes offline.

The systemd unit is `assets/systemd/photoframe-wifi-manager.service` and runs as `kiosk` with `Restart=on-failure`, after `network-online.target`.

## Triage checklist

When recovery is stuck, run this in order:

1. Snapshot service state:
   - `sudo systemctl status photoframe-wifi-manager.service`
   - `/opt/photo-frame/bin/print-status.sh`
2. Inspect persisted state:
   - `sudo cat /var/lib/photo-frame/wifi-state.json`
   - `sudo cat /var/lib/photo-frame/wifi-last.json`
   - `sudo ls -l /var/lib/photo-frame/wifi-request.json`
3. Check NetworkManager:
   - `nmcli dev status`
   - `nmcli connection show --active`
4. Confirm Sway socket exists:
   - `sudo sh -lc 'uid=$(id -u kiosk); ls "/run/user/$uid"/sway-ipc.*.sock'`
5. Validate manual credential apply path:
   - `sudo -u kiosk /opt/photo-frame/bin/wifi-manager nm add --ssid "<ssid>" --psk "<password>"`

## Troubleshooting

### Hotspot never appears

- Confirm logs show `Online -> OfflineGrace -> RecoveryHotspotActive`.
- Verify configured interface name (usually `wlan0`).

### Portal unreachable

- Check UI port usage: `sudo lsof -iTCP:8080 -sTCP:LISTEN`.

### Overlay never appears

- Verify Sway IPC socket exists for `kiosk`.
- Ensure at least one system font is installed (`fonts-dejavu-core` or `fonts-noto-core`).
- Run a manual overlay launch check:

```bash
sudo sh -lc '
  RUNDIR="/run/user/$(id -u kiosk)";
  SWAYSOCK="$(find "$RUNDIR" -maxdepth 1 -type s -name "sway-ipc.*.sock" -print -quit)";
  [ -S "$SWAYSOCK" ] || { echo "No Sway IPC socket for kiosk (is greetd/Sway running?)" >&2; exit 1; };
  sudo -u kiosk SWAYSOCK="$SWAYSOCK" swaymsg -s "$SWAYSOCK" exec "env WINIT_APP_ID=wifi-overlay /opt/photo-frame/bin/wifi-manager overlay --ssid Test --password-file /var/lib/photo-frame/hotspot-password.txt --ui-url http://192.168.4.1:8080/"
'
```

If `/run/user/$(id -u kiosk)` does not exist:

- Start/verify greetd: `sudo systemctl status greetd`
- Enable lingering: `sudo loginctl enable-linger kiosk`

### Provisioning fails repeatedly

- Inspect `/var/lib/photo-frame/wifi-last.json` and `/var/lib/photo-frame/wifi-state.json`.
- Run manual credential apply path:
  - `sudo -u kiosk /opt/photo-frame/bin/wifi-manager nm add --ssid <name> --psk <pass>`
- If it reports `Insufficient privileges`, re-run provisioning to reinstall the polkit rule.

### Wordlist missing

- Re-run `setup/application/modules/20-stage.sh` and `setup/application/modules/30-install.sh` to restore `/opt/photo-frame/share/wordlist.txt`.

## Disable permanently

If you do not want `wifi-manager` to start, disable and mask it:

```bash
# Stop if running
sudo systemctl stop photoframe-wifi-manager.service

# Disable at boot
sudo systemctl disable photoframe-wifi-manager.service

# Prevent any start (including setup scripts)
sudo systemctl mask photoframe-wifi-manager.service

# Optional: remove recovery hotspot profile
sudo nmcli connection down pf-hotspot || true
sudo nmcli connection delete pf-hotspot || true

# Verify status
systemctl is-enabled photoframe-wifi-manager.service   # masked
systemctl is-active photoframe-wifi-manager.service    # inactive
```

Re-enable later:

```bash
sudo systemctl unmask photoframe-wifi-manager.service
sudo systemctl enable --now photoframe-wifi-manager.service
```

Notes:

- `setup/system/modules/60-systemd.sh` enables this unit when present, but masked units stay off across upgrades.
- Removing the NetworkManager polkit rule is optional. If needed:
  - `sudo rm -f /etc/polkit-1/rules.d/90-photoframe-nm.rules`
